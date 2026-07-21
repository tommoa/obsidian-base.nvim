#!/usr/bin/env python3
"""Ensure the worker, Nix package, and release tag share a version."""

import argparse
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parent.parent


def required_match(path: Path, pattern: str) -> str:
    match = re.search(pattern, path.read_text(), re.MULTILINE)
    if not match:
        raise SystemExit(f"could not determine version from {path.relative_to(ROOT)}")
    return match.group(1)


cargo_version = required_match(ROOT / "worker" / "Cargo.toml", r'^version = "([^"]+)"$')
flake_version = required_match(
    ROOT / "flake.nix",
    r'pname = "obsidian-base-worker";\s+version = "([^"]+)";',
)
parser = argparse.ArgumentParser()
parser.add_argument("--tag")
args = parser.parse_args()

if cargo_version != flake_version:
    raise SystemExit(
        f"Cargo and Nix worker versions differ: {cargo_version} != {flake_version}"
    )
if args.tag is not None and args.tag != f"v{cargo_version}":
    raise SystemExit(f"release tag differs: {args.tag} != v{cargo_version}")

print(f"obsidian-base metadata versions match: v{cargo_version}")
