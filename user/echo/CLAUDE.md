# user/echo

Minimal IPC test program. Waits for init's signal, reads "ping" from shared memory, writes "pong" back, and signals init. Also smoke-tests dynamic heap allocation via `Vec`.

This is the simplest proof of userspace execution: demonstrates shared-memory IPC, the `sys` library's syscall interface, and the global allocator backed by `memory_alloc`.
