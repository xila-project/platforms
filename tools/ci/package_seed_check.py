#!/usr/bin/env python3
"""Exercise the package-manager-to-seed pipeline with already built guests."""
import argparse
from pathlib import Path
import subprocess
import re
import tempfile

PLATFORMS = Path(__file__).resolve().parents[2]
APPS = ("calculator", "clock", "pixel_painter", "synth", "text_editor", "weather")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("applications", "package-manager", "wasm-dir", "builder", "preparer", "fs-image"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="xila-package-seed-check-") as directory:
        root = Path(directory)
        lock = []
        for app in APPS:
            package = "xila." + app.replace("_", "-")
            filename = f"{package}-0.1.0-wasm32-wasip1.xpk"
            subprocess.run([
                "python3", str(args.package_manager / "tools/package_wasm.py"),
                "--wasm-root", str(args.wasm_dir), "--wasm-path", f"{app}.wasm",
                "--manifest-root", str(args.applications), "--manifest-path", f"packaging/{app}.json",
                "--shortcut-path", f"executables/{app}/shortcut.json",
                "--staging", str(root / f"{app}-stage"), "--manifest-output", str(root / f"{app}.json"),
                "--output", str(root / "packages"), "--builder", str(args.builder),
            ], check=True)
            checksum = (root / "packages" / f"{filename}.sha256").read_text().split()[0]
            lock.extend([f"[applications.{app}]", f'archive = "packages/{filename}"',
                         f'name = "{package}"', 'version = "0.1.0"', f'sha256 = "{checksum}"', ""])
        lockfile = root / "versions.toml"
        lockfile.write_text("\n".join(lock))
        for profile, bundle in (("native", "native"), ("browser", "wasm")):
            prepared = root / f"{profile}-guests"
            subprocess.run(["python3", str(PLATFORMS / "tools/prepare_seed.py"), "--profile", profile,
                            "--lockfile", str(lockfile), "--output", str(prepared), "--tool", str(args.preparer),
                            "--max-abi", "1"], check=True)
            for suffix, app_dir in (("packaged", prepared), ("direct", args.wasm_dir)):
                subprocess.run([str(args.fs_image), str(root / f"{profile}-{suffix}.img"), "16777216",
                                "--bundle", bundle, "--applications-dir", str(app_dir)], check=True)
            if (root / f"{profile}-packaged.img").read_bytes() != (root / f"{profile}-direct.img").read_bytes():
                raise SystemExit(f"{profile}: packaged and direct seed images differ")
        bad = re.sub(r'(sha256 = ")([0-9a-f])',
                     lambda match: match.group(1) + ("0" if match.group(2) != "0" else "1"),
                     lockfile.read_text(), count=1)
        (root / "bad.toml").write_text(bad)
        result = subprocess.run(["python3", str(PLATFORMS / "tools/prepare_seed.py"), "--profile", "native",
                                 "--lockfile", str(root / "bad.toml"), "--output", str(root / "invalid"),
                                 "--tool", str(args.preparer), "--max-abi", "1"])
        if result.returncode == 0 or (root / "invalid").exists():
            raise SystemExit("invalid checksum was not rejected cleanly")


if __name__ == "__main__":
    main()
