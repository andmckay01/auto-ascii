# RENAME PLAN — everything to `auto-ascii`

Written 2026-09-19 as a save point. **Not started.** The repo at `68e8c43` is
clean, green and pushed; nothing in this document has been applied yet.

Owner decisions (2026-09-19), all three confirmed:

1. **Scope: everything.** The `slpy` token (1097 occurrences), the `sleepy`
   token (279), the magic bytes, and the `.slpy` extension.
2. **Magic: `SLPY` → `ASCI`.**
3. **crates.io: delete the published crates and republish under the new
   names.** (Owner amended the original "leave them, publish 0.2.0" answer to
   "delete and republish" — see §5, which is the part with real constraints.)

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
3. **Magic bytes** in `crates/auto-ascii-format/src/header.rs`: `MAGIC`, and
   the `assert_eq!(&b[0..4], b"SLPY")` unit test beside it.
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
| `FIXTURE_SLPY_SHA` | magic is inside the hashed bytes | re-pin; rename the const to `FIXTURE_ASSET_SHA`. Verify **three consecutive identical builds** before pinning, as its own comment history requires |
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
the **crates.io web UI**, and only while a crate qualifies — broadly: recently
published, negligible downloads, single owner, and **no reverse dependencies**.
All four were published minutes before this plan was written with 0 downloads,
so they should qualify, but **the UI is the authority — confirm there rather
than assuming.**

**Order matters, because of the reverse-dependency rule:**

1. Delete **`auto-ascii` 0.1.0 first** — it depends on the other three, so
   they cannot be deleted while it exists.
2. Then `slpy-term`, then `slpy-core` and `slpy-format` (any order).
3. Each at `https://crates.io/crates/<name>` → Settings → delete.

**If deletion is refused**, fall back to `cargo yank --version 0.1.0 -p <name>`
for each. Yanking does not free the name — but the names being freed does not
matter here, since nothing will be republished under them.

**Then republish under the new names**, leaf-first, verifying each lands.
A first publish of interdependent crates **cannot be dry-run validated** —
`cargo publish --dry-run` fails with "no matching package named …" because the
dependencies are not on the registry yet. That is a dry-run limitation, not a
defect; publish sequentially instead:

```
cargo publish -p auto-ascii-core      # no internal deps
cargo publish -p auto-ascii-format    # no internal deps
cargo publish -p auto-ascii-term      # needs core
cargo publish -p auto-ascii           # needs all three
```

Wait for the index between each. `auto-ascii-eval` and `auto-ascii-factory`
stay **unpublished**, as `slpy-eval`/`sleepy-factory` did.

**Before publishing, clear `target/package/`** — stale `.crate` tarballs there
caused a confusing false failure last time.

**Account gates already cleared:** crates.io email is verified, and the token
is in `~/.cargo/credentials.toml`.

---

## 6. Version number

The owner chose "leave them, publish 0.2.0" and then amended to delete +
republish. With the old crates deleted, **0.1.0 is the honest number** for a
first publish under new names — there is no earlier version for them. Keep
`auto-ascii` at 0.1.0 too if its 0.1.0 is successfully deleted; use 0.2.0 if
it could only be yanked, since crates.io will refuse to reuse a published
version number.

---

## 7. Verification checklist

- [ ] `cargo build --workspace` clean
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo tree -p auto-ascii -e normal | grep -c rayon` → 0
- [ ] every render golden **unchanged** (§3 — the load-bearing invariant)
- [ ] corpus SSIM / edge F1 / flicker identical to `runs/base.json`
- [ ] perf gate still reports 6 benches
- [ ] `./scripts/eval.sh` → ALL GREEN
- [ ] all 6 assets rebuilt as `.ascii` and playable
- [ ] `git grep -i slpy` and `git grep -i sleepy` return only
      `docs/research/`, `runs/`, and this file
- [ ] README's crates.io instructions updated (it currently still says
      "not on crates.io yet" — stale since the 0.1.0 publish)

---

## 8. Rollback

Everything before §5 is a normal commit — `git revert` it. Once a crates.io
delete or publish has happened, that step is not reversible; a deleted name
may be re-registrable but a published version number never is. Do §5 last,
and only after the gate is green.
