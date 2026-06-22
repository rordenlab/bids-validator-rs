//! Parity regression test for the MRS `rules.checks.mrs.*` family.
//!
//! `MRS_MATRIX_SIZE` is fixture-only: it needs only `nifti_header.dim` and
//! `sidecar.MatrixSize`, so it is closed here. Confirmed 1:1 vs
//! @bids/validator 2.4.1.
//!
//! The sibling `MRS_NIFTI_CONSISTENCY` is NOT closed: its selector needs
//! the NIfTI-MRS header extension (`nifti_header.mrs`), which the Rust
//! NIfTI parser does not yet expose. Without that extension neither
//! validator fires it (no divergence); unlocking it is engine work
//! tracked in CLAUDE.md / ISSUE_COVERAGE.md.

mod common;

use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations, nifti};

const DD: &[u8] = br#"{"Name": "mrs", "BIDSVersion": "1.10.0"}"#;
const SVS: &str = "/sub-01/mrs/sub-01_svs.nii";

fn mrs_issues(dim: [i16; 8], sidecar: &str) -> Vec<bids_core::issue::Issue> {
    let img = nifti(dim, [1.0; 8]);
    let files: Vec<(&str, &[u8])> = vec![
        ("/dataset_description.json", DD),
        (SVS, &img),
        ("/sub-01/mrs/sub-01_svs.json", sidecar.as_bytes()),
    ];
    let (_t, ctx) = build_ctx(&files);
    validate_dataset(&ctx).issues.issues
}

#[test]
fn mrs_matrix_size_mismatch_fires() {
    // dim[1..=3] = [2,2,2] but MatrixSize says [8,8,8].
    let issues = mrs_issues([4, 2, 2, 2, 16, 1, 1, 1], r#"{"MatrixSize":[8,8,8]}"#);
    assert_eq!(
        issue_locations(&issues, "MRS_MATRIX_SIZE"),
        vec![SVS.to_string()]
    );
    // The extension-dependent sibling must not fire (no NIfTI-MRS extension).
    assert!(issue_locations(&issues, "MRS_NIFTI_CONSISTENCY").is_empty());
}

#[test]
fn mrs_matrix_size_match_is_silent() {
    let issues = mrs_issues([4, 2, 2, 2, 16, 1, 1, 1], r#"{"MatrixSize":[2,2,2]}"#);
    assert!(issue_locations(&issues, "MRS_MATRIX_SIZE").is_empty());
}
