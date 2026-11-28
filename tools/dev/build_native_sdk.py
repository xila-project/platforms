"""Build the native SDK from the Core revision in Cargo.lock."""

import fcntl
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib


def build(rust_version, target):
    cargo_target = "xtensa-esp32s3-none-elf" if target == "xtensa-esp32s3-elf" else target
    metadata = json.loads(subprocess.check_output([
        "rustup", "run", rust_version, "cargo", "metadata", "--locked",
        "--format-version", "1", "--filter-platform", cargo_target,
    ]))
    [abi] = [package for package in metadata["packages"] if package["name"] == "xila-abi-application"]
    core = Path(abi["manifest_path"]).parents[3]
    revision = subprocess.check_output(["git", "-C", core, "rev-parse", "HEAD"], text=True).strip()
    directory = Path.cwd() / "target" / "native-sdk" / revision
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / "core"

    # The SDK builder writes into its source tree. Keep that tree separate
    # from Cargo's shared checkout, and serialize builds that use it.
    with (directory / "lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        snapshot = directory / ".source-complete"
        if not snapshot.exists():
            shutil.copytree(
                core, source, dirs_exist_ok=True, ignore=shutil.ignore_patterns(".git", "target"),
            )
            snapshot.touch()
        workspace = tomllib.loads((source / "Cargo.toml").read_text())["workspace"]
        major, minor, *_ = workspace["package"]["version"].split(".")
        sdk = source / "target" / "sdk" / target / f"xila-sdk-{major}.{minor}-{target}"
        stamp = sdk / ".build-complete"
        if not stamp.exists():
            pins = tomllib.loads((source / "mise.toml").read_text())["vars"]
            subprocess.run([
                "uv", "run", "--no-project", "--with", f"meson=={pins['meson_version']}",
                "--with", f"ninja=={pins['ninja_version']}", "python",
                str(source / "tools/sdk/build.py"), "build", "--target", target,
            ], check=True, stdout=sys.stderr)
            stamp.touch()
    print(sdk)


if __name__ == "__main__":
    build(*sys.argv[1:])
