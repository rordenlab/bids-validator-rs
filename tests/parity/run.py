#!/usr/bin/env python3
"""Parity sweep with cached Deno output.

The Deno validator takes ~82 s on ds005016 — far too long to run every
iteration. This script caches the multiset of (code, location, subCode)
triples per dataset in `cache/<name>.deno.json` and compares the live
Rust output against the cache.

Cache is regenerated when:
  * `--refresh` is passed (force re-run Deno on every listed dataset).
  * The cache file is missing for a dataset.

Refresh after: (a) the Deno entrypoint changes, (b) the embedded
`@bids/schema` version bumps, (c) dataset content changes.

Usage:
  python3 tests/parity/run.py [dataset_name ...]
  python3 tests/parity/run.py --refresh
  python3 tests/parity/run.py --rust-schema schemas/schema-1.2.1.json
"""
import collections
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
UPSTREAM_ROOT = Path(os.environ.get("BIDS_VALIDATOR_UPSTREAM_ROOT", ROOT / "vendor/bids-validator-2.4.1"))
RUST_BIN = Path(os.environ.get("BIDS_VALIDATOR_RS_BIN", ROOT / "target/release/bids-validator"))
RUST_CONTENT_MODE = "parity"
RUST_LINK_MODE = "parity"
DENO_BIN = Path(os.environ.get("DENO_BIN", "deno"))  # override via DENO_BIN env
DENO_ENTRY = Path(os.environ.get("BIDS_VALIDATOR_DENO_ENTRY", UPSTREAM_ROOT / "src/bids-validator.ts"))
CACHE_DIR = Path(__file__).resolve().parent / "cache"

# Default sweep: the vendor fixtures plus the gitignored bids-examples
# datasets fetched by scripts/fetch_bench_data.py. These are expected to
# be GREEN, so a no-argument run is a clean pass/fail signal. Datasets
# absent on a given machine are skipped (not failed).
DATASETS = {
    "valid_headers": UPSTREAM_ROOT / "tests/data/valid_headers",
    "no_t1w": UPSTREAM_ROOT / "tests/data/no_t1w",
    "ds006_missing-session": UPSTREAM_ROOT / "tests/data/ds006_missing-session",
    "latin-1_description": UPSTREAM_ROOT / "tests/data/latin-1_description",
    "symlinked_subject": UPSTREAM_ROOT / "tests/data/symlinked_subject",
    "broken_pet_example_2-pet_mri": UPSTREAM_ROOT / "tests/data/broken_pet_example_2-pet_mri",
    # Electrophysiology corpus (coordsystem.json -> association/"viewed"
    # walk). Fetched into gitignored data/ by
    # `scripts/fetch_bench_data.py <name>`; skipped here when absent.
    # (CTF-MEG datasets like ds000246 are excluded: Deno
    # treats the `.ds/` recording as one unit, Rust walks its internal
    # files — a separate unimplemented divergence, not in scope here.)
    "eeg_face13": ROOT / "data/eeg_face13",
    "ieeg_epilepsy": ROOT / "data/ieeg_epilepsy",
    "ds114": ROOT / "data/ds114",  # DWI (bval/bvec inheritance)
    "emg_Multimodal": ROOT / "data/emg_Multimodal",  # EMG channels/coordsystem
    "fnirs_tapping": ROOT / "data/fnirs_tapping",  # NIRS channels
    "eeg_rest_fmri": ROOT / "data/eeg_rest_fmri",  # EEG + events
    "asl001": ROOT / "data/asl001",  # ASL (aslcontext, perf sidecar)
    "pet003": ROOT / "data/pet003",  # PET (frame timing sidecar)
}

# Opt-in datasets: machine-specific absolute paths AND/OR known-red
# datasets with documented pre-existing divergences (see KNOWN_DIVERGENCES).
# Excluded from the default sweep so it stays a clean signal; run them
# explicitly, e.g. `python3 tests/parity/run.py ds005016`.
OPT_IN_DATASETS = {
    "ds002606": Path("/Users/chris/src/bidsui/datasets/ds002606"),
    "ds000003": Path("/Users/chris/src/bidsui/datasets/ds000003"),
    "ds005016": Path("/Users/chris/src/bidsui/datasets/ds005016"),
}

# Multiset parity is judged against these codes (the Rust validator emits
# them today). Adding a new code requires regenerating the cache via
# `--refresh` so the cached Deno multiset includes it.
RUST_CODES = {
    # Filename
    "NOT_INCLUDED",
    "ALL_FILENAME_RULES_HAVE_ISSUES",
    "ENTITY_WITH_NO_LABEL",
    "INVALID_ENTITY_LABEL",
    "MISSING_REQUIRED_ENTITY",
    "ENTITY_NOT_IN_RULE",
    "DATATYPE_MISMATCH",
    "EXTENSION_MISMATCH",
    "INVALID_LOCATION",
    "FILENAME_MISMATCH",
    # Sidecar / JSON
    "SIDECAR_KEY_REQUIRED",
    "SIDECAR_KEY_RECOMMENDED",
    "JSON_KEY_REQUIRED",
    "JSON_KEY_RECOMMENDED",
    "JSON_SCHEMA_VALIDATION_ERROR",
    # Internal validators
    "CASE_COLLISION",
    "EMPTY_FILE",
    "MISSING_DATASET_DESCRIPTION",
    "SIDECAR_FIELD_OVERRIDE",
    # CLI / dataset filters
    "BLACKLISTED_MODALITY",
    "UNSUPPORTED_DATASET_TYPE",
    # Dataset-level custom
    "NO_AUTHORS",
    "SUBJECT_FOLDERS",
    # Inheritance walk
    "MULTIPLE_INHERITABLE_FILES",
    # rules.checks (added per Phase 1 NIfTI / rules.checks slice)
    "DUPLICATE_FILES",
    "GZIP_HEADER_COMMENT",
    "GZIP_HEADER_FILENAME",
    "GZIP_HEADER_MTIME",
    "NIFTI_UNIT",
    "NIFTI_PE_DIRECTION_CONSISTENCY",
    "README_FILE_MISSING",
    "SFORM_AND_QFORM_IN_IMAGE_HEADER_ARE_ZERO",
    "TOO_FEW_AUTHORS",
    "EVENTS_TSV_MISSING",
    "PARTICIPANT_ID_MISMATCH",
    "SCANS_FILENAME_NOT_MATCH_DATASET",
    "SUSPICIOUSLY_LONG_EVENT_DESIGN",
    "SUSPICIOUSLY_SHORT_EVENT_DESIGN",
    "SUSPICIOUS_NEGATIVE_EVENT_ONSET",
    "SUSPICIOUS_POSITIVE_EVENT_ONSET",
    # TSV
    "TSV_COLUMN_HEADER_DUPLICATE",
    "TSV_EQUAL_ROWS",
    "TSV_EMPTY_LINE",
    "TSV_COLUMN_MISSING",
    "TSV_COLUMN_ORDER_INCORRECT",
    "TSV_ADDITIONAL_COLUMNS_NOT_ALLOWED",
    "TSV_ADDITIONAL_COLUMNS_MUST_DEFINE",
    "TSV_ADDITIONAL_COLUMNS_UNDEFINED",
    "TSV_INDEX_VALUE_NOT_UNIQUE",
    "TSV_VALUE_INCORRECT_TYPE",
    "TSV_COLUMN_TYPE_REDEFINED",
    "TSV_PSEUDO_AGE_DEPRECATED",
    # Associations
    "BVEC_ROW_LENGTH",
    # DWI rules.checks family (crafted-fixture verified vs Deno 2.4.1; see
    # crates/bids-core/tests/dwi_checks.rs). Need a valid 4D NIfTI header
    # and/or bval/bvec, which the empty-placeholder example datasets lack.
    "VOLUME_COUNT_MISMATCH",
    "BVAL_MULTIPLE_ROWS",
    "BVEC_NUMBER_ROWS",
    "DWI_MISSING_BVAL",
    "DWI_MISSING_BVEC",
    # fmap rules.checks family (crafted-fixture verified vs Deno 2.4.1; see
    # crates/bids-core/tests/fmap_checks.rs).
    "FIELDMAP_WITHOUT_MAGNITUDE_FILE",
    "MISSING_MAGNITUDE1_FILE",
    "ECHOTIME1_2_DIFFERENCE_UNREASONABLE",
    "MAGNITUDE_FILE_WITH_TOO_MANY_DIMENSIONS",
    "EPI_WITH_BVALS_NEEDS_SMALL_BVALS",
    "TOTAL_READOUT_TIME_MUST_DEFINE",
    # NIfTI dimension/pixdim family (crafted-fixture verified vs Deno 2.4.1;
    # see crates/bids-core/tests/nifti_checks.rs).
    "T1W_FILE_WITH_TOO_MANY_DIMENSIONS",
    "BOLD_NOT_4D",
    "NIFTI_DIMENSION",
    "NIFTI_PIXDIM",
    # ASL/perf rules.checks family (crafted-fixture verified vs Deno 2.4.1;
    # see crates/bids-core/tests/asl_checks.rs).
    "ASLCONTEXT_TSV_NOT_CONSISTENT",
    "LABELING_DURATION_LENGTH_NOT_MATCHING_NIFTI",
    "LABELLING_DURATION_NOT_MATCHING_ASLCONTEXT_TSV",
    "FLIP_ANGLE_NOT_MATCHING_NIFTI",
    "FLIP_ANGLE_NOT_MATCHING_ASLCONTEXT_TSV",
    "POST_LABELING_DELAY_NOT_MATCHING_NIFTI",
    "POST_LABELING_DELAY_NOT_MATCHING_ASLCONTEXT_TSV",
    "REPETITIONTIMEPREPARATION_NOT_MATCHING_ASLCONTEXT_TSV",
    "REPETITIONTIME_PREPARATION_NOT_CONSISTENT",
    "ECHO_TIME_NOT_CONSISTENT",
    "M0Type_SET_INCORRECTLY",
    "M0Type_SET_INCORRECTLY_TO_ABSENT",
    "M0Type_SET_INCORRECTLY_TO_ABSENT_IN_ASLCONTEXT",
    "POST_LABELING_DELAY_GREATER",
    "LABELING_DURATION_GREATER",
    "BOLUS_CUT_OFF_DELAY_TIME_GREATER",
    "BOLUS_CUT_OFF_DELAY_TIME_NOT_MONOTONICALLY_INCREASING",
    "VOLUME_TIMING_NOT_MONOTONICALLY_INCREASING",
    "VOLUME_TIMING_AND_REPETITION_TIME_MUTUALLY_EXCLUSIVE",
    "REPETITION_TIME_AND_ACQUISITION_DURATION_MUTUALLY_EXCLUSIVE",
    "VOLUME_TIMING_AND_DELAY_TIME_MUTUALLY_EXCLUSIVE",
    "VOLUME_TIMING_MISSING_ACQUISITION_DURATION",
    "DEPRECATED_ACQUISITION_DURATION",
    "BACKGROUND_SUPPRESSION_PULSE_NUMBER_NOT_CONSISTENT",
    "TOTAL_ACQUIRED_VOLUMES_NOT_CONSISTENT",
    # PET frame-timing family (crafted-fixture verified vs Deno 2.4.1;
    # see crates/bids-core/tests/pet_checks.rs).
    "PET_FRAME_CONSISTENCY",
    "PET_FRAME_CONSISTENCY_FRAME_DURATION",
    "PET_FRAME_CONSISTENCY_FRAME_TIMES_START",
    "NIFTI_PIXDIM_PET",
    # bold/mri timing family (crafted-fixture verified vs Deno 2.4.1; see
    # crates/bids-core/tests/timing_checks.rs).
    "REPETITION_TIME_GREATER_THAN",
    "REPETITION_TIME_MISMATCH",
    "SLICETIMING_VALUES_GREATER_THAN_REPETITION_TIME",
    "SLICETIMING_ELEMENTS",
    "ECHO_TIME_GREATER_THAN",
    "EFFECTIVEECHOSPACING_TOO_LARGE",
    "EFFECTIVEECHOSPACING_LARGER_THAN_TOTALREADOUTTIME",
    # rules.errors emissions (Phase 1 scope expansion)
    "SIDECAR_WITHOUT_DATAFILE",
    # Electrophysiology channel-count checks (exercised by eeg_face13).
    # Deno emits these too; only EEG/iEEG/MEG datasets trigger them, so
    # adding them does not invalidate the MRI/PET dataset caches.
    "EOG_CHANNEL_COUNT_MISMATCH",
    "MISC_CHANNEL_COUNT_MISMATCH",
    # Dataset-description check (exercised by ds114, whose
    # dataset_description.json has a BIDSVersion the schema doesn't know).
    "UNKNOWN_BIDS_VERSION",
}

# Rust can emit a small number of issue codes that are intentionally
# outside the implemented-code parity contract. Keep this list explicit:
# any new Rust-only issue code should fail the sweep until the contract
# is updated or the code is intentionally allowed here.
NON_CONTRACT_RUST_CODES = {
    "FILE_READ",
    "INVALID_FILE_ENCODING",
    "INVALID_GZIP",
    "JSON_INVALID",
    "JSON_NOT_AN_OBJECT",
}

# Known parity divergences that we accept (and won't fix in this slice).
# Each entry is `(dataset_name, code)`; the parity sweep ignores both
# Rust-only and Deno-only counts for these (code, dataset) pairs. Keep
# this list short — every entry is a documented hole in the parity
# contract. Each entry's rationale is in its inline comment below.
# Keep empty unless a divergence is tied to one specific subCode and
# documented in README/audit notes. Do not add
# SIDECAR_KEY_RECOMMENDED / AcquisitionDuration here: Rust should
# suppress detailed deprecated fields like Deno 2.4.1.
KNOWN_DIVERGENCE_ISSUES: set[tuple[str, str, str | None]] = set()

KNOWN_DIVERGENCES: set[tuple[str, str]] = {
    # ds000003: 35 of 47 .nii.gz files are broken git-annex symlinks
    # whose target object isn't checked out locally. Deno's
    # `readBytes` path resolves these via the git-annex layer and
    # parses the gzip header anyway; Rust's `parse_gzip` opens the
    # symlink directly, gets `ENOENT`, and returns `None`. Result:
    # Rust under-fires `GZIP_HEADER_FILENAME` / `GZIP_HEADER_MTIME`
    # by 39 issues each on this dataset. Fixing requires teaching the
    # Rust loader to traverse the git-annex object store, which is
    # plan.md Phase 2 territory.
    ("ds000003", "GZIP_HEADER_FILENAME"),
    ("ds000003", "GZIP_HEADER_MTIME"),
    # ds002606 has /prefs.json (a non-standalone JSON file that's
    # NOT_INCLUDED by both validators). Rust flags it as
    # SIDECAR_WITHOUT_DATAFILE because no data file's inheritance walk
    # picks it up. Deno does NOT flag it: Deno's `walkBack` is called
    # for every non-ignored file including the .json itself, and that
    # walk treats the source as a candidate of its own walk (self-self
    # subset matches), marking source.viewed = true. So Deno's
    # SIDECAR_WITHOUT_DATAFILE check is effectively dead code — it
    # never fires on any .json with a well-formed suffix. We chose to
    # diverge: the Rust implementation skips self in the viewed-set
    # accumulator (only ANCESTOR sidecars count as inheriting), so
    # genuine orphans actually fire. Tracked as a Rust-only signal, not a parity
    # bug.
    ("ds002606", "SIDECAR_WITHOUT_DATAFILE"),
}


def strip_known_divergences(name, ms):
    """Remove accepted divergent codes for this dataset from a multiset."""
    divergent_codes = {c for (ds, c) in KNOWN_DIVERGENCES if ds == name}
    divergent_issues = {(code, subcode) for (ds, code, subcode) in KNOWN_DIVERGENCE_ISSUES if ds == name}
    if not divergent_codes and not divergent_issues:
        return ms
    return collections.Counter({
        k: v
        for k, v in ms.items()
        if k[0] not in divergent_codes and (k[0], k[2]) not in divergent_issues
    })


def unexpected_rust_codes(result):
    """Issue codes emitted by Rust that are not covered by any contract."""
    allowed = RUST_CODES | NON_CONTRACT_RUST_CODES
    codes = {
        i.get("code")
        for i in result.get("issues", {}).get("issues", [])
        if i.get("code") not in allowed
    }
    return sorted(c for c in codes if c)


def run_rust(dataset_path, rust_schema=None):
    cmd = [
        str(RUST_BIN),
        "--content-mode",
        RUST_CONTENT_MODE,
        "--link-mode",
        RUST_LINK_MODE,
    ]
    if rust_schema is not None:
        cmd.extend(["--schema", rust_schema])
    cmd.append(str(dataset_path))
    r = subprocess.run(
        cmd,
        capture_output=True, text=True, check=False,
    )
    if r.returncode != 0 and not r.stdout.strip():
        print(f"  rust failed: {r.stderr[:500]}", file=sys.stderr)
        return None
    return json.loads(r.stdout)


def run_deno(dataset_path):
    r = subprocess.run(
        [str(DENO_BIN), "run", "-A", str(DENO_ENTRY), "--json", str(dataset_path)],
        capture_output=True, text=True, check=False,
    )
    if r.returncode > 1 and not r.stdout.strip():
        print(f"  deno failed: {r.stderr[:500]}", file=sys.stderr)
        return None
    return json.loads(r.stdout)


def multiset(result, codes):
    """(code, location, subCode) Counter, filtered to the given code set."""
    issues = result.get("issues", {}).get("issues", [])
    out = collections.Counter()
    for i in issues:
        code = i.get("code")
        if codes is not None and code not in codes:
            continue
        out[(code, i.get("location"), i.get("subCode"))] += 1
    return out


def serialize_multiset(ms):
    """Sorted-key JSON so cache files diff cleanly across runs."""
    items = sorted(
        [{"code": k[0], "location": k[1], "subCode": k[2], "count": v}
         for k, v in ms.items()],
        key=lambda d: (d["code"], d["location"] or "", d["subCode"] or ""),
    )
    return json.dumps({"codes": items}, indent=2) + "\n"


def deserialize_multiset(text):
    data = json.loads(text)
    return collections.Counter({
        (e["code"], e.get("location"), e.get("subCode")): e["count"]
        for e in data["codes"]
    })


def get_deno_multiset(name, dataset_path, refresh):
    cache_path = CACHE_DIR / f"{name}.deno.json"
    if not refresh and cache_path.exists():
        return deserialize_multiset(cache_path.read_text())
    if not dataset_path.is_dir():
        return None
    print(f"  refreshing Deno cache for {name} (this is slow)...", file=sys.stderr)
    deno = run_deno(dataset_path)
    if deno is None:
        return None
    ms = multiset(deno, RUST_CODES)
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    cache_path.write_text(serialize_multiset(ms))
    return ms


def main():
    refresh = False
    rust_schema = None
    names = []
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        arg = args[i]
        if arg == "--refresh":
            refresh = True
            i += 1
        elif arg == "--rust-schema":
            i += 1
            if i >= len(args):
                print("--rust-schema requires a path or tag", file=sys.stderr)
                sys.exit(2)
            rust_schema = args[i]
            i += 1
        elif arg.startswith("-"):
            print(f"unknown option {arg}", file=sys.stderr)
            sys.exit(2)
        else:
            names.append(arg)
            i += 1
    # Default = the green set; opt-in datasets only run when named explicitly.
    all_datasets = {**DATASETS, **OPT_IN_DATASETS}
    default_run = not names  # no datasets named on the CLI
    names = names or list(DATASETS.keys())
    overall_ok = True
    ran = 0
    skipped = []
    for name in names:
        if name not in all_datasets:
            print(f"unknown dataset {name}")
            sys.exit(2)
        path = all_datasets[name]
        if not path.is_dir():
            print(f"  skip {name} (missing path)")
            skipped.append(name)
            continue
        print(f"=== {name} ===")
        rust = run_rust(path, rust_schema=rust_schema)
        if rust is None:
            print("  ERROR running Rust validator")
            overall_ok = False
            continue
        deno_ms = get_deno_multiset(name, path, refresh)
        if deno_ms is None:
            print("  ERROR: no Deno cache available and Deno run failed")
            overall_ok = False
            continue
        ran += 1

        rust_ms = multiset(rust, RUST_CODES)
        rust_total_all = sum(multiset(rust, None).values())
        unexpected = unexpected_rust_codes(rust)
        if unexpected:
            overall_ok = False
            print(f"  ERROR: unexpected Rust issue codes outside contract: {unexpected}")

        # Strip known-divergent (code, dataset) pairs from both sides
        # before the comparison so the sweep stays green while real
        # parity gaps stay visible (logged via KNOWN_DIVERGENCES).
        rust_ms = strip_known_divergences(name, rust_ms)
        deno_ms = strip_known_divergences(name, deno_ms)
        rust_only = rust_ms - deno_ms
        deno_only = deno_ms - rust_ms

        if not rust_only and not deno_only:
            print(f"  OK ({sum(rust_ms.values())} parity issues; {rust_total_all} total rust issues)")
        else:
            overall_ok = False
            print(f"  MISMATCH (rust={sum(rust_ms.values())} deno={sum(deno_ms.values())})")
            if rust_only:
                print(f"  rust-only ({sum(rust_only.values())}):")
                for k, n in rust_only.most_common(20):
                    print(f"    {n}x {k}")
            if deno_only:
                print(f"  deno-only ({sum(deno_only.values())}):")
                for k, n in deno_only.most_common(20):
                    print(f"    {n}x {k}")

    # Don't let a clean checkout (no fetched datasets) look like a pass:
    # a default run that compared nothing is a failure, not success.
    print(f"\n=== summary: {ran} dataset(s) compared, {len(skipped)} skipped ===")
    if skipped:
        print(f"  skipped: {', '.join(skipped)}")
    if ran == 0:
        print("  ERROR: no datasets were compared. Run scripts/fetch_upstream.py "
              "and scripts/fetch_bench_data.py to populate the parity corpus.")
        overall_ok = False
    elif default_run and skipped:
        print("  NOTE: some default datasets were skipped (not fetched); "
              "coverage is partial.")
    sys.exit(0 if overall_ok else 1)


if __name__ == "__main__":
    main()
