# RENAME PLAN — everything to `auto-ascii`

Written 2026-09-19 as a save point. **EXECUTED 2026-09-19 in commit `da8ae2a`,
pushed; crates.io published the same day.** This document is now the record of
how it was done and what it cost, not a to-do. It deliberately keeps the old
names throughout so the mappings stay readable — do not sweep it.

Post-execution notes are marked inline; §5 and §7 carry the verified outcomes.

Owner decisions (2026-09-19), all three confirmed:

1. **Scope: everything.** The `slpy` token (1097 occurrences), the `sleepy`
   token (279), the magic bytes, and the `.slpy` extension.
2. **Magic: `SLPY` → `ASCI`.**
3. **crates.io: keep the `auto-ascii` name, yank the rest.** (Owner's answer
   moved twice: "leave them, publish 0.2.0" -> "delete and republish" -> back
   to keeping the name, once §5's verified mechanics showed that deleting
   `auto-ascii` costs a 24-hour lockout and then opens the name to anyone.
   **Settled 2026-09-19. Facade publishes as 0.2.0.** See §5 and §6.)

The owner explicitly accepted that **every existing `.slpy` asset dies**:
"that's fine, we will simply process them again." All 12 corpus sources are
present locally, so every asset is rebuildable.

---

## 1. The mappings

| from | to |
|---|---|
| `crates/slpy-core` | `crates/auto-ascii-core` |
| `crates/slpy-format` | `crates/auto-ascii-format` |
| `crates/slpy-term` | `crates/auto-ascii-term` |
| `crates/slpy-eval` | `crates/auto-ascii-eval` |
| `crates/sleepy-factory` | `crates/auto-ascii-factory` |
| `slpy_core::` etc. | `auto_ascii_core::` etc. |
| bin `sleepy-player` | `auto-ascii-player` |
| bin `sleepy-factory` | `auto-ascii-factory` |
| bin `slpy-term-harness` | `auto-ascii-term-harness` |
| magic `*b"SLPY"` | `*b"ASCI"` (`53 4c 50 59` → `41 53 43 49`) |
| extension `.slpy` | `.ascii` (98 literal occurrences) |

`crates/auto-ascii` (the facade) keeps its name. Feature names
(`bin`, `terminal`, `parallel`) are unchanged.

**Leave alone — historical record, not live naming:**
`docs/research/*.md`, `runs/*.html`, and the already-committed reel titles.
The same exemption the previous rename used.

---

## 2. Order of operations

Do it in this order; each step's failure mode is cheap only if the earlier
ones are done.

1. **`git mv` the five crate directories.** Use `git mv` so history follows.
2. **Rewrite tokens**, most-specific first, exactly as the last rename did
   (see commit `af3578d` for the working pattern — an ordered replacement
   list beats a blanket sed):
   - `slpy_core::` → `auto_ascii_core::` (and the other three `slpy_*::`)
   - `crates/slpy-` → `crates/auto-ascii-`
   - `-p slpy-` / `-p sleepy-` → `-p auto-ascii-`
   - `name = "slpy-…"` / `"sleepy-…"` → the new names
   - `.slpy` → `.ascii`
   - bare `slpy` → `ascii`, bare `sleepy` → `auto-ascii` (catch-all, last)
3. **On-disk constants.** The plan originally listed only the magic; a survey
   on 2026-09-19 found **two** byte-level strings and one rendered string:
   - `crates/auto-ascii-format/src/header.rs`: `MAGIC`, and the
     `assert_eq!(&b[0..4], b"SLPY")` unit test beside it.
   - `crates/auto-ascii-format/src/chunk.rs`: **`TRLR_PAYLOAD: &[u8] =
     b"SLPY_END"`** — the trailer written into every asset, missed by the
     original plan. `b"ASCI_END"` is also **8 bytes**, so chunk framing and
     every downstream offset are unaffected. A different length would have
     moved them.
   - `crates/auto-ascii/src/pipeline.rs`: `draw_enlarge_card` renders the
     literal **`"SLEEPYTIME"`** on the "enlarge terminal" card — real on-screen
     text, plus an assertion on it further down the same file. `"AUTO-ASCII"`
     is also **10 characters**, so the card's centering (`(cols - n) / 2`)
     yields identical geometry. No golden covers this card (checked), so this
     is the one intended, non-golden-visible text change.
4. **Cargo.toml wiring**: workspace `members`, `[workspace.dependencies]`
   keys, the `[profile.dev.package.*]` stanzas, and every inter-crate dep.
5. **`cargo build --workspace`** until clean, then `cargo clippy -D warnings`.
6. **Re-pin and regenerate** — see §3.
7. **`./scripts/eval.sh`** must say ALL GREEN.
8. Commit, push.
9. crates.io — see §5. Do this **last**; it is the only irreversible part.

---

## 3. What the rename invalidates

| thing | why | action |
|---|---|---|
| `assets/*.slpy` (6 files) | magic changed → `BadMagic` on open | rebuild all 6 from `corpus/`, rename to `.ascii` |
| `FIXTURE_SLPY_SHA` | magic is inside the hashed bytes | re-pin; rename the const to `FIXTURE_ASSET_SHA`. Verify **three consecutive identical builds** before pinning, as its own comment history requires. Done: `b00e3ecb…` |
| `GOLDEN_SHA256` in `auto-ascii-format/tests/container.rs` | **second byte pin, missed by this plan.** `cargo test` stops at the first failing target, so it only surfaced once the pin above was fixed — run `--no-fail-fast` to see every pin at once | re-pin to `36752865…`. Its delta is NOT purely the format identity: this fixture's own META `factory_version` label grew from `slpy-format-test-0.1.0` to `auto-ascii-format-test-0.1.0` (+6 chars, +1 CBOR length header), shifting every FIDX offset by +7. Verified against a pre-rename worktree: all six FRAM payloads byte-identical |
| `runs/base.json` | `asset_bytes` unchanged, but it is regenerated anyway | no re-baseline expected — confirm the compare passes |
| 36 insta snapshots | may embed crate names in paths/output | `cargo insta review`; re-accept only after reading each diff |
| tier goldens (`*.ansi`) | RENDER output — magic does not reach them | should be untouched. **If one moves, stop**: that means the rename changed rendering, which it must not |
| `linux_console_80x24_f10.txt` | same | same |
| `npm/package.json` | no `slpy` in it | check anyway |

**The load-bearing invariant:** this rename must not change a single rendered
pixel. Every render golden and every corpus quality metric (SSIM, edge F1,
flicker) must come out identical. If any of them move, something was renamed
that was actually load-bearing — stop and find it rather than re-pinning.

---

## 4. Gotchas from the last rename

- **Prose that names the *other* projects is not a rename target.** The last
  rename's blanket pass corrupted `HANDOFF.md`'s naming section: the line
  warning "do not push to either" ended up naming `andmckay01/auto-ascii`,
  which is **origin**. Repaired 2026-09-19. `HANDOFF.md`'s naming section, the
  `sleepytime-naming-map` memory, and any sentence whose job is to distinguish
  this project from `sleepytime` / `sleepytime-memory` / `auto-ascii-legacy`
  must be reviewed by eye, never swept.
- `perf/thresholds.toml` keys mirror bench IDs — if a bench target is renamed,
  the perf gate silently stops matching. Check the gate still reports 6 benches.
- The asset cache keys on the **pipeline fingerprint**, which hashes source
  files. Renaming every file invalidates the whole cache: the first
  `eval.sh` after this will take ~8–13 min in the corpus section. That is
  correct behaviour, not a hang.
- The player must stay rayon-free: re-check
  `cargo tree -p auto-ascii -e normal | grep -c rayon` → **0**.
- This box is **2 physical cores + SMT**, and shared. Any timing taken during
  this work needs a median of 3 with `uptime` logged. See HANDOFF.

---

## 5. crates.io — the part with real constraints

**Published 2026-09-19 19:31–19:32 UTC, all with 0 downloads:**
`slpy-core` 0.1.0, `slpy-format` 0.1.0, `slpy-term` 0.1.0, `auto-ascii` 0.1.0.

**There is no `cargo delete`.** Cargo can only *yank*, which hides a version
from new resolution but leaves it published forever. Deletion exists only in
the **crates.io web UI**.

**Eligibility — verified 2026-09-19 against the enforcing source**
(`rust-lang/crates.io`, `src/controllers/krate/delete.rs`), not inferred:

- You must be an **owner**, and a *user* owner — team owners cannot delete.
- Then **either** the crate is younger than **72 hours**, **or** it has a
  single owner **and** total downloads are within `1000 x ceil(age_days / 30)`.
- **Separately and unconditionally**: a crate with **any reverse dependency**
  cannot be deleted, at any age. This is *not* waived inside the 72 hours.

**This corrects the original premise of this section.** The 72-hour window is a
shortcut that waives the owner and download checks — it is **not a deadline**.
With 0 downloads and a single user owner (`andmckay01` — both verified via the
API on 2026-09-19), all four crates stay deletable **indefinitely**: the
download allowance grows by 1000 for every month of age. There is no rush.

**What does constrain the schedule is a 24-hour lockout after deleting.**
`AVAILABLE_AFTER = 24h`: on delete, crates.io records the name in
`deleted_crates` with `available_at = now + 24h`, and `publish.rs` rejects
*any* publish of that name until then — **including by the original owner**
("A crate with the name `X` was recently deleted. Reuse of this name will be
available after ..."). Once that moment passes the name is open to **anyone**.

Since the facade crate **keeps the name `auto-ascii`**, deleting it opens a
window in which the name is first unpublishable by us and then claimable by
anyone. Deleting early is therefore strictly worse than deleting late: run §5
only once the rename is green and there is something ready to put back.

**THE DECISION (owner, 2026-09-19): keep the name, yank the rest.**
**DONE — see the outcome table in HANDOFF.md item 3.** Nothing was deleted. `auto-ascii` is never surrendered, so the lockout and the squat
window above never open. The three `slpy-*` crates stay on crates.io as yanked
0.1.0s — three unused pages with 0 downloads, which is the whole cost.

Steps, all completed in this order:

1. `cargo publish -p auto-ascii-core`     (no internal deps) ✓
2. `cargo publish -p auto-ascii-format`   (no internal deps) ✓
3. `cargo publish -p auto-ascii-term`     (needs core) ✓
4. `cargo publish -p auto-ascii` at **0.2.0** (needs all three) ✓
5. `cargo yank --version 0.1.0 auto-ascii` — it points at the old `slpy-*` ✓
6. `cargo yank --version 0.1.0 slpy-term` / `slpy-core` / `slpy-format` ✓

Between 4 and 5, a throwaway project ran `cargo add auto-ascii` and built
against the registry copies — proof the replacements worked before anything
was yanked.

Yank last: yanking a dependency of a live version is harmless, but yanking
before the replacement exists leaves a window with nothing installable.

`auto-ascii-eval` and `auto-ascii-factory` stay **unpublished**, as
`slpy-eval`/`sleepy-factory` did.

A first publish of interdependent crates **cannot be dry-run validated** —
`cargo publish --dry-run` fails with "no matching package named ..." because the
dependencies are not on the registry yet. That is a dry-run limitation, not a
defect; publish sequentially and wait for the index between each.

**Before publishing, clear `target/package/`** — stale `.crate` tarballs there
caused a confusing false failure last time.

**Account gates already cleared:** crates.io email is verified, and the token
is in `~/.cargo/credentials.toml`.

**The order kept for reference, if the decision is ever revisited.** The
reverse-dependency rule is unconditional and the live graph is
`auto-ascii` -> `slpy-core`, `slpy-format`, `slpy-term`; `slpy-term` ->
`slpy-core`. So a deletion pass would have to go `auto-ascii` first, then
`slpy-term` and `slpy-format`, then `slpy-core` last.

---

## 6. Version number

**`auto-ascii` publishes as 0.2.0.** Its 0.1.0 stays published (yanked), and
crates.io never lets a version number be reused, so 0.1.0 is spent.

The three new crates — `auto-ascii-core`, `auto-ascii-format`,
`auto-ascii-term` — are first publishes under names that have never existed, so
they go out at **0.1.0**.

This means the workspace carries **two different version numbers** after the
rename: the facade at 0.2.0, the three libraries at 0.1.0. Check whether the
facade's dependency requirements on them say `0.1` and not `0.2`.

---

## 7. Verification checklist

Results recorded 2026-09-19 as the rename was executed.

- [x] `cargo build --workspace` clean — first try, no fixups
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo tree -p auto-ascii -e normal | grep -c rayon` → **0**; the
      `--no-default-features` tree is also free of clap/anyhow/crossterm
- [x] **every render golden unchanged** — the 4 `.ansi` tier goldens are
      byte-identical pure renames, and all 36 insta snapshots differ by
      **exactly one line each**, the `source:` metadata path. Not one rendered
      grid line moved. `linux_console_80x24_f10.txt` untouched.
- [x] **The byte-level proof.** Reverting exactly 16 bytes in a rebuilt asset —
      4 magic, 8 trailer payload, 4 trailer CRC — reproduces the previous
      `FIXTURE_SLPY_SHA` value `e5bc340e…` EXACTLY. So every compressed plane
      byte is bit-identical and the pin move is pure container identity.
      Corroborated across all assets: each rebuilt `.ascii` is the *same byte
      size* as the `.slpy` it replaces.
- [x] `FIXTURE_ASSET_SHA` re-pinned to `b00e3ecb…` after three consecutive
      identical builds, per the constant's own convention
- [x] perf gate keys intact — the 6 bench IDs are descriptive
      (`decode_delta_roll_480x270` etc.) and carried no renamed token, so the
      §4 key/ID hazard could not bite
- [x] examples build in all three feature tiers (M4 acceptance)
- [x] `./scripts/eval.sh` → **ALL GREEN** (251 s: tests 49, clippy 1, fuzz 17,
      perf 33, corpus 151). The corpus section took 151 s, not the 8–13 min
      predicted — the cache invalidation is real but cheaper than feared.
- [x] **corpus SSIM / edge F1 / flicker identical** — checked pre- vs
      post-rename, not just against `base.json`: ssim and edge_f1 match to the
      last digit on all three clips, and flicker / shot / cut / keyframe /
      damage counts compare at `+0.000000`. (The ~1e-5 ssim drift *against*
      `base.json` is present in the committed pre-rename `latest.json` too, so
      it predates this work.)
- [x] `asset_bytes` moved (grass 30,395,405 → 30,498,957) and that is
      **correct**: the pre-rename eval was serving a stale zstd-19 asset cached
      before the 2026-08-31 zstd 19 → 15 change. The rename flushed the cache,
      so the number is now the honest current one — and it matches the
      committed `assets/` build exactly, as the determinism guard (release
      rebuild, byte-compared) confirms. zstd is lossless, hence no metric moved.
- [x] two byte pins re-pinned, both proven to be container-identity only
- [x] assets rebuilt as `.ascii` and playable — verified headless via
      `--sim 213x58:120` (truecolor, all layers active)
- [x] `git grep -i slpy` / `sleepy` returns only `docs/research/`, `runs/`,
      `RENAME-PLAN.md`, HANDOFF's naming section, and the re-pin comment that
      documents the change
- [x] README's crates.io instructions updated — now `auto-ascii = "0.2"`
      (true from the moment §5 step 4 lands)

## 8. Rollback

Everything before §5 is a normal commit — `git revert` it. Once a crates.io
delete or publish has happened, that step is not reversible; a deleted name
may be re-registrable but a published version number never is. Do §5 last,
and only after the gate is green.
