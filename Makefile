# Host target for verification tools that can't run on bare metal.
HOST := aarch64-apple-darwin

.PHONY: test test-all build check clippy fmt bench bench-el0 bench-smp bench-render clean \
        integration-test miri asan fuzz mutants coverage gate nightly \
        stress bench-check bench-baseline audit integration-release visual-test

# -- Core targets --

test:
	cargo t
	cargo test --manifest-path user/drivers/render/Cargo.toml \
		--lib --no-default-features --target $(HOST)

build:
	cargo build -p kernel

check:
	cargo check -p kernel

clippy:
	cargo clippy -p kernel -- -D warnings

fmt:
	cargo +nightly fmt

bench:
	cargo run -p kernel --release --features bench

bench-profile:
	cargo run -p kernel --release --features bench,profile

bench-el0:
	cargo run -p kernel --release --features bench-el0

bench-smp:
	cargo build -p kernel --release --features bench-smp
	hypervisor --no-gpu --timeout 60 target/aarch64-unknown-none/release/kernel

bench-render:
	@cargo build --release 2>&1 | tail -1
	@user/shared/benchmarks/render/bench.sh

integration-test:
	@scripts/integration-test

visual-test:
	@cargo build --release 2>&1 | tail -1
	@test/visual-regression.sh

clean:
	cargo clean

# -- Verification targets --

miri:
	MIRIFLAGS="-Zmiri-isolation-error=warn" cargo +nightly miri test -p kernel --lib --target $(HOST)

asan:
	RUSTFLAGS="-Z sanitizer=address" cargo +nightly test -p kernel --lib --target $(HOST)

fuzz:
	cd kernel && cargo +nightly fuzz run syscall_sequence -- -max_total_time=3600

coverage:
	RUSTFLAGS="-C instrument-coverage" cargo test -p kernel --lib --target $(HOST)
	@echo "Run grcov or llvm-cov to generate report from .profraw files"

mutants:
	CARGO_BUILD_TARGET=aarch64-apple-darwin cargo mutants -p kernel --timeout 30

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

gate: clippy test build visual-test
	@echo "Gate passed: clippy + tests + build + visual"

test-all: gate
	@echo "All tests passed"

nightly: gate miri asan fuzz coverage mutants integration-test integration-release stress bench-check audit visual-test
	@echo "Nightly gate passed: all verification targets"
