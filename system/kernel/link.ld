ENTRY(_start)

KERNEL_VA_OFFSET = 0xFFFF000000000000;
PHYS_BASE        = 0x40080000;

SECTIONS
{
  /*
   * .text.boot runs at physical VA before TTBR1 is live.
   * VMA = LMA = physical address.
   */
  . = PHYS_BASE;

  __text_boot_start = .;
  .text.boot : {
    KEEP(*(.text.boot))
  }
  __text_boot_end = .;

  /*
   * Everything below runs at upper VA (TTBR1).
   * VMA = PA + KERNEL_VA_OFFSET.  LMA = PA (via AT()).
   */
  . = ALIGN(4096) + KERNEL_VA_OFFSET;

  /* --- Executable, read-only (RX) --- */
  __text_start = .;
  .text : AT(ADDR(.text) - KERNEL_VA_OFFSET) ALIGN(16) {
    *(.text .text.*)
  }
  __text_end = .;

  /* --- Read-only, no execute (RO) --- */
  . = ALIGN(4096);
  __rodata_start = .;
  .rodata : AT(ADDR(.rodata) - KERNEL_VA_OFFSET) {
    *(.rodata .rodata.*)
  }
  __rodata_end = .;

  /* --- Read-write, no execute (RW) --- */
  . = ALIGN(4096);
  __data_start = .;
  .data : AT(ADDR(.data) - KERNEL_VA_OFFSET) {
    *(.data .data.*)
  }
  .bss : AT(ADDR(.bss) - KERNEL_VA_OFFSET) ALIGN(16) {
    __bss_start = .;
    *(.bss .bss.*)
    *(COMMON)
    __bss_end = .;
  }
  . = ALIGN(16);
  .bss.stack (NOLOAD) : AT(ADDR(.bss.stack) - KERNEL_VA_OFFSET) ALIGN(16) {
    __stack_bottom = .;
    . = . + 0x10000; /* 64 KiB primary kernel stack */
    __stack_top = .;
  }

  /* Secondary core kernel stacks: 7 cores × 16 KiB = 112 KiB.
   * Core N (1..7) uses __secondary_stacks + N * 0x4000 as its stack top. */
  .bss.stacks (NOLOAD) : AT(ADDR(.bss.stacks) - KERNEL_VA_OFFSET) ALIGN(16) {
    __secondary_stacks = .;
    . = . + (7 * 0x4000); /* 7 × 16 KiB */
    __secondary_stacks_end = .;
  }

  /* Physical companions for boot code (which runs before TTBR1 is live). */
  __bss_start_phys  = __bss_start  - KERNEL_VA_OFFSET;
  __bss_end_phys    = __bss_end    - KERNEL_VA_OFFSET;
  __stack_top_phys  = __stack_top  - KERNEL_VA_OFFSET;

  . = ALIGN(4096);
  __kernel_end = .;
}
