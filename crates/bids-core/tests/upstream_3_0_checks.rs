//! Regressions for behaviour that changed with BIDS Validator 3.0.2 /
//! `@bids/schema` 1.2.7. Each confirmed 1:1 vs @bids/validator 3.0.2 on
//! the identical tree.

mod common;

use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations, nifti};

const DD: &[u8] = br#"{"Name": "d", "BIDSVersion": "1.11.1"}"#;

#[test]
fn beh_onset_duration_fires_only_with_both_columns() {
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        (
            "/sub-01/beh/sub-01_task-a_beh.tsv",
            b"onset\tduration\tresp\n1\t1\t2\n",
        ),
        (
            "/sub-01/beh/sub-01_task-a_beh.json",
            br#"{"TaskName": "a"}"#,
        ),
        ("/sub-01/beh/sub-01_task-b_beh.tsv", b"onset\tresp\n1\t2\n"),
        (
            "/sub-01/beh/sub-01_task-b_beh.json",
            br#"{"TaskName": "b"}"#,
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "BEH_ONSET_DURATION"),
        vec!["/sub-01/beh/sub-01_task-a_beh.tsv"]
    );
}

/// Schema 1.2.7 restricts the Spoiling*/MTParameters sidecar rules to MRI
/// NIfTI files, so inherited `.bval`/`.bvec` no longer repeat them.
#[test]
fn spoiling_and_mt_rules_skip_bval_bvec() {
    let img = nifti([4, 2, 2, 2, 3, 1, 1, 1], [1.0; 8]);
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/dwi/sub-01_dwi.nii", &img),
        ("/sub-01/dwi/sub-01_dwi.bval", b"0 1000 1000\n"),
        ("/sub-01/dwi/sub-01_dwi.bvec", b"0 1 0\n0 0 1\n0 0 0\n"),
        (
            "/sub-01/dwi/sub-01_dwi.json",
            br#"{"SpoilingState": true, "MTState": true, "SpoilingType": "COMBINED"}"#,
        ),
    ]);
    let r = validate_dataset(&ctx);
    for key in [
        "SpoilingType",
        "SpoilingRFPhaseIncrement",
        "SpoilingGradientMoment",
        "MTPulseShape",
    ] {
        let locations = r
            .issues
            .issues
            .iter()
            .filter(|i| i.sub_code.as_deref() == Some(key))
            .map(|i| i.location.as_str())
            .collect::<Vec<_>>();
        assert!(
            locations.iter().all(|l| l.ends_with(".nii")),
            "{key}: {locations:?}"
        );
    }
    assert!(r
        .issues
        .issues
        .iter()
        .any(|i| i.sub_code.as_deref() == Some("MTPulseShape")));
}

/// `_motion.tsv` is headerless (3.0.1): columns are named by the `name`
/// column of the associated `channels.tsv`, so repeated sample values are
/// not a duplicate header.
#[test]
fn motion_tsv_is_headerless() {
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        (
            "/sub-01/motion/sub-01_task-a_tracksys-imu_motion.tsv",
            b"1\t1\t1\n2\t2\t2\n",
        ),
        (
            "/sub-01/motion/sub-01_task-a_tracksys-imu_channels.tsv",
            b"name\tcomponent\ttype\ttracked_point\tunits\n\
              ax\tx\tACCEL\thead\tm/s^2\nay\ty\tACCEL\thead\tm/s^2\naz\tz\tACCEL\thead\tm/s^2\n",
        ),
        (
            "/sub-01/motion/sub-01_task-a_tracksys-imu_motion.json",
            br#"{"TaskName": "a", "SamplingFrequency": 100, "TrackingSystemName": "imu"}"#,
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert!(issue_locations(&r.issues.issues, "TSV_COLUMN_HEADER_DUPLICATE").is_empty());
}
