# system/test

Host-side test suite and QEMU integration scripts for the kernel.

## Unit Tests (`tests/`)

Host-compiled tests that exercise kernel logic in isolation. Each file includes kernel source via `#[path]` with stub dependencies (mock IrqMutex, identity PA/VA mapping). Run with `--test-threads=1` because some tests share global state.

```sh
cargo test -- --test-threads=1
```

## QEMU Scripts

All scripts build the kernel (release) and boot QEMU automatically.

| Script                     | Devices              | Display         | Duration     | What it tests                                            |
| -------------------------- | -------------------- | --------------- | ------------ | -------------------------------------------------------- |
| `smoke.sh`                 | blk                  | none            | 10s          | Boot sequence, serial output markers                     |
| `stress.sh [timeout]`      | blk                  | none            | 180s default | Headless fuzz + IPC/scheduler/timer stress (4 SMP cores) |
| `crash.sh [duration]`      | blk + gpu + keyboard | macOS window    | 30s default  | Rapid keystroke input via AppleScript                    |
| `integration.sh [timeout]` | blk + gpu + keyboard | `-display none` | 15s default  | Full boot + driver spawn + display pipeline              |

**QMP limitation:** QEMU's `sendkey`/`input-send-event` does NOT route to `virtio-keyboard-device`. `crash.sh` works around this by sending real keystrokes to the QEMU window via AppleScript (macOS only). The other scripts don't require keyboard input.

## Conventions

- Test files use category prefixes for subsetting (e.g., `cargo test kernel_` runs only kernel tests, `cargo test mem_` runs memory tests)
- Tests prefixed with `stress_adversarial_` are stress/fuzz tests targeting audit findings
- `#[cfg_attr(miri, ignore)]` on tests incompatible with Miri (FFI, inline asm stubs, integer-to-pointer casts)
- OOM fault injection: `page_allocator::set_fail_after(Some(n))` makes allocations fail after n successes
