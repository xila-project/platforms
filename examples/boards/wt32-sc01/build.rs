include!("../build_support.rs");

fn main() {
    build_board();
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        println!("cargo:rerun-if-changed=main_stack.x");
        let directory = std::env::var("CARGO_MANIFEST_DIR").expect("missing manifest directory");
        println!("cargo:rustc-link-arg=-T{directory}/main_stack.x");
        // The ROM's `memcpy`, `memmove` and `memset` must not write to PSRAM:
        // send every call through `drivers_espressif::rom_memory`.
        for function in ["memcpy", "memmove", "memset"] {
            println!("cargo:rustc-link-arg=-Wl,--wrap={function}");
        }
    }
}
