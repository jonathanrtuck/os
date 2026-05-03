//! Kernel-level configuration constants.
//!
//! Policy decisions and capacity limits that are independent of the target
//! architecture. Platform-specific values (device addresses, RAM layout)
//! live in `frame/arch/aarch64/platform.rs`.

/// Kernel stack size per core. — link.ld sync: `.bss.stack`
pub const KERNEL_STACK_SIZE: usize = 64 * 1024;

/// Maximum number of CPU cores.
pub const MAX_CORES: usize = 8;
