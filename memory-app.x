MEMORY
{
  /* Must match bootloader/memory.x partition map.
     BOOT2 is only used for embassy-boot linker-symbol offsets; the
     bootloader owns the real stage2 image. */
  BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
  BOOTLOADER_STATE : ORIGIN = 0x10008000, LENGTH = 4K
  /* ACTIVE partition — application image */
  FLASH            : ORIGIN = 0x10009000, LENGTH = 896K
  DFU              : ORIGIN = 0x100E9000, LENGTH = 900K

  /* Leave 1K at top of SRAM for panic-persist */
  RAM     : ORIGIN = 0x20000000, LENGTH = 200K
  PANDUMP : ORIGIN = 0x20000000 + 200K, LENGTH = 1K
}

_panic_dump_start = ORIGIN(PANDUMP);
_panic_dump_end   = ORIGIN(PANDUMP) + LENGTH(PANDUMP);

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);

/* ACTIVE == FLASH for the application image. */
__bootloader_active_start = ORIGIN(FLASH) - ORIGIN(BOOT2);
__bootloader_active_end = ORIGIN(FLASH) + LENGTH(FLASH) - ORIGIN(BOOT2);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);

/* Bootloader owns stage2; do not embed .boot2 in the ACTIVE image. */
SECTIONS {
  /DISCARD/ : {
    *(.boot2)
  }
}
