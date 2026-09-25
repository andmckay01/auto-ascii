.PHONY: build release dist test eval clean

build:
	cargo build --release -p auto-ascii --features bin

dist release:
	./scripts/release.sh

test:
	cargo test --workspace

eval:
	./scripts/eval.sh

clean:
	cargo clean
	rm -rf dist
