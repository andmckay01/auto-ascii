# Releases: push a vX.Y.Z tag — see docs/RELEASING.md
.PHONY: build test eval comments lint clean

build:
	cargo build --release -p auto-ascii

test:
	cargo test --workspace

eval:
	./scripts/eval.sh

comments:
	cargo run --quiet --release -p auto-ascii-lint --bin check-comments --

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	$(MAKE) comments

clean:
	cargo clean
	rm -rf dist
