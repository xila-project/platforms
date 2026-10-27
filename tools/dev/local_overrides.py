#!/usr/bin/env python3
"""Write Cargo source patches for adjacent Core and Platforms checkouts."""
import argparse
import json
from pathlib import Path
import tomllib


def packages(root, roots):
    for relative in roots:
        base = root / relative
        for manifest in sorted(base.rglob("Cargo.toml")):
            if any(part in {"target", "tests", "node_modules"} for part in manifest.relative_to(root).parts):
                continue
            if relative == "libraries" and manifest.parent.name == "testing":
                continue
            package = tomllib.loads(manifest.read_text()).get("package")
            if package:
                yield package["name"], manifest.parent.resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--platforms", type=Path, required=True)
    parser.add_argument("--applications", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    sections = {
        "https://github.com/xila-project/core": (args.core, ["core", "libraries", "subsystems", "drivers/core", "executables/wasm"]),
        "https://github.com/xila-project/platforms": (args.platforms, ["drivers", "libraries/testing"]),
        "https://github.com/xila-project/applications": (args.applications, ["executables"]),
    }
    lines = []
    for url, (root, roots) in sections.items():
        lines.append(f"[patch.{json.dumps(url)}]")
        seen = set()
        for name, path in packages(root.resolve(), roots):
            if name in seen:
                raise ValueError(f"duplicate local package {name}")
            seen.add(name)
            lines.append(f"{json.dumps(name)} = {{ path = {json.dumps(str(path))} }}")
        lines.append("")
    args.output.write_text("\n".join(lines))


if __name__ == "__main__":
    main()
