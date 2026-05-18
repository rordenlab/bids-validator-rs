# Message Snapshots

Status date: 2026-05-17.

This directory holds the small Goal 4 message corpus for the Rust beta.
It is intentionally not a full-output snapshot of the large parity
datasets. Full-dataset message snapshots would be noisy, expensive to
refresh, and easy to confuse with the structured parity contract.

`expected.json` stores Deno-compatible expected values for selected
issues from small in-tree datasets:

- `issues.codeMessages`
- `issueMessage`
- `rule`

`check.py` runs the Rust release binary against those datasets and
compares the selected fields.

Run:

```sh
cd /Users/chris/src/bids-validator
cargo build --release --manifest-path rust/Cargo.toml
python3 rust/tests/parity/messages/check.py
```

JSON Schema wording is additionally locked by Rust unit tests in
`bids-core::metadata::message_tests`. Remaining non-blocking prose and
JSON Schema semantic caveats live in `rust/tests/parity/AJV_DIVERGENCES.md`.
