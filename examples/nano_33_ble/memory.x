/* Arduino Nano 33 BLE (nRF52840, 1 MiB of flash, 256 KiB of RAM).

   Flash: the Arduino bootloader owns the first 64 KiB and starts the
   application at 0x10000. The firmware gets 448 KiB; the last 512 KiB hold the
   LittleFS root file system, which the firmware formats when it finds none.
   The linker refuses a firmware that would reach the file system.

   The file system cannot be much smaller: a flash page is 4 KiB, LittleFS
   spends two blocks on every directory, and the default hierarchy of Xila
   alone has 35 of them (288 KiB).

   RAM: the bootloader runs at every reset and uses the bottom of the RAM, so
   what must survive a reset (the message of a crash) lives in the last 256
   bytes, which nothing of the bootloader reaches. */
MEMORY
{
  FLASH   : ORIGIN = 0x00010000, LENGTH = 448K
  STORAGE : ORIGIN = 0x00080000, LENGTH = 512K
  RAM     : ORIGIN = 0x20000000, LENGTH = 256K - 256
  CRASH   : ORIGIN = 0x20000000 + 256K - 256, LENGTH = 256
}

SECTIONS
{
  .crash_record (NOLOAD) :
  {
    KEEP(*(.crash_record));
  } > CRASH
}

_storage_start = ORIGIN(STORAGE);
_storage_size = LENGTH(STORAGE);

/* The stack: the 32 KiB below the crash record. The heap lies between the
   static data and this limit. */
_heap_end = ORIGIN(RAM) + LENGTH(RAM) - 32K;
