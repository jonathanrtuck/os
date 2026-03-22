# system

Bare-metal aarch64 system for QEMU `virt`. Microkernel architecture.

## Structure

```text
system/
├── kernel/        # Microkernel (aarch64-unknown-none) — memory, scheduling, IPC, 28 syscalls
├── libraries/     # Shared userspace libraries (sys, virtio, drawing, fonts, layout, render, scene, ipc, protocol)
├── services/      # Platform services (init, core, drivers)
├── user/          # User programs (text-editor, echo, fuzz, fuzz-helper, stress)
├── test/          # Host-side unit tests + QEMU integration scripts
├── build.rs       # Orchestrates the full build: libraries → user programs → init → kernel
├── run.sh         # VM launcher (hypervisor default, QEMU=1 for QEMU)
└── Cargo.toml     # Single workspace root
```

## Build

```sh
cargo build --release   # Builds everything
cargo run --release     # Builds + launches QEMU
```

A single `cargo build` compiles shared libraries, all userspace programs, init (which embeds them as ELF blobs), and the kernel (which embeds init). See `build.rs`.

## Test

```sh
cd test && cargo test -- --test-threads=1   # 2,091 host-side unit tests
cd test && ./smoke.sh                        # QEMU boot verification
cd test && ./integration.sh                  # Full device pipeline test
cd test && ./stress.sh 45                    # Headless fuzz + stress
```

## Key Design Docs

- `DESIGN.md` — Userspace architecture: libraries, services, drivers, component status
- `kernel/DESIGN.md` — Kernel internals: every subsystem's rationale (1462 lines)

## Conventions

- Rust nightly, `aarch64-unknown-none` target (via `rust-toolchain.toml`)
- No std in kernel or userspace — `#![no_std]` everywhere
- Userspace gets `alloc` via the `sys` library's GlobalAlloc backed by `memory_alloc` syscall
- All userspace ELF binaries are embedded into init at build time
