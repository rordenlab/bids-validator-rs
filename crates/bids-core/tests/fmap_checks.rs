//! Parity regression tests for the fmap `rules.checks.fmap.*` family.
//!
//! Like the DWI family, these were fixture-only: the generic engine, the
//! `magnitude`/`magnitude1` associations, and the NIfTI header context
//! already implement them. Each case below was confirmed 1:1 vs
//! @bids/validator 2.4.1 (same code, same location) on the identical tree.
//!
//! Rules (schema 1.2.1, `rules.checks.fmap.*`):
//!   * FmapFieldmapWithoutMagnitude -> FIELDMAP_WITHOUT_MAGNITUDE_FILE
//!   * FmapPhasediffWithoutMagnitude -> MISSING_MAGNITUDE1_FILE
//!   * EchoTime12DifferenceUnreasonable -> ECHOTIME1_2_DIFFERENCE_UNREASONABLE
//!   * MagnitudeFileWithTooManyDimensions -> MAGNITUDE_FILE_WITH_TOO_MANY_DIMENSIONS
//!   * EPISmallBVals -> EPI_WITH_BVALS_NEEDS_SMALL_BVALS
//!   * TotalReadoutTimeMustDefine -> TOTAL_READOUT_TIME_MUST_DEFINE

mod common;

use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations};

const DD: &[u8] = br#"{"Name": "fmap", "BIDSVersion": "1.10.0"}"#;

/// Minimal uncompressed NIfTI-1. `ndim` selects 3D (`dim[0]=3`) or 4D
/// (`dim[0]=4`, `dim[4]=nvol`).
fn nii(ndim: i16, nvol: i16) -> Vec<u8> {
    let mut h = vec![0u8; 360];
    h[0..4].copy_from_slice(&348i32.to_le_bytes());
    let dims: [i16; 8] = if ndim == 4 {
        [4, 2, 2, 2, nvol, 1, 1, 1]
    } else {
        [3, 2, 2, 2, 1, 1, 1, 1]
    };
    for (i, d) in dims.iter().enumerate() {
        h[40 + i * 2..42 + i * 2].copy_from_slice(&d.to_le_bytes());
    }
    h[70..72].copy_from_slice(&2i16.to_le_bytes());
    h[72..74].copy_from_slice(&8i16.to_le_bytes());
    for i in 0..8 {
        h[76 + i * 4..80 + i * 4].copy_from_slice(&1.0f32.to_le_bytes());
    }
    h[108..112].copy_from_slice(&352.0f32.to_le_bytes());
    h[112..116].copy_from_slice(&1.0f32.to_le_bytes());
    h[123] = 10;
    h[252..254].copy_from_slice(&1i16.to_le_bytes());
    h[254..256].copy_from_slice(&1i16.to_le_bytes());
    h[344..348].copy_from_slice(b"n+1\0");
    h
}

#[test]
fn fieldmap_without_magnitude_fires() {
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/fmap/sub-01_fieldmap.nii", &nii(3, 1)),
        ("/sub-01/fmap/sub-01_fieldmap.json", br#"{"Units": "Hz"}"#),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "FIELDMAP_WITHOUT_MAGNITUDE_FILE"),
        vec!["/sub-01/fmap/sub-01_fieldmap.nii"]
    );
}

#[test]
fn phasediff_without_magnitude1_fires() {
    // Reasonable echo-time difference so ECHOTIME1_2 stays silent.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/fmap/sub-01_phasediff.nii", &nii(3, 1)),
        (
            "/sub-01/fmap/sub-01_phasediff.json",
            br#"{"EchoTime1": 0.005, "EchoTime2": 0.008}"#,
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "MISSING_MAGNITUDE1_FILE"),
        vec!["/sub-01/fmap/sub-01_phasediff.nii"]
    );
}

#[test]
fn echo_time_difference_unreasonable_fires() {
    // diff = 0.015 s, outside the [0.0001, 0.01] window. magnitude1 present
    // so MISSING_MAGNITUDE1_FILE stays silent.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/fmap/sub-01_phasediff.nii", &nii(3, 1)),
        (
            "/sub-01/fmap/sub-01_phasediff.json",
            br#"{"EchoTime1": 0.005, "EchoTime2": 0.02}"#,
        ),
        ("/sub-01/fmap/sub-01_magnitude1.nii", &nii(3, 1)),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "ECHOTIME1_2_DIFFERENCE_UNREASONABLE"),
        vec!["/sub-01/fmap/sub-01_phasediff.nii"]
    );
}

#[test]
fn magnitude_file_with_too_many_dimensions_fires() {
    // A 4D magnitude1 (dim[0] != 3).
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/fmap/sub-01_magnitude1.nii", &nii(4, 5)),
        ("/sub-01/fmap/sub-01_phasediff.nii", &nii(3, 1)),
        (
            "/sub-01/fmap/sub-01_phasediff.json",
            br#"{"EchoTime1": 0.005, "EchoTime2": 0.008}"#,
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "MAGNITUDE_FILE_WITH_TOO_MANY_DIMENSIONS"),
        vec!["/sub-01/fmap/sub-01_magnitude1.nii"]
    );
}

#[test]
fn epi_with_large_bvals_fires() {
    // min(bval) >= 100. TotalReadoutTime present so TOTAL_READOUT stays silent.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/fmap/sub-01_dir-AP_epi.nii", &nii(3, 1)),
        (
            "/sub-01/fmap/sub-01_dir-AP_epi.json",
            br#"{"TotalReadoutTime": 0.05}"#,
        ),
        ("/sub-01/fmap/sub-01_dir-AP_epi.bval", b"1000 1000 1000\n"),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "EPI_WITH_BVALS_NEEDS_SMALL_BVALS"),
        vec!["/sub-01/fmap/sub-01_dir-AP_epi.nii"]
    );
}

#[test]
fn total_readout_time_must_define_fires() {
    // epi with neither TotalReadoutTime nor EffectiveEchoSpacing, no bval.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        ("/sub-01/fmap/sub-01_dir-AP_epi.nii", &nii(3, 1)),
        ("/sub-01/fmap/sub-01_dir-AP_epi.json", b"{}"),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "TOTAL_READOUT_TIME_MUST_DEFINE"),
        vec!["/sub-01/fmap/sub-01_dir-AP_epi.nii"]
    );
}
