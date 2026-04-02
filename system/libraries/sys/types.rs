//! Handle types and common type aliases for the syscall interface.

/// Convenience alias for syscall results.
pub type SyscallResult<T> = Result<T, super::SyscallError>;

/// Handle to a kernel channel endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct ChannelHandle(pub u16);

/// Handle to a registered hardware interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct InterruptHandle(pub u16);

/// Handle to a child process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct ProcessHandle(pub u16);

/// Handle to a scheduling context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct SchedHandle(pub u16);

/// Handle to a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct ThreadHandle(pub u16);

/// Handle to a one-shot timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct TimerHandle(pub u16);

/// Handle to a Virtual Memory Object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct VmoHandle(pub u16);

/// Heap usage statistics.
#[derive(Clone, Copy, Debug)]
pub struct HeapStats {
    /// Cumulative bytes handed out by alloc().
    pub total_allocated: usize,
    /// Cumulative bytes returned by dealloc().
    pub total_freed: usize,
    /// Pages requested from the kernel via memory_alloc().
    pub pages_requested: usize,
}
