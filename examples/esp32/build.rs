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
}
