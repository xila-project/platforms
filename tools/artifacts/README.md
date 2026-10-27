# Prepare a pinned application artifact

`xila-prepare-applications ARCHIVE SHA256 NAME VERSION MAX_ABI OUTPUT.wasm` verifies an archive against the supplied lowercase SHA-256, checks its package identity and version, requires `wasm32-wasip1` and rejects an ABI requirement above `MAX_ABI`. It validates every archive entry with `xila-package-reader` before extracting the single Wasm payload. The output must not already exist. The accepted ABI cannot exceed the Core ABI version compiled into the tool. Supply a lower value when targeting an older versioned runtime; unversioned historical runtimes have no declared compatibility.

This is an integrity check for pinned development artifacts. The checksum and unsigned package do not authenticate a publisher. Supply separately trusted checksums, and do not treat this tool as a production trust policy.

To prepare an entire seed profile, create a TOML lockfile with an entry for each selected application, for example:

```toml
[applications.clock]
archive = "archives/xila.clock-0.1.0-wasm32-wasip1.xpk"
name = "xila.clock"
version = "0.1.0"
sha256 = "<64 lowercase hex characters from a trusted pinned source>"
```

Resolve and verify the checksum source separately; do not use the illustrative placeholder. Build `xila-prepare-applications`, then run `python3 tools/prepare_seed.py --profile native --lockfile versions.toml --output prepared --tool target/debug/xila-prepare-applications --max-abi 1`. The output directory must not exist. Pass that directory to `xila-fs-image --bundle native --applications-dir prepared`. Use `--profile browser` and `--bundle wasm` for the browser set. This step consumes local archives and does not fetch releases or establish publisher trust.
