//! EL0 test stub — pure assembly in .user_code section.

core::arch::global_asm!(
    ".section .user_code, \"ax\"",
    ".global user_test_entry",
    "user_test_entry:",
    "  adr x0, 1f",
    "  mov x1, #15",
    "  mov x8, #1", // SYS_WRITE
    "  svc #0",
    "  mov x8, #0", // SYS_EXIT
    "  svc #0",
    "1: .ascii \"hello from EL0\\n\"",
);

extern "C" {
    pub fn user_test_entry();
}
