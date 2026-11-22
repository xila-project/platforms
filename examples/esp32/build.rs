use std::{
    env, fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Reads the LittleFS partition from the partition table of the selected
/// chip, so that the firmware and the flashed table cannot disagree.
fn write_storage_partition() {
    // The ESP32-P4 board and the WT32-SC01 Plus have 16 MB of flash, the other
    // boards 4 MB. Keep in sync with `--flash-size` in
    // `tools/dev/build_espressif_image.sh`.
    let table = if env::var_os("CARGO_FEATURE_ESP32P4").is_some()
        || env::var_os("CARGO_FEATURE_WT32_SC01_PLUS").is_some()
    {
        "partitions_16mb.csv"
    } else {
        "partitions_4mb.csv"
    };
    println!("cargo:rerun-if-changed={table}");
    let content = fs::read_to_string(table).expect("Cannot read the partition table");
    let number = |field: &str| {
        let field = field.trim();
        match field
            .strip_prefix("0x")
            .or_else(|| field.strip_prefix("0X"))
        {
            Some(hexadecimal) => u32::from_str_radix(hexadecimal, 16),
            None => field.parse(),
        }
        .expect("Invalid number in the partition table")
    };
    let (offset, size) = content
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| line.split(',').map(str::trim).collect::<Vec<_>>())
        .find(|fields| fields.get(2) == Some(&"littlefs"))
        .map(|fields| (number(fields[3]), number(fields[4])))
        .unwrap_or_else(|| panic!("{table} has no littlefs partition"));
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("storage_partition.rs");
    fs::write(
        output,
        format!("const START: u32 = {offset:#x};\nconst LENGTH: u32 = {size:#x};\n"),
    )
    .unwrap();
}

fn main() {
    write_storage_partition();
    // The chip has no clock that survives a power cycle: the firmware starts
    // from the date it was built. `SOURCE_DATE_EPOCH` keeps builds reproducible.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let epoch = env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        });
    println!("cargo:rustc-env=XILA_BUILD_EPOCH={epoch}");

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
}
