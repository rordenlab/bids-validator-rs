//! Local patches applied to the bundled schema at parse time.
//!
//! Each patch works around a known upstream schema bug while we wait
//! for the fix to land in `bids-standard/bids-specification`. The
//! patches live here, not in the embedded JSON, so:
//!
//!   * The raw on-disk schema file (`schemas/schema-*.json`) remains
//!     a verbatim copy of the pinned upstream release. Anyone
//!     diffing against `bids-standard/@bids/schema` sees an unmodified
//!     file.
//!   * `validate_runtime_schema_text` still compares user-supplied
//!     schemas against the raw bytes — no surprise mismatches on
//!     "use my own copy of the same release".
//!   * Each patch carries a self-test that fails when the upstream
//!     fingerprint no longer matches, so a future schema bump that
//!     ships the fix lights up immediately and the developer knows to
//!     delete the patch (rather than silently double-applying it).
//!
//! How to add a patch:
//!   1. Define a [`SchemaPatch`] entry in [`PATCHES`].
//!   2. The `fingerprint` closure inspects the raw parsed schema and
//!      returns `true` if the bug is still present.
//!   3. The `apply` closure mutates the parsed schema to install the
//!      fix. It is called only when `fingerprint` returns `true`.
//!   4. Add a `#[test]` named `<id>_patch_still_needed` (see existing
//!      examples) that fails when `fingerprint` no longer returns true
//!      — that's the signal to delete the patch.

use serde_json::Value;

/// A single schema patch with detection + application logic.
///
/// `id`, `description`, and `upstream_ref` are intentionally read only
/// by tests (where they appear in failure messages so the developer
/// can find the entry and decide whether to delete it). They carry
/// the audit trail at the patch site itself; without them this whole
/// file would just be a bag of anonymous closures.
#[allow(dead_code)] // documentation-only fields, read by test failure messages
pub(crate) struct SchemaPatch {
    /// Stable identifier; cited in test failure messages so the
    /// developer can locate this entry quickly.
    pub id: &'static str,
    /// One-paragraph description of the bug and the fix.
    pub description: &'static str,
    /// Upstream tracker URL (issue, PR, discussion).
    pub upstream_ref: &'static str,
    /// Returns `true` if the *raw* schema is still in the buggy
    /// state. Should be precise enough that a partial upstream fix
    /// is detected too — e.g., check for the specific missing
    /// selector, not just "does the rule exist."
    pub fingerprint: fn(&Value) -> bool,
    /// Mutate the parsed schema to apply the fix. Called only when
    /// `fingerprint` returned `true`.
    pub apply: fn(&mut Value),
}

// Empty since schema 1.2.7 shipped the `spoiling-type-extension-filter`
// fix (SpoilingType/SpoilingRF/SpoilingGradient/MTParameters selectors).
pub(crate) const PATCHES: &[SchemaPatch] = &[];

/// Apply every patch whose fingerprint matches. Idempotent and safe
/// to call once during `schema()` initialization.
pub(crate) fn apply_all(schema: &mut Value) {
    for patch in PATCHES {
        if (patch.fingerprint)(schema) {
            (patch.apply)(schema);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BUNDLED_SCHEMA_JSON;

    fn raw_schema() -> Value {
        serde_json::from_str(BUNDLED_SCHEMA_JSON).expect("bundled schema must parse")
    }

    /// Governance guard: patches must NOT touch subtrees that `build.rs`
    /// emits as static tables (`objects.formats`, `objects.entities`,
    /// `rules.modalities`) — those are generated from the *raw* schema
    /// and would silently diverge from the patched runtime `schema()`.
    /// If a future patch needs to mutate one of these, apply it in
    /// `build.rs` too, then update this guard. See the `build.rs` header.
    #[test]
    fn patches_do_not_touch_statically_emitted_subtrees() {
        let raw = raw_schema();
        let mut patched = raw.clone();
        apply_all(&mut patched);
        for ptr in ["/objects/formats", "/objects/entities", "/rules/modalities"] {
            assert_eq!(
                raw.pointer(ptr),
                patched.pointer(ptr),
                "a schema patch mutated {ptr}, which build.rs emits as a static \
                 table from the RAW schema. Apply the patch in build.rs too, or \
                 the static accessors will diverge from runtime schema()."
            );
        }
    }
}
