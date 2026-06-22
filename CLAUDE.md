# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repository is

A Rust-only beta port of the BIDS Validator. The TypeScript/Deno upstream is *pinned*, not vendored: `upstream.lock.json` and `build.rs` lock to BIDS Validator `2.4.1` (commit `94ea9719…`). `scripts/fetch_upstream.py` downloads that release into `vendor/bids-validator-2.4.1/` (gitignored) so Deno parity tests and oracle exporters can run against the exact upstream rules and fixtures.

Goal: a CLI (`bids-validator`) with **structured issue parity** (`code`, `location`, `subCode`) against the Deno validator on the implemented slice. Message prose is intentionally not byte-for-byte. JSON output only — `--format text` is parsed and rejected.

## Common commands

```sh
# One-time bootstrap (fetches pinned upstream into vendor/)
python3 scripts/fetch_upstream.py
cargo build --release --locked

# CI parity check (matches .github/workflows/rust_beta.yml)
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings

# Spell-check — run BEFORE every commit (enforced by .github/workflows/codespell.yml).
# Reads .codespellrc; must exit 0. For a legitimate domain term, reword or add
# it to `ignore-words-list` in .codespellrc rather than disabling the check.
codespell

# Run a single test
cargo test -p bids-core --test deno_oracle deno_filename_oracle_cases_pass
cargo test -p bids-core some_test_name

# Smoke test a built binary (used by CI)
python3 scripts/beta_smoke.py --bin target/release/bids-validator --skip-external

# Deno-vs-Rust dataset parity sweep (uses cached Deno output)
python3 tests/parity/run.py                  # all datasets
python3 tests/parity/run.py valid_headers    # single dataset
python3 tests/parity/run.py --refresh        # regenerate Deno cache

# Regenerate the Deno oracle manifests under tests/oracle/cases/
deno run --allow-read --allow-write --allow-env tests/oracle/export.ts

# Benchmarks (parity smoke + pet002 + DataLad ds005016)
python3 scripts/fetch_bench_data.py pet002
python3 scripts/fetch_bench_data.py ds005016
python3 benches/validator_perf.py --runs 1
```

The Deno binary is expected at `/Users/chris/.deno/bin/deno` (override via `DENO_BIN`); parity scripts also accept `BIDS_VALIDATOR_UPSTREAM_ROOT` and `BIDS_VALIDATOR_RS_BIN`.

## Architecture

Workspace ([Cargo.toml](Cargo.toml)) with one binary crate at the root and three internal crates under [crates/](crates/):

- **Root crate (`bids-validator-rs`)** — [src/main.rs](src/main.rs) is a thin shell that calls into four sibling modules: [src/cli.rs](src/cli.rs) (clap `Cli`, `OutputFormat`/`ContentMode`/`LinkMode` enums, `version_string`, flag accept/reject helpers), [src/discover.rs](src/discover.rs) (filesystem walk with `walkdir` + `ignore`, datatype inference, `.bidsignore` handling), [src/validate.rs](src/validate.rs) (the validation pipeline — builds `DatasetContext`, calls `bids_core::run::validate_dataset`, applies dataset-type/blacklist filters and warning suppression, recurses into derivative roots), and [src/output.rs](src/output.rs) (JSON serializer). [src/lib.rs](src/lib.rs) re-exports the three internal crates as `core`/`expr`/`schema`. Each module owns its own `#[cfg(test)] mod tests`.
- **`bids-schema-codegen`** ([crates/bids-schema-codegen/](crates/bids-schema-codegen/)) — embeds [schemas/schema-1.2.1.json](schemas/schema-1.2.1.json) via `include_str!`. **Hybrid model**: most subtrees are lazily `serde_json::from_str`'d into a typed `Schema` cached in `OnceLock`; hot subtrees (`objects.formats`, `objects.entities`, `rules.modalities`) are emitted as `&'static` slices by [crates/bids-schema-codegen/build.rs](crates/bids-schema-codegen/build.rs) and `include!`'d. When adding a new static table, edit `build.rs::write_*`, `src/lib.rs`, and the call sites together.
- **`bids-expr`** ([crates/bids-expr/](crates/bids-expr/)) — evaluator for the JS-flavored mini-DSL used in schema `selectors`, `checks`, and `level_addendum`. Semantics intentionally mirror the TS reference (`src/schema/expressionLanguage.ts`), which wraps each expression in `new Function('context', 'with(context) { return …}')`: loose `==`, JS truthiness (empty array/object truthy), short-circuit `&&`/`||` returning operand values, `"x" in obj`, bare identifiers resolve in the context's top scope. Unimplemented constructs (`+`/`-`, ternary, regex literals, object literals) return `ParseError` rather than silently accepting them. Parity oracle in `tests/oracle.rs` consumes `schema.meta.expression_tests`.
- **`bids-core`** ([crates/bids-core/src/](crates/bids-core/src/)) — the actual validator: filename rules, sidecar inheritance, NIfTI/gzip/TIFF parsers, TSV column checks, association resolution, content/link policy, issue assembly. `run::validate_dataset` is the entry point; `issue::IssueList` is the output type.

The root [build.rs](build.rs) embeds the Rust git commit and the pinned upstream version/commit into the binary's `--version` output (overridable via `BIDS_VALIDATOR_GIT_COMMIT`, `BIDS_VALIDATOR_UPSTREAM_VERSION`, `BIDS_VALIDATOR_UPSTREAM_COMMIT` env vars).

## Schema is build-time material

The bundled schema is **load-bearing for parity**: every issue can change when it bumps. Runtime `--schema` input is currently accepted only when structurally identical to the bundled `schema-1.2.1.json`, because the generated tables (and the validation engine that reads them) are compiled from that one file. URLs, `stable`/`latest`, and other version tags are parsed and rejected. See [schemas/SUPPORTED.md](schemas/SUPPORTED.md) for the schema-bump process — it requires regenerating tables and refreshing parity caches.

**Schema patch layer** ([crates/bids-schema-codegen/src/patches.rs](crates/bids-schema-codegen/src/patches.rs)): `schema()` parses the raw JSON to a `Value`, runs local patches (working around known upstream schema bugs), then deserializes the typed `Schema`. The raw bytes (`BUNDLED_SCHEMA_JSON`, `bundled_schema_value()`, the `--schema` byte-compare) stay unpatched, so "bring your own copy of the same release" still matches. **Invariant**: patches run *after* `build.rs` emits its static tables, so a patch that mutates a statically emitted subtree (`objects.formats`, `objects.entities`, `rules.modalities`) would NOT reach those tables — such a patch must be applied in `build.rs` too. Today's only patch (`spoiling-type-extension-filter`) touches `rules.sidecars`, which is not emitted statically. Each patch carries a `*_patch_still_needed` self-test that fails when the upstream fix lands → delete the patch.

## Parity philosophy

Parity is measured on the **multiset of `(code, location, subCode)` triples** per dataset, not on message text. [tests/parity/run.py](tests/parity/run.py) maintains a cache of Deno output under [tests/parity/cache/](tests/parity/cache/) because some datasets take ~80s in Deno. Refresh the cache (`--refresh`) when: the Deno entrypoint changes, `@bids/schema` bumps, or dataset content changes. The whitelist `RUST_CODES` in `run.py` is the set of codes the Rust validator currently emits — adding a new code requires a cache refresh so the cached Deno multiset includes it.

Two Rust-only flags are *not* parity flags and should not be benchmarked against Deno without documenting the policy difference:

- `--content-mode parity|thorough` — controls whether to read through symlinks/git-annex object bytes.
- `--link-mode parity|no-follow` — controls symlinked-directory discovery.

Full flag inventory (implemented, no-op accepted, and parse-and-reject) is in [CLI_PARITY.md](CLI_PARITY.md). Beta scope, version output format, and the release checklist are in [BETA.md](BETA.md).

## Tests at a glance

- `cargo test --workspace` — unit + integration tests across all crates.
- `cargo test -p bids-core --test deno_oracle` — replays manifests under [tests/oracle/cases/](tests/oracle/cases/) (generated from the pinned vendor release; gitignored). See [tests/oracle/README.md](tests/oracle/README.md) for case contract and how to extend.
- `tests/parity/run.py` — dataset-level multiset comparison against cached Deno output.
- `crates/bids-expr/tests/oracle.rs` — expression-language parity against `schema.meta.expression_tests`.

## Open follow-ups

- **Crafted-fixture pattern for header-dependent codes.** Codes that need a valid NIfTI header or specific sidecar/TSV content can't be reached by fetching valid example datasets (bids-examples ships empty placeholder `.nii.gz`). Method: build a synthetic dataset in a Rust integration test (`crates/bids-core/tests/<family>_checks.rs`, shared builder + `nifti()` in `tests/common/mod.rs`), confirm the code fires 1:1 vs Deno on the identical tree *during development*, then commit the assertion (CI-gated, no Deno/external data) and promote the code to `RUST_CODES` + update `ISSUE_COVERAGE.md` (status table sums to 202). All families closed so far were "fixture-only" — the generic `check_rules` engine, the associations (incl. `magnitude`/`m0scan` via the generic arm), and the NIfTI header context already implemented them.

  | Family | Codes | Test file | Status |
  | --- | --: | --- | --- |
  | DWI | 5 | `dwi_checks.rs` | done |
  | fmap | 6 | `fmap_checks.rs` | done |
  | NIfTI-dimension | 4 | `nifti_checks.rs` | done |
  | ASL/perf | 25 | `asl_checks.rs` | done |
  | PET frame-timing | 4 | `pet_checks.rs` | done |
  | bold/mri timing | 7 | `timing_checks.rs` | done (`REPETITION_TIME_GREATER_THAN`/`_MISMATCH`, `SLICETIMING_VALUES_GREATER_THAN_REPETITION_TIME`, `SLICETIMING_ELEMENTS`, `ECHO_TIME_GREATER_THAN`, `EFFECTIVEECHOSPACING_TOO_LARGE`/`_LARGER_THAN_TOTALREADOUTTIME`) |
  | MRS | 1 | `mrs_checks.rs` | partial — `MRS_MATRIX_SIZE` done; **`MRS_NIFTI_CONSISTENCY` blocked** (needs the NIfTI-MRS header *extension* `nifti_header.mrs`, not parsed by the Rust NIfTI reader — an engine gap, not fixture-only) |
  | **derivative metadata** | ~2 | — | **next** (`MISSING_RESOLUTION_DESCRIPTION`/`MISSING_DENSITY_DESCRIPTION`; need `dataset.dataset_description.DatasetType=="derivative"` + `res`/`den` entity + `Resolution`/`Density` object — set DatasetType explicitly in the fixture, `build_ctx` defaults to Raw) |
  | sidecar-value / TSV-column | many | — | queued (`PHASE_UNITS`, `PHASE_SUFFIX_DEPRECATED`, `EVENT_ONSET_ORDER`, `AGE_89`, `MISSING_ONSET_COLUMN`) |
  | **NIfTI-MRS extension** | — | — | blocked engine work: parse the NIfTI-MRS header extension into `nifti_header.mrs` to unlock `MRS_NIFTI_CONSISTENCY` |
- **Closing the parity gap (52→202 codes) is mostly a test-data problem, but raw fetch yield is low.** `crates/bids-core/src/checks.rs::check_rules` is a generic engine over every `rules.checks.*` leaf, and `bids-expr` supports all operators the schema uses — so ~104 of the 126 missing codes are "fixture-only" in principle. **Caveat learned from the DWI probe (`ds114`):** bids-examples ships *empty placeholder* `.nii.gz`, so the many checks that need a NIfTI header (volume counts, dims, `*_NOT_MATCHING_NIFTI`) stay dormant — Deno emits `NIFTI_HEADER_UNREADABLE` (140× on ds114) where Rust reads nothing, and neither runs the header-dependent checks. Adding `ds114` therefore closed only **`UNKNOWN_BIDS_VERSION`** (1 code). To close header-dependent codes the fixture must carry *valid* tiny NIfTI headers (the `pet002` rewrite approach in `scripts/fetch_bench_data.py::tiny_nifti_bytes`), tuned per check — i.e. per-code fixture engineering, not bulk fetching. Cheap fetch-only wins remain for TSV/sidecar-value/dataset-level checks; the header-dependent majority is the bigger lift. Loop flow proven: add dataset → `run.py --refresh` → triage `unexpected` codes → verify Deno emits the same → promote to `RUST_CODES` + update `ISSUE_COVERAGE.md` counts.
- **Electrophysiology corpus (done; CTF-MEG still a gap).** The coordsystem miss happened because the parity suite had zero MEG/EEG/iEEG datasets. Now `eeg_face13` (EEG) and `ieeg_epilepsy` (iEEG, multi-target `space-*` coordsystem) are in `tests/parity/run.py::DATASETS`, fetched into gitignored `data/` by `scripts/fetch_bench_data.py <name>`. Like the rest of the parity suite, both `data/` and `tests/parity/cache/*.json` are **gitignored/local-only** — the parity sweep is a developer tool, not CI-gated (CI runs `cargo test`). The *durable, CI-gated* regression guard for the coordsystem fix is therefore the unit tests in [crates/bids-core/tests/sidecar_without_datafile.rs](crates/bids-core/tests/sidecar_without_datafile.rs); the fetched datasets are an extra local cross-check. **Still missing: a CTF-MEG dataset** (`ds000246` was tried and dropped) — Deno treats the `.ds/` recording bundle as one unit while Rust walks its internal `.acq`/`.res4`/`.meg4`/… files, producing dozens of `ALL_FILENAME_RULES_HAVE_ISSUES`/`NOT_INCLUDED`/`EMPTY_FILE` divergences. Implementing `.ds`-bundle handling is the prerequisite for MEG parity coverage.
- **Pre-existing ds005016 divergences (NOT regressions).** `tests/parity/run.py ds005016` is red independent of any recent change: rust-only `SIDECAR_WITHOUT_DATAFILE` at `/scans.json` (an inheritance-walk gap — `scans.tsv` views `scans.json` in Deno; different walk from the association fix), deno-only `JSON_KEY_RECOMMENDED` for `GeneratedBy`/`SourceDatasets`, deno-only `TSV_ADDITIONAL_COLUMNS_UNDEFINED` for `participants.group`, and out-of-contract `MULTIPLE_README_FILES`/`README_FILE_SMALL`. `ds005016`/`ds000003`/`ds002606` are wired to machine-specific absolute paths and are now in `run.py`'s opt-in `OPT_IN_DATASETS` (run by name, e.g. `python3 tests/parity/run.py ds005016`); the no-argument sweep runs only the expected-green set.
- **`ContentCache` write-lock on hot hits** ([crates/bids-core/src/content.rs](crates/bids-core/src/content.rs)): LRU recency refresh takes a write lock on every cache hit; shared root/session sidecars are hot under the rayon per-file map. Bounded, not a leak — a possible throughput ceiling on very large datasets.
