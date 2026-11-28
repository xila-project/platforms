/* Arduino Nano 33 BLE (nRF52840, 1 MiB of flash, 256 KiB of RAM) — native
   developer and linked-C layout.

   Flash: the Arduino bootloader owns the first 64 KiB and starts the
   application at 0x10000. Enabling `native-dev` or `native-linked` expands the
   firmware past the default 448 KiB slot, so this layout gives the firmware
   576 KiB (`0x10000..0xA0000`) and leaves 384 KiB (`0xA0000..0x100000`) for
   the LittleFS root file system (96 4-KiB pages, still above the 288 KiB
   needed by the default Xila directory hierarchy).

   RAM: identical to `memory.x`. The bootloader runs at every reset and uses
   the bottom of the RAM, so what must survive a reset (the message of a crash)
   lives in the last 256 bytes, which nothing of the bootloader reaches. */
MEMORY
{
  FLASH   : ORIGIN = 0x00010000, LENGTH = 576K
  STORAGE : ORIGIN = 0x000A0000, LENGTH = 384K
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

/* The C SDK uses position-independent code. The static link resolves the
   global offset table entries, so the table can stay in flash. */
SECTIONS
{
  .linked_got : { *(.got .got.*) } > FLASH
} INSERT AFTER .rodata;
