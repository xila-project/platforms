#!/usr/bin/env bash
# Installs the pinned Espressif QEMU and espflash under
# target/espressif-emulator/tools. Every download is verified against the
# SHA-256 digest recorded here; the versions come from `[vars]` in mise.toml.
#
#   tools/dev/install_espressif_emulator.sh QEMU_VERSION ESPFLASH_VERSION [xtensa|riscv32]
#   tools/dev/install_espressif_emulator.sh --espflash-only ESPFLASH_VERSION
set -euo pipefail

espflash_version=$2
root="$PWD/target/espressif-emulator/tools"
mkdir -p "$root"

case "$(uname -s)/$(uname -m)" in
    Linux/x86_64)
        host=x86_64
        xtensa_sha=fbc3bffa9fa71192847969a627a584aabe2e86d85bed4902a096ff06c2b8a815
        riscv32_sha=afa59465495295b8aa4f804768983fa30b0bbf33d0f68a047f9caa436b9164c0
        espflash_sha=a9bf20c76ab42d0d4330bbc33707c4424d5541f5129149d889dbfa8dab476910
        ;;
    Linux/aarch64)
        host=aarch64
        xtensa_sha=dad4a8f44b5649b1f110963d479a77edb0ad2484cde79403e3a19c50efdd5281
        riscv32_sha=4038e03d6e312b2372106516855a2c72eda02b49f6480917c55e56e04d958781
        espflash_sha=a2081c002f6050a287007a93ba01cdd4c9d111181b69d5ddf7bdb71d33841c65
        ;;
    *) echo "The Espressif tools require Linux x86_64 or aarch64." >&2; exit 2 ;;
esac

download() {
    local url=$1 file=$2 sha=$3
    curl --fail --location --retry 3 --silent --show-error --output "$file" "$url"
    printf '%s  %s\n' "$sha" "$file" | sha256sum --check --quiet
}

if [[ $1 != --espflash-only ]]; then
    qemu_version=$1
    architecture=${3:-xtensa}
    case "$architecture" in
        xtensa) qemu="$root/qemu-$qemu_version"; qemu_sha=$xtensa_sha ;;
        riscv32) qemu="$root/qemu-riscv32-$qemu_version"; qemu_sha=$riscv32_sha ;;
        *) echo "Unsupported QEMU architecture: $architecture" >&2; exit 2 ;;
    esac

    # `esp_develop_9.2.2_20261109` is released as tag `esp-develop-9.2.2-20261109`.
    qemu_tag="esp-develop-${qemu_version%_*}-${qemu_version##*_}"
    qemu_bin="$qemu/qemu/bin/qemu-system-$architecture"
    if [[ ! -x $qemu_bin ]]; then
        archive="qemu-$architecture-softmmu-esp_develop_$qemu_version-$host-linux-gnu.tar.xz"
        stage=$(mktemp -d "$root/qemu-download.XXXXXXXX")
        download "https://github.com/espressif/qemu/releases/download/$qemu_tag/$archive" \
            "$stage/$archive" "$qemu_sha"
        tar -xJf "$stage/$archive" -C "$stage"
        mv "$stage" "$qemu"
    fi
    "$qemu_bin" --version | head -n 1
fi

espflash="$root/espflash-$espflash_version"
if [[ ! -x $espflash/espflash ]]; then
    archive="espflash-$host-unknown-linux-musl.zip"
    stage=$(mktemp -d "$root/espflash-download.XXXXXXXX")
    download "https://github.com/esp-rs/espflash/releases/download/v$espflash_version/$archive" \
        "$stage/$archive" "$espflash_sha"
    unzip -q "$stage/$archive" -d "$stage"
    mv "$stage" "$espflash"
fi
"$espflash/espflash" --version
