// The build script of every Espressif board crate includes this file:
//
//     include!("../build_support.rs");
//
//     fn main() {
//         build_board();
//     }
//
// It links the firmware with esp-hal's linker scripts and passes the storage
// partition of the crate's `partitions.csv` to the firmware, so that the table
// and the firmware cannot disagree.

use std::{env, fs};

/// The partition subtype of LittleFS in `partitions.csv`.
const STORAGE_SUBTYPE: &str = "littlefs";

fn build_board() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        println!("cargo:rustc-link-arg=-Tlinkall.x");
    }

    // esp-hal's ESP32-S3 linker script reserves the flash address range of
    // `.rodata` inside the instruction mapping with the `.rotext_dummy`
    // placeholder, because the same flash pages are mapped for both data and
    // instruction access. The placeholder is an empty `NOLOAD` section that
    // binutils flags as writable, so the linker merges it with `.text` into one
    // read, write and execute `LOAD` segment and warns. Nothing is ever written
    // there: the segment is backed by the read-only flash cache mapping, and the
    // placeholder holds no data. Code is already separate from the writable
    // data, which lives in the RAM segments. The linker cannot silence the
    // warning for a single segment, so this disables it for the Xtensa
    // firmware, the only target whose script defines such a section. Revisit it
    // when esp-hal stops flagging the placeholder as writable.
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        println!("cargo:rustc-link-arg=-Wl,--no-warn-rwx-segments");
    }

    println!("cargo:rerun-if-changed=partitions.csv");
    let table = fs::read_to_string("partitions.csv").expect("cannot read partitions.csv");
    let (offset, length) = find_partition(&table, STORAGE_SUBTYPE)
        .unwrap_or_else(|| panic!("partitions.csv has no `{STORAGE_SUBTYPE}` partition"));
    println!("cargo:rustc-env=XILA_STORAGE_OFFSET={offset}");
    println!("cargo:rustc-env=XILA_STORAGE_LENGTH={length}");
}

/// The offset and size of the first partition of `subtype` in an ESP-IDF
/// partition table: `Name, Type, SubType, Offset, Size, Flags` per line, `#`
/// for comments.
fn find_partition(table: &str, subtype: &str) -> Option<(u32, u32)> {
    table.lines().find_map(|line| {
        let line = line.split('#').next().unwrap_or_default();
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() < 5 || fields[2] != subtype {
            return None;
        }
        Some((parse_size(fields[3]), parse_size(fields[4])))
    })
}

/// A hexadecimal (`0x…`) or decimal number of bytes, optionally with a `K` or
/// `M` suffix as in ESP-IDF tables.
fn parse_size(text: &str) -> u32 {
    let (number, multiplier) = match text.as_bytes().last() {
        Some(b'K' | b'k') => (&text[..text.len() - 1], 1024),
        Some(b'M' | b'm') => (&text[..text.len() - 1], 1024 * 1024),
        _ => (text, 1),
    };
    let value = match number
        .strip_prefix("0x")
        .or_else(|| number.strip_prefix("0X"))
    {
        Some(hexadecimal) => u32::from_str_radix(hexadecimal, 16),
        None => number.parse(),
    }
    .unwrap_or_else(|_| panic!("invalid size in partitions.csv: `{text}`"));
    value
        .checked_mul(multiplier)
        .unwrap_or_else(|| panic!("size out of range in partitions.csv: `{text}`"))
}
