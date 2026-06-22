# Rust CLI Parity Inventory

Status date: 2026-05-17.

Reference: `src/setup/options.ts`.

The Rust beta remains JSON-first. Flags below are either implemented or parsed and rejected explicitly so users do not get silent Deno/Rust behavior drift.

| Deno flag | Rust status | Notes |
| --- | --- | --- |
| `<dataset_directory>` | implemented | Required positional dataset path. |
| `--json` | implemented | Deprecated Deno alias; forces compact JSON. Rust also defaults to compact JSON. |
| `--format json` | implemented | Compact JSON. |
| `--format json_pp` | implemented | Pretty JSON. |
| `--format text` | parse and reject | Text console renderer is post-beta; use JSON. |
| `-s, --schema <URL-or-tag>` | implemented, beta-limited | Local schema paths are accepted only when structurally identical to bundled `@bids/schema` 1.2.1. Matching tags `1.2.1`, `v1.2.1`, and `1.11.1` select the bundled schema. URLs and other remote tags are rejected. |
| `-c, --config <file>` | parse and reject | Severity override config is not ported yet. |
| `--max-rows <nrows>` | parse and reject | Rust validates all TSV rows with bounded byte reads; Deno row limiting is not ported. |
| `-v, --verbose` | implemented no-op | Accepted for automation compatibility; JSON output is unaffected. |
| `--ignoreWarnings` | implemented | Warning issues are removed from root and recursive derivative results. |
| `--ignoreNiftiHeaders` | implemented | Alias for Rust `--ignore-nifti-headers`. |
| `--debug <level>` | implemented no-op | Accepted for automation compatibility; Rust beta does not emit debug logs. |
| `--filenameMode` | parse and reject | Stdin filename-only frontend is post-beta. |
| `--datasetTypes <types>` | implemented | Accepts repeated or comma-separated values: `raw`, `derivative`, `study`. Emits `UNSUPPORTED_DATASET_TYPE`. |
| `--blacklistModalities <modalities>` | implemented | Accepts repeated or comma-separated modality names case-insensitively. Emits `BLACKLISTED_MODALITY` per datatype directory. |
| `-r, --recursive` | implemented | Validates conformant direct children of `<dataset>/derivatives/` and emits `derivativesSummary`. |
| `-p, --prune` | implemented | Keeps derivative validation disabled even when `--recursive` is present; root pruning remains the default. |
| `-o, --outfile <file>` | implemented | Writes selected JSON output to the file path. |
| `--preferredRemote <name>` | parse and reject | Requires Deno/git-annex frontend behavior. |
| `--git-ref [ref]` | parse and reject | Requires Deno/git frontend behavior. Bare flag parses as `HEAD` before rejection. |
| `--color`, `--no-color` | implemented no-op | Accepted for automation compatibility; JSON output has no color. |

Rust-only beta flags:

| Rust flag | Status | Notes |
| --- | --- | --- |
| `--content-mode parity\|thorough` | implemented | Controls content reads for git-annex/symlink targets. Default `parity`. |
| `--link-mode parity\|follow\|no-follow` | implemented | Controls symlinked-directory discovery. Default `parity`, which registers symlinked directories but does not traverse them to match Deno 2.4.1. |
| `--ignore-nifti-headers` | implemented | Kebab-case form of Deno `--ignoreNiftiHeaders`. |

Validation requirements for this inventory:

- `cargo test -p bids-cli`
- `python3 tests/parity/run.py`
- `python3 tests/parity/bench.py --runs 1 --warmups 0`
