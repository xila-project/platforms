#!/usr/bin/env bash
# Size-optimize the seven Applications WASI guests in place, as Applications'
# own `build:applications` task does for its released bundles.
#
# usage: tools/optimize_wasm.sh <release-wasm-directory>
#   Needs `wasm-opt` from binaryen at `binaryen_version` (mise.toml), the same
#   release Applications pins.
set -euo pipefail

directory="${1:?usage: optimize_wasm.sh <release-wasm-directory>}"
for application in calculator clock weather pixel_painter text_editor synth digits; do
  module="$directory/$application.wasm"
  # The enabled proposals are exactly those the host accepts: keep them in
  # sync with xila-project/applications executables/wasm/src/host/engine.rs
  # and its `build:applications` task, so the optimizer can neither reject a
  # guest nor emit an instruction the engine refuses. Everything else stays off.
  wasm-opt -Oz \
    --strip-debug \
    --strip-dwarf \
    --strip-producers \
    --enable-mutable-globals \
    --enable-sign-ext \
    --enable-nontrapping-float-to-int \
    --enable-multivalue \
    --enable-bulk-memory \
    --enable-reference-types \
    --enable-tail-call \
    --enable-extended-const \
    --enable-custom-page-sizes \
    "$module" -o "$module.optimized"
  mv -- "$module.optimized" "$module"
done
