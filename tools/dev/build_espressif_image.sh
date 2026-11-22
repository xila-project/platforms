#!/usr/bin/env bash
# Builds the ESP32 example and merges the bootloader, partition table and
# application into one flash image: target/espressif-emulator/BOARD-flash.bin.
#
#   tools/dev/build_espressif_image.sh ESPFLASH_VERSION RUST_VERSION ESP_GCC_VERSION [esp32s3|esp32c6|esp32p4|wt32-sc01-plus]
#
# The chip names build the emulator or development board images; the other
# names are boards. Each build also copies the firmware to
# target/espressif-emulator/BOARD.elf, for `espflash flash`.
#
# S3 boards need the `esp` toolchain from `mise run setup:espressif-rust`.
# C6 and P4 need the stable RISC-V targets and Clang for LittleFS.
set -euo pipefail

espflash_version=$1
rust_version=$2
esp_gcc_version=$3
board=${4:-esp32s3}
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
    rustup run esp cargo "${build_args[@]}" --target "$target" --features "$1" -Zbuild-std=core,alloc
}

# rustup runs the esp toolchain directly: the mise Cargo wrapper is host-only.
build_args=(build --locked -p xila-esp32-example --profile release-embedded)
case "$board" in
    esp32s3)
        chip=esp32s3
        target=xtensa-esp32s3-none-elf
        flash_mode=dio
        flash_size=4mb
        build_esp32s3 qemu-display
        ;;
    wt32-sc01-plus)
        # Wireless-Tag WT32-SC01 Plus: ESP32-S3 with 16 MB of flash.
        chip=esp32s3
        target=xtensa-esp32s3-none-elf
        flash_mode=dio
        flash_size=16mb
        build_esp32s3 wt32-sc01-plus
        ;;
    esp32c6)
        chip=esp32c6
        target=riscv32imac-unknown-none-elf
        # platforms#130: QEMU rounds down the ROM's DIO address and dummy
        # phases separately, shifting flash reads. Use espflash's default
        # again once the pinned QEMU release contains the fix.
        flash_mode=qio
        flash_size=4mb
        CC_riscv32imac_unknown_none_elf=clang \
            rustup run "$rust_version" cargo "${build_args[@]}" --target "$target" --features qemu-esp32c6
        ;;
    esp32p4)
        chip=esp32p4
        target=riscv32imafc-unknown-none-elf
        flash_mode=dio
        # This is the flash capacity of the ESP32-P4X-Function-EV-Board.
        flash_size=16mb
        # LittleFS (littlefs2-sys) takes its compiler flags from Core
        # (core#801, core#808), which exports the single-float ABI of the target.
        CC_riscv32imafc_unknown_none_elf=clang \
            rustup run "$rust_version" cargo "${build_args[@]}" --target "$target" --features esp32p4
        ;;
    *) echo "Unsupported ESP32 chip or board: $board" >&2; exit 2 ;;
esac

# Every board of a chip builds to the same path: keep a copy per board.
cp "$CARGO_TARGET_DIR/$target/release-embedded/esp32_example" "$root/$board.elf"

# One partition table per flash size; the firmware reads the same file in build.rs.
partition_table="examples/esp32/partitions_$flash_size.csv"
test -f "$partition_table" || { echo "Missing partition table $partition_table." >&2; exit 1; }

report="$root/$board-save-image.log"
if ! "$root/tools/espflash-$espflash_version/espflash" save-image --skip-update-check \
    --chip "$chip" --flash-size "$flash_size" --flash-mode "$flash_mode" \
    --partition-table "$partition_table" --partition-table-offset 0x8000 \
    --merge "$root/$board.elf" "$root/$board-flash.bin" \
    > "$report" 2>&1; then
    cat "$report" >&2
    echo "Could not build the $board flash image with $partition_table." >&2
    exit 1
fi
cat "$report"

# Fail clearly when the firmware outgrows its partition, and warn before it does.
sizes=$(sed -n 's/^App\/part\. size: *\([0-9,]*\)\/\([0-9,]*\) bytes.*/\1 \2/p' "$report")
test -n "$sizes" || { echo "Could not read the firmware size from espflash." >&2; exit 1; }
read -r used capacity <<< "${sizes//,/}"
if (( used > capacity )); then
    echo "The $board firmware ($used bytes) does not fit in the factory partition of $partition_table ($capacity bytes)." >&2
    exit 1
fi
if (( used * 3 > capacity * 2 )); then
    echo "Warning: the $board firmware uses $used of $capacity bytes; $partition_table should keep at least 50% headroom." >&2
fi
echo "Firmware: $root/$board.elf"
echo "Flash image: $root/$board-flash.bin"
