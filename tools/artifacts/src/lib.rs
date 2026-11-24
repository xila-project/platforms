use tar_no_std::TarArchiveRef;
use xila_package_manifest::{MANIFEST_PATH, SIGNATURE_PATH, XilaVersion};
use xila_package_verification::{ContentVerifier, PublicKey};

/// The Xila version of the Core revision compiled into this tool.
pub const RUNNING_XILA_VERSION: XilaVersion =
    XilaVersion::new(xila_version::RUNNING.major, xila_version::RUNNING.minor);

/// One file of an extracted bundle, at its bundle-relative path.
pub struct BundleFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// Verify a released archive against a pinned digest, identity and the running
/// Xila version (and its publisher signature when `publisher` is given), then
/// return every file of the bundle it contains: `.xila/manifest.json`,
/// `.xila/manifest.sig` when signed, and each declared payload. Extracting them
/// yields a ready bundle.
pub fn prepare(
    bytes: &[u8],
    name: &str,
    version: &str,
    digest: [u8; 32],
    publisher: Option<&PublicKey>,
) -> Result<Vec<BundleFile>, &'static str> {
    let mut verifier =
        ContentVerifier::new(bytes.len() as u64, digest).map_err(|_| "invalid size")?;
    verifier.update(bytes).map_err(|_| "invalid archive size")?;
    verifier.verify().map_err(|_| "archive checksum mismatch")?;
    // Both paths check every declared file's size and SHA-256.
    let manifest = match publisher {
        Some(key) => xila_package_reader::verify_signed(bytes, key)
            .map_err(|_| "invalid package or publisher signature")?,
        None => xila_package_reader::inspect_unsigned(bytes).map_err(|_| "invalid package")?,
    };
    if manifest.name != name || manifest.version != version || manifest.target != "wasm32-wasip1" {
        return Err("package is incompatible");
    }
    if !xila_package_manifest::compatible(manifest.xila_version, RUNNING_XILA_VERSION) {
        return Err("package requires a Xila version incompatible with this runtime");
    }
    let entrypoint = manifest
        .application
        .as_ref()
        .ok_or("package lacks an application descriptor")?
        .entrypoint
        .as_str();
    if !xila_package_manifest::is_bundle_executable(entrypoint)
        || !entrypoint.ends_with(".wasm")
        || manifest
            .files
            .iter()
            .filter(|item| item.path == entrypoint)
            .count()
            != 1
    {
        return Err("package entrypoint is incompatible");
    }

    let archive = TarArchiveRef::new(bytes).map_err(|_| "invalid archive")?;
    let mut files = Vec::new();
    for entry in archive.entries() {
        let path = entry.filename();
        let path = path.as_str().map_err(|_| "invalid archive path")?;
        let declared = path == MANIFEST_PATH
            || path == SIGNATURE_PATH
            || manifest.files.iter().any(|file| file.path == path);
        if !declared {
            return Err("archive contains an undeclared file");
        }
        if files.iter().any(|file: &BundleFile| file.path == path) {
            return Err("archive contains a duplicate file");
        }
        files.push(BundleFile {
            path: path.into(),
            bytes: entry.data().to_vec(),
        });
    }
    let payload = &files
        .iter()
        .find(|file| file.path == entrypoint)
        .ok_or("missing payload")?
        .bytes;
    // Exactly the proposals the Xila host engine accepts in its default build
    // (executables/wasm/src/host/engine.rs), so a bundle is refused here when
    // the engine would refuse it. Floating-point types are core Wasm, not a
    // proposal. Custom page sizes are needed by guests linked with 1-byte
    // pages; memory64 is only accepted by the engine's memory_64 build.
    let features = wasmparser::WasmFeatures::FLOATS
        | wasmparser::WasmFeatures::MUTABLE_GLOBAL
        | wasmparser::WasmFeatures::SIGN_EXTENSION
        | wasmparser::WasmFeatures::SATURATING_FLOAT_TO_INT
        | wasmparser::WasmFeatures::MULTI_VALUE
        | wasmparser::WasmFeatures::BULK_MEMORY
        | wasmparser::WasmFeatures::REFERENCE_TYPES
        | wasmparser::WasmFeatures::TAIL_CALL
        | wasmparser::WasmFeatures::EXTENDED_CONST
        | wasmparser::WasmFeatures::CUSTOM_PAGE_SIZES;
    wasmparser::Validator::new_with_features(features)
        .validate_all(payload)
        .map_err(|_| "invalid or incompatible Wasm module")?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::{RUNNING_XILA_VERSION, prepare};
    use ed25519_dalek::{Signer, SigningKey};
    use xila_package_manifest::{
        Application, FORMAT_VERSION, Manifest, ManifestFile, SCHEMA_URL, XilaVersion,
    };
    use xila_package_verification::PublicKey;
    use xila_package_writer::{SourceFile, write_package};

    const WASM: &[u8] = b"\0asm\x01\0\0\0";
    const SHORTCUT: &[u8] = br#"{"name":"Clock"}"#;

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        let mut hash = xila_package_verification::Sha256Verifier::new();
        hash.update(bytes).unwrap();
        hash.finalize().unwrap()
    }

    fn hex(bytes: &[u8]) -> String {
        sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn manifest(target: &str, xila_version: XilaVersion, payload: &[u8]) -> Manifest {
        Manifest {
            schema: Some(SCHEMA_URL.into()),
            format_version: FORMAT_VERSION,
            name: "xila.clock".into(),
            version: "1.0.0".into(),
            target: target.into(),
            xila_version,
            capabilities: vec![],
            files: vec![
                ManifestFile {
                    path: "executables/clock.wasm".into(),
                    size: payload.len() as u64,
                    sha256: hex(payload),
                },
                ManifestFile {
                    path: "shortcut.json".into(),
                    size: SHORTCUT.len() as u64,
                    sha256: hex(SHORTCUT),
                },
            ],
            application: Some(Application {
                entrypoint: "executables/clock.wasm".into(),
                display_name: "Clock".into(),
                icon: "".into(),
                show_in_launcher: true,
            }),
            metadata: None,
        }
    }

    fn package_for_xila(
        target: &str,
        xila_version: XilaVersion,
        payload: &[u8],
        signer: Option<&SigningKey>,
    ) -> Vec<u8> {
        let manifest = manifest(target, xila_version, payload);
        let signature = signer.map(|key| {
            let raw = xila_package_manifest::encode(&manifest).unwrap();
            let mut buffer = vec![0; raw.len() + 64];
            let message =
                xila_package_manifest::signed_message_into(raw.as_bytes(), &mut buffer).unwrap();
            key.sign(message).to_bytes()
        });
        let mut archive = Vec::new();
        write_package(
            &mut archive,
            &manifest,
            &[
                SourceFile {
                    path: "executables/clock.wasm",
                    bytes: payload,
                },
                SourceFile {
                    path: "shortcut.json",
                    bytes: SHORTCUT,
                },
            ],
            signature.as_ref(),
        )
        .unwrap();
        archive
    }

    fn package(target: &str, payload: &[u8], signer: Option<&SigningKey>) -> Vec<u8> {
        package_for_xila(target, RUNNING_XILA_VERSION, payload, signer)
    }

    #[test]
    fn extracts_the_complete_bundle() {
        let bytes = package("wasm32-wasip1", WASM, None);
        let files = prepare(&bytes, "xila.clock", "1.0.0", sha256(&bytes), None).unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                ".xila/manifest.json",
                "executables/clock.wasm",
                "shortcut.json"
            ]
        );
        assert_eq!(files[1].bytes, WASM);
        assert_eq!(files[2].bytes, SHORTCUT);
    }

    #[test]
    fn accepts_byte_and_default_pages_but_not_memory64() {
        // (memory 1 (pagesize 1)), (memory 1) and (memory i64 1).
        let byte_pages: &[u8] = b"\0asm\x01\0\0\0\x05\x04\x01\x08\x01\x00";
        let default_pages: &[u8] = b"\0asm\x01\0\0\0\x05\x03\x01\x00\x01";
        let memory64: &[u8] = b"\0asm\x01\0\0\0\x05\x03\x01\x04\x01";
        // (type (func (result f32))): core floating-point types.
        let float_type: &[u8] = b"\0asm\x01\0\0\0\x01\x05\x01\x60\x00\x01\x7d";
        for (payload, valid) in [
            (byte_pages, true),
            (default_pages, true),
            (float_type, true),
            (memory64, false),
        ] {
            let bytes = package("wasm32-wasip1", payload, None);
            let result = prepare(&bytes, "xila.clock", "1.0.0", sha256(&bytes), None);
            assert_eq!(result.is_ok(), valid, "{payload:?}");
        }
    }

    #[test]
    fn rejects_proposals_the_host_engine_does_not_accept() {
        // (memory 1) (memory 1): multi-memory.
        let two_memories: &[u8] = b"\0asm\x01\0\0\0\x05\x05\x02\x00\x01\x00\x01";
        // (type (func (result v128))): SIMD.
        let simd_type: &[u8] = b"\0asm\x01\0\0\0\x01\x05\x01\x60\x00\x01\x7b";
        for payload in [two_memories, simd_type] {
            let bytes = package("wasm32-wasip1", payload, None);
            let result = prepare(&bytes, "xila.clock", "1.0.0", sha256(&bytes), None);
            assert!(result.is_err(), "{payload:?}");
        }
    }

    #[test]
    fn verifies_the_publisher_signature_when_requested() {
        let signer = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let key = |signer: &SigningKey| {
            PublicKey::from_bytes(&signer.verifying_key().to_bytes()).unwrap()
        };
        let signed = package("wasm32-wasip1", WASM, Some(&signer));
        let digest = sha256(&signed);
        let files = prepare(&signed, "xila.clock", "1.0.0", digest, Some(&key(&signer))).unwrap();
        assert!(files.iter().any(|file| file.path == ".xila/manifest.sig"));
        assert!(prepare(&signed, "xila.clock", "1.0.0", digest, Some(&key(&other))).is_err());
        let unsigned = package("wasm32-wasip1", WASM, None);
        assert!(
            prepare(
                &unsigned,
                "xila.clock",
                "1.0.0",
                sha256(&unsigned),
                Some(&key(&signer))
            )
            .is_err()
        );
    }

    #[test]
    fn verifies_identity_target_xila_version_and_integrity_before_extracting() {
        let bytes = package("wasm32-wasip1", WASM, None);
        let expected = sha256(&bytes);
        assert!(prepare(&bytes, "xila.clock", "1.0.0", expected, None).is_ok());
        assert!(prepare(&bytes, "xila.other", "1.0.0", expected, None).is_err());
        assert!(prepare(&bytes, "xila.clock", "2.0.0", expected, None).is_err());
        for incompatible in [
            XilaVersion::new(
                RUNNING_XILA_VERSION.major,
                RUNNING_XILA_VERSION.minor.wrapping_add(1),
            ),
            XilaVersion::new(
                RUNNING_XILA_VERSION.major.wrapping_add(1),
                RUNNING_XILA_VERSION.minor,
            ),
        ] {
            let future = package_for_xila("wasm32-wasip1", incompatible, WASM, None);
            assert_eq!(
                prepare(&future, "xila.clock", "1.0.0", sha256(&future), None).err(),
                Some("package requires a Xila version incompatible with this runtime")
            );
        }
        let mut corrupt = bytes.clone();
        let index = corrupt.windows(4).position(|w| w == b"\0asm").unwrap();
        corrupt[index] ^= 1;
        assert!(prepare(&corrupt, "xila.clock", "1.0.0", expected, None).is_err());
        assert!(prepare(&corrupt, "xila.clock", "1.0.0", sha256(&corrupt), None).is_err());
        for package in [
            package("wasm32-wasip1", b"abc", None),
            package("wasm32-unknown-unknown", WASM, None),
        ] {
            assert!(prepare(&package, "xila.clock", "1.0.0", sha256(&package), None).is_err());
        }
    }
}
