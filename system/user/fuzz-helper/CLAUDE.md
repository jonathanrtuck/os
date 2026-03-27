# user/fuzz-helper

Minimal child process spawned by `fuzz/` for cross-process lifecycle tests. Behavior is controlled by the first byte written to shared memory by the parent:

- `0x01` -- Block forever on a timer (test process kill while blocked)
- `0x02` -- Busy-loop calling yield (test kill while running)
- `0x03` -- Exit immediately (test rapid spawn/exit)
- `0x04` -- Create blocking threads, then exit main thread (test thread cleanup)
- `0x05` -- Rapid channel create/close loop (test resource churn)
- `0x06` -- Signal channel handle 0 and exit (test handle transfer)
