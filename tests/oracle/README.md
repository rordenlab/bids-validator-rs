# Deno Oracle Regression Harness

This harness ports selected TypeScript validator tests into language-neutral JSON manifests, then lets Rust tests consume those manifests.

The JSON manifests in `cases/` are generated from the pinned vendor release and are ignored by git. Track the exporter and Rust consumers; regenerate the manifests after changing the upstream pin or exporter.

It is not a TypeScript-to-Rust test transpiler. The exporter calls Deno reference functions and records stable inputs/outputs for behavior that is part of the Rust contract.

## Current Coverage

| Manifest | Source TypeScript Test | Rust Consumer |
| --- | --- | --- |
| `cases/entities.json` | `src/schema/entities.test.ts` | `cargo test -p bids-core --test deno_oracle deno_filename_oracle_cases_pass` |
| `cases/gzip.json` | `src/files/gzip.test.ts` | `cargo test -p bids-core --test deno_oracle deno_gzip_oracle_cases_pass` |
| `cases/tiff.json` | `src/files/tiff.test.ts` | `cargo test -p bids-core --test deno_oracle deno_tiff_oracle_cases_pass` |

Dataset-level parity still lives in `tests/parity/run.py` because it compares normalized issue multisets and uses cached Deno output for slow datasets.

Expression-language schema oracle coverage already lives in `crates/bids-expr/tests/oracle.rs`; it consumes `schema.meta.expression_tests`, which grows with `@bids/schema`.

## Regenerate

From the repository root:

```sh
deno run --allow-read --allow-write --allow-env tests/oracle/export.ts
```

If `deno` is not on `PATH` in the local shell:

```sh
/Users/chris/.deno/bin/deno run --allow-read --allow-write --allow-env tests/oracle/export.ts
```

Then run:

```sh
cargo test -p bids-core --test deno_oracle
```

## Case Contract

Each manifest has:

- `schemaVersion`: manifest format version.
- `generatedBy`: exporter path.
- `cases`: stable oracle cases.

Each case should include:

- `id`: stable unique case ID.
- `kind`: behavior family.
- `sourceTest`: original TypeScript test file.
- `status`: usually `required`; use `expected_fail` only with a documented Rust limitation.
- `capabilities`: feature labels such as `filename`, `gzip`, `tiff`, `ome`, `dataset`, or `expression`.
- `expected`: Deno reference output.

## Extension Rules

Prefer adding exporters in this order:

1. Pure functions with small inputs and outputs.
2. File parser tests backed by `tests/data`.
3. Dataset-level tests that can compare normalized issue identity.
4. Capability-gated tests for git, browser, network, HED, or citation.

Avoid exporting Deno implementation internals that are not Rust beta contracts, such as logging, browser-only openers, or private object layout.
