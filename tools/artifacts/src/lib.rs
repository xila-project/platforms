use tar_no_std::TarArchiveRef;
use xila_package_manifest::{MANIFEST_PATH, SIGNATURE_PATH};
use xila_package_verification::{ContentVerifier, PublicKey};

/// One file of an extracted bundle, at its bundle-relative path.
pub struct BundleFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// Verify a released archive against a pinned digest, identity and ABI (and
/// its publisher signature when `publisher` is given), then return every file
/// of the bundle it contains: `.xila/manifest.json`, `.xila/manifest.sig` when
/// signed, and each declared payload. Extracting them yields a ready bundle.
pub fn prepare(
    bytes: &[u8],
    name: &str,
    version: &str,
    max_abi: u32,
    digest: [u8; 32],
    publisher: Option<&PublicKey>,
) -> Result<Vec<BundleFile>, &'static str> {
    if max_abi > abi_declarations::APPLICATION_ABI_VERSION {
        return Err("requested ABI exceeds this runtime version");
    }
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
    if manifest.name != name
        || manifest.version != version
        || manifest.target != "wasm32-wasip1"
        || manifest.minimum_xila_abi > max_abi
    {
        return Err("package is incompatible");
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
    let mut features = wasmparser::WasmFeatures::default();
    features.set(wasmparser::WasmFeatures::MEMORY64, false);
    wasmparser::Validator::new_with_features(features)
        .validate_all(payload)
        .map_err(|_| "invalid or incompatible Wasm module")?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::prepare;
    use ed25519_dalek::{Signer, SigningKey};
    use xila_package_manifest::{Application, FORMAT_VERSION, Manifest, ManifestFile, SCHEMA_URL};
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

    fn manifest(target: &str, payload: &[u8]) -> Manifest {
        Manifest {
            schema: Some(SCHEMA_URL.into()),
            format_version: FORMAT_VERSION,
            name: "xila.clock".into(),
            version: "1.0.0".into(),
            target: target.into(),
            minimum_xila_abi: 1,
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
        }
    }

    fn package(target: &str, payload: &[u8], signer: Option<&SigningKey>) -> Vec<u8> {
        let manifest = manifest(target, payload);
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

    #[test]
    fn extracts_the_complete_bundle() {
        let bytes = package("wasm32-wasip1", WASM, None);
        let files = prepare(&bytes, "xila.clock", "1.0.0", 1, sha256(&bytes), None).unwrap();
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
    fn verifies_the_publisher_signature_when_requested() {
        let signer = SigningKey::from_bytes(&[7; 32]);
        let other = SigningKey::from_bytes(&[8; 32]);
        let key = |signer: &SigningKey| {
            PublicKey::from_bytes(&signer.verifying_key().to_bytes()).unwrap()
        };
        let signed = package("wasm32-wasip1", WASM, Some(&signer));
        let digest = sha256(&signed);
        let files = prepare(
            &signed,
            "xila.clock",
            "1.0.0",
            1,
            digest,
            Some(&key(&signer)),
        )
        .unwrap();
        assert!(files.iter().any(|file| file.path == ".xila/manifest.sig"));
        assert!(
            prepare(
                &signed,
                "xila.clock",
                "1.0.0",
                1,
                digest,
                Some(&key(&other))
            )
            .is_err()
        );
        let unsigned = package("wasm32-wasip1", WASM, None);
        assert!(
            prepare(
                &unsigned,
                "xila.clock",
                "1.0.0",
                1,
                sha256(&unsigned),
                Some(&key(&signer))
            )
            .is_err()
        );
    }

    #[test]
    fn verifies_identity_target_abi_and_integrity_before_extracting() {
        let bytes = package("wasm32-wasip1", WASM, None);
        let expected = sha256(&bytes);
        assert!(prepare(&bytes, "xila.clock", "1.0.0", 1, expected, None).is_ok());
        assert!(prepare(&bytes, "xila.other", "1.0.0", 1, expected, None).is_err());
        assert!(prepare(&bytes, "xila.clock", "2.0.0", 1, expected, None).is_err());
        assert!(prepare(&bytes, "xila.clock", "1.0.0", 0, expected, None).is_err());
        assert!(prepare(&bytes, "xila.clock", "1.0.0", 2, expected, None).is_err());
        let mut corrupt = bytes.clone();
        let index = corrupt.windows(4).position(|w| w == b"\0asm").unwrap();
        corrupt[index] ^= 1;
        assert!(prepare(&corrupt, "xila.clock", "1.0.0", 1, expected, None).is_err());
        assert!(prepare(&corrupt, "xila.clock", "1.0.0", 1, sha256(&corrupt), None).is_err());
        for package in [
            package("wasm32-wasip1", b"abc", None),
            package("wasm32-unknown-unknown", WASM, None),
        ] {
            assert!(prepare(&package, "xila.clock", "1.0.0", 1, sha256(&package), None).is_err());
        }
    }
}
