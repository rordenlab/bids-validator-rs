//! Parity regression tests for the bold/mri timing `rules.checks.{func,mri}.*` family.
//!
//! Fixture-only. Notably exercises the math-heavy expressions for the first
//! time at parity: `REPETITION_TIME_MISMATCH` uses `pixdim[4] * 10 ** (...)`
//! with a nested `index(...)` over `nifti_header.xyzt_units.t`, and
//! `EFFECTIVEECHOSPACING_TOO_LARGE` indexes `nifti_header.dim` by the
//! phase-encoding axis. Each case confirmed 1:1 vs @bids/validator 2.4.1.

mod common;

use bids_core::issue::Issue;
use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations, nifti};

const DD: &[u8] = br#"{"Name": "t", "BIDSVersion": "1.10.0"}"#;
const BOLD: &str = "/sub-01/func/sub-01_task-rest_bold.nii";
const ANAT: &str = "/sub-01/anat/sub-01_T1w.nii";

fn run(img: &str, json_path: &str, dim: [i16; 8], pixdim: [f32; 8], sidecar: &str) -> Vec<Issue> {
    let bytes = nifti(dim, pixdim);
    let files: Vec<(&str, &[u8])> = vec![
        ("/dataset_description.json", DD),
        (img, &bytes),
        (json_path, sidecar.as_bytes()),
    ];
    let (_t, ctx) = build_ctx(&files);
    validate_dataset(&ctx).issues.issues
}

fn bold(pixdim: [f32; 8], sidecar: &str) -> Vec<Issue> {
    run(
        BOLD,
        "/sub-01/func/sub-01_task-rest_bold.json",
        [4, 2, 2, 2, 4, 1, 1, 1],
        pixdim,
        sidecar,
    )
}

const P: [f32; 8] = [1.0; 8];

#[test]
fn repetition_time_greater_than() {
    // pixdim[4] = 200 matches RT = 200 (no MISMATCH); 200 > 100 fires GREATER.
    let issues = bold(
        [1.0, 1.0, 1.0, 1.0, 200.0, 1.0, 1.0, 1.0],
        r#"{"RepetitionTime":200}"#,
    );
    assert_eq!(
        issue_locations(&issues, "REPETITION_TIME_GREATER_THAN"),
        vec![BOLD.to_string()]
    );
}

#[test]
fn repetition_time_mismatch() {
    // pixdim[4] = 1.0 s (xyzt t = sec) vs RepetitionTime = 2.0 -> mismatch.
    let issues = bold(P, r#"{"RepetitionTime":2.0}"#);
    assert_eq!(
        issue_locations(&issues, "REPETITION_TIME_MISMATCH"),
        vec![BOLD.to_string()]
    );
}

#[test]
fn slicetiming_values_greater_than_repetition_time() {
    // pixdim[4] = 2.0 matches RT = 2.0 (no MISMATCH); SliceTiming has 2
    // entries matching dim[3] = 2 (no ELEMENTS); max(SliceTiming)=5 > RT=2.
    let issues = bold(
        [1.0, 1.0, 1.0, 1.0, 2.0, 1.0, 1.0, 1.0],
        r#"{"SliceTiming":[0,5],"RepetitionTime":2.0}"#,
    );
    assert_eq!(
        issue_locations(&issues, "SLICETIMING_VALUES_GREATER_THAN_REPETITION_TIME"),
        vec![BOLD.to_string()]
    );
}

#[test]
fn slicetiming_elements_not_matching_slice_count() {
    // 3 SliceTiming entries vs dim[3] = 2, no SliceEncodingDirection.
    let issues = bold(P, r#"{"SliceTiming":[0,1,2]}"#);
    assert_eq!(
        issue_locations(&issues, "SLICETIMING_ELEMENTS"),
        vec![BOLD.to_string()]
    );
}

#[test]
fn echo_time_greater_than() {
    let issues = run(
        ANAT,
        "/sub-01/anat/sub-01_T1w.json",
        [3, 2, 2, 2, 1, 1, 1, 1],
        P,
        r#"{"EchoTime":2.0}"#,
    );
    assert_eq!(
        issue_locations(&issues, "ECHO_TIME_GREATER_THAN"),
        vec![ANAT.to_string()]
    );
}

#[test]
fn effective_echo_spacing_too_large() {
    // PhaseEncodingDirection "j" -> dim[1] (= 64). RT=0.01 >=
    // EES(0.001) * 64 = 0.064 is false -> fires.
    let issues = run(
        BOLD,
        "/sub-01/func/sub-01_task-rest_bold.json",
        [4, 64, 2, 2, 4, 1, 1, 1],
        [1.0, 1.0, 1.0, 1.0, 0.01, 1.0, 1.0, 1.0],
        r#"{"RepetitionTime":0.01,"EffectiveEchoSpacing":0.001,"PhaseEncodingDirection":"j"}"#,
    );
    assert_eq!(
        issue_locations(&issues, "EFFECTIVEECHOSPACING_TOO_LARGE"),
        vec![BOLD.to_string()]
    );
}

#[test]
fn effective_echo_spacing_larger_than_total_readout_time() {
    let issues = run(
        ANAT,
        "/sub-01/anat/sub-01_T1w.json",
        [3, 2, 2, 2, 1, 1, 1, 1],
        P,
        r#"{"EffectiveEchoSpacing":0.1,"TotalReadoutTime":0.05}"#,
    );
    assert_eq!(
        issue_locations(&issues, "EFFECTIVEECHOSPACING_LARGER_THAN_TOTALREADOUTTIME"),
        vec![ANAT.to_string()]
    );
}
