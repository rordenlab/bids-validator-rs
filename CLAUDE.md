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
