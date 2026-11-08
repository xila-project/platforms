<h1>
<picture>
  <source media="(prefers-color-scheme: dark)" srcset=".github/brand/header-dark.svg">
  <img src=".github/brand/header-light.svg" alt="Xila Platforms — Platform drivers and runtime assembly." width="1280">
</picture>
</h1>

[Documentation](https://www.xila.dev/docs/) ·
[Try Xila](https://platforms.xila.dev/) ·
[Community](https://matrix.to/#/#xila:anneraud.fr)

This workspace assembles Xila runtimes, drivers, the integration harness and seed images. Core and Applications are pinned Git dependencies in `Cargo.toml`; the current branches are part of an unfinished coordinated migration.

For local development install [mise](https://mise.jdx.dev/), run `mise bootstrap packages apply --manager apt`, and run `npm ci` for the graphics font converter. Tasks activate their own Rust toolchains. `mise run build:native` builds the native runtime and `mise run check:browser` checks the browser assembly. `mise run test:seed` runs filesystem-image and package-preparation tests.

`mise run image:native` and `mise run image:browser` seed the released, signed application bundles pinned in `tools/fs_image/profiles/bundles.lock.json`. To seed locally built guests instead (for example another locale, since translations are compiled into each guest), bundle them with `tools/bundle_applications.sh <applications-source> <wasm-dir> <output>` and set `XILA_BUNDLES_DIR` to that output. The output paths must not already exist. The image generator validates its output. Select the ABI level from the target runtime rather than assuming one. See [`tools/fs_image`](tools/fs_image/README.md).

For the Docker runtime image, supply bundle directories as a BuildKit named context, for example `docker buildx build --build-context applications=/absolute/path/to/bundles -f Dockerfile.runtime .`. The browser entry point and OPFS code live in `examples/wasm`; native runtime and headless options are documented in `examples/native`.

The browser example is published by this repository at [platforms.xila.dev](https://platforms.xila.dev/). The root opens the English example; localized builds are under `demonstrations/wasm/<locale>/`. A successful main-branch CI run triggers the Pages deployment. Core documentation remains at [documentation.xila.dev](https://documentation.xila.dev/).

For local integration against adjacent working copies, generate a Cargo patch file and use it explicitly:

```sh
python3 tools/dev/local_overrides.py --core ../core --applications ../applications --platforms . --output /tmp/xila-platform-patches.toml
cargo metadata --config /tmp/xila-platform-patches.toml --format-version 1
cargo check --config /tmp/xila-platform-patches.toml -p xila-native-example
```

Regenerate `Cargo.lock` without that local configuration before committing. All Core packages must resolve to the same checkout; do not mix local paths and unpatched Git copies.

The **Cross-repository integration** Actions workflow accepts explicit Core and Applications commits and runs the platform assembly plus the application integration-test compile against those checkouts. Select the Platforms branch when dispatching it.

For CJK builds, set `XILA_FONT_CATALOG_ROOTS` to the Applications checkout (or a path-list of catalog roots) so Core includes those translated characters in its generated font subset. The root must contain the application `executables/` and `libraries/` directories.

The ESP32 sources and Wokwi board files are retained from Core, but the current Espressif driver is gated out on host targets and still contains legacy API references. No ESP32 or Wokwi build has been established by these host checks; a supported ESP-IDF toolchain and board-specific build are needed before claiming that port works.

## Package installation (native runtime)

The native runtime mounts the CLI-only `package` command at
`/applications/executables/package`. It installs v1 `.xpk` bundles
from paths in Xila's VFS, using 512-byte TAR streaming and a full Ed25519
publisher-signature / SHA-256 inventory check **only at installation time**.
Normal execution of the installed Wasm never repeats the package signature
check. The installed package is stored at
`/applications/bundles/<package-id>/`; application-owned shortcuts in the bundle are discovered automatically by
Graphical Shell when opening the drawer. Upgrades and package downloading are
not implemented yet.

```text
package install /temporary/example.xpk
package install /temporary/example.xpk --public-key /temporary/developer.pub
package install /temporary/example.xpk --allow-untrusted
package list
```

The default trust anchor is a raw 32-byte Ed25519 key at
`/configuration/system/packages/trusted.pub`. Passing `--public-key`
explicitly trusts another signer **for this installation only**. Passing
`--allow-untrusted` explicitly skips *publisher authentication*, but never
SHA-256, TAR layout or compatibility validation. System-wide installation
currently requires a root (UID 0) Xila task; the seeded administrator login
is UID 1000 and has no implicit privilege escalation. A privileged package
service is future work. See [the CLI README](https://github.com/xila-project/applications/tree/main/executables/package)
for limitations and security assumptions.

<!-- xila:footer:start -->

---

Part of **[Xila](https://www.xila.dev/)** ·
[Repositories](https://github.com/xila-project) ·
[Community](https://matrix.to/#/#xila:anneraud.fr)

<!-- xila:footer:end -->
