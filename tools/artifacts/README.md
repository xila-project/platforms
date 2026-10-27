# Prepare a pinned application artifact

`xila-prepare-applications ARCHIVE SHA256 NAME VERSION MAX_ABI OUTPUT.wasm` verifies an archive against the supplied lowercase SHA-256, checks its package identity and version, requires `wasm32-wasip1` and rejects an ABI requirement above `MAX_ABI`. It validates every archive entry with `xila-package-reader` before extracting the single Wasm payload. The output must not already exist. Supply an ABI value established by the target runtime; this command does not infer runtime compatibility.

This is an integrity check for pinned development artifacts. The checksum and unsigned package do not authenticate a publisher. Supply separately trusted checksums, and do not treat this tool as a production trust policy.
