# Reference filesystem images

`fs_image` creates a raw disk with the Xila MBR signature, a Xila partition and
LittleFS. It exclusively creates its output, remounts it, and verifies imported
contents and metadata. Existing output files are never replaced.

The image engine is the `xila-tools-image` library of
[`xila-project/tools`](https://github.com/xila-project/tools), also behind
`xila image build`. Platforms depends on it through Git, so `Cargo.lock` pins
it with this firmware's Core and the images always match the Core revision that
mounts them. This crate keeps Platforms' seed data (`profiles/`, `shortcuts/`,
`repositories/`) and a thin command line that passes it to the library.

```sh
# Empty disk (16 MiB by default).
cargo run -p xila-fs-image -- empty.img
# Import a directory; host ownership and timestamps are deliberately ignored.
cargo run -p xila-fs-image -- populated.img 16777216 --root ./root
# Seed a profile from verified application bundle directories (xila.clock/, ...).
cargo run -p xila-fs-image -- native.img 16777216 --bundle native --bundles-dir bundles
cargo run -p xila-fs-image -- browser.img 16777216 --bundle wasm --bundles-dir bundles
# Seed the classic WT32-SC01 storage partition with native shortcuts.
mise run image:wt32-sc01
# Seed the WT32-SC01 Plus storage partition with locally built bundles.
XILA_BUNDLES_DIR="$PWD/bundles" mise run image:wt32-sc01-plus
```

A `native` image lists the Debug shortcut only for a native runtime that
provides the Debug control transport (`/devices/graphics/debug/transport`),
which the Unix native example mounts. `--native-runtime unix|windows` names the
runtime that will mount the image; it defaults to the family of the host
building it, so pass it when building an image for another host. The published
reference `native.img` is built with `--native-runtime unix`.

The official package registry is seeded as the `xila` repository
([`repositories/xila.json`](repositories/xila.json), in
`/configuration/system/packages/repositories/`), pinned to its root key
`AJEDFQ2MPtUO0vu/BnC7xX5Ag8sBuq6DF5+ShKfCQgU=` (fingerprint
`f0df 8609 b382 1414 7e39 51c0 b40b c2ea`), so `package install <name>` needs no
trust prompt.

Each application is seeded as a self-contained bundle at
`/applications/bundles/<package-id>/` (`.xila/manifest.json`, `executables/`,
`shortcut.json`, ...), exactly as an installed package. Bundle directories come
from either source:

- **Released bundles** (official images): `tools/prepare_seed.py` downloads the
  archives pinned in [`profiles/bundles.lock.json`](profiles/bundles.lock.json)
  (URL, version, SHA-256), verifies their identity, Xila version compatibility,
  file hashes and publisher signature (`publisher_key`), and extracts complete
  bundles.
- **Locally built guests** (Pages demos, Docker, development): translations are
  compiled into each guest, so locale-specific images bundle their own build with
  `tools/bundle_applications.sh <applications-source> <wasm-dir> <output>`, which
  runs `xila-package bundle build` (unsigned) with each application's
  `packaging/<app>.json` and `shortcut.json`.

Native applications are linked into the runtime; only their shortcuts
([`shortcuts/`](shortcuts)) are seeded, in
`/configuration/shared/applications/shortcuts/`.

Host-side population currently requires Linux or macOS. It opens source entries
relative to directory descriptors, rejecting symlink substitutions and FIFOs
even if the source tree changes during generation. Other native platforms can
consume the images published by CI.

Sizes must be multiples of 512 bytes and accommodate the MBR and LittleFS (at
least 1049600 bytes; populated bundles need more). Import accepts regular files
and directories with UTF-8 names, rejecting symlinks and special files. It uses
sorted traversal, zero timestamps, root ownership, directory/binary mode 0755
and other file mode 0644. Account directories and global application data and
configuration (`/data/applications`, `/configuration/applications`) are 0700;
applications keep their data per user. Repeating a build with identical
inputs and tool versions produces identical bytes; host file permissions and
modification times do not enter the image.

The native and browser profiles both contain Calculator, Clock, Digits, Pixel Painter, Synth, Text Editor and
Weather. In the browser, Weather and `web_request` reach the network through the `fetch`-backed
`https_client` device, so they only work against servers that allow cross-origin requests.
Both include native application shortcuts. The `wt32-sc01` and `wt32-sc01-plus`
profiles share the Activity, File Manager, Settings and Terminal shortcuts and
seed the official registry. The Plus profile also includes the same seven Wasm
applications. Each board task writes a raw 4096-byte-block LittleFS image to
`target/seed/<board>/storage.img`, sized to the `storage` partition in its
[partition table](../../examples/boards/README.md).
Set `EMBEDDED_I18N_LOCALE=en`, `fr`, `es`, `de`, `pt-BR`, `it`, `pl`, `ja` or `zh-CN`
while building both guests and images. No passwords or random salts are included.
An explicit pending marker allows runtime account initialization to finish after
an interruption; existing saved credentials are preserved.

`mise run image:native`, `mise run image:browser` and `mise run image:wt32-sc01-plus` seed the released bundles
(or `XILA_BUNDLES_DIR`, if set) and write `target/seed/<profile>/seed.img` (or `storage.img` for the
WT32-SC01 Plus).
These recipes replace only their
generated reference outputs, never a live runtime disk. Generated images are
ignored by Git. CI publishes localized reference images as artifacts, and serves
the matching browser image beside each localized runtime.

An image update does not upgrade existing installations. Application updates
are a separate workflow. The raw image can also be used as the source for a
whole-disk flashing operation on a target with matching geometry and layout;
flashing an existing disk replaces its data. MCU flashing and hardware startup
are not validated by this host/browser workflow.
