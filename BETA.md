# Rust Validator Beta

Status date: 2026-06-22.

This beta is a native Rust validator for the current parity slice. It is intended for technical testers who can compare JSON output against the Deno validator and report precise differences.

It is not a full replacement for the Deno validator yet.

## Supported Scope

- Standalone CLI binary.
- JSON output only: compact JSON by default, `--json`, `--format json`, and `--format json_pp`.
- Bundled `@bids/schema` `1.2.1`, targeting BIDS `1.11.1`.
- Limited runtime schema selection:
  - no `--schema`,
  - `--schema schemas/schema-1.2.1.json`,
  - `--schema 1.2.1`,
  - `--schema v1.2.1`,
  - `--schema 1.11.1`.
- Implemented Deno-compatible flags:
  - `--ignoreWarnings`,
  - `--ignoreNiftiHeaders` / `--ignore-nifti-headers`,
  - `--datasetTypes`,
  - `--blacklistModalities`,
  - `--recursive`,
  - `--prune`,
  - `--outfile`.
- Rust-only policy flags:
  - `--content-mode parity|thorough`,
  - `--link-mode parity|follow|no-follow`.

## Known Omissions

- Not all Deno validator issue families are implemented.
- Text output is rejected; use JSON.
- `--config`, `--max-rows`, `--filenameMode`, `--preferredRemote`, and `--git-ref` are parsed and rejected.
- URL schemas, `stable`, `latest`, and arbitrary newer schema files are rejected for beta.
- Python wheels are deferred until standalone CLI artifacts and smoke tests are stable.
- WASM and N-API bindings are post-beta.
- HED and citation behavior remains limited to the implemented parity slice described in `../plan.md`.

## Default Command

```sh
./bids-validator /path/to/dataset > rust-output.json
```

The default is equivalent to:

```sh
./bids-validator \
  --format json \
  --content-mode parity \
  --link-mode parity \
  /path/to/dataset > rust-output.json
```

Validation issues produce JSON and exit nonzero, matching CI-friendly validator behavior.

## Deno Comparison

Run the Deno validator with JSON output:

```sh
deno run -A src/bids-validator.ts /path/to/dataset --json > deno-output.json
```

Run the Rust beta:

```sh
./bids-validator /path/to/dataset > rust-output.json
```

When comparing results, start with structured issue identity:

- `code`,
- `location`,
- `subCode`.

Message prose is representative and beta-targeted, not guaranteed byte-for-byte across every issue.

## Content And Link Policies

Use default parity behavior for Deno comparisons:

```sh
./bids-validator --content-mode parity --link-mode parity /path/to/dataset
```

In `--link-mode parity`, symlinked directories are registered as path entries but are not traversed. This matches Deno 2.4.1 behavior on fixtures such as `symlinked_subject`.

Use thorough content reads when local symlink or git-annex object bytes are present and you want more complete local checks:

```sh
./bids-validator --content-mode thorough /path/to/dataset
```

Use follow-link discovery when you explicitly want Rust to validate through symlinked directory trees:

```sh
./bids-validator --link-mode follow /path/to/dataset
```

Use no-follow discovery when a dataset may contain large or hostile symlinked directory trees:

```sh
./bids-validator --link-mode no-follow /path/to/dataset
```

Do not compare `thorough`, `follow`, or `no-follow` output directly against Deno without documenting the policy difference.

## Version Output

Include this in every beta report:

```sh
./bids-validator --version
```

The output includes:

- Rust binary version,
- git commit when available,
- beta label,
- schema source,
- `@bids/schema` version,
- BIDS version,
- available/default content and link policy modes.

## Build Standalone CLI

Build from a checkout:

```sh
cargo build --release --locked
```

The binary is:

- Unix: `target/release/bids-validator`
- Windows: `rust\target\release\bids-validator.exe`

Release artifacts should be produced from clean native builders for:

- `aarch64-apple-darwin`,
- `x86_64-apple-darwin`,
- `x86_64-unknown-linux-gnu`,
- `x86_64-pc-windows-msvc`.

Artifact naming:

```text
bids-validator-rust-beta-<version>-<target-triple>[.exe]
```

Checksum commands:

```sh
shasum -a 256 bids-validator-rust-beta-*
```

Windows PowerShell:

```powershell
Get-FileHash .\bids-validator-rust-beta-*.exe -Algorithm SHA256
```

Publish the checksum values with the artifacts.

## Smoke Test

From a checkout, build and run the smoke test:

```sh
python3 scripts/beta_smoke.py --build
```

For a downloaded artifact:

```sh
python3 scripts/beta_smoke.py --bin /path/to/bids-validator
```

The smoke test prints the exact command, version output, exit code, issue count, and SHA-256 hash of each JSON output. It validates the small pinned upstream fixture when available, otherwise it creates a temporary tiny dataset for CI/artifact smoke coverage. It also validates `/Users/chris/src/bidsui/datasets/ds002606` when present.

## Reporting Issues

Use the `Rust beta validator report` issue template and include:

- exact command,
- full `--version` output,
- dataset description and availability,
- output hash from `scripts/beta_smoke.py` when possible,
- Deno comparison output or a summary of structured differences,
- whether `--content-mode thorough`, `--link-mode follow`, or `--link-mode no-follow` was used.

## Release Checklist

1. Clean checkout.
2. `cargo fmt --all -- --check`.
3. `cargo test --workspace`.
4. `cargo clippy --workspace --all-targets -- -D warnings`.
5. `cargo build --release --locked`.
6. `python3 scripts/beta_smoke.py --bin target/release/bids-validator --skip-external`.
7. Run `cargo deny check` if `cargo-deny` is installed; otherwise document that dependency audit was not run in the release notes.
8. Build native artifacts for the target triples above.
9. Publish SHA-256 checksums with the artifacts.
