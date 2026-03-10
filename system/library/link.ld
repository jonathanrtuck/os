/* Userspace init process linker script.
 *
 * Base VA 0x400000 (4 MB) — low enough to leave room for stack/heap above,
 * high enough to catch null-pointer dereferences. All sections page-aligned
 * so the kernel's ELF loader can map whole pages with correct permissions
 * (the loader assumes page-aligned segment vaddrs).
 */
ENTRY(_start)
SECTIONS {
    . = 0x400000;
    .text   : ALIGN(4096) { *(.text .text.*) }
    .rodata : ALIGN(4096) { *(.rodata .rodata.*) }
    .data   : ALIGN(4096) { *(.data .data.*) }
    .bss    : ALIGN(4096) { *(.bss .bss.* COMMON) }
}
