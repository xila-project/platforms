# Prepare a pinned application artifact

`xila-prepare-applications ARCHIVE SHA256 NAME VERSION MAX_ABI OUTPUT_DIR [PUBLIC_KEY]` verifies an archive against the supplied lowercase SHA-256, checks its package identity and version, requires `wasm32-wasip1`, rejects an ABI requirement above `MAX_ABI`, and validates the Wasm entrypoint with memory64 disabled. Every archive entry is checked with `xila-package-reader`; with `PUBLIC_KEY` (a raw 32-byte Ed25519 key) the publisher signature is required too. It then extracts the **complete bundle** (`.xila/manifest.json`, `.xila/manifest.sig` when signed, and every declared file) into `OUTPUT_DIR`, which must not exist. A valid module and declared ABI do not prove that every import is supported by a particular runtime. The accepted ABI cannot exceed the Core ABI version compiled into the tool.

To prepare an entire seed profile, `tools/prepare_seed.py` reads a JSON lockfile (default [`tools/fs_image/profiles/bundles.lock.json`](../fs_image/profiles/bundles.lock.json)) with an entry per application, for example:

```json
{
  "publisher_key": "<base64 raw Ed25519 public key>",
  "applications": {
    "clock": {
      "name": "xila.clock",
      "version": "0.1.0",
      "url": "https://github.com/xila-project/applications/releases/download/xila.clock-v0.1.0/xila.clock-0.1.0-wasm32-wasip1.xpk",
      "sha256": "<64 lowercase hex characters>"
    }
  }
}
```

Use `"archive": "<path relative to the lockfile>"` instead of `url` for a local archive; URLs must be HTTPS. Build `xila-prepare-applications`, then run `python3 tools/prepare_seed.py --profile native --output bundles --tool target/debug/xila-prepare-applications --max-abi 1` and pass the directory to `xila-fs-image --bundle native --bundles-dir bundles`. Use `--profile browser` and `--bundle wasm` for the browser set. The signature proves which key signed a bundle; whether to trust that key is decided by the pinned `publisher_key` (see `xila-project/registry`).
