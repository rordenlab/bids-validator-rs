//! Parity regression tests for the ASL/perf `rules.checks.*` family.
//!
//! The largest fixture-only family: the generic engine, the `aslcontext`
//! and `m0scan` associations, the NIfTI header context, and the
//! array/`max`/`count`/`sorted`/`intersects` expression ops already
//! implement all of these. Every code below was confirmed 1:1 vs
//! @bids/validator 2.4.1 on the identical synthetic tree (25 codes, zero
//! rust-only/deno-only divergence).
//!
//! Covers `rules.checks` under `perf`/`asl` plus the shared
//! VolumeTiming/BolusCutOff rules that fire on `asl`.

mod common;

use bids_core::issue::Issue;
use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations, nifti};

const DD: &[u8] = br#"{"Name": "asl", "BIDSVersion": "1.10.0"}"#;
const ASL: &str = "/sub-01/perf/sub-01_asl.nii";

fn nii(nvol: i16) -> Vec<u8> {
    nifti([4, 2, 2, 2, nvol, 1, 1, 1], [1.0; 8])
}

fn aslcontext(types: &[&str]) -> String {
    format!("volume_type\n{}\n", types.join("\n"))
}

/// Validate a single-subject ASL tree and return its issues.
fn asl_issues(nvol: i16, ctx_types: Option<&[&str]>, sidecar: &str, m0scan: bool) -> Vec<Issue> {
    let asl = nii(nvol);
    let m0 = nii(1);
    let ctx_tsv = ctx_types.map(aslcontext);
    let mut files: Vec<(&str, &[u8])> = vec![
        ("/dataset_description.json", DD),
        (ASL, &asl),
        ("/sub-01/perf/sub-01_asl.json", sidecar.as_bytes()),
    ];
    if let Some(c) = ctx_tsv.as_deref() {
        files.push(("/sub-01/perf/sub-01_aslcontext.tsv", c.as_bytes()));
    }
    if m0scan {
        files.push(("/sub-01/perf/sub-01_m0scan.nii", &m0));
        files.push((
            "/sub-01/perf/sub-01_m0scan.json",
            br#"{"IntendedFor": "bids::sub-01/perf/sub-01_asl.nii"}"#,
        ));
    }
    // `_t`, `ctx`, and the owned byte buffers above all live to the end of
    // this function, so the borrowed `files` entries stay valid through
    // validation; `issues` is owned and outlives them.
    let (_t, ctx) = build_ctx(&files);
    validate_dataset(&ctx).issues.issues
}

fn fires(issues: &[Issue], code: &str) -> bool {
    issue_locations(issues, code) == vec![ASL.to_string()]
}

const CL4: &[&str] = &["control", "label", "control", "label"];

#[test]
fn array_lengths_not_matching_nifti_and_aslcontext() {
    // Every per-volume array is length 2 while dim[4] = 4 and aslcontext
    // has 4 rows — trips the whole array-length cluster at once.
    let issues = asl_issues(
        4,
        Some(CL4),
        r#"{"PostLabelingDelay":[1,2],"FlipAngle":[1,2],"LabelingDuration":[1,2],"RepetitionTimePreparation":[1,2],"EchoTime":[1,2]}"#,
        false,
    );
    for code in [
        "POST_LABELING_DELAY_NOT_MATCHING_NIFTI",
        "POST_LABELING_DELAY_NOT_MATCHING_ASLCONTEXT_TSV",
        "FLIP_ANGLE_NOT_MATCHING_NIFTI",
        "FLIP_ANGLE_NOT_MATCHING_ASLCONTEXT_TSV",
        "LABELING_DURATION_LENGTH_NOT_MATCHING_NIFTI",
        "LABELLING_DURATION_NOT_MATCHING_ASLCONTEXT_TSV",
        "REPETITIONTIMEPREPARATION_NOT_MATCHING_ASLCONTEXT_TSV",
        "REPETITIONTIME_PREPARATION_NOT_CONSISTENT",
        "ECHO_TIME_NOT_CONSISTENT",
    ] {
        assert!(fires(&issues, code), "expected {code} at {ASL}");
    }
}

#[test]
fn aslcontext_tsv_not_consistent() {
    // 3 aslcontext rows vs dim[4] = 4.
    let issues = asl_issues(4, Some(&["control", "label", "control"]), "{}", false);
    assert!(fires(&issues, "ASLCONTEXT_TSV_NOT_CONSISTENT"));
}

#[test]
fn m0type_separate_without_m0scan() {
    let issues = asl_issues(4, Some(CL4), r#"{"M0Type":"separate"}"#, false);
    assert!(fires(&issues, "M0Type_SET_INCORRECTLY"));
}

#[test]
fn m0type_absent_with_m0scan_file() {
    let issues = asl_issues(4, Some(CL4), r#"{"M0Type":"absent"}"#, true);
    assert!(fires(&issues, "M0Type_SET_INCORRECTLY_TO_ABSENT"));
}

#[test]
fn m0type_absent_with_m0scan_in_aslcontext() {
    let issues = asl_issues(
        4,
        Some(&["control", "label", "m0scan", "label"]),
        r#"{"M0Type":"absent"}"#,
        false,
    );
    assert!(fires(
        &issues,
        "M0Type_SET_INCORRECTLY_TO_ABSENT_IN_ASLCONTEXT"
    ));
}

#[test]
fn post_labeling_delay_greater() {
    let issues = asl_issues(
        2,
        Some(&["control", "label"]),
        r#"{"PostLabelingDelay":[5,12]}"#,
        false,
    );
    assert!(fires(&issues, "POST_LABELING_DELAY_GREATER"));
}

#[test]
fn labeling_duration_greater() {
    let issues = asl_issues(
        2,
        Some(&["control", "label"]),
        r#"{"LabelingDuration":[5,12]}"#,
        false,
    );
    assert!(fires(&issues, "LABELING_DURATION_GREATER"));
}

#[test]
fn bolus_cutoff_delay_time_greater() {
    let issues = asl_issues(
        2,
        Some(&["control", "label"]),
        r#"{"BolusCutOffDelayTime":[5,12]}"#,
        false,
    );
    assert!(fires(&issues, "BOLUS_CUT_OFF_DELAY_TIME_GREATER"));
}

#[test]
fn bolus_cutoff_delay_time_not_monotonic() {
    // [3, 1] is non-monotonic but max < 10, isolating the monotonic check.
    let issues = asl_issues(
        2,
        Some(&["control", "label"]),
        r#"{"BolusCutOffDelayTime":[3,1]}"#,
        false,
    );
    assert!(fires(
        &issues,
        "BOLUS_CUT_OFF_DELAY_TIME_NOT_MONOTONICALLY_INCREASING"
    ));
}

#[test]
fn volume_timing_not_monotonic() {
    // FrameAcquisitionDuration present so VOLUME_TIMING_MISSING_ACQUISITION
    // stays silent, isolating the monotonic check.
    let issues = asl_issues(
        3,
        Some(&["control", "label", "control"]),
        r#"{"VolumeTiming":[0,2,1],"FrameAcquisitionDuration":[1,1,1]}"#,
        false,
    );
    assert!(fires(&issues, "VOLUME_TIMING_NOT_MONOTONICALLY_INCREASING"));
}

#[test]
fn volume_timing_and_repetition_time_mutually_exclusive() {
    let issues = asl_issues(
        3,
        Some(&["control", "label", "control"]),
        r#"{"VolumeTiming":[0,1,2],"RepetitionTime":2.0}"#,
        false,
    );
    assert!(fires(
        &issues,
        "VOLUME_TIMING_AND_REPETITION_TIME_MUTUALLY_EXCLUSIVE"
    ));
}

#[test]
fn repetition_time_and_acquisition_duration_mutually_exclusive() {
    let issues = asl_issues(
        3,
        Some(&["control", "label", "control"]),
        r#"{"FrameAcquisitionDuration":[1,1,1],"RepetitionTime":2.0}"#,
        false,
    );
    assert!(fires(
        &issues,
        "REPETITION_TIME_AND_ACQUISITION_DURATION_MUTUALLY_EXCLUSIVE"
    ));
}

#[test]
fn volume_timing_and_delay_time_mutually_exclusive() {
    let issues = asl_issues(
        3,
        Some(&["control", "label", "control"]),
        r#"{"VolumeTiming":[0,1,2],"DelayTime":1.0,"FrameAcquisitionDuration":[1,1,1]}"#,
        false,
    );
    assert!(fires(
        &issues,
        "VOLUME_TIMING_AND_DELAY_TIME_MUTUALLY_EXCLUSIVE"
    ));
}

#[test]
fn volume_timing_missing_acquisition_duration() {
    let issues = asl_issues(
        3,
        Some(&["control", "label", "control"]),
        r#"{"VolumeTiming":[0,1,2]}"#,
        false,
    );
    assert!(fires(&issues, "VOLUME_TIMING_MISSING_ACQUISITION_DURATION"));
}

#[test]
fn deprecated_acquisition_duration() {
    let issues = asl_issues(
        3,
        Some(&["control", "label", "control"]),
        r#"{"VolumeTiming":[0,1,2],"AcquisitionDuration":5.0}"#,
        false,
    );
    assert!(fires(&issues, "DEPRECATED_ACQUISITION_DURATION"));
}

#[test]
fn background_suppression_pulse_number_not_consistent() {
    let issues = asl_issues(
        4,
        Some(CL4),
        r#"{"BackgroundSuppressionNumberPulses":3,"BackgroundSuppressionPulseTime":[1,2]}"#,
        false,
    );
    assert!(fires(
        &issues,
        "BACKGROUND_SUPPRESSION_PULSE_NUMBER_NOT_CONSISTENT"
    ));
}

#[test]
fn total_acquired_volumes_not_consistent() {
    // 2 control + 2 label volumes, but TotalAcquiredPairs says 3.
    let issues = asl_issues(4, Some(CL4), r#"{"TotalAcquiredPairs":3}"#, false);
    assert!(fires(&issues, "TOTAL_ACQUIRED_VOLUMES_NOT_CONSISTENT"));
}
