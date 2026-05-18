# bids-validator-rs

Lean Rust-only split of the BIDS Validator Rust beta. The repository
keeps the Rust implementation and pins the TypeScript/Deno validator
as an external upstream release for rules, fixtures, Deno oracle
export, and parity benchmarking.

Current pin:

- BIDS Validator: `2.4.1`
- Upstream commit: `94ea9719efbbac7531fed8ad8a2a7bf764cee51f`
- Rust package version: `0.1.20260517`

`bids-validator -V` reports the upstream validator version/commit and
the Rust package version, for example:

```text
bids-validator 2.4.1 commit 94ea971 rust 0.1.20260517
```

## Layout

```text
bids-validator-rs/
├── Cargo.toml
├── Cargo.lock
├── src/                  # root CLI/library facade
├── crates/               # internal Rust crates
├── schemas/              # bundled schema consumed at compile time
├── scripts/              # upstream/data fetch and smoke helpers
├── tests/                # Rust parity/oracle harness inputs
└── benches/              # benchmark entrypoints
```

The crate boundaries are intentionally preserved from the working
prototype. Flattening everything into one Rust module tree can happen
later, but this split keeps the beta behavior reviewable.

## Bootstrap

```sh
python3 scripts/fetch_upstream.py
cargo build --release --locked
target/release/bids-validator -V
```

The upstream source is downloaded to `vendor/bids-validator-2.4.1`
and is ignored by git.

## Tests

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 tests/parity/run.py valid_headers
```

After `scripts/fetch_upstream.py`, the Deno oracle exporter can be
regenerated with:

```sh
deno run --allow-read --allow-write --allow-env tests/oracle/export.ts
```

The exported oracle manifests under `tests/oracle/cases/` are generated
from the pinned vendor release and are ignored by git.

## Benchmarks

Correctness is covered by the Rust tests, oracle tests, and cached Deno
parity harness. The benchmark script is only for optimization work and
uses the optimized Rust release binary.

```sh
cargo build --release --locked
```

The standard local benchmark datasets are kept outside git:

```sh
python3 scripts/fetch_bench_data.py pet002
python3 scripts/fetch_bench_data.py ds005016
python3 benches/validator_perf.py pet002 ds005016
```

`pet002` is copied from `bids-standard/bids-examples` into
`data/pet002-tiny`, with tiny valid NIfTI files replacing empty `.nii`
and `.nii.gz` placeholders. This is the default image-included
structural and metadata benchmark. Pass `--bids-examples-ref <commit>`
to pin the source snapshot for a published benchmark run.

`ds005016` is cloned from OpenNeuro/DataLad without `datalad get`, so
annexed images remain git-annex links. This is the default DataLad
link-policy benchmark and provides a larger tree of roughly 6000 files.
Full image downloads are intentionally opt-in:

```sh
python3 scripts/fetch_bench_data.py ds005016 --get-all
python3 benches/validator_perf.py data/ds005016
```

The default benchmark reports Rust wall time, peak RSS, and issue
count. It does not gate on Deno output parity; use the test and parity
harnesses for correctness. The default Rust mode is Deno-like discovery
and content policy:

- `--content-mode parity`
- `--link-mode parity`

Use `--content-mode thorough` to measure the Rust-only path that reads
available symlink/git-annex target content. Use `--include-deno` only
for deliberate comparison runs; it can be slow on `ds005016`.

Rust-vs-Deno comparison table for optimization reports. These values
come from one local release run on 2026-05-17 using
`--content-mode parity --link-mode no-follow`. Treat them as a coarse
baseline; use multiple completed runs before making fine-grained
optimization claims.

The Deno column below is the pinned BIDS Validator `2.4.1` oracle used
by this repository. It does not include
[bids-standard/bids-validator#406](https://github.com/bids-standard/bids-validator/pull/406),
an open DataLad/git-annex performance PR that shares the isomorphic-git
cache and memoizes `git-annex` ref lookup. PR #406 reports a large CPU
reduction on `ds005016`, so the 2.4.1 Deno number should not be read as
the best possible TypeScript/Deno baseline.

```sh
python3 benches/validator_perf.py --runs 1 --warmups 0 --content-mode parity --link-mode no-follow --include-deno pet002
python3 benches/validator_perf.py --runs 1 --warmups 0 --content-mode parity --link-mode no-follow ds005016
```

| Dataset | Rust (ms) | Deno 2.4.1 (ms) | Deno PR #406 (ms) | Rust (MiB) | Deno 2.4.1 (MiB) |
| --- | ---: | ---: | ---: | ---: | ---: |
| `pet002` | 18 | 380 | not measured | 15 | 247|
| `ds005016` | 1944 | >600000 | not measured locally | 55 | not captured |

The `ds005016` Deno run was manually stopped after more than ten
minutes without producing JSON, so its time is reported as a lower
bound and peak RSS is unavailable. A fair follow-up comparison should
run the same command against a checkout containing PR #406 via
`--upstream <patched-validator-checkout>`.

The Rust `ds005016` run completed and emitted 15998 issues. This
dataset contains many git-annex symlinked files, not symlinked
directories; `--content-mode parity` is therefore the main skip-content
performance feature. `--link-mode no-follow` is still used for the
benchmark profile because it is the most aggressive Rust discovery
policy, but it does not materially change datasets without symlinked
directories.

Parity guardrail for tests, not benchmark timing:

- Do not hide recurring mismatches in parity prose. Either fix them
  or list the exact accepted divergence in `tests/parity/run.py`.
- `SIDECAR_KEY_RECOMMENDED / AcquisitionDuration` should not appear as
  a Rust-only mismatch for BIDS Validator 2.4.1. It is a deprecated
  sidecar field, and Deno 2.4.1 suppresses it. A Rust unit test and the
  `valid_headers` parity check cover this regression.

Current benchmark notes:

- `pet002` is the standard small tree benchmark with image files. It
  replaces the older `ds000117` benchmark because `ds000117` predates
  many sidecar definitions and produced benchmark noise unrelated to
  Rust performance.
- `ds005016` is the DataLad symlink benchmark. Do not use `--get-all`
  for routine benchmark runs.
