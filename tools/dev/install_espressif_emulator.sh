#!/usr/bin/env bash
# Installs the pinned Espressif QEMU (Xtensa) and espflash under
# target/espressif-emulator/tools. Every download is verified against the
# SHA-256 digest recorded here; the versions come from `[vars]` in mise.toml.
#
#   tools/dev/install_espressif_emulator.sh QEMU_VERSION ESPFLASH_VERSION
set -euo pipefail

qemu_version=$1
espflash_version=$2
root="$PWD/target/espressif-emulator/tools"
mkdir -p "$root"

case "$(uname -s)/$(uname -m)" in
    Linux/x86_64)
        host=x86_64
        qemu_sha=fbc3bffa9fa71192847969a627a584aabe2e86d85bed4902a096ff06c2b8a815
        espflash_sha=a9bf20c76ab42d0d4330bbc33707c4424d5541f5129149d889dbfa8dab476910
        ;;
    Linux/aarch64)
        host=aarch64
        qemu_sha=dad4a8f44b5649b1f110963d479a77edb0ad2484cde79403e3a19c50efdd5281
        espflash_sha=a2081c002f6050a287007a93ba01cdd4c9d111181b69d5ddf7bdb71d33841c65
        ;;
    *) echo "The Espressif emulator tasks require Linux x86_64 or aarch64." >&2; exit 2 ;;
esac

download() {
    local url=$1 file=$2 sha=$3
    curl --fail --location --retry 3 --silent --show-error --output "$file" "$url"
    printf '%s  %s\n' "$sha" "$file" | sha256sum --check --quiet
}

# `esp_develop_9.2.2_20261109` is released as tag `esp-develop-9.2.2-20261109`.
qemu_tag="esp-develop-${qemu_version%_*}-${qemu_version##*_}"
qemu="$root/qemu-$qemu_version"
if [[ ! -x $qemu/qemu/bin/qemu-system-xtensa ]]; then
    archive="qemu-xtensa-softmmu-esp_develop_$qemu_version-$host-linux-gnu.tar.xz"
    stage=$(mktemp -d "$root/qemu-download.XXXXXXXX")
    download "https://github.com/espressif/qemu/releases/download/$qemu_tag/$archive" \
        "$stage/$archive" "$qemu_sha"
    tar -xJf "$stage/$archive" -C "$stage"
    mv "$stage" "$qemu"
fi
"$qemu/qemu/bin/qemu-system-xtensa" --version | head -n 1

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
