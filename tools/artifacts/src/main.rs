use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

fn digest(hex: &str) -> Result<[u8; 32], &'static str> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("checksum must be 64 lowercase hexadecimal characters");
    }
    let mut result = [0; 32];
    for (byte, pair) in result.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
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

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() != 6 {
        return Err(
            "usage: xila-prepare-applications ARCHIVE SHA256 NAME VERSION MAX_ABI OUTPUT.wasm"
                .into(),
        );
    }
    let archive = fs::read(&args[0])?;
    let payload = xila_prepare_applications::prepare(
        &archive,
        &args[2],
        &args[3],
        args[4].parse()?,
        digest(&args[1])?,
    )?;
    let path = Path::new(&args[5]);
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = output.write_all(&payload) {
        drop(output);
        let _ = fs::remove_file(path);
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
