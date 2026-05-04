# Host target for running tests (the workspace default is aarch64-unknown-none).
HOST_TARGET := aarch64-apple-darwin

.PHONY: test build check clippy fmt bench clean

test:
	cargo test -p kernel --lib --target $(HOST_TARGET)

build:
	cargo build -p kernel

check:
	cargo check -p kernel

clippy:
	cargo clippy -p kernel -- -D warnings

fmt:
	cargo +nightly fmt

bench:
	cargo run -p kernel --release

clean:
	cargo clean
