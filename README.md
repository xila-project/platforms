# Xila Platforms

This workspace assembles Xila runtimes, drivers, the integration harness and seed images. Core and Applications are pinned Git dependencies in `Cargo.toml`; the current branches are part of an unfinished coordinated migration.

For local development install [mise](https://mise.jdx.dev/), run `mise bootstrap packages apply --manager apt`, and run `npm ci` for the graphics font converter. Tasks activate their own Rust toolchains. `mise run build:native` builds the native runtime and `mise run check:browser` checks the browser assembly. `mise run test:seed` runs filesystem-image and package-preparation tests.

Build guest applications separately in the Applications workspace, then set `XILA_APPLICATIONS_DIR` to the directory containing their `.wasm` files and run `mise run image:native` or `mise run image:browser`. The output paths must not already exist. The image generator validates its output. `tools/artifacts` can validate pinned development `.xpk` archives and extract individual payloads into that directory; its checksum does not authenticate a publisher. Select the ABI level from the target runtime rather than assuming one.

For the Docker runtime image, supply those already built guests as a BuildKit named context, for example `docker buildx build --build-context applications=/absolute/path/to/wasm -f Dockerfile.runtime .`. The browser entry point and OPFS code live in `examples/wasm`; native runtime and headless options are documented in `examples/native`.

For local integration against adjacent working copies, generate a Cargo patch file and use it explicitly:

```sh
python3 tools/dev/local_overrides.py --core ../core --applications ../applications --platforms . --output /tmp/xila-platform-patches.toml
cargo metadata --config /tmp/xila-platform-patches.toml --format-version 1
cargo check --config /tmp/xila-platform-patches.toml -p xila-native-example
```

Regenerate `Cargo.lock` without that local configuration before committing. All Core packages must resolve to the same checkout; do not mix local paths and unpatched Git copies.

The **Cross-repository integration** Actions workflow accepts explicit Core and Applications commits and runs the platform assembly plus the application integration-test compile against those checkouts. Select the Platforms branch when dispatching it.

For CJK builds, set `XILA_FONT_CATALOG_ROOTS` to the Applications checkout (or a path-list of catalog roots) so Core includes those translated characters in its generated font subset. The root must contain the application `executables/` and `libraries/` directories.
