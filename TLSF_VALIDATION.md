# TLSF migration validation

Base: `2b32e7ec621e4563cbca47ea1ca88ab2d0e7cf2a`.
Depends on Core's `perf/tlsf-allocator` branch. No Core git revision or lockfile
update is committed here: update those after Core merges.

All existing allocator implementations were migrated:

| Backend | Change | Validation |
|---|---|---|
| std (native) | Two capability-selected mmap regions use Core RegionHeap, FL=usize::BITS, SL=8; contiguous growth uses mremap; realloc attempts in place before same-capability fallback | 23 memory tests pass; native example build and bounded terminal startup; Clippy passes |
| wasm (browser) | Existing memory_grow region uses RegionHeap, FL=32, SL=8; grow/shrink realloc and size-class-aware growth | `cargo build -p xila-drivers-wasm --target wasm32-unknown-unknown` passes |
| Nordic (nRF52840) | Existing caller-supplied fixed region uses RegionHeap, FL=16, SL=8; realloc attempts in place | `cargo build -p xila-drivers-nordic --target thumbv7em-none-eabihf` passes (default nrf52840/usb-console features) |

Repository search found no other live system heap implementation. Existing
capability acceptance/rejection, region startup API, critical-section/RefCell
locks, page sizes, cache/protection defaults and global allocator export macros
are retained. Native profiling/heaptrack builds keep allocation/free notifications
by using moving realloc; their combined feature check passes. Uninstrumented
realloc keeps the originating region's capability requirements when it moves.

Statistics now include occupied TLSF headers and padding, and exclude sentinels;
used + free remains usable total. Queries walk blocks under the existing lock.
The previous native test expecting requested bytes exactly was updated to check
physical occupancy and the total invariant. Core documents allocator overhead,
fragmentation regressions and measured latency improvements; no fixed subsystem
pools are introduced.

## Reproduction

Generate an untracked Cargo configuration with `tools/dev/local_overrides.py`
using `--core`, `--platforms`, `--applications`, `--package-manager`, `--output`.
Pass it to build/test with `cargo --config /absolute/path/local.toml ...`; Clippy
requires `cargo clippy --config /absolute/path/local.toml ...`.
Set `CARGO_TARGET_DIR` beneath the enclosing workspace's `target/work/`.
Run `npm ci` in Core once and set `XILA_LV_FONT_CONV_DIR` to Core's
`node_modules/lv_font_conv` when building Platforms graphics.

Commands (all against local Core):

```sh
cargo --config "$override" test -p xila-drivers-std memory::tests
cargo clippy --config "$override" -p xila-drivers-std --lib -- -D warnings
cargo --config "$override" check -p xila-drivers-std --features profiling,heaptrack
cargo --config "$override" build -p xila-drivers-nordic --target thumbv7em-none-eabihf
cargo --config "$override" build -p xila-drivers-wasm --target wasm32-unknown-unknown
cargo --config "$override" build -p xila-native-example --no-default-features
cargo --config "$override" run -p xila-fs-image -- "$scratch/seed.img"
timeout 10 "$CARGO_TARGET_DIR/debug/native_example" --terminal --no-network \
  --disk-path "$scratch/native-disk.img" --seed-image "$scratch/seed.img"
```

The workspace-pinned Applications source uses localization APIs removed from
Core main; the first native build failed there. For the successful build, a
read-only archive of Applications main `8215a1a` was used instead. It also needs
current Package Manager APIs, so a read-only archive of Package Manager main
`528be81` was used. Neither checkout was changed. The local override and resulting
Cargo.lock were not committed.

The native terminal run initialized the VFS and default hierarchy, reached the
username prompt, and was stopped by the 10-second timeout (exit 124). It used an
empty generated seed: this is startup coverage, not application/UI workload
completion. Wasm and Nordic were compiled, not run in a browser or on hardware.
Build directories were removed after validation.
