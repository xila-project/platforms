#!/usr/bin/env bash
# Builds the ESP32 example and merges the bootloader, partition table and
# application into one flash image: target/espressif-emulator/CHIP-flash.bin.
#
#   tools/dev/build_espressif_image.sh ESPFLASH_VERSION RUST_VERSION ESP_GCC_VERSION [esp32s3|esp32c6|esp32p4]
#
# S3 needs the `esp` toolchain from `mise run setup:espressif-rust`.
# C6 and P4 need the stable RISC-V targets and Clang for LittleFS.
set -euo pipefail

espflash_version=$1
rust_version=$2
esp_gcc_version=$3
chip=${4:-esp32s3}
root="$PWD/target/espressif-emulator"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
mkdir -p "$root"

build_esp32s3() {
    local compiler esp_root gcc_bin
    compiler=$(rustup which --toolchain esp rustc)
    case "$("$compiler" --version)" in
        *"($rust_version.0)") ;;
        *) echo "Expected the Espressif Rust toolchain $rust_version.0; run mise run setup:espressif-rust." >&2; exit 1 ;;
    esac
    esp_root=${compiler%/bin/rustc}
    gcc_bin="$esp_root/xtensa-esp-elf/esp-$esp_gcc_version/xtensa-esp-elf/bin"
    test -x "$gcc_bin/xtensa-esp32s3-elf-gcc" || {
        echo "Expected Xtensa GCC $esp_gcc_version in $gcc_bin." >&2; exit 1
    }
    local clang_libraries=("$esp_root"/xtensa-esp32-elf-clang/*/esp-clang/lib)

    # Call8 cannot reach the ROM's memcpy from flash, so use long C calls.
    export PATH="$gcc_bin:$PATH"
    export CC_xtensa_esp32s3_none_elf=xtensa-esp32s3-elf-gcc
    export CFLAGS_xtensa_esp32s3_none_elf=-mlongcalls
    # Use Espressif's libclang and keep bindgen off the host's C library headers.
    export LIBCLANG_PATH="${clang_libraries[0]}"
    export BINDGEN_EXTRA_CLANG_ARGS_xtensa_esp32s3_none_elf=-ffreestanding
    rustup run esp cargo "${build_args[@]}" --target "$target" --features qemu-display -Zbuild-std=core,alloc
}

# rustup runs the esp toolchain directly: the mise Cargo wrapper is host-only.
build_args=(build --locked -p xila-esp32-example --profile release-embedded)
case "$chip" in
    esp32s3)
        target=xtensa-esp32s3-none-elf
        flash_mode=dio
        flash_size=4mb
        build_esp32s3
        ;;
    esp32c6)
        target=riscv32imac-unknown-none-elf
        # platforms#130: QEMU rounds down the ROM's DIO address and dummy
        # phases separately, shifting flash reads. Use espflash's default
        # again once the pinned QEMU release contains the fix.
        flash_mode=qio
        flash_size=4mb
        CC_riscv32imac_unknown_none_elf=clang \
            rustup run "$rust_version" cargo "${build_args[@]}" --target "$target" --features esp32c6
        ;;
    esp32p4)
        target=riscv32imafc-unknown-none-elf
        flash_mode=dio
        # This is the flash capacity of the ESP32-P4-Function-EV-Board.
        flash_size=16mb
        # LVGL needs explicit compiler flags until lvgl-rust-sys derives them.
        CC_riscv32imafc_unknown_none_elf=clang \
        CFLAGS_riscv32imafc_unknown_none_elf="-march=rv32imafc -mabi=ilp32f" \
            rustup run "$rust_version" cargo "${build_args[@]}" --target "$target" --features esp32p4
        ;;
    *) echo "Unsupported ESP32 chip: $chip" >&2; exit 2 ;;
esac

"$root/tools/espflash-$espflash_version/espflash" save-image --skip-update-check \
    --chip "$chip" --flash-size "$flash_size" --flash-mode "$flash_mode" \
    --partition-table examples/esp32/partitions.csv --partition-table-offset 0x8000 \
    --merge "$CARGO_TARGET_DIR/$target/release-embedded/esp32_example" "$root/$chip-flash.bin"
echo "Flash image: $root/$chip-flash.bin"
