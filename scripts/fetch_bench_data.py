#!/usr/bin/env python3
"""Fetch optional benchmark datasets.

The default image benchmark is a lightweight copy of
`bids-standard/bids-examples/pet002` with tiny valid NIfTI files written
over any empty `.nii` or `.nii.gz` placeholders. This keeps the standard
benchmark small while preserving realistic file and header checks.

`ds005016` is the default DataLad/OpenNeuro benchmark. By default it
clones only the repository and leaves annexed image contents as
git-annex links. Use `--get-all` only when a full local image-content
benchmark is intentionally required.
"""

from __future__ import annotations

import argparse
import gzip
import json
import shutil
import struct
import subprocess
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIDS_EXAMPLES_REF = "master"
BIDS_EXAMPLES_URL = "https://github.com/bids-standard/bids-examples/archive/{ref}.tar.gz"
# pet002 is the image benchmark; the electrophysiology datasets each carry
# a coordsystem.json and exercise the sidecar-association ("viewed") walk
# that the parity suite otherwise never touches (no MEG/EEG/iEEG corpus).
BIDS_EXAMPLES_DATASETS = {
    "pet002",
    "eeg_face13",
    "ieeg_epilepsy",
    "ds114",
    "emg_Multimodal",
    "fnirs_tapping",
    "eeg_rest_fmri",
    "asl001",
    "pet003",
}
# Datasets whose NIfTI placeholders get rewritten to tiny valid headers
# (and fetched to data/<name>-tiny). Only image datasets need this.
BIDS_EXAMPLES_TINY = {"pet002"}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "dataset",
        nargs="?",
        default="pet002",
        help="benchmark dataset: pet002 or an OpenNeuro/DataLad dataset id such as ds005016",
    )
    parser.add_argument("--dest", type=Path, default=None)
    parser.add_argument("--force", action="store_true", help="replace an existing destination")
    parser.add_argument(
        "--bids-examples-ref",
        default=BIDS_EXAMPLES_REF,
        help="bids-examples branch, tag, or commit used for pet002",
    )
    parser.add_argument(
        "--no-tiny-nifti",
        action="store_true",
        help="leave bids-examples NIfTI placeholders exactly as fetched",
    )
    parser.add_argument(
        "--get-all",
        action="store_true",
        help="for DataLad datasets only: run `datalad get -r` after install",
    )
    args = parser.parse_args()

    if args.dataset in BIDS_EXAMPLES_DATASETS:
        # The tiny-NIfTI rewrite (and its `-tiny` dest suffix) only applies
        # to image datasets with NIfTI placeholders. The electrophysiology
        # parity datasets have none, so they fetch verbatim to data/<name>
        # — the path tests/parity/run.py expects.
        tiny_nifti = (args.dataset in BIDS_EXAMPLES_TINY) and not args.no_tiny_nifti
        dest_name = f"{args.dataset}-tiny" if tiny_nifti else args.dataset
        dest = args.dest or ROOT / "data" / dest_name
        fetch_bids_examples_dataset(
            args.dataset,
            dest,
            args.bids_examples_ref,
            tiny_nifti=tiny_nifti,
            force=args.force,
        )
        return 0

    dest = args.dest or ROOT / "data" / args.dataset
    fetch_datalad_dataset(args.dataset, dest, get_all=args.get_all, force=args.force)
    return 0


def fetch_bids_examples_dataset(dataset: str, dest: Path, ref: str, *, tiny_nifti: bool, force: bool) -> None:
    dest = resolve_dest(dest)
    if dest.exists():
        if not force:
            print(f"dataset already present: {dest}")
            return
        shutil.rmtree(dest)

    archive = download_bids_examples_archive(ref)
    with tempfile.TemporaryDirectory(prefix="bids-examples-") as tmpdir:
        tmp = Path(tmpdir)
        with tarfile.open(archive, "r:gz") as tar:
            safe_extract(tar, tmp)
        roots = [p for p in tmp.iterdir() if p.is_dir()]
        if len(roots) != 1:
            raise SystemExit(f"expected one archive root, got {roots}")
        source = roots[0] / dataset
        if not source.is_dir():
            raise SystemExit(f"archive does not contain {dataset}: {archive}")
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(source, dest)

    rewritten = rewrite_nifti_placeholders(dest) if tiny_nifti else 0
    manifest = {
        "dataset": dataset,
        "source": "bids-standard/bids-examples",
        "ref": ref,
        "path": str(dest.relative_to(ROOT) if dest.is_relative_to(ROOT) else dest),
        "tiny_nifti": tiny_nifti,
        "rewritten_nifti_files": rewritten,
    }
    manifest_path = dest.parent / f"{dest.name}.benchmark.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {dest}")
    if tiny_nifti:
        print(f"rewrote {rewritten} NIfTI placeholder(s) with tiny valid headers")


def fetch_datalad_dataset(dataset: str, dest: Path, *, get_all: bool, force: bool) -> None:
    dest = resolve_dest(dest)
    if dest.exists():
        if not force:
            print(f"dataset already present: {dest}")
            return
        else:
            shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    run(["datalad", "clone", f"https://github.com/OpenNeuroDatasets/{dataset}", str(dest)])
    if get_all:
        print("warning: --get-all may download a large amount of image data")
        run(["datalad", "get", "-r", str(dest)])


def resolve_dest(dest: Path) -> Path:
    dest = dest if dest.is_absolute() else ROOT / dest
    return dest


def download_bids_examples_archive(ref: str) -> Path:
    cache = ROOT / ".cache" / "bench"
    cache.mkdir(parents=True, exist_ok=True)
    safe_ref = ref.replace("/", "_")
    archive = cache / f"bids-examples-{safe_ref}.tar.gz"
    if not archive.exists():
        url = BIDS_EXAMPLES_URL.format(ref=ref)
        print(f"downloading {url}")
        with urllib.request.urlopen(url) as response, archive.open("wb") as out:
            shutil.copyfileobj(response, out)
    return archive


def safe_extract(tar: tarfile.TarFile, dest: Path) -> None:
    dest = dest.resolve()
    for member in tar.getmembers():
        target = (dest / member.name).resolve()
        if target != dest and dest not in target.parents:
            raise SystemExit(f"unsafe archive member: {member.name}")
    tar.extractall(dest)


def rewrite_nifti_placeholders(dataset: Path) -> int:
    nifti = tiny_nifti_bytes()
    rewritten = 0
    for path in dataset.rglob("*"):
        if not path.is_file():
            continue
        name = path.name.lower()
        if name.endswith(".nii.gz"):
            path.write_bytes(gzip.compress(nifti, compresslevel=9, mtime=0))
            rewritten += 1
        elif name.endswith(".nii"):
            path.write_bytes(nifti)
            rewritten += 1
    return rewritten


def tiny_nifti_bytes() -> bytes:
    header = bytearray(348)
    struct.pack_into("<i", header, 0, 348)
    struct.pack_into("<8h", header, 40, 3, 2, 2, 2, 1, 1, 1, 1)
    struct.pack_into("<h", header, 70, 2)  # uint8
    struct.pack_into("<h", header, 72, 8)
    struct.pack_into("<8f", header, 76, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0)
    struct.pack_into("<f", header, 108, 352.0)
    struct.pack_into("<f", header, 112, 1.0)
    header[123] = 10  # millimeters and seconds
    struct.pack_into("<h", header, 252, 1)
    struct.pack_into("<h", header, 254, 1)
    header[344:348] = b"n+1\0"
    return bytes(header) + b"\0\0\0\0" + (b"\0" * 8)


def run(cmd: list[str]) -> None:
    print("$ " + " ".join(cmd))
    subprocess.run(cmd, check=True)


if __name__ == "__main__":
    raise SystemExit(main())
