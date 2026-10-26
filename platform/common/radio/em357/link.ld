/* The EM357's memory map for the SDK's gcc.ld, which has one only for EFR32.
   Flash is at 0x08000000; Nest's bootloader keeps the first 8 KiB, so the
   application address table lands at 0x08002000. */
ENTRY(Reset_Handler)

MEMORY {
  flash (rx) : ORIGIN = 0x08000000, LENGTH = 192K
  ram  (rwx) : ORIGIN = 0x20000000, LENGTH = 12K
}

RAM_BOTTOM      = 0x20000000;
RAM_TOP         = RAM_BOTTOM + 12K - 1;
MFB_BOTTOM      = 0x08000000;
MFB_TOP         = MFB_BOTTOM + 192K - 1;
FIB_BOTTOM      = 0x08040000;
FIB_TOP         = 0x080407FF;
FLASH_PAGE_SIZE = 0x800;
DEFAULT_BTL_SIZE = 0x2000;
UNUSED_RAM_SIZE = 0;
SIMEEPROM_SIZE  = 8K;

/* The IAR-built libraries name the stack and heap by IAR's segment symbols. */
"CSTACK$$Base"  = __CSTACK__begin;
"CSTACK$$Limit" = __CSTACK__end;
"EMHEAP$$Base"  = __EMHEAP__begin;
"EMHEAP$$Limit" = __EMHEAP__end;

INCLUDE "gcc.ld"

/* Non-loaded sections, after everything: an orphan placed before .text resets
   its location to 0. IAR's own metadata is dropped. */
SECTIONS {
  .ARM.attributes 0 : { KEEP(*(.ARM.attributes)) }
  /DISCARD/ : { *(.iar.*) *(.iar_vfe_header) *(.comment) *(__DLIB_PERTHREAD_init) }
}
