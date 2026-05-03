//! Kernel console output.
//!
//! Provides [`print!`] and [`println!`] macros that route all formatted output
//! through the architecture's serial driver. Code outside `arch/` uses these
//! macros and never names the underlying device.

/// Print formatted text to the kernel console.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;

        let _ = write!($crate::arch::serial::Writer, $($arg)*);
    }};
}

/// Print formatted text to the kernel console, followed by a newline.
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {{
        use core::fmt::Write;

        let _ = writeln!($crate::arch::serial::Writer, $($arg)*);
    }};
}
