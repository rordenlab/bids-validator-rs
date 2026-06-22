//! Parity regression tests for the PET `rules.checks.{pet,nifti}.*` family.
//!
//! Fixture-only, like DWI/fmap/NIfTI/ASL: the generic engine + the NIfTI
//! header context + `length` already implement these. Each case confirmed
//! 1:1 vs @bids/validator 2.4.1 on the identical synthetic tree.
//!
//! Rules (schema 1.2.1):
//!   * pet.PETFrameConsistency            -> PET_FRAME_CONSISTENCY                   (len(FrameDuration) == len(FrameTimesStart))
//!   * pet.PETFrameConsistencyFrameDuration -> PET_FRAME_CONSISTENCY_FRAME_DURATION  (len(FrameDuration) == dim[4])
//!   * pet.PETFrameConsistencyFrameTimesStart -> PET_FRAME_CONSISTENCY_FRAME_TIMES_START (len(FrameTimesStart) == dim[4])
//!   * nifti.NiftiPixdimPET               -> NIFTI_PIXDIM_PET   (min(pixdim[1..=3]) > 0; the pet-only sibling of NIFTI_PIXDIM)

mod common;

use bids_core::issue::Issue;
use bids_core::run::validate_dataset;
use common::{build_ctx, issue_locations, nifti};

const DD: &[u8] = br#"{"Name": "pet", "BIDSVersion": "1.10.0"}"#;
const PET: &str = "/sub-01/pet/sub-01_pet.nii";

fn pet_issues(nvol: i16, pixdim: [f32; 8], sidecar: &str) -> Vec<Issue> {
    let img = nifti([4, 2, 2, 2, nvol, 1, 1, 1], pixdim);
    let files: Vec<(&str, &[u8])> = vec![
        ("/dataset_description.json", DD),
        (PET, &img),
        ("/sub-01/pet/sub-01_pet.json", sidecar.as_bytes()),
    ];
    let (_t, ctx) = build_ctx(&files);
    validate_dataset(&ctx).issues.issues
}

fn fires(issues: &[Issue], code: &str) -> bool {
    issue_locations(issues, code) == vec![PET.to_string()]
}

const PIX_OK: [f32; 8] = [1.0; 8];

#[test]
fn pet_frame_consistency() {
    // FrameDuration (len 3) != FrameTimesStart (len 4).
    let issues = pet_issues(
        4,
        PIX_OK,
        r#"{"FrameDuration":[1,2,3],"FrameTimesStart":[0,1,2,3]}"#,
    );
    assert!(fires(&issues, "PET_FRAME_CONSISTENCY"));
}

#[test]
fn pet_frame_duration_not_matching_nifti() {
    // FrameDuration (len 3) != dim[4] (4); FrameTimesStart also len 3 so
    // PET_FRAME_CONSISTENCY stays silent.
    let issues = pet_issues(
        4,
        PIX_OK,
        r#"{"FrameDuration":[1,2,3],"FrameTimesStart":[0,1,2]}"#,
    );
    assert!(fires(&issues, "PET_FRAME_CONSISTENCY_FRAME_DURATION"));
}

#[test]
fn pet_frame_times_start_not_matching_nifti() {
    let issues = pet_issues(
        4,
        PIX_OK,
        r#"{"FrameDuration":[1,2,3],"FrameTimesStart":[0,1,2]}"#,
    );
    assert!(fires(&issues, "PET_FRAME_CONSISTENCY_FRAME_TIMES_START"));
}

#[test]
fn nifti_pixdim_pet_fires_on_zero_voxel_size() {
    // pixdim[2] = 0. Frame arrays match dim[4] = 2 so only the pixdim
    // check fires. NIFTI_PIXDIM (non-pet) must NOT fire (suffix == pet).
    let issues = pet_issues(
        2,
        [1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        r#"{"FrameDuration":[1,2],"FrameTimesStart":[0,1]}"#,
    );
    assert!(fires(&issues, "NIFTI_PIXDIM_PET"));
    assert!(
        issue_locations(&issues, "NIFTI_PIXDIM").is_empty(),
        "non-pet NIFTI_PIXDIM must not fire on a pet file"
    );
}
