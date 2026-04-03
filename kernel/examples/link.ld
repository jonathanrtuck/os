/* Userspace linker script for example programs.
 *
 * Code loads at 4 MiB (USER_CODE_BASE). Sections aligned to 16 KiB pages
 * so the kernel's ELF loader can map them with correct R/RW/RX permissions.
 */
ENTRY(_start)
SECTIONS {
    . = 0x0000000000400000;
    .text   : ALIGN(16384) { *(.text .text.*) }
    .rodata : ALIGN(16384) { *(.rodata .rodata.*) }
    .data   : ALIGN(16384) { *(.data .data.*) }
    .bss    : ALIGN(16384) { *(.bss .bss.* COMMON) }
}
