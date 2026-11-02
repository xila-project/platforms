#!/usr/bin/env bash
# Package locally built Wasm guests as unsigned bundles, using each
# application's own packaging/<app>.json and shortcut.json from an Applications
# source checkout. Used for locale-specific images (translations are compiled
# into the Wasm), where released bundles only provide the default locale.
#
# usage: bundle_applications.sh <applications-source> <wasm-dir> <output-dir>
# Set XILA_PACKAGE to an existing xila-package binary to skip installing one.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <applications-source> <wasm-dir> <output-dir>" >&2
  exit 2
fi
source_root="$1"
wasm_dir="$2"
output="$3"
platforms="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

builder="${XILA_PACKAGE:-}"
if [[ -z "$builder" ]]; then
  # Same package_manager revision as the verification crates Platforms pins.
  revision="$(sed -n 's/^xila-package-reader = .*rev = "\([0-9a-f]\{40\}\)".*/\1/p' "$platforms/tools/artifacts/Cargo.toml")"
  if [[ ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
    echo "cannot determine the pinned package_manager revision" >&2
    exit 1
  fi
  root="$platforms/target/tools/xila-package-$revision"
  if [[ ! -x "$root/bin/xila-package" ]]; then
    # Own target directory: a shared CARGO_TARGET_DIR/build.target-dir can hold
    # stale artifacts from other package_manager revisions.
    CARGO_TARGET_DIR="$root/target" cargo install --locked --quiet --root "$root" \
      --git https://github.com/xila-project/package_manager --rev "$revision" xila-package-builder
  fi
  builder="$root/bin/xila-package"
fi

# Refuse to mix new bundles into an existing directory.
mkdir -p "$(dirname "$output")"
mkdir "$output"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

sort -u "$platforms/tools/fs_image/profiles/native.txt" "$platforms/tools/fs_image/profiles/browser.txt" |
  while read -r app; do
    [[ -n "$app" ]] || continue
    source_directory="$stage/$app"
    mkdir -p "$source_directory/executables"
    cp "$source_root/packaging/$app.json" "$source_directory/xila-package.json"
    cp "$source_root/executables/$app/shortcut.json" "$source_directory/shortcut.json"
    cp "$wasm_dir/$app.wasm" "$source_directory/executables/$app.wasm"
    "$builder" bundle build "$source_directory" "$output/xila.${app//_/-}"
  done
