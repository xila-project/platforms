#!/usr/bin/env bash
# Builds an Espressif board crate of examples/boards and merges the bootloader,
# the board's partition table and the firmware into one flash image:
# target/espressif-emulator/BOARD-flash.bin, next to a copy of the firmware,
# target/espressif-emulator/BOARD.elf, for `espflash flash`.
#
#   tools/dev/build_espressif_image.sh ESPFLASH_VERSION RUST_VERSION ESP_GCC_VERSION BOARD [elf] [CARGO_ARGUMENTS...]
#
# BOARD is a folder of examples/boards: esp32-s3-qemu, esp32-c6-qemu,
# esp32-p4-function-ev-board, wt32-sc01 or wt32-sc01-plus.
#
# Xtensa boards need the `esp` toolchain from `mise run setup:espressif-rust`.
# RISC-V boards need the stable RISC-V targets and Clang for LittleFS.
set -euo pipefail

if (( $# < 4 )); then
    echo "Usage: $0 ESPFLASH_VERSION RUST_VERSION ESP_GCC_VERSION BOARD [elf|image] [CARGO_ARGUMENTS...]" >&2
    exit 2
fi

espflash_version=$1
rust_version=$2
esp_gcc_version=$3
board=$4
# An optional fifth argument `elf` stops after the firmware ELF, without the
# flash image; every argument after it (or after BOARD) goes to Cargo.
output=image
if [[ ${5:-} == elf || ${5:-} == image ]]; then
    output=$5
    shift 5
else
    shift 4
fi
directory="examples/boards/$board"
test -f "$directory/Cargo.toml" || { echo "Unknown Espressif board: $board" >&2; exit 2; }
package="xila-board-$board"
binary=${board//-/_}
partition_table="$directory/partitions.csv"
root="$PWD/target/espressif-emulator"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"

build_xtensa() {
    local compiler esp_root gcc_bin target_environment
    compiler=$(rustup which --toolchain esp rustc)
    case "$("$compiler" --version)" in
        *"($rust_version.0)") ;;
        *) echo "Expected the Espressif Rust toolchain $rust_version.0; run mise run setup:espressif-rust." >&2; exit 1 ;;
    esac
    esp_root=${compiler%/bin/rustc}
    gcc_bin="$esp_root/xtensa-esp-elf/esp-$esp_gcc_version/xtensa-esp-elf/bin"
    test -x "$gcc_bin/xtensa-$chip-elf-gcc" || {
        echo "Expected Xtensa GCC $esp_gcc_version in $gcc_bin." >&2; exit 1
    }
    local clang_libraries=("$esp_root"/xtensa-esp32-elf-clang/*/esp-clang/lib)

    # Call8 cannot reach the ROM's memcpy from flash, so use long C calls.
    export PATH="$gcc_bin:$PATH"
    target_environment=${target//-/_}
    # Use Espressif's libclang and keep bindgen off the host's C library headers.
    export LIBCLANG_PATH="${clang_libraries[0]}"
    export "CC_$target_environment=xtensa-$chip-elf-gcc"
    export "CFLAGS_$target_environment=-mlongcalls${c_flags:+ $c_flags}"
    export "BINDGEN_EXTRA_CLANG_ARGS_$target_environment=-ffreestanding"
    # bindgen passes no `--target` for host builds, such as the LVGL bindings
    # that the WebAssembly runtime's build script reads, and Espressif's
    # libclang defaults to a 32-bit target: name the host explicitly.
    local host
    host=$(rustup run esp rustc -vV | sed -n 's/^host: //p')
    export "BINDGEN_EXTRA_CLANG_ARGS_${host//-/_}=--target=$host"
    rustup run esp cargo "${build_args[@]}" --target "$target" -Zbuild-std=core,alloc
    objdump="$gcc_bin/xtensa-$chip-elf-objdump"
}

# The ESP32 compare-and-set instruction (S32C1I) is not supported on PSRAM.
# The firmware may use it in one function only, which the atomic operations
# of xila-drivers-espressif call for internal memory. A use is the instruction
# within a few instructions of the write to SCOMPARE1 (`wsr` or `xsr`) that it
# needs: the disassembler also decodes the constants between functions, and
# some of them look like the instruction alone. A precompiled library with its
# own sequences needs more than an exception here: without the `s32c1i`
# feature, the exception handlers no longer save SCOMPARE1 (platforms#208).
check_esp32_atomics() {
    local elf=$1 allowed=xila_esp32_hardware_compare_and_set offenders
    offenders=$("$objdump" -d --no-show-raw-insn "$elf" | awk -v allowed="$allowed" '
        /^[0-9a-f]+ <.*>:$/ { name = substr($2, 2, length($2) - 3); armed = 0; next }
        $2 == "wsr.scompare1" || $2 == "xsr.scompare1" { armed = 8; next }
        $2 == "s32c1i" && armed > 0 { count[name]++ }
        armed > 0 { armed-- }
        END {
            if (!(allowed in count)) print "(no use in " allowed ")"
            for (name in count) if (name != allowed) print name
        }')
    if [[ -n "$offenders" ]]; then
        echo "S32C1I must only be used in $allowed, which keeps it off PSRAM; found:" >&2
        echo "$offenders" >&2
        exit 1
    fi
    echo "Atomics: S32C1I is only used in $allowed"
}

# Extra C compiler flags of an Xtensa board.
c_flags=
# rustup runs the esp toolchain directly: the mise Cargo wrapper is host-only.
build_args=(build --locked -p "$package" --profile release-embedded "$@")
# The flash size must match the board's partition table. Boards that do not
# set a flash frequency use espflash's default for the chip.
flash_frequency_arguments=()
case "$board" in
    esp32-s3-qemu)
        chip=esp32s3
        target=xtensa-esp32s3-none-elf
        flash_mode=dio
        flash_size=4mb
        build_xtensa
        ;;
    wt32-sc01)
        chip=esp32
        target=xtensa-esp32-none-elf
        # WT32-SC01 modules ship with ESP32 revision v1.0 silicon, older than
        # esp-hal's default minimum (v3.0). The PSRAM cache workaround in
        # .cargo/config.toml covers the revision v1 erratum.
        export ESP_HAL_CONFIG_MIN_CHIP_REVISION=100
        # The same workaround for the C code (LittleFS, LVGL): it may be given
        # PSRAM memory, as the heap uses PSRAM for large allocations. Its
        # atomic operations, if it ever has any, call the functions of
        # xila-drivers-espressif, as Rust's do, instead of using S32C1I.
        c_flags="-mfix-esp32-psram-cache-issue -mfix-esp32-psram-cache-strategy=memw -mdisable-hardware-atomics"
        flash_mode=dio
        flash_size=4mb
        build_xtensa
        check_esp32_atomics "$CARGO_TARGET_DIR/$target/release-embedded/$binary"
        ;;
    wt32-sc01-plus)
        chip=esp32s3
        target=xtensa-esp32s3-none-elf
        # The module's flash reads in quad I/O at 80 MHz, twice as fast as
        # dual I/O for code and read-only data that miss the cache.
        flash_mode=qio
        flash_frequency_arguments=(--flash-freq 80mhz)
        flash_size=16mb
        build_xtensa
        ;;
    esp32-c6-qemu)
        chip=esp32c6
        target=riscv32imac-unknown-none-elf
        # platforms#130: QEMU rounds down the ROM's DIO address and dummy
        # phases separately, shifting flash reads. Use espflash's default
        # again once the pinned QEMU release contains the fix.
        flash_mode=qio
        flash_size=4mb
        CC_riscv32imac_unknown_none_elf=clang \
            rustup run "$rust_version" cargo "${build_args[@]}" --target "$target"
        ;;
    esp32-p4-function-ev-board)
        chip=esp32p4
        target=riscv32imafc-unknown-none-elf
        flash_mode=dio
        flash_size=16mb
        # LittleFS (littlefs2-sys) takes its compiler flags from Core
        # (core#801, core#808), which exports the single-float ABI of the target.
        CC_riscv32imafc_unknown_none_elf=clang \
            rustup run "$rust_version" cargo "${build_args[@]}" --target "$target"
        ;;
    *) echo "No image settings for the Espressif board $board." >&2; exit 2 ;;
esac

mkdir -p "$root"
cp "$CARGO_TARGET_DIR/$target/release-embedded/$binary" "$root/$board.elf"

if [[ "$output" == elf ]]; then
    echo "Firmware: $root/$board.elf"
    exit 0
fi

report="$root/$board-save-image.log"
if ! "$root/tools/espflash-$espflash_version/espflash" save-image --skip-update-check \
    --chip "$chip" --flash-size "$flash_size" --flash-mode "$flash_mode" \
    "${flash_frequency_arguments[@]}" \
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
