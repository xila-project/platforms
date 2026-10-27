use tar_no_std::TarArchiveRef;
use xila_package_verification::ContentVerifier;

/// Validate an unsigned development archive against a pinned digest and identity.
/// Integrity does not authenticate a publisher.
pub fn prepare(
    bytes: &[u8],
    name: &str,
    version: &str,
    max_abi: u32,
    digest: [u8; 32],
) -> Result<Vec<u8>, &'static str> {
    if max_abi > abi_declarations::APPLICATION_ABI_VERSION {
        return Err("requested ABI exceeds this runtime version");
    }
    let mut verifier =
        ContentVerifier::new(bytes.len() as u64, digest).map_err(|_| "invalid size")?;
    verifier.update(bytes).map_err(|_| "invalid archive size")?;
    verifier.verify().map_err(|_| "archive checksum mismatch")?;
    let manifest = xila_package_reader::inspect_unsigned(bytes).map_err(|_| "invalid package")?;
    if manifest.name != name
        || manifest.version != version
        || manifest.target != "wasm32-wasip1"
        || manifest.minimum_xila_abi > max_abi
        || manifest.files.len() != 1
        || !manifest.files[0].path.ends_with(".wasm")
    {
        return Err("package is incompatible");
    }
    let archive = TarArchiveRef::new(bytes).map_err(|_| "invalid archive")?;
    let path = &manifest.files[0].path;
    archive
        .entries()
        .find_map(|entry| {
            (entry.filename().as_str().ok() == Some(path.as_str())).then(|| entry.data().to_vec())
        })
        .ok_or("missing payload")
}

#[cfg(test)]
mod tests {
    use super::prepare;
    use xila_package_manifest::{FORMAT_VERSION, Manifest, ManifestFile};
    use xila_package_writer::{SourceFile, write_package};

    fn package(target: &str) -> Vec<u8> {
        let manifest = Manifest {
            format_version: FORMAT_VERSION,
            name: "xila.clock".into(),
            version: "1.0.0".into(),
            target: target.into(),
            minimum_xila_abi: 1,
            capabilities: vec![],
            files: vec![ManifestFile {
                path: "bin/clock.wasm".into(),
                size: 3,
                sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            }],
        };
        let mut archive = Vec::new();
        write_package(
            &mut archive,
            &manifest,
            &[SourceFile {
                path: "bin/clock.wasm",
                bytes: b"abc",
            }],
            None,
        )
        .unwrap();
        archive
    }

    #[test]
    fn verifies_identity_target_abi_and_integrity_before_extracting() {
        let bytes = package("wasm32-wasip1");
        let mut hash = xila_package_verification::Sha256Verifier::new();
        hash.update(&bytes).unwrap();
        let expected = hash.finalize().unwrap();
        assert_eq!(
            prepare(&bytes, "xila.clock", "1.0.0", 1, expected).unwrap(),
            b"abc"
        );
        assert!(prepare(&bytes, "xila.other", "1.0.0", 1, expected).is_err());
        assert!(prepare(&bytes, "xila.clock", "2.0.0", 1, expected).is_err());
        assert!(prepare(&bytes, "xila.clock", "1.0.0", 0, expected).is_err());
        assert!(prepare(&bytes, "xila.clock", "1.0.0", 2, expected).is_err());
        let mut corrupt = bytes.clone();
        let index = corrupt.windows(3).position(|w| w == b"abc").unwrap();
        corrupt[index] ^= 1;
        assert!(prepare(&corrupt, "xila.clock", "1.0.0", 1, expected).is_err());
        let mut hash = xila_package_verification::Sha256Verifier::new();
        hash.update(&corrupt).unwrap();
        assert!(prepare(&corrupt, "xila.clock", "1.0.0", 1, hash.finalize().unwrap()).is_err());
        let wrong_target = package("wasm32-unknown-unknown");
        let mut hash = xila_package_verification::Sha256Verifier::new();
        hash.update(&wrong_target).unwrap();
        assert!(
            prepare(
                &wrong_target,
                "xila.clock",
                "1.0.0",
                1,
                hash.finalize().unwrap()
            )
            .is_err()
        );
    }
}
