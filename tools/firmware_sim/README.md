# Native firmware update simulator

**Experimental.** Native NOR reference backend for Core's [`firmware_update`](https://github.com/xila-project/core/tree/main/libraries/firmware_update) transaction owner, and the `xila-firmware-sim` CLI. It implements `firmware_update::FirmwareBackend`; the transaction contract, trust model and byte ABI are documented in Core. It is not a production bootloader and gives no hardware-rooted secure boot or anti-rollback guarantee.

## Reference flash and image formats

NOR geometry is intentionally small for exhaustive tests: 256-byte erase sectors, 64-byte program pages, minimum write one byte, erased value FF, program only 1→0, no program crossing a page. This is simulation geometry, not an ESP partition table. Durable write counts include only successfully completed page programs; on any program error the transaction is invalid even if a prefix reached flash.

| Region | Offset | Bytes |
| --- | --- | --- |
| Boot record 0 / 1 | 0 / 256 | 256 each, first 64 used |
| Slot A / B | 512 / 4608 | 4096 each |
| Per-slot signed metadata | slot start | 144 in reserved 256-byte header sector |
| Per-slot image | slot start + 256 | at most 3840 |

Total flash backing: **8704 bytes**. Each 64-byte journal record has magic `FWJ1`, generation u64, confirmed/pending slot bytes, trial byte, software security floor u64, reserved zeros and SHA-256 over its first 32 bytes. `BEGIN` first commits a fresh generation, invalidating proofs retained across abort or backend reconstruction. Write the other record's sector while keeping the newest valid record untouched. Select the largest valid generation; ties/no valid record require recovery. The checksum detects torn/corrupt metadata; it does not authenticate metadata against a malicious host.

The simulator-only manifest is 144 bytes: exact 16-byte domain `XILA-FW-SIM-v1\0\0`, target bytes 16–31, logical size LE u64 at 32, security version LE u64 at 40, SHA-256 at 48–79, strict ordinary Ed25519 signature over **exactly bytes 0–79** at 80–143. No public key comes from this manifest. This format is not accepted by ESP-IDF or MCUboot.

`MemoryNor` borrows caller storage; `FileNor` streams raw file I/O with fixed scratch and holds an exclusive OS file lock. It writes/syncs an injected prefix before returning failure; no whole-image buffer, rename or host journal is used to stand in for NOR atomicity. Host `sync_data` is a persistence aid for process tests, not proof of physical flash/cache/brownout behavior. Repeated zero-progress I/O is not treated as success. Faults select zero-based erase/program operation and a byte prefix (0 = before; operation length or larger = after). The medium rejects further mutation after the cut; reconstruct it to model reboot.

## Usage

The simulator and its tests create public-key/signed synthetic fixtures using a deterministic **test-only** seed and start separate `xila-firmware-sim` processes for staging, trial, revert and confirmation. No private production key is present. From this repository, supply your own **test** public key, manifest and image files:

```sh
cargo run --bin xila-firmware-sim -- \
  init --flash /tmp/fw.img --key test-public-key.bin --target xila-native-test \
  --manifest factory.manifest --image factory.bin
# Same public key/target options are required for every command below.
xila-firmware-sim update --flash /tmp/fw.img --key test-public-key.bin --target xila-native-test \
  --manifest next.manifest --image next.bin
xila-firmware-sim boot --flash /tmp/fw.img --key test-public-key.bin --target xila-native-test --confirm
xila-firmware-sim inspect --flash /tmp/fw.img --key test-public-key.bin --target xila-native-test
```

`cargo run` does not install the binary in PATH; use `target/debug/xila-firmware-sim` or the same `cargo run ... --` prefix. `update --stage-only` closes without activation. `update` or `boot --fault-operation N --fault-byte K` injects a torn operation; rerun without fault flags to simulate reset.

## Measurements

The allocation regression instruments the Rust allocator with a positive control and measures provision, begin/write, strict signature/readback, activate, boot and confirm: **zero heap allocation requests** in the core and in-memory backend path. The VFS/runtime/harness allocates separately (one bounded context marker per open, namespace/runtime state and a 64 KiB test filesystem); the native file backend uses OS resources. Flash storage is not RAM. On the native x86_64 build the simulator is 368 bytes and its transaction owner is 608 bytes; these figures include bounded state, not the backing medium. The test prints native `size_of` values for the backend/transaction and backing flash; these are not peak stack, linked flash or target ABI results.

## Reproduce

```sh
mise run firmware:test
```
