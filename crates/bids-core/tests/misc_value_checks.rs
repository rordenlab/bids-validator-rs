//! Parity regression tests for assorted fixture-only `rules.checks.*` codes
//! that don't belong to a larger imaging family: events onset ordering,
//! the deprecated `phase` suffix, and phase-part units.
//!
//! Each confirmed 1:1 vs @bids/validator 2.4.1 on the identical tree.
//! (`AGE_89` is deliberately not here — it did not fire in either
//! validator with a simple `participants.tsv` fixture, so there is no
//! divergence to lock; revisit if a real dataset exercises it.)

mod common;

use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations, nifti};

const DD: &[u8] = br#"{"Name": "d", "BIDSVersion": "1.10.0"}"#;
const BOLD_JSON: &[u8] = br#"{"TaskName": "rest", "RepetitionTime": 2.0}"#;

fn bold4d() -> Vec<u8> {
    nifti([4, 2, 2, 2, 4, 1, 1, 1], [1.0; 8])
}

#[test]
fn event_onset_order_fires_when_unsorted() {
    let img = bold4d();
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/func/sub-01_task-rest_bold.nii", &img),
        ("/sub-01/func/sub-01_task-rest_bold.json", BOLD_JSON),
        // onset column is 5 then 1 — not numerically sorted.
        (
            "/sub-01/func/sub-01_task-rest_events.tsv",
            b"onset\tduration\n5\t1\n1\t1\n",
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "EVENT_ONSET_ORDER"),
        vec!["/sub-01/func/sub-01_task-rest_events.tsv"]
    );
}

#[test]
fn event_onset_order_silent_when_sorted() {
    let img = bold4d();
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/func/sub-01_task-rest_bold.nii", &img),
        ("/sub-01/func/sub-01_task-rest_bold.json", BOLD_JSON),
        (
            "/sub-01/func/sub-01_task-rest_events.tsv",
            b"onset\tduration\n1\t1\n5\t1\n",
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert!(issue_locations(&r.issues.issues, "EVENT_ONSET_ORDER").is_empty());
}

#[test]
fn phase_suffix_deprecated_fires() {
    let img = bold4d();
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/func/sub-01_task-rest_phase.nii", &img),
        ("/sub-01/func/sub-01_task-rest_phase.json", BOLD_JSON),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "PHASE_SUFFIX_DEPRECATED"),
        vec!["/sub-01/func/sub-01_task-rest_phase.nii"]
    );
}

#[test]
fn phase_suffix_silent_for_canonical_bold() {
    // Negative control: a plain `bold` suffix must not trip the check.
    let img = bold4d();
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/func/sub-01_task-rest_bold.nii", &img),
        ("/sub-01/func/sub-01_task-rest_bold.json", BOLD_JSON),
    ]);
    let r = validate_dataset(&ctx);
    assert!(issue_locations(&r.issues.issues, "PHASE_SUFFIX_DEPRECATED").is_empty());
}

// Canonical entity order is `task-` before `part-`, so the basename matches
// the schema's reconstructed order and no FILENAME_MISMATCH is emitted.
const PHASE_BOLD: &str = "/sub-01/func/sub-01_task-rest_part-phase_bold.nii";

fn phase_units_issues(units_json: &[u8]) -> Vec<bids_core::issue::Issue> {
    let img = bold4d();
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        (PHASE_BOLD, &img),
        (
            "/sub-01/func/sub-01_task-rest_part-phase_bold.json",
            units_json,
        ),
    ]);
    validate_dataset(&ctx).issues.issues
}

#[test]
fn phase_units_fires_for_non_rad_units() {
    let issues =
        phase_units_issues(br#"{"TaskName": "rest", "RepetitionTime": 2.0, "Units": "deg"}"#);
    assert_eq!(
        issue_locations(&issues, "PHASE_UNITS"),
        vec![PHASE_BOLD.to_string()]
    );
    // The fixture should be otherwise well-formed (canonical entity order).
    assert!(
        issue_locations(&issues, "FILENAME_MISMATCH").is_empty(),
        "fixture must not trip unrelated filename checks"
    );
}

#[test]
fn phase_units_silent_for_rad_and_arbitrary() {
    for ok in [
        br#"{"TaskName":"rest","RepetitionTime":2.0,"Units":"rad"}"#.as_slice(),
        br#"{"TaskName":"rest","RepetitionTime":2.0,"Units":"arbitrary"}"#.as_slice(),
    ] {
        let issues = phase_units_issues(ok);
        assert!(
            issue_locations(&issues, "PHASE_UNITS").is_empty(),
            "rad/arbitrary units must not fire PHASE_UNITS"
        );
    }
}
