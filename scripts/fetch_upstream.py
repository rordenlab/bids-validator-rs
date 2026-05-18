#!/usr/bin/env python3
"""Fetch the pinned upstream BIDS Validator source release.

The Rust repository intentionally does not vendor the full TypeScript
validator. This script downloads the pinned release source into
`vendor/bids-validator-2.4.1` so Deno parity tests and
oracle exporters can use the exact upstream rules/tests they target.
"""

from __future__ import annotations

import argparse
import json
import shutil
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION = "2.4.1"
COMMIT = "94ea9719efbbac7531fed8ad8a2a7bf764cee51f"
ARCHIVE_URL = f"https://github.com/bids-standard/bids-validator/archive/{COMMIT}.tar.gz"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", default=VERSION)
    parser.add_argument("--commit", default=COMMIT)
    parser.add_argument("--url", default=ARCHIVE_URL)
    parser.add_argument("--dest", type=Path, default=ROOT / "vendor" / f"bids-validator-{VERSION}")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    dest = args.dest if args.dest.is_absolute() else ROOT / args.dest
    if dest.exists():
        if not args.force:
            print(f"upstream already present: {dest}")
            return 0
        shutil.rmtree(dest)

    cache = ROOT / ".cache" / "upstream"
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / f"bids-validator-{args.version}-{args.commit[:12]}.tar.gz"
    if not archive.exists():
        print(f"downloading {args.url}")
        with urllib.request.urlopen(args.url) as response, archive.open("wb") as out:
            shutil.copyfileobj(response, out)

    with tempfile.TemporaryDirectory(prefix="bids-validator-upstream-") as tmpdir:
        tmp = Path(tmpdir)
        with tarfile.open(archive, "r:gz") as tar:
            safe_extract(tar, tmp)
        roots = [p for p in tmp.iterdir() if p.is_dir()]
        if len(roots) != 1:
            raise SystemExit(f"expected one archive root, got {roots}")
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(roots[0]), dest)

    manifest = {
        "bids_validator_version": args.version,
        "bids_validator_commit": args.commit,
        "archive_url": args.url,
        "vendor_dir": str(dest.relative_to(ROOT) if dest.is_relative_to(ROOT) else dest),
    }
    (ROOT / "upstream.lock.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {dest}")
    return 0


def safe_extract(tar: tarfile.TarFile, dest: Path) -> None:
    dest = dest.resolve()
    for member in tar.getmembers():
        target = (dest / member.name).resolve()
        if target != dest and dest not in target.parents:
            raise SystemExit(f"unsafe archive member: {member.name}")
    tar.extractall(dest)


if __name__ == "__main__":
    raise SystemExit(main())
