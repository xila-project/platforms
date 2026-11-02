# Reference filesystem images

`fs_image` creates a raw disk with the Xila MBR signature, a Xila partition and
LittleFS. It exclusively creates its output, remounts it, and verifies imported
contents and metadata. Existing output files are never replaced.

```sh
# Empty disk (16 MiB by default).
cargo run -p xila-fs-image -- empty.img
# Import a directory; host ownership and timestamps are deliberately ignored.
cargo run -p xila-fs-image -- populated.img 16777216 --root ./root
# Seed a profile from verified application bundle directories (xila.clock/, ...).
cargo run -p xila-fs-image -- native.img 16777216 --bundle native --bundles-dir bundles
cargo run -p xila-fs-image -- browser.img 16777216 --bundle wasm --bundles-dir bundles
```

Each application is seeded as a self-contained bundle at
`/applications/bundles/<package-id>/` (`.xila/manifest.json`, `executables/`,
`shortcut.json`, ...), exactly as an installed package. Bundle directories come
from either source:

- **Released bundles** (official images): `tools/prepare_seed.py` downloads the
  archives pinned in [`profiles/bundles.lock.json`](profiles/bundles.lock.json)
  (URL, version, SHA-256), verifies their identity, ABI, file hashes and
  publisher signature (`publisher_key`), and extracts complete bundles.
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

The native profile contains Calculator, Clock, Pixel Painter, Synth, Text Editor and
Weather; the browser profile contains Calculator, Clock, Pixel Painter, Synth and Text Editor.
Both include native application shortcuts. Set `EMBEDDED_I18N_LOCALE=en`, `fr`, `es`, `de`, `pt-BR`, `it` or `pl`
while building both guests and images. No passwords or random salts are included.
An explicit pending marker allows runtime account initialization to finish after
an interruption; existing saved credentials are preserved.

`mise run image:native` and `mise run image:browser` seed the released bundles
(or `XILA_BUNDLES_DIR`, if set) and write `target/seed/<profile>/seed.img`.
These recipes replace only their
generated reference outputs, never a live runtime disk. Generated images are
ignored by Git. CI publishes localized reference images as artifacts, and serves
the matching browser image beside each localized runtime.

An image update does not upgrade existing installations. Application updates
are a separate workflow. The raw image can also be used as the source for a
whole-disk flashing operation on a target with matching geometry and layout;
flashing an existing disk replaces its data. MCU flashing and hardware startup
are not validated by this host/browser workflow.
