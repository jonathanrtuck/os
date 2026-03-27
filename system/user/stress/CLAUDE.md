# user/stress

IPC and scheduler stress test. Saturates channel signaling, wait multiplexing, and timer paths under high concurrency.

Creates 3 channel pairs with worker threads doing tight ping-pong loops (wait then signal, 10M iterations each). The main thread simultaneously creates and destroys timers to stress the timer table and allocator. Runs headless (no GPU needed).

Reproduces the syscall patterns that trigger kernel crashes under concurrent load.
