ENTRY(_start)

SECTIONS
{
  /*
   * QEMU `virt` default RAM starts at 0x4000_0000.
   * A common convention is to place the kernel at 0x4008_0000.
   *
   * Sections are 4KB-aligned so each group can have distinct page
   * permissions in the L3 table: .text (RX), .rodata (RO), .data/.bss (RW).
   */
  . = 0x40080000;

  /* --- Executable, read-only (RX) --- */
  __text_start = .;
  .text.boot : {
    KEEP(*(.text.boot))
  }
  .text : ALIGN(16) {
    *(.text .text.*)
  }
  __text_end = .;

  /* --- Read-only, no execute (RO) --- */
  . = ALIGN(4096);
  __rodata_start = .;
  .rodata : {
    *(.rodata .rodata.*)
  }
  __rodata_end = .;

  /* --- Read-write, no execute (RW) --- */
  . = ALIGN(4096);
  __data_start = .;
  .data : {
    *(.data .data.*)
  }
  .bss : ALIGN(16) {
    __bss_start = .;
    *(.bss .bss.*)
    *(COMMON)
    __bss_end = .;
  }
  . = ALIGN(16);
  .bss.stack (NOLOAD) : ALIGN(16) {
    __stack_bottom = .;
    . = . + 0x10000; /* 64 KiB stack */
    __stack_top = .;
  }
  . = ALIGN(0x200000);     /* next 2MB boundary -> lands in bulk RAM block */
  .user_code : {
    *(.user_code)
  }
  __kernel_end = .;
}
