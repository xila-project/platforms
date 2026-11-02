use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};
use xila_package_verification::PublicKey;

const USAGE: &str =
    "usage: xila-prepare-applications ARCHIVE SHA256 NAME VERSION MAX_ABI OUTPUT_DIR [PUBLIC_KEY]";

fn digest(hex: &str) -> Result<[u8; 32], &'static str> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("checksum must be 64 lowercase hexadecimal characters");
    }
    let mut result = [0; 32];
    for (byte, pair) in result.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        let nibble = |c: u8| {
            if c.is_ascii_digit() {
                c - b'0'
            } else {
                c - b'a' + 10
            }
        };
        *byte = (nibble(pair[0]) << 4) | nibble(pair[1]);
    }
    Ok(result)
}

fn public_key(path: &str) -> Result<PublicKey, Box<dyn std::error::Error>> {
    let raw: [u8; 32] = fs::read(path)?
        .try_into()
        .map_err(|_| "public key must be exactly 32 raw bytes")?;
    Ok(PublicKey::from_bytes(&raw).map_err(|_| "invalid Ed25519 public key")?)
}

fn write_bundle(
    output: &Path,
    files: &[xila_prepare_applications::BundleFile],
) -> std::io::Result<()> {
    for file in files {
        // Paths were validated by the package reader; join component-wise.
        let path: PathBuf = file
            .path
            .split('/')
            .fold(output.to_path_buf(), |path, part| path.join(part));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?
            .write_all(&file.bytes)?;
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() != 6 && args.len() != 7 {
        return Err(USAGE.into());
    }
    let archive = fs::read(&args[0])?;
    let key = args.get(6).map(|path| public_key(path)).transpose()?;
    let files = xila_prepare_applications::prepare(
        &archive,
        &args[2],
        &args[3],
        args[4].parse()?,
        digest(&args[1])?,
        key.as_ref(),
    )?;
    // Creating the directory first refuses to replace an existing bundle.
    let output = Path::new(&args[5]);
    fs::create_dir(output)?;
    if let Err(error) = write_bundle(output, &files) {
        let _ = fs::remove_dir_all(output);
        return Err(error.into());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
