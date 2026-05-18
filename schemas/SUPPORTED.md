# Supported Runtime Schemas

Status date: 2026-05-17.

The Rust beta embeds `@bids/schema` schema version `1.2.1`, targeting
BIDS version `1.11.1`.

Supported schema selectors:

- No `--schema`: use the embedded `schemas/schema-1.2.1.json`.
- `--schema schemas/schema-1.2.1.json`: accepted if the file is
  structurally identical to the embedded schema.
- `--schema 1.2.1`, `--schema v1.2.1`, or `--schema 1.11.1`: select
  the embedded schema.

Unsupported for beta:

- URL schemas.
- Remote tags such as `stable`, `latest`, or older/newer `v*` tags.
- Arbitrary newer or edited schema JSON files.

Reason: the current Rust validator still uses generated rule tables
compiled from `schema-1.2.1.json`. Runtime schema input is validated at
startup, but validation logic still reads the generated bundled tables.
Accepting structurally different schemas would imply output drift that
the current binary cannot honor correctly.

Schema-bump process:

1. Add the new schema JSON under `schemas/`.
2. Update `bids-schema-codegen` build inputs and embedded path.
3. Regenerate generated schema tables.
4. Run the full workspace tests, clippy, release build, bundled parity
   sweep, runtime-schema parity sweep, and benchmark script.
5. Refresh Deno caches only when the schema bump changes the parity
   contract.
