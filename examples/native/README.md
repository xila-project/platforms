# Native example

Prepare an independent reference disk, then run the example:

```sh
mise run image:native
cargo run -p xila-native-example --bin native_example -- --terminal --no-network \
  --seed-image target/seed/native/seed.img
```

Use `--gui` for the graphical interface. The default live disk is `./drive.img`;
select another with `--disk-path PATH`. The default seed path is `./seed.img`.
When packaging the executable, distribute a matching seed alongside it, or pass
its explicit path. The default disk size is 16 MiB; custom seeds and
`--disk-size BYTES` must agree.

Host audio is enabled by default (the `audio` feature), so audio applications
such as Synth can play through the default host output. Linux builds require
ALSA development files (`libasound2-dev` and `pkg-config` on Debian/Ubuntu,
installed by `mise bootstrap packages apply --manager apt`). Build with
`--no-default-features` for a silent runtime without ALSA, as the container
image does.

Only an absent live disk is imported. Import validates a temporary copy before
publishing it without replacement. Existing disks do not require a seed and are
never resized or automatically reformatted, including empty or corrupt files.
A mount error requires inspecting or restoring that disk; to start another
installation, choose a different disk path. Do not delete a disk that contains
data you want to keep.

A new installation creates **administrator** with an empty password. Existing
accounts and passwords load on subsequent starts. Explicit pending installation
state permits completion after interruption without resetting saved credentials.
Applications and shortcuts are already in the image; startup performs no guest
compilation or bundled application installation. The native bundle includes
Calculator, Clock, Pixel Painter, Synth and Weather. Updating the reference image does
not replace applications on existing disks.

The `prebuilt-applications` feature remains accepted for older profiling commands,
but every build now uses the disk image. `XILA_PROFILE_APP_DIR` remains useful to
the profiling recorder for hashing source guest binaries; it no longer changes
runtime loading. See the repository's profiling instructions for preparing and
selecting a seed before measurement.

With the `audio` feature enabled, the example mounts `/devices/audio/playback`
using the default host audio output, and logs `Native audio ready`. Audio
initialization failures are logged and leave the rest of the OS usable. To
check output without speakers, route the runtime to a PipeWire null sink and
record its monitor:

```sh
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=xila-test media.class=Audio/Sink object.linger=true audio.position=[FL FR] }'
pw-record --target xila-test -P '{ stream.capture.sink=true }' capture.wav &
PIPEWIRE_NODE=xila-test cargo run -p xila-native-example --bin native_example -- --display-driver headless
```
See [Synth](../../executables/synth/README.md) for controls, silent mode and checks.

## Inspecting the graphical UI

The graphical interface uses the native window driver by default. Select the
output-only headless driver to run without an X11 or Wayland display server:

```sh
cargo run -p xila-native-example --bin native_example -- --display-driver headless --resolution 800x600
```

`--resolution` applies to either graphical driver. Headless resolutions are
limited to 2,073,600 pixels. Use `--display-driver native` to select the window
driver explicitly. The headless mode starts the Debug application automatically
in the background, so it does not cover the UI being inspected. The
display-driver option conflicts with `--terminal`.

In a second terminal, run the host client as the same OS user as the runtime:

```sh
cargo run -p xila-drivers-native --bin xila-debug -- diagnostics
cargo run -p xila-drivers-native --bin xila-debug -- resolution
cargo run -p xila-drivers-native --bin xila-debug -- screenshot /tmp/xila-ui.png
cargo run -p xila-drivers-native --bin xila-debug -- ui-tree
cargo run -p xila-drivers-native --bin xila-debug -- wait-for --text Settings --timeout-ms 3000
cargo run -p xila-drivers-native --bin xila-debug -- pointer move 120 80
cargo run -p xila-drivers-native --bin xila-debug -- pointer button press
cargo run -p xila-drivers-native --bin xila-debug -- key enter press
cargo run -p xila-drivers-native --bin xila-debug -- press-key enter
cargo run -p xila-drivers-native --bin xila-debug -- text 'hello xila'
```

The Debug application uses Xila graphics, task, and memory subsystem APIs for
screen capture, UI inspection, input injection, and diagnostics. The selected
driver only supplies display output and any physical inputs. The host client
receives a bounded raw RGBA screenshot and writes it as PNG to the requested
path. JSON headers are limited to 64 KiB and screenshot data to 8 MiB. The
transport is available on Unix hosts.

`wait-for --text TEXT` polls the UI tree for a case-sensitive substring of an
individual element's text. Its timeout defaults to 3000 ms and must be between
1 and 60000 ms; it covers connecting and receiving each response as well as
polling. It returns JSON and a nonzero status on timeout or transport failure.
A timeout with `error.truncated: true` means at least one snapshot omitted part
of the tree, so it cannot establish that the text was absent. Success means the
text was observed in a snapshot; it does not promise that the UI remains in
that state or that queued input has been consumed.

`press-key` queues its press and release together. `text` accepts up to 4096
UTF-8 bytes, rejects control characters, and queues whole character pairs. The
client retries when the bounded input queue is full, for up to 30 seconds. Its
JSON response reports `status: "queued"`: applications may not have processed
the input yet. On an error, `confirmed_queued_bytes` is a UTF-8 byte offset for
the acknowledged prefix. `last_request_status: "unknown"` means the last
request may also have been queued; inspect the UI before retrying to avoid
duplicate input. Unicode is passed to LVGL text widgets; native Xila window key
events still expose only an 8-bit character and guest window events do not expose
key data. Applications that consume those events cannot reliably receive Unicode;
verify the destination widget before sending text.

### Selecting controls and waiting for state

Inspect `capabilities` first to discover the protocol version, operations and limits.
The tree covers the active screen and top/system layers after layout, including
shell chrome and popups. Screenshots composite those layers in display order.
Tree IDs are snapshot-local and must never be used as persistent selectors.

```sh
# Prefix these with cargo run -p xila-drivers-native --bin xila-debug --
capabilities
find --role textbox --name 'User name'
click --role textbox --name 'User name'
text administrator
wait-for --role textbox --name 'User name' --text administrator --timeout-ms 3000
click --role button --name Login
pointer click 120 80
wait-for --role buttonmatrix --button 7 --stable-ms 150 --timeout-ms 5000
click --role buttonmatrix --button 7
wait-for --role slider --name Volume --value 42 --enabled true
```

`find` requires exactly one matching visible element and a complete tree.
`click` resolves a unique control under the graphics lock, hit-tests its current
clipped center, and atomically queues its pointer click. It rejects ambiguous,
obscured or truncated results. Click accepts exact `--role`, `--name`, `--text`
and `--button` predicates; `--button` selects a virtual matrix/keyboard button.
Roles accept LVGL class names or semantic roles. Other click predicates are
rejected rather than silently ignored. Coordinate clicks remain available.

`wait-for` accepts these predicates plus `--value`, `--checked`, `--focused`,
`--enabled` (boolean values are `true` or `false`) and `--layer screen|top|system`.
Its `--text` predicate is a substring; other string predicates are exact.
Hidden, fully clipped and transparent elements are excluded. A match describes
an observation, not general UI idle or input completion. Use widget-specific
state instead of a launcher label to establish app readiness. `--stable-ms`
requires a unique matching element's observed properties (excluding snapshot
indexes) to remain unchanged for that interval;
it does not freeze clocks, animations or other parts of the UI.

The tree includes geometry/clipping, raw LVGL state, scroll position, main-part
resolved colors, opacity, padding/gap, layout and font line height. Supported
range widgets include value/range; dropdowns/rollers include selection text;
matrices include up to 64 virtual buttons per widget and 128 across a tree.
Password widgets and their descendants redact text and content, while the
password textarea's public placeholder remains usable as a selector. Screenshot
content follows what LVGL visibly renders, including its normal password masking.

Tree responses keep a usable prefix within the 64 KiB header budget and mark
`truncated` when objects are omitted. Per-field truncation flags and
`buttons_truncated` expose content caps; UTF-8 is never split mid-character.
Visibility describes hidden state, geometry and opacity; it does not prove an
element is unoccluded. Click performs a separate hit-test. Per-app CPU/memory,
Wasm trap history, log streaming and global frame-idle guarantees are not
provided; `capabilities` reports unavailable instrumentation explicitly.

All ordinary requests, including the complete screenshot payload and text
queue retries, have a total deadline. The default is 30 seconds; put a
`--timeout-ms 1..60000` option before the command to change it:

```sh
cargo run -p xila-drivers-native --bin xila-debug -- --timeout-ms 500 screenshot /tmp/xila-ui.png
```

`wait-for` defaults to 3000 ms, honors a global `--timeout-ms` when supplied,
and lets a command-level `--timeout-ms` override that default. Duplicate flags
and unknown layer names are rejected.

## Wi-Fi control-plane demo

Run the deterministic mock alongside the headless UI, without TUN/TAP privileges:

```sh
cargo run -p xila-native-example --bin native_example -- --no-network --mock-wifi \
  --display-driver headless --resolution 800x600 \
  --disk-path /tmp/xila-wifi-demo.img --seed-image target/seed/native/seed.img
```

After login, open Settings → Network → wlan0. Its Wi-Fi panel controls the radio,
scans, connects, and disconnects through `/devices/network/wlan0`. The synthetic
`Xila demo` network uses WPA2 and the public test password `xila-demo`;
`Xila open` requires no password. Incorrect credentials exercise authentication
failure. The mock uses an explicit Xila task, 16 maximum scan results, and
configurable delays/outcomes. It does not control host Wi-Fi, persist profiles,
or provide Internet connectivity: its packet device is a smoltcp loopback.
A hardware or TAP packet device can use the same controller API independently.

Run the hardware-free VFS lifecycle regression with
`cargo test -p xila-drivers-native --test wifi`. Generic payload validation and command
identifier uniqueness are covered by `cargo test -p xila-network --test wifi_types`.

## System activity dashboard

The native System activity app reports uptime, registered tasks and allocator
usage with asynchronous refresh, explicit unavailable metrics, and a bounded
start/stop demo workload. See [its metric sources and smoke test](../../executables/activity/README.md).
It is compiled into the native runtime; it has no guest Wasm binary.

## Digits demo

The `digits` default feature registers the small CPU MNIST model at
`/devices/nn/digits`. `--no-default-features` omits it. The model and the guest
belong to Applications; this runtime selects the portable NN backend.
Both reference image profiles include the signed `xila.digits` release. Build
`mise run image:native` or `mise run image:browser`, then use a new disk to import
the updated seed and open **Digits** from the launcher. Existing disks keep their
installed applications.

For local development from the multi-repository workspace, create a Cargo patch
configuration:

```sh
python3 repositories/platforms/tools/dev/local_overrides.py \
  --core repositories/core --platforms repositories/platforms \
  --applications repositories/applications --output /tmp/xila-local.toml
cd repositories/applications
cargo build --config /tmp/xila-local.toml --target wasm32-wasip1 \
  --profile release-wasm -p xila-calculator -p xila-clock -p xila-weather \
  -p xila-pixel-painter -p xila-text-editor -p xila-synth -p xila-digits
cd ../platforms
tools/bundle_applications.sh ../applications \
  ../applications/target/wasm32-wasip1/release-wasm target/digits-bundles
cargo run --config /tmp/xila-local.toml -p xila-fs-image -- \
  target/digits-seed.img 16777216 --bundle native --bundles-dir target/digits-bundles
cargo run --config /tmp/xila-local.toml -p xila-native-example --bin native_example -- \
  --gui --no-network --disk-path target/digits-drive.img --seed-image target/digits-seed.img
```

The seed and bundle commands create new paths; choose another path on a second
run. Open **Digits** in the launcher, draw a digit, then release to recognize.
Use an absent demo disk path to import the new seed without changing an existing
installation. The same local bundles work with `--bundle wasm` for the browser
seed; copy that seed beside the browser runtime as described in its README.
Local patches rewrite Cargo.lock; do not commit those source substitutions.

## Built-in native C sample

The host runtime links `xila-c-demo` from Applications and mounts
`/applications/executables/c_demo`. Log in to the terminal (a fresh image uses
`administrator` with an empty password), then run:

```text
/applications/executables/c_demo hello world
```

It prints its arguments, native ABI version and clocks, then writes and reads
`c-demo.txt` in the current user's home through the native C ABI. The Rust bridge
grants standard streams and the user's home; C borrows those startup objects.
This is compiled into the firmware and is not a dynamically loaded package.
Only desktop native targets include it; browser and hardware examples do not.

## Shutdown

Leaving the shell (`exit`, end of input), the `poweroff` command and SIGINT or
SIGTERM all run Core's lifecycle pipeline and end this Xila process; the host
is never powered off or rebooted. `poweroff` and `reboot` call the lifecycle
API, which accepts a request only from users allowed to use the power device
`/devices/power` (root and the provisioned administrator group 1000, mode
0660). Its native implementation exits the process; `reboot` reports that
restart is unsupported.

The pipeline signals the shell and every application task it launched, waits
up to 5 seconds for them to finish, refuses to continue while files are still
open, freezes writes, unmounts LittleFS (syncing the disk image), then powers
off through `/devices/power` with the shell's status, 0 after `poweroff`, or
128 + signal after SIGINT/SIGTERM. Signal handlers only update atomics. If
shutdown fails (an application ignores the stop request, or the disk cannot be
synced), the process stays alive with every resource retained and reports the
error; another SIGINT/SIGTERM then forces the exit, losing only data LittleFS
had not committed. Runtime services (graphics, network, Debug) are not joined
and end with the process.
