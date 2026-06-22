# Parity and Benchmark Baselines

Reference process for the wall-time, peak-RSS, and output non-regression gates in [plan.md](../../plan.md#per-milestone-target-5-min).

The primary machine-readable artifact is [`cache/benchmarks.json`](cache/benchmarks.json). It stores, per dataset:

- Rust wall time and peak RSS.
- Deno wall time and peak RSS when explicitly refreshed.
- Deno output baseline hash for the implemented issue-code multiset.
- Rust output hash and whether it matches the cached Deno output.

The Deno validator is intentionally not run by default. Some local datasets under `/Users/chris/src/bidsui/datasets` take minutes to run in Deno, so iteration should use cached Deno output and live Rust runs. Rust runs in `--content-mode parity --link-mode parity` by default. In link parity mode, symlinked directories are registered as path entries but not traversed, matching Deno 2.4.1 on `symlinked_subject`. Use `--content-mode thorough` to benchmark the opt-in path that reads local annex/symlink targets when present, `--link-mode follow` to traverse symlinked directories, or `--link-mode no-follow` to skip symlinked directories entirely. Deno output comparison is skipped outside the default modes because the result is intentionally not parity-equivalent.

## Current Snapshot

Captured 2026-05-17 with `python3 tests/parity/bench.py --runs 1 --warmups 0` after a release build. Each value below is one timed Rust run using cached Deno output; use `--runs 5 --warmups 1` and refresh Deno deliberately when accepting a milestone baseline.

| Dataset | Rust ms | Rust RSS MiB | Deno ms | Deno RSS MiB | Output |
| --- | ---: | ---: | ---: | ---: | --- |
| `valid_headers` | 16.2 | 13.9 | cached output only | cached output only | OK, 60 issues |
| `no_t1w` | 15.8 | 14.1 | cached output only | cached output only | OK, 118 issues |
| `ds006_missing-session` | 38.9 | 14.2 | cached output only | cached output only | OK, 583 issues |
| `latin-1_description` | 12.1 | 14.0 | cached output only | cached output only | OK, 30 issues |
| `symlinked_subject` | 11.8 | 13.6 | cached output only | cached output only | OK, 9 issues |
| `broken_pet_example_2-pet_mri` | 12.7 | 13.9 | cached output only | cached output only | OK, 47 issues |
| `ds002606` | 142.5 | 14.9 | cached output only | cached output only | OK, 690 issues |
| `ds000003` | 52.2 | 14.9 | cached output only | cached output only | OK, 992 issues |
| `ds005016` | 5639.2 | 48.2 | cached output only | cached output only | OK, 15998 issues |

Notes:

- The standalone benchmark defaults are `pet002` from `bids-standard/bids-examples` for a small image-included tree and `ds005016` from OpenNeuro/DataLad for large git-annex link behavior. `ds002606` and `ds000003` are retained only as additional parity regression datasets.
- These are not perfect apples-to-apples full-validator timings. Rust runs the implemented Phase 1 issue-code slice; Deno runs the full validator when refreshed. On git-annex datasets, Rust can also sometimes read local annex-object symlink targets directly where Deno reports `FILE_READ` and leaves content-derived contexts unset. The default Rust `--content-mode parity` path intentionally skips direct content reads on annex symlinks to match Deno issue output; use `--content-mode thorough` when the goal is more complete local checks rather than Deno parity.
- `ds000003` and `ds005016` have Deno output caches, but their Deno timing/RSS refresh was not accepted in this snapshot. A full Deno refresh during this handoff spent several minutes in the external datasets; `ds005016` was stopped after exceeding the old timing expectation by a wide margin. Refresh them deliberately and individually when a new Deno baseline is actually needed.
- Peak RSS is collected with Python `os.wait4` child resource usage. This avoids `/usr/bin/time -l` on macOS, which can fail in sandboxed environments when it tries to read `sysctl kern.clockrate`.
- `cache/benchmarks.json` also contains Rust timing/RSS entries for all 18 directories currently under `/Users/chris/src/bidsui/datasets`. Deno output is cached only where explicitly refreshed; missing Deno baselines are marked as such so Rust iteration can continue.

## Commands

Fast parity check against cached Deno output:

```sh
cd /Users/chris/src/bids-validator
cargo build --release --manifest-path Cargo.toml
python3 tests/parity/run.py
```

Fast benchmark iteration. This runs Rust live and reuses cached Deno output/timing:

```sh
python3 tests/parity/bench.py
```

Thorough content-read benchmark. This reads local annex/symlink targets when available and skips Deno output comparison:

```sh
python3 tests/parity/bench.py --content-mode thorough ds005016
```

Follow-link discovery benchmark. This traverses symlinked directories and skips Deno output comparison:

```sh
python3 tests/parity/bench.py --link-mode follow symlinked_subject
```

Scan every local dataset directory while allowing missing Deno baselines:

```sh
python3 tests/parity/bench.py --external-all --allow-missing-deno
```

Refresh Deno output/timing for a specific dataset. Use a timeout for slow external datasets:

```sh
python3 tests/parity/bench.py \
  --refresh-deno-output \
  --refresh-deno-bench \
  --timeout-s 600 \
  ds005016
```

## Refresh Policy

Re-run Deno only when one of these changes:

- `src/bids-validator.ts` or Deno-side validation behavior.
- The embedded `@bids/schema` version.
- Dataset content.

Otherwise, use the cached Deno `.deno.json` output files and refresh only Rust timing/RSS. The non-regression gates are:

- Wall-time regression > 10%: fail.
- Peak-RSS regression > 20%: fail.
