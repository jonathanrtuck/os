# Kernel Examples

Self-contained userspace programs that demonstrate the kernel's syscall API.
No dependencies beyond `core` — each example includes its own minimal runtime
with inline assembly syscall wrappers.

## Examples

| Example    | Demonstrates                                             |
| ---------- | -------------------------------------------------------- |
| `hello`    | Serial output, clock read, clean exit                    |
| `channels` | Channel IPC: create, shared memory write, signal, wait   |
| `threads`  | Thread creation, stack allocation, futex synchronization |
| `vmo`      | Virtual memory objects: create, map, handle read/write   |

## Build

```sh
cd kernel/examples
cargo build --release
```

Produces ELF binaries in `target/aarch64-unknown-none/release/`.

## Run

Each example replaces the kernel's stub init via the `OS_INIT_ELF` environment
variable:

```sh
# Build the kernel with the hello example as init
cd kernel
OS_INIT_ELF=examples/target/aarch64-unknown-none/release/hello cargo build --release

# Boot it
hypervisor target/aarch64-unknown-none/release/kernel
```

## Writing your own

1. Create `src/bin/myprogram.rs` with `#![no_std]`, `#![no_main]`
2. Import syscall wrappers from `kernel_examples` (the shared `lib.rs`)
3. Define `#[unsafe(no_mangle)] pub extern "C" fn _start() -> !`
4. Call `exit()` when done — returning from `_start` is undefined behavior
5. Add a `[[bin]]` entry to `Cargo.toml`

## Syscall ABI

Invoke via `svc #0`. Syscall number in `x8`, arguments in `x0`–`x5`, result
in `x0`. Negative return values are errors. All other registers are preserved.

See `../SYSCALLS.md` for the complete 46-syscall reference.

## Architecture

```text
examples/
├── .cargo/config.toml   # Target and linker flags
├── Cargo.toml            # Package with multiple [[bin]] targets
├── link.ld               # Userspace linker script (code at 4 MiB)
├── src/
│   ├── lib.rs            # Shared runtime: syscall wrappers, panic handler
│   └── bin/
│       ├── hello.rs      # Hello world
│       ├── channels.rs   # Channel IPC
│       ├── threads.rs    # Multi-threading + futex
│       └── vmo.rs        # Virtual memory objects
└── README.md
```

The shared `lib.rs` provides raw `syscall0`–`syscall4` functions and
higher-level wrappers for common operations. Each binary is a standalone
program that the kernel can boot as PID 1.
