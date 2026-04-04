# user/fuzz

Adversarial kernel syscall fuzzer. Exercises every syscall with invalid, edge-case, and hostile arguments. The kernel must never panic, hang, or corrupt state.

Phases 1-12 are single-process tests (bad arguments, resource exhaustion, concurrency). Phases 13-17 are cross-process tests using `fuzz-helper` (kill races, blocking, resource contention).

Uses raw `svc #0` inline assembly to bypass `sys` library validation, ensuring the kernel handles truly malformed inputs. Runs automatically in headless mode (no GPU detected).
