# auto-ascii — convenience targets. The real gates live in scripts/
# (scripts/eval.sh is what "green" means; scripts/release.sh ships binaries).

.PHONY: build release dist test eval clean

# The player binary for THIS machine (target/release/auto-ascii-player).
build:
	cargo build --release -p auto-ascii --features bin

# Stripped release binaries into dist/: native + static musl + windows-gnu
# cross, each gated < 5 MB (M5 item E). Linux-hosted; needs rustup + apt.
dist release:
	./scripts/release.sh

test:
	cargo test --workspace

# The one-command evaluation loop (tests, clippy, fuzz, perf gates, corpus).
eval:
	./scripts/eval.sh

clean:
	cargo clean
	rm -rf dist

# --- macOS ------------------------------------------------------------------
# There is no macOS cross build (no osxcross by policy — PLAN §7 M5): build on
# a Mac instead. Both Apple Silicon and Intel work from source:
#
#     make build          # or: cargo build --release -p auto-ascii --features bin
#     strip target/release/auto-ascii-player
#
# The player uses only crossterm + POSIX termios/ioctl (the same unix session
# layer as Linux), ffmpeg is not needed at playback time, and Terminal.app /
# iTerm2 / kitty-on-mac are all covered by the same capability probe. The
# factory also builds on mac unchanged; it shells out to ffmpeg
# (`brew install ffmpeg`) for ingest only.
