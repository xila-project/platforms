use std::{
    env,
    time::{SystemTime, UNIX_EPOCH},
};

fn main() {
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
