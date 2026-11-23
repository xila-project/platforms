include!("../build_support.rs");

fn main() {
    build_board();
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        println!("cargo:rerun-if-changed=main_stack.x");
        let directory = std::env::var("CARGO_MANIFEST_DIR").expect("missing manifest directory");
        println!("cargo:rustc-link-arg=-T{directory}/main_stack.x");
    }
}
