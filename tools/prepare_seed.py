#!/usr/bin/env python3
"""Prepare a seed profile from explicitly pinned, local .xpk archives."""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
SAFE = re.compile(r"^[a-z][a-z0-9_]*$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def prepare(profile: str, lockfile: Path, output: Path, tool: Path, max_abi: int) -> None:
    if profile not in ("native", "browser"):
        raise ValueError("profile must be native or browser")
    names = (ROOT / "tools/fs_image/profiles" / f"{profile}.txt").read_text().splitlines()
    if len(names) != len(set(names)) or any(not SAFE.fullmatch(name) for name in names):
        raise ValueError("invalid seed profile")
    applications = tomllib.loads(lockfile.read_text())["applications"]
    if not output.parent.is_dir():
        raise ValueError("output parent directory does not exist")
    with tempfile.TemporaryDirectory(prefix=".xila-seed-artifacts-", dir=output.parent) as stage:
        stage = Path(stage)
        for name in names:
            item = applications[name]
            if item["name"] != "xila." + name.replace("_", "-"):
                raise ValueError(f"incorrect package identity for {name}")
            if not SHA256.fullmatch(item["sha256"]):
                raise ValueError(f"invalid pinned checksum for {name}")
            archive = (lockfile.parent / item["archive"]).resolve(strict=True)
            subprocess.run([str(tool.resolve(strict=True)), str(archive), item["sha256"], item["name"], item["version"], str(max_abi), str(stage / f"{name}.wasm")], check=True)
        # Creating the output directory first refuses to replace any existing files.
        output.mkdir()
        try:
            for name in names:
                (stage / f"{name}.wasm").rename(output / f"{name}.wasm")
        except Exception:
            shutil.rmtree(output)
            raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("native", "browser"), required=True)
    parser.add_argument("--lockfile", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--tool", type=Path, required=True)
    parser.add_argument("--max-abi", type=int, required=True)
    args = parser.parse_args()
    if args.max_abi < 0:
        parser.error("--max-abi must be nonnegative")
    prepare(args.profile, args.lockfile, args.output, args.tool, args.max_abi)


if __name__ == "__main__":
    main()
