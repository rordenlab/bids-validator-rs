#!/usr/bin/env python3
"""Smoke test a Rust beta bids-validator binary.

The script intentionally accepts validator exit codes 0 and 1: datasets
with validation issues exit nonzero, but still produce the JSON payload
beta testers need to compare and report.
"""

import argparse
import hashlib
import json
import shlex
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BIN = ROOT / "target/release/bids-validator"
SMALL_DATASET = ROOT / "vendor/bids-validator-2.4.1/tests/data/valid_headers"
DEFAULT_EXTERNAL = Path("/Users/chris/src/bidsui/datasets/ds002606")

VERSION_FIELDS = [
    "git commit",
    "beta label",
    "schema source",
    "@bids/schema",
    "bids version",
    "content mode",
    "link mode",
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bin",
        default=DEFAULT_BIN,
        type=Path,
        help="bids-validator binary to test",
    )
    parser.add_argument(
        "--build",
        action="store_true",
        help="build target/release/bids-validator before testing",
    )
    parser.add_argument(
        "--external-dataset",
        default=DEFAULT_EXTERNAL,
        type=Path,
        help="optional larger dataset to test if it exists",
    )
    parser.add_argument(
        "--skip-external",
        action="store_true",
        help="skip the optional external dataset even if it exists",
    )
    args = parser.parse_args()

    if args.build:
        run_checked(
            [
                "cargo",
                "build",
                "--release",
                "--locked",
                "--manifest-path",
                str(ROOT / "Cargo.toml"),
            ]
        )

    binary = args.bin
    if not binary.exists():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    version = run_checked([str(binary), "--version"])
    missing = [field for field in VERSION_FIELDS if field not in version.stdout]
    if missing:
        print(f"--version missing fields: {', '.join(missing)}", file=sys.stderr)
        print(version.stdout, file=sys.stderr)
        return 2
    print(version.stdout.strip())

    smoke_dataset("small", binary, SMALL_DATASET)
    if not args.skip_external and args.external_dataset.exists():
        smoke_dataset("external", binary, args.external_dataset)
    elif not args.skip_external:
        print(f"external: skipped, dataset not found: {args.external_dataset}")

    return 0


def smoke_dataset(label: str, binary: Path, dataset: Path) -> None:
    result = run_checked(
        [
            str(binary),
            "--content-mode",
            "parity",
            "--link-mode",
            "parity",
            str(dataset),
        ],
        allow_validation_exit=True,
    )
    payload = json.loads(result.stdout)
    issues = payload["issues"]["issues"]
    digest = hashlib.sha256(result.stdout.encode("utf-8")).hexdigest()
    print(
        f"{label}: exit={result.returncode} issues={len(issues)} "
        f"sha256={digest} dataset={dataset}"
    )


def run_checked(cmd, allow_validation_exit: bool = False) -> subprocess.CompletedProcess:
    print("$ " + shlex.join(str(part) for part in cmd))
    result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    allowed = {0}
    if allow_validation_exit:
        allowed.add(1)
    if result.returncode not in allowed:
        if result.stdout:
            print(result.stdout[-2000:], file=sys.stderr)
        if result.stderr:
            print(result.stderr[-2000:], file=sys.stderr)
        raise SystemExit(result.returncode)
    return result


if __name__ == "__main__":
    raise SystemExit(main())
