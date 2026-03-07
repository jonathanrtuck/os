// Init process — the first userspace program loaded by the kernel.
//
// Compiled into a standalone ELF by build.rs, embedded in the kernel image,
// and loaded into a fresh per-process address space at boot.
// Uses PC-relative `adr` so the code works at any VA (the kernel maps it
// at the vaddr specified in the ELF program headers).

.section .text
.global _start
_start:
    adr x0, msg         // PC-relative: works regardless of load address
    mov x1, #15         // length of "hello from EL0\n"
    mov x8, #1          // SYS_WRITE
    svc #0
    mov x8, #0          // SYS_EXIT
    svc #0

.section .rodata
msg:
    .ascii "hello from EL0\n"
