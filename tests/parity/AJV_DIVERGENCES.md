# AJV Message Compatibility And Remaining Divergences

Goal 4 adds a compatibility formatter for common JSON Schema message
patterns emitted as `JSON_SCHEMA_VALIDATION_ERROR`. The structured
parity contract remains the multiset of `{code, location, subCode}`
triples; message snapshots cover representative beta-target prose.

The parity sweep (`rust/tests/parity/run.py`) deliberately ignores
`issueMessage`. Message compatibility is checked separately by:

- `cargo test -p bids-core metadata::message_tests`
- `python3 rust/tests/parity/messages/check.py`

## Covered message patterns

| Trigger | Deno/AJV-compatible Rust output | Raw `jsonschema` wording |
| --- | --- | --- |
| Type mismatch (value should be array, is string) | `must be array` | `"" is not of type "array"` |
| Type mismatch (value should be string, is number) | `must be string` | `1234 is not of type "string"` |
| Enum mismatch | `must be equal to one of the allowed values` | `"foo" is not one of …` |
| `minimum` / `maximum` violation | `must be >= 0` / `must be <= 89` | `-1.0 is less than the minimum 0` / `90.0 is greater than the maximum 89` |
| `pattern` mismatch | `must match pattern "<re>"` | `"abc" does not match "^[0-9]+$"` |
| Missing required property | `must have required property 'X'` | `"X" is a required property` |
| `additionalProperties` violation | `must NOT have additional properties` | `Additional properties are not allowed (…were unexpected)` |

The trigger column is the kind of failure; what BIDS issue code emits
depends on the rule that triggered the validation, not on the message.
All triggers above produce `JSON_SCHEMA_VALIDATION_ERROR` (when reached
from a sidecar/JSON rule that schema-validates a present field).

Additional covered output-shape compatibility:

- `issues.codeMessages` is serialized like Deno.
- `rules.checks` messages are code-level messages, not per-issue
  `issueMessage` fields.
- Custom schema issue messages, such as `NO_AUTHORS`, are code-level
  messages; the per-issue `issueMessage` remains the field description.

## Known semantic differences (NOT just message phrasing)

Mostly absent for the schema patterns BIDS uses. One known item:

1. **ECMAScript-only regex constructs.** AJV uses the JS regex engine;
   the Rust `regex` crate rejects lookahead (`(?!…)`), backreferences,
   and most non-RE2 features. Three `objects.formats` patterns use
   `(?!/)` negative lookahead: `dataset_relative`, `file_relative`,
   `participant_relative`, `stimuli_relative`, `bids_uri`. When the
   Rust code tries to compile such a pattern, `tables.rs::compile_regex`
   logs a `warn:` and falls back to `^.*$` — which silently accepts
   inputs AJV would reject. Real impact: a `stim_file` column value of
   `/abs/path` (absolute, leading slash) passes the Rust type-check but
   would fail Deno's. **Mitigation**: swap `regex` for `fancy-regex` on
   these patterns; tracked in `audit_response.md`.

2. **Anchoring of `anyOf` patterns.** AJV anchors only the outer
   alternation: `^pat1$ | ^pat2$` style. Rust wraps the whole
   alternation as `^(?:pat1|pat2)$`, which is *stricter* — the AJV
   form would accept strings that match the prefix of an alternative.
   Real impact: `objects.columns.group__emg.anyOf` is `string|number`
   and the `string` pattern is `.*` (matches anything) so the
   anchoring difference is absorbed. Any future stricter multi-format
   column could desync. **Mitigation**: anchor each `anyOf` member
   individually; tracked in `audit_response.md`.

## Refreshing

When `@bids/schema` bumps or the Rust `jsonschema` crate version
changes, re-run the parity sweep and the message snapshots. Add any
new prose patterns above; promote any new *semantic* divergence to its
own section.
