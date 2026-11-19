#!/usr/bin/env bash
# Builds the ESP32-S3 example for Espressif's QEMU and merges the bootloader,
# partition table and application into one 4 MiB flash image:
# target/espressif-emulator/esp32s3-flash.bin.
#
#   tools/dev/build_espressif_image.sh ESPFLASH_VERSION ESP_RUST_VERSION ESP_GCC_VERSION
#
# Needs the `esp` toolchain from `mise run setup:espressif-rust` and the espflash
# from `mise run setup:espressif-emulator`.
set -euo pipefail

espflash_version=$1
esp_rust_version=$2
esp_gcc_version=$3
root="$PWD/target/espressif-emulator"
target=xtensa-esp32s3-none-elf
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
mkdir -p "$root"

compiler=$(rustup which --toolchain esp rustc)
case "$("$compiler" --version)" in
    *"($esp_rust_version)") ;;
    *) echo "Expected the Espressif Rust toolchain $esp_rust_version; run mise run setup:espressif-rust." >&2; exit 1 ;;
esac
esp_root=${compiler%/bin/rustc}
gcc_bin="$esp_root/xtensa-esp-elf/esp-$esp_gcc_version/xtensa-esp-elf/bin"
test -x "$gcc_bin/xtensa-esp32s3-elf-gcc" || {
    echo "Expected Xtensa GCC $esp_gcc_version in $gcc_bin." >&2; exit 1
}
clang_libraries=("$esp_root"/xtensa-esp32-elf-clang/*/esp-clang/lib)

# GCC compiles LVGL, the linker. Call8 cannot reach the ROM's memcpy from flash,
# so every C call is a long call, as in ESP-IDF.
export PATH="$gcc_bin:$PATH"
export CC_xtensa_esp32s3_none_elf=xtensa-esp32s3-elf-gcc
export CFLAGS_xtensa_esp32s3_none_elf=-mlongcalls
# Bindgen reads LVGL's headers with Espressif's libclang; -ffreestanding stops it
# from loading the host's C library headers (as in Core's Xtensa CI job).
export LIBCLANG_PATH="${clang_libraries[0]}"
export BINDGEN_EXTRA_CLANG_ARGS_xtensa_esp32s3_none_elf=-ffreestanding

# rustup runs the esp toolchain directly: the mise Cargo wrapper is host-only.
rustup run esp cargo build --locked -p xila-esp32-example --profile release-embedded \
    --target "$target" --features qemu-display -Zbuild-std=core,alloc
"$root/tools/espflash-$espflash_version/espflash" save-image --skip-update-check \
    --chip esp32s3 --flash-size 4mb --flash-mode dio \
    --partition-table examples/esp32/partitions.csv --partition-table-offset 0x8000 \
    --merge "$CARGO_TARGET_DIR/$target/release-embedded/esp32_example" "$root/esp32s3-flash.bin"
echo "Flash image: $root/esp32s3-flash.bin"
