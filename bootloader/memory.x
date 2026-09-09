MEMORY
{
  /* NOTE 1 K = 1 KiBi = 1024 bytes */
  /* Pico W / RP2040: 2 MiB flash. ACTIVE sized for ScoreboardCtrl (~512 KiB
     release image with cyw43 firmware + UI) plus headroom; DFU = ACTIVE + 4K. */
  BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
  FLASH            : ORIGIN = 0x10000100, LENGTH = 32K - 0x100
  BOOTLOADER_STATE : ORIGIN = 0x10008000, LENGTH = 4K
  ACTIVE           : ORIGIN = 0x10009000, LENGTH = 896K
  DFU              : ORIGIN = 0x100E9000, LENGTH = 900K

  RAM              : ORIGIN = 0x20000000, LENGTH = 264K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(BOOT2);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(BOOT2);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);
