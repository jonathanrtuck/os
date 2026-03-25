/* Userspace linker script.
 *
 * Base VA 0x400000 (4 MB) — low enough to leave room for stack/heap above,
 * high enough to catch null-pointer dereferences. All sections 16 KiB-aligned
 * so the kernel's ELF loader can map whole pages with correct permissions
 * (the loader assumes page-aligned segment vaddrs). Must match kernel PAGE_SIZE.
 */
ENTRY(_start)
SECTIONS {
    . = 0x400000;
    .text   : ALIGN(16384) { *(.text .text.*) }
    .rodata : ALIGN(16384) { *(.rodata .rodata.*) }
    .data   : ALIGN(16384) { *(.data .data.*) }
    .bss    : ALIGN(16384) { *(.bss .bss.* COMMON) }
}
