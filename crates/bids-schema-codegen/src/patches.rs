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

pub(crate) const PATCHES: &[SchemaPatch] = &[SchemaPatch {
    id: "spoiling-type-extension-filter",
    description: "rules.sidecars.mri.SpoilingType is missing the \
        `modality == \"mri\"` and `match(extension, \"^\\.nii(\\.gz)?$\")` \
        selectors that its siblings (MRISequenceSpecifics, \
        MRIInstitutionInformation) carry. Without the extension \
        filter, the rule fires once for every file in an inheritance \
        group whose merged sidecar has `SpoilingState: true` — \
        triplicating the warning on DWI runs (.nii.gz + .bval + .bvec).",
    upstream_ref: "https://github.com/bids-standard/bids-specification/issues \
        (search: SpoilingType selectors)",
    fingerprint: spoiling_type_bug_present,
    apply: spoiling_type_apply,
}];

/// Apply every patch whose fingerprint matches. Idempotent and safe
/// to call once during `schema()` initialization.
pub(crate) fn apply_all(schema: &mut Value) {
    for patch in PATCHES {
        if (patch.fingerprint)(schema) {
            (patch.apply)(schema);
        }
    }
}

fn spoiling_type_bug_present(schema: &Value) -> bool {
    let Some(selectors) = schema
        .pointer("/rules/sidecars/mri/SpoilingType/selectors")
        .and_then(|v| v.as_array())
    else {
        // Rule moved or removed upstream — fingerprint no longer
        // matches; treat as "fixed" so we stop applying.
        return false;
    };
    // The fixed state (matching the sibling rules) needs BOTH a
    // `modality` filter and an `extension` match. The bug is present
    // unless both are there — a partial upstream fix (one but not the
    // other) still counts as buggy, so we keep patching and the
    // self-test keeps flagging it for human review.
    let strs: Vec<&str> = selectors.iter().filter_map(|v| v.as_str()).collect();
    let has_modality = strs.iter().any(|s| s.contains("modality"));
    let has_extension = strs.iter().any(|s| s.contains("extension"));
    !(has_modality && has_extension)
}

fn spoiling_type_apply(schema: &mut Value) {
    // Replace with the form every other rules.sidecars.mri.* leaf
    // uses. We keep the original `sidecar.SpoilingState == true`
    // gating selector and add the two missing siblings ahead of it.
    let Some(selectors) = schema
        .pointer_mut("/rules/sidecars/mri/SpoilingType/selectors")
        .and_then(|v| v.as_array_mut())
    else {
        return;
    };
    let original = std::mem::take(selectors);
    selectors.push(Value::String("modality == \"mri\"".to_string()));
    selectors.push(Value::String(
        "match(extension, \"^\\.nii(\\.gz)?$\")".to_string(),
    ));
    selectors.extend(original);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BUNDLED_SCHEMA_JSON;

    fn raw_schema() -> Value {
        serde_json::from_str(BUNDLED_SCHEMA_JSON).expect("bundled schema must parse")
    }

    fn find_patch(id: &str) -> &'static SchemaPatch {
        PATCHES
            .iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("no patch with id={id}"))
    }

    /// Detector: this test fails when the upstream schema has been
    /// fixed (i.e., the rule now has the extension filter). When it
    /// fails, the developer should:
    ///   1. Confirm the fix landed in `bids-standard/bids-specification`.
    ///   2. Delete the patch entry from `PATCHES`.
    ///   3. Delete this test.
    ///
    /// Don't just suppress the assertion — the entry's `apply` will
    /// otherwise become a no-op forever and we'll lose the audit trail.
    #[test]
    fn spoiling_type_patch_still_needed() {
        let raw = raw_schema();
        let patch = find_patch("spoiling-type-extension-filter");
        assert!(
            (patch.fingerprint)(&raw),
            "SpoilingType bug appears fixed in the bundled schema. \
             Remove this patch from crates/bids-schema-codegen/src/patches.rs \
             (id={}). Upstream: {}",
            patch.id,
            patch.upstream_ref,
        );
    }

    /// Confirms the patched schema has the expected selectors. Acts
    /// as a regression test on the `apply` function.
    #[test]
    fn spoiling_type_patched_at_runtime() {
        let raw = raw_schema();
        let mut patched = raw.clone();
        apply_all(&mut patched);
        let selectors = patched
            .pointer("/rules/sidecars/mri/SpoilingType/selectors")
            .and_then(|v| v.as_array())
            .expect("SpoilingType.selectors must exist after patching")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(
            selectors.iter().any(|s| s.contains("modality")),
            "patched selectors missing modality filter: {selectors:?}"
        );
        assert!(
            selectors.iter().any(|s| s.contains("extension")),
            "patched selectors missing extension filter: {selectors:?}"
        );
        assert!(
            selectors.iter().any(|s| s.contains("SpoilingState")),
            "patched selectors must keep the original SpoilingState gate: {selectors:?}"
        );
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

    /// `apply_all` is idempotent — running it twice doesn't add the
    /// selectors a second time. Guards against the fingerprint
    /// returning `true` after the patch is already applied.
    #[test]
    fn apply_all_is_idempotent() {
        let mut value = raw_schema();
        apply_all(&mut value);
        let before_count = value
            .pointer("/rules/sidecars/mri/SpoilingType/selectors")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap();
        apply_all(&mut value);
        let after_count = value
            .pointer("/rules/sidecars/mri/SpoilingType/selectors")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap();
        assert_eq!(before_count, after_count, "apply_all must be idempotent");
    }
}
