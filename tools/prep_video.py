#!/usr/bin/env python3
"""
prep_video.py — auto-ascii corpus-preparation front door.

Normalizes any source video onto a consistent canvas (default 1920x1080) so the
offline factory (PLAN.md §5; the future `sleepy-factory` ingest stage) always
receives canvas-normalized input regardless of source aspect. Pure stdlib;
shells out to ffmpeg/ffprobe. Originals are never modified.

Pipeline — ONE ffmpeg invocation per output, in this filtergraph order:

  1. CUT + CONCAT   Each --clip START-END becomes its own seeked input
                    (`-ss/-t` before `-i`; frame-accurate because we re-encode),
                    then video+audio are stitched with the concat filter.
  2. SCALE          Contain-fit into the canvas (aspect preserved, Lanczos).
                    Scaled tile dims are forced even so all tiling/crop
                    arithmetic stays integral and perfectly centered.
  3. BOOMERANG      Optional --min-duration: append reversed / forward copies
                    alternately until long enough. CAVEAT: ffmpeg's reverse /
                    areverse buffer the ENTIRE stream in RAM (roughly
                    w*h*1.5 bytes per frame at this point in the graph);
                    intended for short loops, not long features.
  4. MIRROR FILL    Leftover canvas space is filled with alternately flipped
                    copies of the scaled tile so every seam is a reflection:
                    horizontally  [...O][M][O][M][O...]  centered on the
                    original (M = hflip; adjacent tiles mirror about their
                    shared edge, so seams are invisible), vertically the same
                    idea with vflip. The tile count per side is COMPUTED
                    (ceil(gap_per_side / tile)); a 9:16 source on a 16:9
                    canvas needs 2+ tiles per side, never assume one.
                    --fill mirror-invert additionally color-negates the fill
                    tiles only — the center tile is never touched.
                    Geometry runs in rgb24 so odd crop offsets can never
                    shift 4:2:0 chroma; we convert back to yuv420p at the end.
  5. ENCODE         h264 crf 18, preset medium, yuv420p, +faststart; audio is
                    re-encoded aac 192k (required anyway for clean concat).
"""

import argparse
import json
import math
import os
import shlex
import subprocess
import sys

FFMPEG = "ffmpeg"
FFPROBE = "ffprobe"


def die(msg: str, code: int = 1):
    print(f"prep_video: error: {msg}", file=sys.stderr)
    sys.exit(code)


def run(cmd, verbose=False, capture=True):
    if verbose:
        print("+ " + shlex.join(cmd), file=sys.stderr)
    proc = subprocess.run(cmd, capture_output=capture, text=True)
    if proc.returncode != 0:
        tail = (proc.stderr or "").strip().splitlines()[-15:] if capture else []
        die(f"command failed ({cmd[0]}, exit {proc.returncode})"
            + ("\n  " + "\n  ".join(tail) if tail else ""))
    return proc


# --------------------------------------------------------------------------- #
# Probing
# --------------------------------------------------------------------------- #

def probe(path: str) -> dict:
    proc = run([FFPROBE, "-v", "error", "-print_format", "json",
                "-show_format", "-show_streams", path])
    return json.loads(proc.stdout)


def probe_summary(path: str):
    """Return (display_w, display_h, duration_s, has_audio).

    display_* account for sample aspect ratio and rotation side-data:
    ffmpeg autorotates on decode, so the filtergraph sees post-rotation frames
    and all layout math must use post-rotation dimensions.
    """
    info = probe(path)
    vstreams = [s for s in info.get("streams", []) if s.get("codec_type") == "video"]
    if not vstreams:
        die(f"no video stream in {path}")
    v = vstreams[0]
    w, h = int(v["width"]), int(v["height"])

    # Sample aspect ratio -> display width.
    sar = v.get("sample_aspect_ratio", "1:1")
    try:
        num, den = (int(x) for x in sar.split(":"))
        if num > 0 and den > 0 and num != den:
            w = round(w * num / den)
    except ValueError:
        pass

    # Rotation side data (portrait phone footage etc.).
    rot = 0
    for sd in v.get("side_data_list", []):
        if "rotation" in sd:
            rot = int(sd["rotation"])
    if abs(rot) % 180 == 90:
        w, h = h, w

    dur = None
    for src in (info.get("format", {}).get("duration"), v.get("duration")):
        if src is not None:
            dur = float(src)
            break
    if dur is None or dur <= 0:
        die(f"could not determine duration of {path}")

    has_audio = any(s.get("codec_type") == "audio" for s in info.get("streams", []))
    return w, h, dur, has_audio


# --------------------------------------------------------------------------- #
# CLI parsing helpers
# --------------------------------------------------------------------------- #

def parse_timestamp(s: str, ctx: str) -> float:
    """Accept SS, MM:SS, HH:MM:SS, each with optional fractional seconds."""
    parts = s.strip().split(":")
    if not 1 <= len(parts) <= 3:
        die(f"{ctx}: invalid timestamp '{s}' (use SS, MM:SS or HH:MM:SS)")
    total = 0.0
    for i, p in enumerate(parts):
        last = i == len(parts) - 1
        try:
            v = float(p) if last else int(p)
        except ValueError:
            die(f"{ctx}: invalid timestamp '{s}' (bad field '{p}')")
        if v < 0:
            die(f"{ctx}: invalid timestamp '{s}' (negative field)")
        if i > 0 and v >= 60:
            die(f"{ctx}: invalid timestamp '{s}' (field '{p}' must be < 60)")
        total = total * 60 + v
    return total


def parse_clip(spec: str):
    ctx = f"--clip {spec}"
    if spec.count("-") != 1:
        die(f"{ctx}: expected START-END (e.g. 0:30-0:45)")
    a, b = spec.split("-")
    start = parse_timestamp(a, ctx)
    end = parse_timestamp(b, ctx)
    return start, end, spec


def parse_canvas(spec: str):
    try:
        w_s, h_s = spec.lower().split("x")
        w, h = int(w_s), int(h_s)
    except ValueError:
        die(f"--canvas {spec}: expected WxH (e.g. 1920x1080)")
    if w < 16 or h < 16:
        die(f"--canvas {spec}: canvas too small (min 16x16)")
    ew, eh = w - w % 2, h - h % 2
    if (ew, eh) != (w, h):
        print(f"prep_video: note: canvas {w}x{h} forced even -> {ew}x{eh}",
              file=sys.stderr)
    return ew, eh


def fmt_ts(t: float) -> str:
    m, s = divmod(t, 60)
    h, m = divmod(int(m), 60)
    return f"{h}:{m:02d}:{s:06.3f}" if h else f"{m}:{s:06.3f}"


# --------------------------------------------------------------------------- #
# Geometry
# --------------------------------------------------------------------------- #

def contain_fit(dw: int, dh: int, cw: int, ch: int):
    """Largest even WxH that fits in the canvas preserving dw:dh aspect."""
    if dw * ch >= dh * cw:          # source wider than canvas -> width-limited
        sw, sh = cw, min(ch, round(dh * cw / dw))
    else:                           # taller than canvas -> height-limited
        sw, sh = min(cw, round(dw * ch / dh)), ch
    sw -= sw % 2
    sh -= sh % 2
    return max(sw, 2), max(sh, 2)


# --------------------------------------------------------------------------- #
# Filtergraph builders
# --------------------------------------------------------------------------- #

def build_concat(parts, n_inputs: int, has_audio: bool):
    """Stitch the N seeked inputs (one per --clip) back to back."""
    pads = ""
    for i in range(n_inputs):
        parts.append(f"[{i}:v]setpts=PTS-STARTPTS[cv{i}]")
        pads += f"[cv{i}]"
        if has_audio:
            parts.append(f"[{i}:a]asetpts=PTS-STARTPTS[ca{i}]")
            pads += f"[ca{i}]"
    out_a = "[acat]" if has_audio else ""
    parts.append(f"{pads}concat=n={n_inputs}:v=1:a={int(has_audio)}[vcat]{out_a}")
    return "vcat", ("acat" if has_audio else None)


def build_boomerang(parts, v: str, a, m: int):
    """Alternate forward/reversed copies of the assembled clip, m segments
    total (segment 0 forward). reverse/areverse buffer everything in RAM."""
    n_fwd, n_rev = (m + 1) // 2, m // 2

    fwd = [f"bfv{k}" for k in range(n_fwd)]
    parts.append(f"[{v}]split={n_fwd + 1}"
                 + "".join(f"[{x}]" for x in fwd) + "[brsrc]")
    rev = [f"brv{k}" for k in range(n_rev)]
    if n_rev == 1:
        parts.append(f"[brsrc]reverse[{rev[0]}]")
    else:
        parts.append("[brsrc]reverse[brall]")
        parts.append(f"[brall]split={n_rev}" + "".join(f"[{x}]" for x in rev))

    afwd = arev = None
    if a is not None:
        afwd = [f"bfa{k}" for k in range(n_fwd)]
        parts.append(f"[{a}]asplit={n_fwd + 1}"
                     + "".join(f"[{x}]" for x in afwd) + "[basrc]")
        arev = [f"bra{k}" for k in range(n_rev)]
        if n_rev == 1:
            parts.append(f"[basrc]areverse[{arev[0]}]")
        else:
            parts.append("[basrc]areverse[baall]")
            parts.append(f"[baall]asplit={n_rev}" + "".join(f"[{x}]" for x in arev))

    pads = ""
    for k in range(m):
        pads += f"[{(fwd if k % 2 == 0 else rev)[k // 2]}]"
        if a is not None:
            pads += f"[{(afwd if k % 2 == 0 else arev)[k // 2]}]"
    out_a = "[bma]" if a is not None else ""
    parts.append(f"{pads}concat=n={m}:v=1:a={int(a is not None)}[bmv]{out_a}")
    return "bmv", ("bma" if a is not None else None)


def build_mirror_axis(parts, cur: str, axis: str, tile: int, canvas: int,
                      cross: int, invert: bool):
    """Fill the gap along one axis with the [...O][M][O][M][O...] pattern.

    An odd-length strip of alternately flipped copies is stacked, centered on
    the original, then cropped back to the canvas length. Adjacent tiles are
    mirror images about their shared edge, so every seam is invisible; tiles
    two apart repeat the original orientation, and the crop at the canvas
    edge just truncates a reflection mid-tile, which is still seamless.

      tile   size of current image along this axis (even)
      canvas canvas size along this axis (even)
      cross  size along the other axis (unchanged by this step)
    """
    gap = canvas - tile
    if gap <= 0:
        return cur                       # source already fills this axis
    per_side = gap // 2                  # even - even => integral
    n = math.ceil(per_side / tile)       # tiles PER SIDE — computed, generic
    total = 2 * n + 1                    # odd strip, original at the center
    flip = "hflip" if axis == "h" else "vflip"
    stack = "hstack" if axis == "h" else "vstack"

    srcs = [f"t{axis}{j}" for j in range(total)]
    parts.append(f"[{cur}]split={total}" + "".join(f"[{s}]" for s in srcs))

    outs = []
    for j in range(total):
        i = j - n                        # signed index; 0 = the original
        chain = []
        if abs(i) % 2 == 1:
            chain.append(flip)           # neighbours are mirrored
        if invert and i != 0:
            chain.append("negate")       # fill tiles only, never the original
        if not chain:
            chain.append("null")
        out = f"x{axis}{j}"
        parts.append(f"[{srcs[j]}]{','.join(chain)}[{out}]")
        outs.append(out)

    stacked = f"s{axis}"
    parts.append("".join(f"[{o}]" for o in outs) + f"{stack}=inputs={total}[{stacked}]")

    off = (total * tile - canvas) // 2   # exact center: both terms even
    cropped = f"c{axis}"
    if axis == "h":
        parts.append(f"[{stacked}]crop={canvas}:{cross}:{off}:0[{cropped}]")
    else:
        parts.append(f"[{stacked}]crop={cross}:{canvas}:0:{off}[{cropped}]")
    return cropped


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #

EPILOG = """examples:
  # Full video onto the default 1920x1080 canvas, mirror fill:
  tools/prep_video.py corpus/grass-field-windy.mp4

  # Compare fill modes (fill tiles color-negated in the second):
  tools/prep_video.py corpus/grass-field-windy.mp4 --fill mirror        --out corpus/prepared/grass-mirror.mp4
  tools/prep_video.py corpus/grass-field-windy.mp4 --fill mirror-invert --out corpus/prepared/grass-mirror-invert.mp4

  # Cut two snippets, stitch them (audio kept), then normalize:
  tools/prep_video.py corpus/sheep-counting-neroni.mp4 --clip 0:30-0:45 --clip 5:00-5:15

  # Loop a short clip out to >= 20 s by boomerang (fwd/rev/fwd/...):
  tools/prep_video.py corpus/grass-field-windy.mp4 --min-duration 20

  # Custom canvas:
  tools/prep_video.py in.mp4 --canvas 1280x720 --out prepared/in-720p.mp4

timestamps: SS, MM:SS or HH:MM:SS, fractional seconds allowed (e.g. 1:23.5).
default output: <input-dir>/prepared/<input-stem>-prepared.mp4
"""


def main():
    ap = argparse.ArgumentParser(
        prog="prep_video.py",
        description="Normalize a source video onto a fixed canvas with seamless "
                    "mirror-fill; optionally cut/stitch snippets and boomerang-"
                    "extend. Corpus prep front door for the auto-ascii factory.",
        epilog=EPILOG,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", help="source video (never modified)")
    ap.add_argument("--canvas", default="1920x1080", metavar="WxH",
                    help="canvas size (default 1920x1080; forced even)")
    ap.add_argument("--fill", choices=["mirror", "mirror-invert"], default="mirror",
                    help="fill mode for leftover canvas space: pure reflection, "
                         "or reflection with color-negated fill tiles")
    ap.add_argument("--clip", action="append", default=[], metavar="START-END",
                    help="cut this time range; repeatable, concatenated in the "
                         "order given BEFORE canvas normalization")
    ap.add_argument("--min-duration", type=float, default=None, metavar="SECONDS",
                    help="extend by boomerang (forward/reversed alternating) "
                         "until at least this long. ffmpeg's reverse buffers "
                         "the whole stream in RAM — short clips only")
    ap.add_argument("--out", default=None, metavar="PATH",
                    help="output path (default: <input-dir>/prepared/"
                         "<stem>-prepared.mp4)")
    ap.add_argument("--verbose", action="store_true",
                    help="print the full ffmpeg command line")
    args = ap.parse_args()

    src = os.path.abspath(args.input)
    if not os.path.isfile(src):
        die(f"input not found: {args.input}")
    cw, ch = parse_canvas(args.canvas)
    if args.min_duration is not None and args.min_duration <= 0:
        die("--min-duration must be > 0")

    dw, dh, duration, has_audio = probe_summary(src)

    # ---- clips -------------------------------------------------------------
    clips = [parse_clip(c) for c in args.clip]
    for start, end, raw in clips:
        if start >= end:
            die(f"--clip {raw}: start ({fmt_ts(start)}) must be before "
                f"end ({fmt_ts(end)})")
        if end > duration + 0.05:
            die(f"--clip {raw}: end {fmt_ts(end)} is beyond input duration "
                f"{fmt_ts(duration)} ({duration:.3f}s)")
    assembled = sum(e - s for s, e, _ in clips) if clips else duration

    # ---- output path -------------------------------------------------------
    if args.out:
        out = os.path.abspath(args.out)
    else:
        stem = os.path.splitext(os.path.basename(src))[0]
        out = os.path.join(os.path.dirname(src), "prepared", f"{stem}-prepared.mp4")
    if os.path.exists(out) and os.path.samefile(out, src):
        die("output path equals input path; originals are never modified")
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)

    # ---- geometry ----------------------------------------------------------
    sw, sh = contain_fit(dw, dh, cw, ch)
    gx, gy = cw - sw, ch - sh
    needs_fill = gx > 0 or gy > 0

    # ---- boomerang count ---------------------------------------------------
    m = 1
    if args.min_duration is not None and assembled < args.min_duration:
        m = math.ceil(args.min_duration / assembled - 1e-9)
        if m > 64:
            die(f"--min-duration {args.min_duration}: would need {m} boomerang "
                f"segments of {assembled:.2f}s; refusing (>64). Use a longer "
                f"source or clips.")

    # ---- assemble the single ffmpeg command --------------------------------
    cmd = [FFMPEG, "-hide_banner", "-nostdin", "-loglevel", "error", "-y"]
    if clips:
        for start, end, _ in clips:
            cmd += ["-ss", f"{start:.6f}", "-t", f"{end - start:.6f}", "-i", src]
    else:
        cmd += ["-i", src]

    parts = []
    if clips:
        v, a = build_concat(parts, len(clips), has_audio)
    else:
        v, a = "0:v", ("0:a" if has_audio else None)

    # Scale first (tile size), boomerang second (reverse buffers less at tile
    # resolution than at canvas resolution), then mirror-fill.
    parts.append(f"[{v}]scale={sw}:{sh}:flags=lanczos,setsar=1[scaled]")
    v = "scaled"

    if m > 1:
        v, a = build_boomerang(parts, v, a, m)

    if needs_fill:
        # rgb24 for the geometry stage: crop offsets can be odd without ever
        # shifting 4:2:0 chroma, and negate is an exact RGB negation.
        parts.append(f"[{v}]format=rgb24[base]")
        v = "base"
        invert = args.fill == "mirror-invert"
        v = build_mirror_axis(parts, v, "h", sw, cw, sh, invert)
        v = build_mirror_axis(parts, v, "v", sh, ch, cw, invert)

    parts.append(f"[{v}]format=yuv420p[vout]")

    cmd += ["-filter_complex", ";".join(parts), "-map", "[vout]"]
    if a is not None:
        cmd += ["-map", a if ":" in a else f"[{a}]"]
        cmd += ["-c:a", "aac", "-b:a", "192k"]
    cmd += ["-c:v", "libx264", "-crf", "18", "-preset", "medium",
            "-pix_fmt", "yuv420p", "-movflags", "+faststart", out]

    # ---- report the plan ---------------------------------------------------
    def side_info(gap, tile):
        if gap <= 0:
            return "no fill"
        n = math.ceil((gap // 2) / tile)
        return f"{gap // 2}px/side -> {n} tile(s)/side (strip of {2 * n + 1})"

    print(f"prep_video: {os.path.basename(src)}: {dw}x{dh} {duration:.3f}s"
          f"{' +audio' if has_audio else ''}")
    if clips:
        spans = ", ".join(f"{fmt_ts(s)}-{fmt_ts(e)}" for s, e, _ in clips)
        print(f"  clips     : {spans} (assembled {assembled:.3f}s)")
    if m > 1:
        print(f"  boomerang : {m} segments -> ~{assembled * m:.1f}s "
              f"(>= {args.min_duration}s)")
    print(f"  canvas    : {cw}x{ch}, scaled tile {sw}x{sh}, fill={args.fill}")
    print(f"  h-gap     : {side_info(gx, sw)}")
    print(f"  v-gap     : {side_info(gy, sh)}")

    run(cmd, verbose=args.verbose, capture=True)

    # ---- verify ------------------------------------------------------------
    ow, oh, odur, oaud = probe_summary(out)
    print(f"  wrote     : {out}")
    print(f"  output    : {ow}x{oh} {odur:.3f}s{' +audio' if oaud else ''}")
    if (ow, oh) != (cw, ch):
        die(f"output is {ow}x{oh}, expected {cw}x{ch}")


if __name__ == "__main__":
    main()
