# Host target for running tests (the workspace default is aarch64-unknown-none).
HOST_TARGET := aarch64-apple-darwin

.PHONY: test build check clippy fmt bench clean integration-test \
        miri asan fuzz mutants coverage gate nightly \
        stress bench-check bench-baseline audit integration-release

# -- Core targets --

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

integration-test:
	@scripts/integration-test

clean:
	cargo clean

# -- Verification targets --

miri:
	cargo +nightly miri test -p kernel --lib --target $(HOST_TARGET)

asan:
	RUSTFLAGS="-Z sanitizer=address" cargo +nightly test -p kernel --lib --target $(HOST_TARGET)

fuzz:
	cd kernel && cargo +nightly fuzz run syscall_sequence -- -max_total_time=3600

coverage:
	RUSTFLAGS="-C instrument-coverage" cargo test -p kernel --lib --target $(HOST_TARGET)
	@echo "Run grcov or llvm-cov to generate report from .profraw files"

mutants:
	CARGO_BUILD_TARGET=$(HOST_TARGET) cargo mutants -p kernel --timeout 30

# -- Bare-metal targets --

integration-release:
	@scripts/integration-test --release

stress:
	@scripts/integration-test --stress 100

bench-check:
	@scripts/bench-test

bench-baseline:
	@scripts/bench-test --update-baseline

audit:
	cargo audit

# -- Gates --

gate: clippy test build
	@echo "Gate passed: clippy + tests + build"

nightly: gate miri asan fuzz coverage integration-test integration-release stress
	@echo "Nightly gate passed: all verification targets"
