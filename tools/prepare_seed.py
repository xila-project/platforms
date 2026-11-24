#!/usr/bin/env python3
"""Prepare a seed profile's bundles from pinned, released .xpk archives.

The JSON lockfile pins each application by identity, version and SHA-256, and
either an HTTPS `url` or a local `archive` path (relative to the lockfile).
Each archive is verified (digest, identity, Xila version compatibility and,
when the lockfile has a `publisher_key`, its publisher signature) and extracted
as a complete bundle to <output>/<package-id>/, ready for
`fs_image --bundles-dir`.
"""
import argparse
import base64
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
PROFILES = ("native", "browser", "wt32-sc01-plus", "wt32-sc01-plus-clock")
SAFE = re.compile(r"^[a-z][a-z0-9_]*$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def fetch(item: dict, lockfile: Path, destination: Path) -> Path:
    if "archive" in item:
        return (lockfile.parent / item["archive"]).resolve(strict=True)
    url = item["url"]
    if not url.startswith("https://"):
        raise ValueError(f"refusing non-HTTPS bundle URL: {url}")
    with urllib.request.urlopen(url, timeout=60) as response, destination.open("xb") as output:
        shutil.copyfileobj(response, output)
    return destination


def prepare(profile: str, lockfile: Path, output: Path, tool: Path) -> None:
    if profile not in PROFILES:
        raise ValueError(f"profile must be one of {', '.join(PROFILES)}")
    names = (ROOT / "tools/fs_image/profiles" / f"{profile}.txt").read_text().splitlines()
    names = [name for name in names if name]
    if len(names) != len(set(names)) or any(not SAFE.fullmatch(name) for name in names):
        raise ValueError("invalid seed profile")
    lock = json.loads(lockfile.read_text())
    applications = lock["applications"]
    if not output.parent.is_dir():
        raise ValueError("output parent directory does not exist")
    with tempfile.TemporaryDirectory(prefix=".xila-seed-bundles-", dir=output.parent) as stage:
        stage = Path(stage)
        public_key = []
        if "publisher_key" in lock:
            key = base64.b64decode(lock["publisher_key"], validate=True)
            if len(key) != 32:
                raise ValueError("publisher_key must encode 32 bytes")
            (stage / "publisher.key").write_bytes(key)
            public_key = [str(stage / "publisher.key")]
        for name in names:
            item = applications[name]
            identifier = "xila." + name.replace("_", "-")
            if item["name"] != identifier:
                raise ValueError(f"incorrect package identity for {name}")
            if not SHA256.fullmatch(item["sha256"]):
                raise ValueError(f"invalid pinned checksum for {name}")
            archive = fetch(item, lockfile, stage / f"{identifier}.xpk")
            subprocess.run([str(tool.resolve(strict=True)), str(archive), item["sha256"], item["name"],
                            item["version"], str(stage / identifier), *public_key], check=True)
        # Creating the output directory first refuses to replace any existing files.
        output.mkdir()
        try:
            for name in names:
                identifier = "xila." + name.replace("_", "-")
                (stage / identifier).rename(output / identifier)
        except Exception:
            shutil.rmtree(output)
            raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=PROFILES, required=True)
    parser.add_argument("--lockfile", type=Path, default=ROOT / "tools/fs_image/profiles/bundles.lock.json")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--tool", type=Path, required=True)
    args = parser.parse_args()
    try:
        prepare(args.profile, args.lockfile, args.output, args.tool)
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
