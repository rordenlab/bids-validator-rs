//! Parity regression tests for the DWI `rules.checks.dwi.*` family.
//!
//! These codes were "fixture-only": the generic `check_rules` engine and
//! the NIfTI/bval/bvec eval context already implement them, but no parity
//! dataset exercised them (bids-examples ships empty placeholder `.nii.gz`,
//! so `nifti_header` is null and the volume checks never run). Each case
//! below was confirmed to match @bids/validator 2.4.1 1:1 (same code, same
//! location) on the identical synthetic tree before being committed here.
//!
//! Rules (schema 1.2.1, `rules.checks.dwi.*`):
//!   * DWIVolumeCount  -> VOLUME_COUNT_MISMATCH  (bval/bvec n_cols == dim[4])
//!   * DWIBvalRows     -> BVAL_MULTIPLE_ROWS      (bval n_rows == 1)
//!   * DWIBvecRows     -> BVEC_NUMBER_ROWS        (bvec n_rows == 3)
//!   * DWIMissingBval  -> DWI_MISSING_BVAL        ("bval" in associations)
//!   * DWIMissingBvec  -> DWI_MISSING_BVEC        ("bvec" in associations)

mod common;

use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations, nifti};

const DD: &[u8] = br#"{"Name": "dwi", "BIDSVersion": "1.10.0"}"#;
// A 6-column, 1-row bval and a 6-column, 3-row bvec — the well-formed shape.
const BVAL_6: &[u8] = b"0 1000 1000 1000 1000 1000\n";
const BVEC_6X3: &[u8] = b"0 1 0 0 1 0\n0 0 1 0 0 1\n0 0 0 1 0 0\n";

/// 4D NIfTI with `dim = [4, 2, 2, 2, nvol, ...]`. The schema selector
/// accepts `.nii` as well as `.nii.gz`, so we write the raw header.
fn nii4d(nvol: i16) -> Vec<u8> {
    nifti([4, 2, 2, 2, nvol, 1, 1, 1], [1.0; 8])
}

#[test]
fn volume_count_mismatch_fires_when_bval_cols_disagree_with_dim4() {
    // 5 volumes in the header, 6 columns in bval/bvec -> mismatch.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/dwi/sub-01_dwi.nii", &nii4d(5)),
        ("/sub-01/dwi/sub-01_dwi.bval", BVAL_6),
        ("/sub-01/dwi/sub-01_dwi.bvec", BVEC_6X3),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "VOLUME_COUNT_MISMATCH"),
        vec!["/sub-01/dwi/sub-01_dwi.nii"]
    );
}

#[test]
fn volume_count_match_is_silent() {
    // 6 volumes, 6 columns -> no mismatch.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/dwi/sub-01_dwi.nii", &nii4d(6)),
        ("/sub-01/dwi/sub-01_dwi.bval", BVAL_6),
        ("/sub-01/dwi/sub-01_dwi.bvec", BVEC_6X3),
    ]);
    let r = validate_dataset(&ctx);
    assert!(issue_locations(&r.issues.issues, "VOLUME_COUNT_MISMATCH").is_empty());
}

#[test]
fn bval_multiple_rows_fires() {
    // bval with 2 rows (still 6 cols, matching dim4=6 so volume is silent).
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/dwi/sub-01_dwi.nii", &nii4d(6)),
        (
            "/sub-01/dwi/sub-01_dwi.bval",
            b"0 1000 1000 1000 1000 1000\n0 1000 1000 1000 1000 1000\n",
        ),
        ("/sub-01/dwi/sub-01_dwi.bvec", BVEC_6X3),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "BVAL_MULTIPLE_ROWS"),
        vec!["/sub-01/dwi/sub-01_dwi.nii"]
    );
}

#[test]
fn bvec_number_rows_fires_when_not_three() {
    // bvec with 2 rows instead of 3.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/dwi/sub-01_dwi.nii", &nii4d(6)),
        ("/sub-01/dwi/sub-01_dwi.bval", BVAL_6),
        ("/sub-01/dwi/sub-01_dwi.bvec", b"0 1 0 0 1 0\n0 0 1 0 0 1\n"),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "BVEC_NUMBER_ROWS"),
        vec!["/sub-01/dwi/sub-01_dwi.nii"]
    );
}

#[test]
fn dwi_missing_bval_fires() {
    // dwi.nii with a bvec but no bval.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/dwi/sub-01_dwi.nii", &nii4d(6)),
        ("/sub-01/dwi/sub-01_dwi.bvec", BVEC_6X3),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "DWI_MISSING_BVAL"),
        vec!["/sub-01/dwi/sub-01_dwi.nii"]
    );
}

#[test]
fn dwi_missing_bvec_fires() {
    // dwi.nii with a bval but no bvec.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/dwi/sub-01_dwi.nii", &nii4d(6)),
        ("/sub-01/dwi/sub-01_dwi.bval", BVAL_6),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "DWI_MISSING_BVEC"),
        vec!["/sub-01/dwi/sub-01_dwi.nii"]
    );
}
