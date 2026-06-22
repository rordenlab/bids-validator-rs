//! Parity regression tests for the NIfTI-dimension `rules.checks.*` family.
//!
//! Fixture-only, like DWI/fmap: the generic engine + the parsed NIfTI
//! header context already implement these. Each case confirmed 1:1 vs
//! @bids/validator 2.4.1 on the identical synthetic tree.
//!
//! Rules (schema 1.2.1):
//!   * anat.T1wFileWithTooManyDimensions -> T1W_FILE_WITH_TOO_MANY_DIMENSIONS (dim[0] == 3)
//!   * func.BoldNot4d                     -> BOLD_NOT_4D                       (dim[0] == 4)
//!   * nifti.NiftiDimension               -> NIFTI_DIMENSION   (shape non-empty, all extents > 0)
//!   * nifti.NiftiPixdim                  -> NIFTI_PIXDIM       (all voxel sizes > 0; suffix != pet)

mod common;

use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations};

const DD: &[u8] = br#"{"Name": "nii", "BIDSVersion": "1.10.0"}"#;

/// Uncompressed NIfTI-1 with caller-chosen `dim` and `pixdim` arrays
/// (8 entries each: `[0]` is ndims / qfac, `[1..]` are extents / sizes).
fn nii(dim: [i16; 8], pixdim: [f32; 8]) -> Vec<u8> {
    let mut h = vec![0u8; 360];
    h[0..4].copy_from_slice(&348i32.to_le_bytes());
    for (i, d) in dim.iter().enumerate() {
        h[40 + i * 2..42 + i * 2].copy_from_slice(&d.to_le_bytes());
    }
    h[70..72].copy_from_slice(&2i16.to_le_bytes());
    h[72..74].copy_from_slice(&8i16.to_le_bytes());
    for (i, p) in pixdim.iter().enumerate() {
        h[76 + i * 4..80 + i * 4].copy_from_slice(&p.to_le_bytes());
    }
    h[108..112].copy_from_slice(&352.0f32.to_le_bytes());
    h[112..116].copy_from_slice(&1.0f32.to_le_bytes());
    h[123] = 10;
    h[252..254].copy_from_slice(&1i16.to_le_bytes());
    h[254..256].copy_from_slice(&1i16.to_le_bytes());
    h[344..348].copy_from_slice(b"n+1\0");
    h
}

const PIX_OK: [f32; 8] = [1.0; 8];

#[test]
fn t1w_with_too_many_dimensions_fires() {
    // 4D T1w (dim[0] = 4, should be 3).
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        (
            "/sub-01/anat/sub-01_T1w.nii",
            &nii([4, 2, 2, 2, 5, 1, 1, 1], PIX_OK),
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "T1W_FILE_WITH_TOO_MANY_DIMENSIONS"),
        vec!["/sub-01/anat/sub-01_T1w.nii"]
    );
}

#[test]
fn bold_not_4d_fires() {
    // 3D bold (dim[0] = 3, should be 4).
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        (
            "/sub-01/func/sub-01_task-rest_bold.nii",
            &nii([3, 2, 2, 2, 1, 1, 1, 1], PIX_OK),
        ),
        (
            "/sub-01/func/sub-01_task-rest_bold.json",
            br#"{"TaskName": "rest", "RepetitionTime": 2.0}"#,
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "BOLD_NOT_4D"),
        vec!["/sub-01/func/sub-01_task-rest_bold.nii"]
    );
}

#[test]
fn nifti_dimension_fires_on_zero_extent() {
    // A zero-length spatial extent (dim[2] = 0) -> min(shape) == 0.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        (
            "/sub-01/anat/sub-01_T2w.nii",
            &nii([3, 2, 0, 2, 1, 1, 1, 1], PIX_OK),
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "NIFTI_DIMENSION"),
        vec!["/sub-01/anat/sub-01_T2w.nii"]
    );
}

#[test]
fn nifti_pixdim_fires_on_zero_voxel_size() {
    // A zero voxel size (pixdim[2] = 0) -> min(voxel_sizes) == 0. Suffix
    // FLAIR (not pet), so the rule applies.
    let (_t, ctx) = build_ctx(&[
        ("/dataset_description.json", DD),
        (
            "/sub-01/anat/sub-01_FLAIR.nii",
            &nii(
                [3, 2, 2, 2, 1, 1, 1, 1],
                [1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0],
            ),
        ),
    ]);
    let r = validate_dataset(&ctx);
    assert_eq!(
        issue_locations(&r.issues.issues, "NIFTI_PIXDIM"),
        vec!["/sub-01/anat/sub-01_FLAIR.nii"]
    );
}
