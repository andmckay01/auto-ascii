#!/usr/bin/env python3
"""tools/soak.py — M5 resize-storm soak harness (PLAN §7 M5 item A).

Forks the release `auto-ascii-player` onto a fresh pty via `pty.fork()` — the
pty becomes the child's *controlling* terminal, so TIOCSWINSZ on the master
delivers real SIGWINCHes to the player, exactly like a user dragging a
terminal corner — then plays the asset with `--loop` and storms randomized
resizes at it for `--duration` seconds (default 3600).

What it does, continuously, from one single-threaded select loop:

  * every 50–200 ms (uniform): TIOCSWINSZ to a random size in
    20x6..500x140, with an ~8% chance of the sub-minimum 5x3 (below the
    32x9 floor -> exercises the "enlarge terminal" card path);
  * drains the master pty into a rotation-capped log: the FIRST 2 MB go to
    `head.log`, the LAST 10 MB are kept in a ring and flushed to `tail.log`
    every 30 s and at exit (disk usage stays bounded no matter how many
    GB the player emits over an hour);
  * every 10 s: samples the player's VmRSS from /proc/<pid>/status into
    `rss.csv` (unix_ts,elapsed_s,rss_kb);
  * logs every resize into `resizes.csv` and a progress line each minute.

At the deadline it writes a literal `q` to the pty (the player's quit
key), drains until EOF, and records the exit status. Escalation if the
player ignores `q`: SIGTERM after 15 s, SIGKILL after 20 s — both count
as failures. `summary.json` records exit status, byte totals, resize
counts, whether the RESTORE_SEQ bytes (`ESC[0m ESC[?25h ESC[?7h
ESC[?1049l`, auto-ascii-term/src/restore.rs) appear in the tail, a post-warmup
least-squares RSS slope in MB/h, a short escaped tail preview, and the
**structural escape-stream check** (M5 acceptance A: "no desync in
captured output — final frames still parse as valid escape streams"):
both `head.log` and the tail ring are run through a strict VT parser
(`check_escape_stream`) that accepts EXACTLY what the player is specified
to emit — the probe volley, the session enter/restore CSI modes, CUP
within the storm's size bounds, well-formed tier SGRs, the ?2026 wrap and
printable/UTF-8 ground text — and reports anything else (truncated CSI,
out-of-bounds CUP, stray control bytes) as a structural error. A
diff-baseline desync that never crashes the player is caught here, not
just by the exit code.

Harness exit code: 0 = ran the full duration, player exited 0 on `q`, the
restore bytes were seen, and both captured logs passed the structural
check; nonzero otherwise (see `fail_reasons` in summary.json). The
RSS-slope acceptance (< 1 MB/h after warmup, PLAN §7 M5) is *reported*,
not gated here — the hour-long evidence in rss.csv is evaluated by the
reviewer.

Standalone modes:

    tools/soak.py --check-logs DIR   # re-run the structural check over an
                                     # existing outdir's head.log/tail.log
    tools/soak.py --self-test        # validator self-checks (good stream
                                     # passes; corrupted streams are caught)

Smoke mode (~1 min sanity check of the harness itself):

    tools/soak.py --duration 60 --asset assets/clip.ascii --outdir /tmp/soak-smoke

Full soak, detached:

    setsid nohup tools/soak.py --duration 3600 --asset assets/clip.ascii \
        --outdir runs/soak-1h > runs/soak-1h/harness.out 2>&1 &

Python 3.8+ stdlib only. The player binary is NOT built here — build it
first: `cargo build --release -p auto-ascii --features bin`.
"""

from __future__ import annotations

import argparse
import fcntl
import json
import os
import pty
import random
import select
import signal
import struct
import sys
import termios
import time
from collections import deque
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Resize storm parameters (PLAN §7 M5 item A).
RESIZE_MIN_S = 0.050
RESIZE_MAX_S = 0.200
COLS_RANGE = (20, 500)
ROWS_RANGE = (6, 140)
SUBMIN_SIZE = (5, 3)  # below the 32x9 viewport floor -> "enlarge" card
SUBMIN_PROB = 0.08
INITIAL_SIZE = (120, 40)

# Log rotation: keep the first 2 MB + the last 10 MB of pty output.
HEAD_CAP = 2 * 1024 * 1024
TAIL_CAP = 10 * 1024 * 1024
TAIL_FLUSH_IVL_S = 30.0

RSS_IVL_S = 10.0
PROGRESS_IVL_S = 60.0
QUIT_GRACE_S = 15.0
KILL_GRACE_S = 5.0

# auto-ascii-term/src/restore.rs RESTORE_SEQ: SGR reset, cursor show, autowrap on,
# leave alt screen. Emitted by shutdown/atexit/signal/Drop paths.
RESTORE_SEQ = b"\x1b[0m\x1b[?25h\x1b[?7h\x1b[?1049l"

ABORT = False


def _on_signal(signum: int, _frame) -> None:
    global ABORT
    ABORT = True


class OutputLog:
    """First-2MB + last-10MB rotation over an unbounded pty byte stream.

    The head is streamed straight to `head.log` until full. The tail is an
    in-memory chunk ring (bounded at ~TAIL_CAP) rewritten atomically to
    `tail.log` every TAIL_FLUSH_IVL_S — bounding *disk writes* too, instead
    of funneling the player's full multi-GB/h output through the disk.
    """

    def __init__(self, outdir: Path):
        self.head_path = outdir / "head.log"
        self.tail_path = outdir / "tail.log"
        self._head = open(self.head_path, "wb")
        self._head_left = HEAD_CAP
        self._ring: deque[bytes] = deque()
        self._ring_len = 0
        self.total = 0

    def append(self, data: bytes) -> None:
        self.total += len(data)
        if self._head_left > 0:
            take = data[: self._head_left]
            self._head.write(take)
            self._head_left -= len(take)
            if self._head_left == 0:
                self._head.flush()
        self._ring.append(data)
        self._ring_len += len(data)
        # Trim whole chunks while the ring stays >= TAIL_CAP without them.
        while self._ring and self._ring_len - len(self._ring[0]) >= TAIL_CAP:
            self._ring_len -= len(self._ring.popleft())

    def tail_bytes(self) -> bytes:
        return b"".join(self._ring)[-TAIL_CAP:]

    def flush_tail(self) -> None:
        tmp = self.tail_path.with_suffix(".log.tmp")
        with open(tmp, "wb") as f:
            f.write(self.tail_bytes())
        os.replace(tmp, self.tail_path)

    def close(self) -> None:
        self._head.flush()
        self._head.close()
        self.flush_tail()


def set_winsize(fd: int, cols: int, rows: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def read_rss_kb(pid: int) -> int | None:
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])  # kB
    except OSError:
        pass
    return None


def child_alive(pid: int) -> tuple[bool, int | None]:
    """Non-blocking reap. Returns (alive, raw_waitstatus_or_None)."""
    try:
        wpid, status = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        return False, None
    if wpid == 0:
        return True, None
    return False, status


def drain(master: int, log: OutputLog) -> bool:
    """Read everything currently buffered. Returns True on EOF/EIO."""
    while True:
        try:
            data = os.read(master, 65536)
        except BlockingIOError:
            return False
        except OSError:
            # EIO: slave side fully closed (Linux pty semantics) -> EOF.
            return True
        if not data:
            return True
        log.append(data)


def rss_slope_mb_per_h(samples: list[tuple[float, int]], warmup_s: float) -> float | None:
    """Least-squares slope of RSS over elapsed time, post-warmup, in MB/h."""
    pts = [(t, kb) for t, kb in samples if t >= warmup_s]
    if len(pts) < 2:
        return None
    n = len(pts)
    mt = sum(t for t, _ in pts) / n
    mr = sum(kb for _, kb in pts) / n
    num = sum((t - mt) * (kb - mr) for t, kb in pts)
    den = sum((t - mt) ** 2 for t, _ in pts)
    if den == 0:
        return None
    kb_per_s = num / den
    return kb_per_s * 3600.0 / 1024.0


# ---------------------------------------------------------------------------
# Structural escape-stream check (M5 acceptance A: "no panics/desync in
# captured output — final frames still parse as valid escape streams").
#
# A strict VT parser over the captured pty bytes, in the spirit of the
# byte-exact interpreter in crates/auto-ascii/tests/scrub_overlay.rs: it
# accepts EXACTLY the sequences the player is specified to emit and reports
# anything else. The player's full output vocabulary (auto-ascii-term/src/{ansi,
# render,restore,probe}.rs):
#
#   * probe volley:  CSI > 0 q | CSI ? 2026 $ p | DCS + q 524742 ST |
#                    CSI 16 t | CSI c   (one write, start of stream)
#   * session enter: CSI ? 1049 h, CSI ? 25 l, CSI ? 7 l
#   * frames:        CUP `CSI row ; col H` (1-based, bounded by the storm's
#                    max size), SGR runs (truecolor 38/48;2;R;G;B, 256-color
#                    38/48;5;N, 16-color 30-37/40-47/90-97/100-107, reset 0),
#                    optional CSI ? 2026 h ... l wrap, glyphs as printable
#                    ASCII / UTF-8
#   * restore:       CSI 0 m, CSI ? 25 h, CSI ? 7 h, CSI ? 1049 l
#
# A desync (truncated CSI, CUP outside any size the storm ever set, stray
# control bytes, malformed SGR) is a structural error even when the player
# survives it — exactly the failure mode the exit code cannot see.
# ---------------------------------------------------------------------------

# No storm size ever exceeds these; a CUP beyond them is desync evidence
# even without per-chunk timing (the pty is never larger than the largest
# size the harness sets).
MAX_CUP_COLS = max(COLS_RANGE[1], INITIAL_SIZE[0])
MAX_CUP_ROWS = max(ROWS_RANGE[1], INITIAL_SIZE[1])
# Longest legitimate CSI body: a full fg+bg truecolor SGR
# "38;2;255;255;255;48;2;255;255;255" = 33 bytes. Anything much longer is a
# runaway (missing final byte swallowing the frame).
CSI_MAX_BODY = 40
MODE_PARAMS = {b"?1049", b"?25", b"?7", b"?2026"}
SGR_SIMPLE = frozenset(
    [0] + list(range(30, 38)) + list(range(40, 48))
    + list(range(90, 98)) + list(range(100, 108))
)


def _sgr_error(body: bytes) -> str | None:
    """Validate one SGR parameter body; None = well-formed."""
    if not body:
        return "empty SGR (never emitted)"
    parts = body.split(b";")
    try:
        nums = [int(p) for p in parts]
    except ValueError:
        return f"non-numeric SGR param in {body!r}"
    i = 0
    while i < len(nums):
        n = nums[i]
        if n in SGR_SIMPLE:
            i += 1
        elif n in (38, 48):
            if i + 1 >= len(nums):
                return f"truncated {n} SGR in {body!r}"
            if nums[i + 1] == 2:  # truecolor: 38;2;R;G;B
                if i + 4 >= len(nums) or any(v > 255 for v in nums[i + 2 : i + 5]):
                    return f"malformed {n};2 SGR in {body!r}"
                i += 5
            elif nums[i + 1] == 5:  # 256-color: 38;5;N
                if i + 2 >= len(nums) or nums[i + 2] > 255:
                    return f"malformed {n};5 SGR in {body!r}"
                i += 3
            else:
                return f"unknown {n};{nums[i + 1]} SGR form in {body!r}"
        else:
            return f"SGR code {n} the player never emits in {body!r}"
    return None


def _csi_error(final: int, body: bytes) -> str | None:
    """Validate one complete CSI against the player's vocabulary."""
    f = chr(final)
    if f == "H":  # CUP row;col, 1-based
        parts = body.split(b";")
        if len(parts) != 2 or not all(p.isdigit() for p in parts):
            return f"malformed CUP body {body!r}"
        row, col = int(parts[0]), int(parts[1])
        if not (1 <= row <= MAX_CUP_ROWS):
            return f"CUP row {row} outside 1..{MAX_CUP_ROWS} (desync?)"
        if not (1 <= col <= MAX_CUP_COLS):
            return f"CUP col {col} outside 1..{MAX_CUP_COLS} (desync?)"
        return None
    if f == "m":
        return _sgr_error(body)
    if f in "hl":
        return None if body in MODE_PARAMS else f"unexpected mode {body!r} for '{f}'"
    if f == "q":  # volley XTVERSION query
        return None if body == b">0" else f"unexpected 'q' body {body!r}"
    if f == "p":  # volley DECRQM 2026 query
        return None if body == b"?2026$" else f"unexpected 'p' body {body!r}"
    if f == "t":  # volley cell-size query
        return None if body == b"16" else f"unexpected 't' body {body!r}"
    if f == "c":  # volley DA1 sentinel query
        return None if body == b"" else f"unexpected 'c' body {body!r}"
    return f"CSI final {f!r} the player never emits (body {body!r})"


def check_escape_stream(data: bytes, *, resync_start: bool = False,
                        allow_truncated_end: bool = False) -> dict:
    """Strict structural parse of captured player output.

    `resync_start`: the capture begins at an arbitrary ring-buffer cut —
    skip to the first ESC before judging (the skipped prefix may be the
    printable interior of a cut sequence). `allow_truncated_end`: the
    capture ends at a byte cap (head.log), so one final incomplete
    sequence is not an error. Returns a summary dict; `error_count == 0`
    means the stream is structurally valid.
    """
    errors: list[str] = []
    n_seq = n_cup = 0
    total_errors = 0

    def err(offset: int, msg: str) -> None:
        nonlocal total_errors
        total_errors += 1
        if len(errors) < 20:
            errors.append(f"@{offset}: {msg}")

    i = 0
    skipped = 0
    if resync_start:
        i = data.find(b"\x1b")
        skipped = len(data) if i < 0 else i
        i = len(data) if i < 0 else i
    end = len(data)
    while i < end:
        b = data[i]
        if b == 0x1B:
            if i + 1 >= end:
                if not allow_truncated_end:
                    err(i, "lone ESC at end of capture")
                break
            nxt = data[i + 1]
            if nxt == ord("["):
                j = i + 2
                while j < end and not (0x40 <= data[j] <= 0x7E):
                    j += 1
                if j >= end:
                    if not allow_truncated_end:
                        err(i, "unterminated CSI at end of capture")
                    break
                if j - (i + 2) > CSI_MAX_BODY:
                    err(i, f"CSI body {j - (i + 2)} bytes long (runaway)")
                else:
                    e = _csi_error(data[j], data[i + 2 : j])
                    if e:
                        err(i, e)
                n_seq += 1
                if data[j] == ord("H"):
                    n_cup += 1
                i = j + 1
            elif nxt == ord("P"):  # DCS: only the volley's XTGETTCAP query
                st = data.find(b"\x1b\\", i + 2)
                if st < 0:
                    if not allow_truncated_end:
                        err(i, "unterminated DCS at end of capture")
                    break
                if data[i + 2 : st] != b"+q524742":
                    err(i, f"DCS body {data[i + 2:st]!r} the player never emits")
                n_seq += 1
                i = st + 2
            else:
                err(i, f"ESC followed by {chr(nxt)!r} (not CSI/DCS)")
                i += 2
        elif 0x20 <= b <= 0x7E or b in (0x09, 0x0A, 0x0D):
            i += 1
        elif 0xC2 <= b <= 0xF4:  # UTF-8 lead byte (glyphs above ASCII)
            need = 1 if b <= 0xDF else 2 if b <= 0xEF else 3
            if i + need >= end:
                if not allow_truncated_end:
                    err(i, "truncated UTF-8 sequence at end of capture")
                break
            if all(0x80 <= data[i + k] <= 0xBF for k in range(1, need + 1)):
                i += need + 1
            else:
                err(i, f"malformed UTF-8 sequence at lead byte 0x{b:02x}")
                i += 1
        else:
            err(i, f"control/invalid byte 0x{b:02x} outside an escape")
            i += 1

    return {
        "bytes": len(data),
        "resync_skipped_bytes": skipped,
        "sequences": n_seq,
        "cups": n_cup,
        "error_count": total_errors,
        "errors": errors,  # first 20
    }


def check_logs(outdir: Path) -> tuple[dict, list[str]]:
    """Run the structural check over an outdir's head.log + tail.log.
    Returns (report, fail_reasons)."""
    report: dict = {}
    reasons: list[str] = []
    head_path, tail_path = outdir / "head.log", outdir / "tail.log"
    for name, path in (("head", head_path), ("tail", tail_path)):
        if not path.is_file():
            reasons.append(f"{path.name} missing — nothing to check")
            continue
        data = path.read_bytes()
        # head.log starts at the stream start and is cut at a byte cap;
        # tail.log starts at an arbitrary ring cut (unless the run was short
        # enough that the ring still holds the stream start) and ends at
        # process EOF, where a clean exit ends on a complete sequence.
        chk = check_escape_stream(
            data,
            resync_start=(name == "tail" and not data.startswith(b"\x1b")),
            allow_truncated_end=(name == "head"),
        )
        report[name] = chk
        if chk["error_count"]:
            reasons.append(
                f"escape-stream structural errors in {path.name} "
                f"({chk['error_count']}; first: {chk['errors'][0]})"
            )
    return report, reasons


def self_test() -> int:
    """Validator self-checks: a specified-vocabulary stream passes; each
    corruption class is caught. Returns a process exit code."""
    volley = b"\x1b[>0q\x1b[?2026$p\x1bP+q524742\x1b\\\x1b[16t\x1b[c"
    enter = b"\x1b[?1049h\x1b[?25l\x1b[?7l"
    frame = (b"\x1b[?2026h\x1b[1;1H\x1b[38;5;120;48;5;16m~~soak~~"
             b"\x1b[12;40H\x1b[38;2;255;250;205m\xe2\x96\x80\xe2\x96\x84"
             b"\x1b[140;500H\x1b[0mx\x1b[?2026l")
    good = volley + enter + frame + RESTORE_SEQ
    cases: list[tuple[str, bytes, dict, bool]] = [
        ("clean stream", good, {}, True),
        ("head cut mid-CSI", good[:-9], {"allow_truncated_end": True}, True),
        ("tail ring cut resync", b"8;5;120m junk" + good,
         {"resync_start": True}, True),
        ("truncated CSI mid-stream", b"\x1b[38;5\x1b[1;1Hx" + RESTORE_SEQ, {}, False),
        ("out-of-bounds CUP", b"\x1b[999;600Hx", {}, False),
        ("CUP col 0", b"\x1b[1;0Hx", {}, False),
        ("unknown CSI final (ED)", b"\x1b[2J", {}, False),
        ("SGR the player never emits", b"\x1b[5m", {}, False),
        ("malformed 38;5 SGR", b"\x1b[38;5;999m", {}, False),
        ("stray control byte", b"ok\x07ok", {}, False),
        ("bare ESC in ground", b"\x1bXoops", {}, False),
        ("malformed UTF-8", b"\xe2\x28\xa1", {}, False),
        ("truncated end not allowed", good[:-9], {}, False),
    ]
    failed = 0
    for name, data, kwargs, want_clean in cases:
        chk = check_escape_stream(data, **kwargs)
        ok = (chk["error_count"] == 0) == want_clean
        print(f"self-test: {'ok  ' if ok else 'FAIL'} {name}: "
              f"errors={chk['error_count']} {chk['errors'][:1]}")
        failed += not ok
    print(f"self-test: {'PASS' if not failed else f'FAIL ({failed} cases)'}")
    return 0 if not failed else 1


def spawn_player(player: Path, asset: Path) -> tuple[int, int]:
    """pty.fork + exec. The child gets the pty as controlling terminal, so
    TIOCSWINSZ on the master raises SIGWINCH in the player — the whole
    point of the harness."""
    pid, master = pty.fork()
    if pid == 0:  # child
        try:
            env = dict(os.environ)
            # Deterministic passive hints: a plain 256-color xterm. The
            # probe volley goes unanswered on this pty (deadline ~250 ms),
            # exactly like piping through a dumb wrapper. --no-cache keeps
            # the "no replies" result out of the user's probe cache.
            env["TERM"] = "xterm-256color"
            for k in ("COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION",
                      "TMUX", "SSH_CONNECTION", "SSH_TTY"):
                env.pop(k, None)
            os.execve(str(player), [str(player), str(asset), "--loop", "--no-cache"], env)
        except Exception:  # noqa: BLE001 — child must never unwind into the harness
            os._exit(127)
    os.set_blocking(master, False)
    return pid, master


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--duration", type=float, default=3600.0,
                    help="soak length in seconds (default 3600; 60 = smoke mode)")
    ap.add_argument("--asset", type=Path, help="the .ascii asset to loop")
    ap.add_argument("--player", type=Path, default=REPO / "target/release/auto-ascii-player")
    ap.add_argument("--outdir", type=Path,
                    help="directory for head.log/tail.log/rss.csv/resizes.csv/summary.json")
    ap.add_argument("--seed", type=int, default=None,
                    help="seed the resize RNG for a reproducible storm")
    ap.add_argument("--check-logs", type=Path, metavar="DIR",
                    help="no soak: run the structural escape-stream check over "
                         "an existing outdir's head.log/tail.log and exit")
    ap.add_argument("--self-test", action="store_true",
                    help="no soak: run the escape-stream validator self-checks and exit")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    if args.check_logs:
        report, reasons = check_logs(args.check_logs)
        print(json.dumps({"escape_check": report, "fail_reasons": reasons}, indent=2))
        return 0 if not reasons else 1

    if args.asset is None:
        ap.error("--asset is required (unless --check-logs/--self-test)")
    if args.outdir is None:
        ap.error("--outdir is required (unless --check-logs/--self-test)")
    if not args.player.is_file():
        sys.exit(f"player binary not found: {args.player}\n"
                 f"build it: cargo build --release -p auto-ascii --features bin")
    if not args.asset.is_file():
        sys.exit(f"asset not found: {args.asset}")

    args.outdir.mkdir(parents=True, exist_ok=True)
    (args.outdir / "harness.pid").write_text(f"{os.getpid()}\n")
    rng = random.Random(args.seed)

    signal.signal(signal.SIGTERM, _on_signal)
    signal.signal(signal.SIGINT, _on_signal)

    log = OutputLog(args.outdir)
    rss_csv = open(args.outdir / "rss.csv", "w")
    rss_csv.write("unix_ts,elapsed_s,rss_kb\n")
    rsz_csv = open(args.outdir / "resizes.csv", "w")
    rsz_csv.write("elapsed_s,cols,rows\n")

    start_iso = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    pid, master = spawn_player(args.player, args.asset)
    set_winsize(master, *INITIAL_SIZE)
    print(f"soak: player pid={pid} asset={args.asset.name} duration={args.duration:.0f}s "
          f"start={start_iso}", flush=True)

    t0 = time.monotonic()
    deadline = t0 + args.duration
    next_resize = t0 + rng.uniform(RESIZE_MIN_S, RESIZE_MAX_S)
    next_rss = t0  # sample immediately
    next_flush = t0 + TAIL_FLUSH_IVL_S
    next_progress = t0 + PROGRESS_IVL_S

    resizes = submin_resizes = 0
    rss_samples: list[tuple[float, int]] = []
    eof = False
    early_status: int | None = None

    while not ABORT:
        now = time.monotonic()
        if now >= deadline:
            break
        alive, status = child_alive(pid)
        if not alive:
            early_status = status
            break
        timeout = max(0.0, min(next_resize, next_rss, next_flush, next_progress, deadline) - now)
        r, _, _ = select.select([master], [], [], timeout)
        if r:
            eof = drain(master, log)
            if eof:
                _, early_status = child_alive(pid)
                break
        now = time.monotonic()
        if now >= next_resize:
            if rng.random() < SUBMIN_PROB:
                cols, rows = SUBMIN_SIZE
                submin_resizes += 1
            else:
                cols = rng.randint(*COLS_RANGE)
                rows = rng.randint(*ROWS_RANGE)
            set_winsize(master, cols, rows)
            resizes += 1
            rsz_csv.write(f"{now - t0:.3f},{cols},{rows}\n")
            next_resize = now + rng.uniform(RESIZE_MIN_S, RESIZE_MAX_S)
        if now >= next_rss:
            kb = read_rss_kb(pid)
            if kb is not None:
                rss_samples.append((now - t0, kb))
                rss_csv.write(f"{time.time():.1f},{now - t0:.1f},{kb}\n")
                rss_csv.flush()
            next_rss = now + RSS_IVL_S
        if now >= next_flush:
            log.flush_tail()
            rsz_csv.flush()
            next_flush = now + TAIL_FLUSH_IVL_S
        if now >= next_progress:
            kb = rss_samples[-1][1] if rss_samples else 0
            print(f"soak: t={now - t0:7.0f}s rss={kb / 1024:7.1f}MB "
                  f"out={log.total / 1e6:9.1f}MB resizes={resizes}", flush=True)
            next_progress = now + PROGRESS_IVL_S

    ran_s = time.monotonic() - t0
    aborted = ABORT
    early_exit = early_status is not None or (eof and ran_s < args.duration)

    # Graceful quit: 'q' is the player's quit key. Drain the restore bytes.
    sent_quit = False
    kill_used = None
    status = early_status
    if status is None:
        alive, status = child_alive(pid)
        if alive:
            try:
                os.write(master, b"q")
                sent_quit = True
            except OSError:
                pass
            quit_deadline = time.monotonic() + QUIT_GRACE_S
            kill_deadline = quit_deadline + KILL_GRACE_S
            while True:
                alive, status = child_alive(pid)
                if not alive:
                    break
                now = time.monotonic()
                if now >= kill_deadline:
                    os.kill(pid, signal.SIGKILL)
                    kill_used = "SIGKILL"
                elif now >= quit_deadline:
                    os.kill(pid, signal.SIGTERM)
                    kill_used = kill_used or "SIGTERM"
                r, _, _ = select.select([master], [], [], 0.2)
                if r and drain(master, log):
                    _, status = os.waitpid(pid, 0)
                    break
        else:
            early_exit = True
    # Final drain of anything left in the pty buffer (restore bytes).
    for _ in range(50):
        r, _, _ = select.select([master], [], [], 0.1)
        if not r or drain(master, log):
            break
    os.close(master)

    if status is None:
        exitcode = None
    elif hasattr(os, "waitstatus_to_exitcode"):
        exitcode = os.waitstatus_to_exitcode(status)  # < 0 = -signum
    else:  # pragma: no cover — pre-3.9 fallback
        exitcode = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -os.WTERMSIG(status)

    tail = log.tail_bytes()
    log.close()
    rss_csv.close()
    rsz_csv.close()

    warmup_s = min(600.0, args.duration / 4.0)
    slope = rss_slope_mb_per_h(rss_samples, warmup_s)

    fail_reasons = []
    if early_exit:
        fail_reasons.append("player exited before the deadline")
    if aborted:
        fail_reasons.append("harness aborted by signal")
    if kill_used:
        fail_reasons.append(f"player ignored 'q' ({kill_used} used)")
    if exitcode != 0:
        fail_reasons.append(f"player exit code {exitcode!r} (want 0)")
    if RESTORE_SEQ not in tail:
        fail_reasons.append("RESTORE_SEQ not found in output tail")

    # M5 acceptance A: structural escape-stream check over the captured
    # output — a desync the player survives must still fail the soak.
    escape_check, escape_reasons = check_logs(args.outdir)
    fail_reasons.extend(escape_reasons)

    summary = {
        "start": start_iso,
        "asset": str(args.asset),
        "player": str(args.player),
        "duration_requested_s": args.duration,
        "duration_ran_s": round(ran_s, 1),
        "seed": args.seed,
        "player_pid": pid,
        "resizes": resizes,
        "submin_resizes": submin_resizes,
        "bytes_captured": log.total,
        "sent_quit": sent_quit,
        "kill_used": kill_used,
        "exit_code": exitcode,
        "restore_seq_in_tail": RESTORE_SEQ in tail,
        "rss_samples": len(rss_samples),
        "rss_first_kb": rss_samples[0][1] if rss_samples else None,
        "rss_last_kb": rss_samples[-1][1] if rss_samples else None,
        "rss_warmup_s": warmup_s,
        "rss_slope_mb_per_h": None if slope is None else round(slope, 3),
        "escape_check": escape_check,
        "pass": not fail_reasons,
        "fail_reasons": fail_reasons,
        "tail_preview": tail[-512:].decode("latin-1").encode("unicode_escape").decode("ascii"),
    }
    (args.outdir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"soak: {'PASS' if summary['pass'] else 'FAIL'} "
          f"exit_code={exitcode} resizes={resizes} (submin={submin_resizes}) "
          f"bytes={log.total} rss_slope={summary['rss_slope_mb_per_h']}MB/h "
          f"reasons={fail_reasons}", flush=True)
    return 0 if summary["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
