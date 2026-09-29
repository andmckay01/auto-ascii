.PHONY: build release dist test eval comments lint clean

build:
	cargo build --release -p auto-ascii-cli

dist release:
	./scripts/release.sh

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
