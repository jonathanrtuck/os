/*
 * Init service linker script — userspace binary at low VA.
 *
 * Must match INIT_CODE_VA in kernel/src/bootstrap.rs.
 */

ENTRY(_start)

INIT_VA_BASE = 0x00200000;

SECTIONS
{
    . = INIT_VA_BASE;

    .text : {
        *(.text .text.*)
    }

    . = ALIGN(16);
    .rodata : {
        *(.rodata .rodata.*)
    }

    . = ALIGN(16);
    .data : {
        *(.data .data.*)
    }

    . = ALIGN(16);
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment .note.*)
    }
}
