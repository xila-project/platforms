# Browser example

The published example is at [platforms.xila.dev](https://platforms.xila.dev/). Localized versions are available under `demonstrations/wasm/<locale>/`.

From the repository root, prepare the separate application disk and build the
browser runtime:

```sh
mise run image:browser
mkdir -p target/seed/wasm
cp target/seed/browser/seed.img target/seed/wasm/seed.img
mise run check:browser
cd examples/wasm
RUSTUP_TOOLCHAIN=stable trunk serve
```

Trunk copies `target/seed/wasm/seed.img` beside the runtime. Building the Rust
runtime alone requires no prebuilt guest binaries; serving a fresh installation
requires this image. Build guests and images with the same
`EMBEDDED_I18N_LOCALE` as the runtime (`en`, `fr`, `es`, `de`, `pt-BR`, `it`, `pl`, `ja` or `zh-CN`).

Serve the example over localhost or HTTPS. Saved disks require support for
the origin-private file system, `FileSystemFileHandle.createWritable`, and
Web Locks.

## Running guest WebAssembly applications

Calculator, Clock, Pixel Painter, Text Editor and Synth are provisioned in
`seed.img` at build time, along with their shortcuts. Memory starts with that
image on every page load. A new saved disk starts with the same image and
commits it after mounting LittleFS and initializing accounts.

Clock's settings directory is provisioned per user at
`/users/<user-id>/applications/clock/configuration`. Its saved preferences and
alarm follow the persistence limits of the selected disk. Clock alerts require the application to stay
open; browser background-tab throttling can delay them.

On a fresh disk, log in as **administrator** with an empty password; otherwise
use your saved credentials. Open the application drawer using the Xila button
in the dock. Select
**Calculator**, **Clock**, **Pixel Painter**, **Text Editor** or **Synth**. Text Editor uses a temporary on-disk working copy and edits UTF-8-safe 4 KiB pages. They can also be launched from Xila's terminal:

```text
/applications/executables/wasm /applications/executables/calculator
/applications/executables/wasm /applications/executables/clock
/applications/executables/wasm /applications/executables/pixel_painter
/applications/executables/wasm /applications/executables/text_editor
/applications/executables/wasm /applications/executables/synth
```

The example mounts the Wasmi-backed runtime at `/applications/executables/wasm`, with the same
graphics bindings as the native example. For other guests that are already
present in Xila's virtual filesystem:

```text
/applications/executables/wasm /path/to/guest.wasm
```

WASI Preview 1 command guests should export `_start: () -> ()`. Use
`/applications/executables/wasm --install /path/to/guest.wasm` to call `__install: () -> ()`
instead. Graphical guests must use compatible Xila graphics bindings; see the
[runtime documentation](../../executables/wasm/README.md) for supported APIs and
limits.

## Localized and CJK builds

Build the Applications guests (`XILA_FONT_CATALOG_ROOTS=/path/to/applications`)
and browser runtime with the same `EMBEDDED_I18N_LOCALE`:

- **Simplified Chinese (`zh-CN`):** Core vendors `SourceHanSansSC-Regular.otf`
  and subsets only the characters used in translation catalogs and localized
  date/time formats.
- **Japanese (`ja`):** Core reads `SourceHanSansJP-Regular.otf` from
  `XILA_JAPANESE_FONT_PATH`. From the repository root, run `mise -E ci run ci:font-japanese`
  to check out the locked Core revision into `.ci-core-fonts` and download and
  verify the font, then export:

  ```sh
  export XILA_JAPANESE_FONT_PATH="$PWD/.ci-core-fonts/subsystems/graphics/fonts/SourceHanSansJP-Regular.otf"
  ```

For full multi-locale Pages builds (`mise -E ci run pages:build`), ensure
`.application-src` holds the locked `xila-project/applications` checkout (or
restrict `XILA_DEMO_LOCALES`). When `ja` is included, `pages:build` runs
`ci:font-japanese` to check the Core revision and verify the font. If
`XILA_JAPANESE_FONT_PATH` is unset, it defaults to the verified font.

## Visual identity

The browser help panel uses local Montserrat normal and italic variable web fonts, following the shared 400/600/700 text, heading and emphasis hierarchy. Trunk copies `fonts/` alongside the example; font rendering needs no Google Fonts connection. Keep `fonts/OFL.txt` with those files. The font and logo source revision is recorded in `fonts/README.md`.

Trunk also copies `licenses/THIRD_PARTY_NOTICES.txt` (linked from the browser help panel) alongside each demonstration. That file carries the copyright notices and license texts for Montserrat (`OFL-1.1`), Source Han Sans JP and SC (`OFL-1.1`, embedded in `ja` and `zh-CN` builds), Font Awesome Free (`OFL-1.1` for the embedded icon font; `CC BY 4.0` for the help-panel chevron SVGs), and base Unscii (Public Domain / CC0) and its public domain source font Fixedsys Excelsior.

The OS canvas still uses the existing localized LVGL Montserrat Regular subsets at 10/14/18/28px, and Unscii for terminal content. Web font files are not compiled into the OS font tables or loaded into MCU flash. Bold wordmarks are outlined SVG assets.

## Browser limitations

- Choose a saved disk to keep files and settings across reloads. Other guest
  files must already be on the selected disk before launching.
- Click **Enable audio** in the browser page before launching Synth. The click
  starts one `WebAudioDevice` and mounts it at `/devices/audio/playback`; Synth
  then writes ordinary PCM through its file descriptor. If Synth is already
  open, choose **Retry audio** inside its window. Browsers may require this
  gesture because audio contexts cannot start during page initialization.
- Applications are distributed in a separate seed image. The disk cache still
  occupies 16 MiB; running a guest also needs Wasmi's module and guest memory.
- Guests run cooperatively in Wasmi on the browser's executor, not as separate
  native browser WebAssembly instances. Fuel exhaustion and supported WASI
  suspension calls yield to the executor.
- There is no Xila network manager in this example. The browser HTTP device
  uses Fetch and remains subject to browser security restrictions, including
  CORS; it does not provide raw sockets.
- Runtime limitations still apply, including the supported graphics subset,
  clock-only WASI polling, unsupported suspension in WebAssembly start sections,
  and the currently unused stack-size option.

Per-call WASI diagnostics are logged at Trace level, which the browser logger
does not display. Informational messages, warnings, and errors remain visible.

## Browser verification

Verified in headless Chromium 154 with a Trunk development build using Rust
stable. Temporary startup probes loaded guest files into the example's VFS and
launched them through the mounted `/applications/executables/wasm` executable:

- A minimal guest executed a host call, exhausted and replenished its fuel,
  resumed after WASI `sched_yield`, and handled success and nonzero `proc_exit`.
- The real `calculator` guest, built for `wasm32-wasip1`, evaluated `1 + 2 = 3`
  through LVGL button events, used WASI clock polling, and exited successfully
  with its window removed after a close request.

The storage regression suite also exercises OPFS persistence, first-use seed
import through Fetch, zero-byte placeholders, failed imports, corrupt existing
disks, lease handling and reopening an existing disk without downloading a seed.
The image-builder tests remount populated disks and check reproducibility.

The probes are not included in normal example startup. Generated
guest binaries are not committed. This is focused browser validation, not a
claim of full WASI/graphics compatibility or support across all browsers.

## Disk modes

**Memory (default).** Leave **Keep my files between visits** off in the
Help panel. Each tab starts from the current `seed.img` and keeps changes
in its own memory. Reloading or closing the tab discards all changes.
Memory uses no storage file or Web Lock, and the saved-disk controls are hidden.

**Saved disks.** Turn on **Keep my files between visits** to restart with a
saved disk. Xila reopens the last saved disk it used successfully if that
disk still exists, or creates a fresh one from the current seed. The Help
panel then lets you select, create, or remove a saved disk. The example
stores each 16 MiB disk in the origin-private file system. New images are
named `xila_YYYY-MM-DD.img`, with a numeric suffix for repeated creations on
the same date. The panel lists `xila_drive.img` and other disks from earlier
versions. The switch starts off for visitors who previously used the browser
example before it offered this setting. Xila remembers explicit choices when
browser preferences are available.

Changing the switch or choosing another disk restarts Xila. Leaving a
running saved disk saves pending changes first. A Web Lock allows only one
tab to open a given saved disk at a time. Removing an inactive saved disk
requires confirmation and permanently deletes its contents.

Xila remembers the seed fingerprint of each newly saved disk. The panel
marks a disk whose fingerprint differs from the current seed as **older
seed version**. Disks created before fingerprinting show **seed version
unknown**. You can open either if it is still compatible with the runtime.
If a disk fails to mount, the Help panel displays the error and offers
**Rebuild from the current version** when the seed is available. Rebuilding
requires confirmation and replaces the disk's files and settings once the
current seed mounts and saves successfully. You can turn off **Keep my files
between visits** to restart in Memory, or create another saved disk while
retaining the older disk.

Synchronous OPFS handles are available only in workers, while this example's
UI and filesystem run on the browser main thread. The drive therefore keeps a
16 MiB memory cache and asynchronously commits an atomic disk snapshot at boot,
every second when dirty, and on normal shutdown. Saving temporarily needs
another 16 MiB snapshot, plus browser-internal buffering. There is no growing
queue of disk snapshots. Clean disks are not rewritten.

**Durability:** synchronous filesystem flushes update the cache, not OPFS.
`DriveDevice::sync().await` waits for an OPFS commit and reports storage errors;
autosave errors are logged and retried. Abrupt reloads, tab closure, or crashes
can lose changes since the last successful save. Background-tab timer throttling
can increase that window. OPFS is browser-managed storage: quota limits, private
browsing, clearing site data, or browser eviction can remove it. This is not a
substitute for backups. Changing the disk size does not silently resize an
existing image.

Reload in Memory to reset the demo. Use **Rebuild from the current version**
to reset an older or unmountable saved disk, or remove an inactive disk using
the panel.

## Browser storage tests

Install `wasm-bindgen-cli` matching the resolved `wasm-bindgen` dependency and a
Chrome/Chromium WebDriver matching your browser, then run from the repository root:

```sh
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
CHROMEDRIVER=/path/to/chromedriver \
cargo test -p xila-drivers-wasm --target wasm32-unknown-unknown --test opfs
```

For a non-default browser binary, set `WASM_BINDGEN_TEST_WEBDRIVER_JSON` to a JSON
file containing `{"goog:chromeOptions":{"binary":"/path/to/chromium"}}`.
These tests use real OPFS files, including MBR/LittleFS save/reopen coverage.
The disk-name and collision rules can also be tested with
`node --test examples/wasm/tests/disk_manager.test.mjs`.

To test mode switching and recovery in Chromium, serve the built example as
shown above, install Playwright locally, and run the browser regression:

```sh
npm install --no-save --package-lock=false playwright
npx playwright install chromium
XILA_BROWSER_EXAMPLE_ADDRESS=http://localhost:8080 \
node --test examples/wasm/tests/browser_disk_modes.test.mjs
```

See [Synth](../../executables/synth/README.md) for controls, silent mode,
existing-disk behavior and the audio smoke-test procedure.

## Digits NN demo

The default `neural-network` feature mounts a runtime CPU model loader at
`/devices/nn/cpu` using the shared portable CPU backend (no model is embedded in
the firmware; `--no-default-features` omits it); applications write a TFLite
model to that node and send the `LOAD` control command. `mise run image:browser`
includes the pinned, signed Digits release in the reference image. For a local
guest build, follow the commands in `../native/README.md` and create a seed with `--bundle wasm` and
those bundles. Serve it as `seed.img` beside the matching browser runtime. Reload in Memory
or create a saved disk to use the new seed while retaining older saved disks. The guest uses Xila's VFS NN API inside the browser host,
without WebGPU or a separate JavaScript inference engine.
