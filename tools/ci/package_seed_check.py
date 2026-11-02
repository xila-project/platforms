#!/usr/bin/env python3
"""Exercise the package-manager-to-seed pipeline with already built guests.

Packages each guest with the reusable workflow's packager, extracts the
archives through prepare_seed.py, and checks that the resulting images equal
those built from locally bundled guests (tools/bundle_applications.sh).
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
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
        lock = {"applications": {}}
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
            lock["applications"][app] = {"name": package, "version": "0.1.0",
                                         "archive": f"packages/{filename}", "sha256": checksum}
        lockfile = root / "bundles.lock.json"
        lockfile.write_text(json.dumps(lock))
        direct = root / "direct-bundles"
        subprocess.run(["bash", str(PLATFORMS / "tools/bundle_applications.sh"), str(args.applications),
                        str(args.wasm_dir), str(direct)], check=True,
                       env=dict(os.environ, XILA_PACKAGE=str(args.builder)))
        for profile, bundle in (("native", "native"), ("browser", "wasm")):
            packaged = root / f"{profile}-bundles"
            subprocess.run(["python3", str(PLATFORMS / "tools/prepare_seed.py"), "--profile", profile,
                            "--lockfile", str(lockfile), "--output", str(packaged), "--tool", str(args.preparer),
                            "--max-abi", "1"], check=True)
            for suffix, bundles in (("packaged", packaged), ("direct", direct)):
                subprocess.run([str(args.fs_image), str(root / f"{profile}-{suffix}.img"), "16777216",
                                "--bundle", bundle, "--bundles-dir", str(bundles)], check=True)
            if (root / f"{profile}-packaged.img").read_bytes() != (root / f"{profile}-direct.img").read_bytes():
                raise SystemExit(f"{profile}: packaged and locally bundled seed images differ")
        bad = json.loads(lockfile.read_text())
        digest = bad["applications"]["calculator"]["sha256"]
        bad["applications"]["calculator"]["sha256"] = ("0" if digest[0] != "0" else "1") + digest[1:]
        (root / "bad.json").write_text(json.dumps(bad))
        result = subprocess.run(["python3", str(PLATFORMS / "tools/prepare_seed.py"), "--profile", "native",
                                 "--lockfile", str(root / "bad.json"), "--output", str(root / "invalid"),
                                 "--tool", str(args.preparer), "--max-abi", "1"])
        if result.returncode == 0 or (root / "invalid").exists():
            raise SystemExit("invalid checksum was not rejected cleanly")


if __name__ == "__main__":
    main()
