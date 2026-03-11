# user

Userspace programs. Each is a `#![no_std]` ELF binary embedded into init at build time.

| Program        | Purpose                                                                                |
| -------------- | -------------------------------------------------------------------------------------- |
| `echo/`        | Minimal test program (proof of userspace execution)                                    |
| `fuzz/`        | Adversarial syscall fuzzer: 31 phases of invalid/edge-case syscalls                    |
| `fuzz-helper/` | Child process spawned by the fuzzer for process lifecycle tests                        |
| `stress/`      | IPC/scheduler/timer stress test: 3 channel pairs, 10M iterations per worker, 7 threads |

## Conventions

- Programs are built by `system/build.rs` and embedded as `&[u8]` ELF blobs in init
- `fuzz/` and `stress/` run automatically in headless mode (no GPU detected)
- All programs use `libraries/sys` for syscalls and heap allocation
