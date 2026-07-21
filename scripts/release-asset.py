#!/usr/bin/env python3
"""Stage and smoke-test a release worker asset."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parent.parent
REQUESTS = (
    '{"id":1,"request":{"method":"initialize","params":{"vault_root":"."}}}\n'
    '{"id":2,"request":{"method":"inspect","params":{}}}\n'
    '{"id":3,"request":{"method":"shutdown","params":{}}}\n'
)


def fail(message: str) -> None:
    raise SystemExit(message)


def stage_and_smoke_test(target: str, asset_name: str) -> Path:
    suffix = ".exe" if os.name == "nt" else ""
    source = (
        ROOT
        / "worker"
        / "target"
        / target
        / "release"
        / f"obsidian-base-worker{suffix}"
    )
    if not source.is_file():
        fail(f"built worker is missing: {source}")
    asset = ROOT / asset_name
    shutil.copy2(source, asset)
    process = subprocess.run(
        [str(asset.resolve())], input=REQUESTS, text=True, capture_output=True
    )
    if process.returncode != 0:
        fail(
            f"worker smoke test failed ({process.returncode}): {process.stderr.strip()}"
        )
    lines = []
    try:
        lines = [json.loads(line) for line in process.stdout.splitlines()]
    except json.JSONDecodeError as error:
        fail(f"worker smoke test emitted invalid JSON: {error}")
    for request_id in (1, 2):
        if not any(
            line.get("id") == request_id
            and line.get("response", {}).get("type") == "success"
            for line in lines
        ):
            fail(f"worker smoke test did not acknowledge request {request_id}: {lines}")
    return asset


def main() -> None:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    subparser = commands.add_parser("stage")
    subparser.add_argument("--target", required=True)
    subparser.add_argument("--asset", required=True)
    args = parser.parse_args()

    stage_and_smoke_test(args.target, args.asset)


if __name__ == "__main__":
    main()
