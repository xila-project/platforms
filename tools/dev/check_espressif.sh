#!/usr/bin/env bash
# Checks and lints `xila-drivers-espressif` for every supported ESP32 chip.
#
#   tools/dev/check_espressif.sh            # every chip
#   tools/dev/check_espressif.sh esp32s3 …  # selected chips
#
# Every chip is checked with the `esp` rustup toolchain (`espup install`). It is
# based on Rust 1.97, the version Xila declares as its MSRV, so the matrix does
# not compile with a newer compiler. Xtensa chips take rustc/clippy-driver from
# ESP_TOOLCHAIN and Cargo from RISCV_TOOLCHAIN; both default to `esp`.
# Environment: ESP_TOOLCHAIN (default `esp`), RISCV_TOOLCHAIN (default
# ESP_TOOLCHAIN), CARGO_CONFIG (an extra `--config` file, e.g. local overrides).
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ESP_TOOLCHAIN="${ESP_TOOLCHAIN:-esp}"
RISCV_TOOLCHAIN="${RISCV_TOOLCHAIN:-$ESP_TOOLCHAIN}"

CONFIG_ARGS=()
if [[ -n "${CARGO_CONFIG:-}" ]]; then
  CONFIG_ARGS=(--config "$CARGO_CONFIG")
fi

# chip:target:architecture:features. ESP32-C2, ESP32-C3 and ESP32-S2 lack native
# atomic compare-and-swap; esp-hal's chip features enable
# `portable-atomic/unsafe-assume-single-core` for them so Core and `gpio` build
# on every supported chip.
CHIPS=(
  "esp32:xtensa-esp32-none-elf:xtensa:gpio,i2c,spi,uart"
  "esp32s2:xtensa-esp32s2-none-elf:xtensa:gpio,i2c,spi,uart"
  "esp32s3:xtensa-esp32s3-none-elf:xtensa:gpio,i2c,spi,uart"
  "esp32c2:riscv32imc-unknown-none-elf:riscv:gpio,i2c,spi,uart"
  "esp32c3:riscv32imc-unknown-none-elf:riscv:gpio,i2c,spi,uart"
  "esp32c5:riscv32imac-unknown-none-elf:riscv:gpio,i2c,spi,uart"
  "esp32c6:riscv32imac-unknown-none-elf:riscv:gpio,i2c,spi,uart"
  "esp32c61:riscv32imac-unknown-none-elf:riscv:gpio,i2c,spi,uart"
  "esp32h2:riscv32imac-unknown-none-elf:riscv:gpio,i2c,spi,uart"
  "esp32p4:riscv32imafc-unknown-none-elf:riscv:gpio,i2c,spi,uart"
  "esp32s31:riscv32imafc-unknown-none-elf:riscv:gpio,i2c,spi,uart"
)

known_chip() {
  local entry
  for entry in "${CHIPS[@]}"; do
    [[ "${entry%%:*}" == "$1" ]] && return 0
  done
  return 1
}

for chip in "$@"; do
  if ! known_chip "$chip"; then
    echo "unknown chip: $chip (expected one of: $(printf '%s ' "${CHIPS[@]%%:*}"))" >&2
    exit 2
  fi
done

selected() {
  [[ "$#" -eq 1 ]] && return 0
  local wanted
  for wanted in "${@:2}"; do
    [[ "$wanted" == "$1" ]] && return 0
  done
  return 1
}

ESP_RUSTC="$(rustup which --toolchain "$ESP_TOOLCHAIN" rustc 2>/dev/null || true)"
ESP_CLIPPY_DRIVER="$(rustup which --toolchain "$ESP_TOOLCHAIN" clippy-driver 2>/dev/null || true)"
INSTALLED_TARGETS="$(rustup target list --installed --toolchain "$RISCV_TOOLCHAIN" 2>/dev/null || true)"

# cargo <subcommand> for one chip, with the right compiler and standard library.
cargo_for() {
  local architecture="$1" target="$2" subcommand="$3"
  shift 3
  local common=(
    --manifest-path "$ROOT_DIR/Cargo.toml"
    "${CONFIG_ARGS[@]}"
    -p xila-drivers-espressif
    --target "$target"
  )
  if [[ "$architecture" == "xtensa" ]]; then
    if [[ "$subcommand" == "clippy" ]]; then
      RUSTC="$ESP_RUSTC" RUSTC_WORKSPACE_WRAPPER="$ESP_CLIPPY_DRIVER" \
        cargo "+$RISCV_TOOLCHAIN" rustc "${common[@]}" -Zbuild-std=core,alloc "$@" \
        -- -D warnings
    else
      RUSTC="$ESP_RUSTC" \
        cargo "+$RISCV_TOOLCHAIN" check "${common[@]}" -Zbuild-std=core,alloc "$@"
    fi
  else
    local build_std=()
    if ! grep -qx "$target" <<<"$INSTALLED_TARGETS"; then
      build_std=("-Zbuild-std=core,alloc")
    fi
    if [[ "$subcommand" == "clippy" ]]; then
      env -u RUSTC -u RUSTC_WORKSPACE_WRAPPER \
        cargo "+$RISCV_TOOLCHAIN" clippy "${common[@]}" "${build_std[@]}" "$@" -- -D warnings
    else
      env -u RUSTC -u RUSTC_WORKSPACE_WRAPPER \
        cargo "+$RISCV_TOOLCHAIN" check "${common[@]}" "${build_std[@]}" "$@"
    fi
  fi
}

RESULTS=()
FAILURES=0

record() {
  RESULTS+=("$1|$2|$3|$4")
  if [[ "$3" != "PASS" || "$4" != "PASS" ]]; then
    FAILURES=$((FAILURES + 1))
  fi
}

for entry in "${CHIPS[@]}"; do
  IFS=':' read -r chip target architecture features <<<"$entry"
  selected "$chip" "$@" || continue

  if [[ "$architecture" == "xtensa" && (-z "$ESP_RUSTC" || -z "$ESP_CLIPPY_DRIVER") ]]; then
    echo "=== $chip: the '$ESP_TOOLCHAIN' toolchain is missing (install it with espup)" >&2
    record "$chip" "$features" "MISSING" "MISSING"
    continue
  fi

  echo "=== $chip ($target, features: $features)"
  arguments=(--no-default-features --features "$chip,$features")
  check="FAIL"
  clippy="FAIL"
  cargo_for "$architecture" "$target" check "${arguments[@]}" && check="PASS"
  cargo_for "$architecture" "$target" clippy "${arguments[@]}" && clippy="PASS"
  record "$chip" "$features" "$check" "$clippy"
done

echo
printf "%-10s %-22s %-8s %-8s\n" "Chip" "Features" "Check" "Clippy"
for row in "${RESULTS[@]}"; do
  IFS='|' read -r chip features check clippy <<<"$row"
  printf "%-10s %-22s %-8s %-8s\n" "$chip" "$features" "$check" "$clippy"
done

if [[ "$FAILURES" -gt 0 ]]; then
  echo
  echo "$FAILURES chip(s) failed." >&2
  exit 1
fi
