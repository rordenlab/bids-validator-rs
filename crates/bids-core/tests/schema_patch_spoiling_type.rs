//! End-to-end regression test for the `SpoilingType` schema patch.
//!
//! See `crates/bids-schema-codegen/src/patches.rs` for the patch
//! definition and the upstream-fix detector. This test exercises the
//! full validator path: it sets up a synthetic DWI scan whose merged
//! sidecar has `SpoilingState: true` and asserts the
//! `SIDECAR_KEY_RECOMMENDED` warning fires **exactly once** — on the
//! `.nii.gz` file, not on its `.bval`/`.bvec` siblings.
//!
//! Without the patch, the same fixture produces three warnings (one
//! per sibling in the inheritance group). When the upstream
//! `bids-standard/bids-specification` fix lands and the patch is
//! removed, this test still passes — because the upstream fix has the
//! same effect.

use std::fs;
use std::path::PathBuf;

use bids_core::filename::parse_filename;
use bids_core::inheritance::build_index;
use bids_core::metadata::DatasetSubjects;
use bids_core::policy::{ContentMode, ContentPolicy};
use bids_core::rules::DatasetType;
use bids_core::run::{validate_dataset, DatasetContext, DiscoveredFile};
use tempfile::TempDir;

fn discover(root: &std::path::Path, files: &[(&str, &[u8])]) -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    for (bids_path, body) in files {
        let rel = bids_path.trim_start_matches('/');
        let fs_path = root.join(rel);
        fs::create_dir_all(fs_path.parent().unwrap()).unwrap();
        fs::write(&fs_path, body).unwrap();
        let name = bids_path
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(bids_path);
        let datatype = infer_datatype(bids_path);
        out.push(DiscoveredFile {
            bids_path: bids_path.to_string(),
            fs_path,
            datatype,
            parsed: parse_filename(name),
        });
    }
    out
}

fn infer_datatype(bids_path: &str) -> String {
    const DATATYPES: &[&str] = &[
        "anat", "func", "fmap", "dwi", "perf", "meg", "eeg", "ieeg", "beh", "pet", "micr",
        "motion", "nirs", "mrs", "emg",
    ];
    let parent = bids_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let last = parent.rsplit_once('/').map(|(_, l)| l).unwrap_or(parent);
    if DATATYPES.contains(&last) {
        last.to_string()
    } else {
        String::new()
    }
}

fn locations_with_subcode(
    issues: &[bids_core::issue::Issue],
    code: &str,
    subcode: &str,
) -> Vec<String> {
    let mut out: Vec<String> = issues
        .iter()
        .filter(|i| i.code == code && i.sub_code.as_deref() == Some(subcode))
        .map(|i| i.location.clone())
        .collect();
    out.sort();
    out
}

#[test]
fn spoiling_type_fires_once_on_dwi_nii_only() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // A DWI scan with `SpoilingState: true` in its sidecar.
    // `SpoilingType` is recommended-but-absent; the rule should fire
    // exactly once (on the .nii.gz) because the patched selectors
    // include `match(extension, "^\\.nii(\\.gz)?$")`.
    let files = discover(
        &root,
        &[
            (
                "/dataset_description.json",
                br#"{"Name": "ds", "BIDSVersion": "1.10.0"}"#,
            ),
            ("/sub-01/dwi/sub-01_dwi.json", br#"{"SpoilingState": true}"#),
            ("/sub-01/dwi/sub-01_dwi.nii.gz", b"fake-nii"),
            ("/sub-01/dwi/sub-01_dwi.bval", b"0 0 0"),
            ("/sub-01/dwi/sub-01_dwi.bvec", b"0 0 0"),
        ],
    );
    let index = build_index(&files);
    let policy = ContentPolicy::new(ContentMode::Parity);
    let cache = bids_core::content::ContentCache::new(policy);
    let ctx = DatasetContext {
        root,
        files,
        index,
        dataset_type: DatasetType::Raw,
        dataset_modalities: Vec::new(),
        dataset_subjects: DatasetSubjects {
            sub_dirs: Vec::new(),
            participant_id: None,
            phenotype: None,
        },
        ignore_nifti_headers: false,
        cache,
    };
    let result = validate_dataset(&ctx);
    let locs = locations_with_subcode(
        &result.issues.issues,
        "SIDECAR_KEY_RECOMMENDED",
        "SpoilingType",
    );
    assert_eq!(
        locs,
        vec!["/sub-01/dwi/sub-01_dwi.nii.gz".to_string()],
        "SpoilingType warning must fire exactly once (on the .nii.gz). \
         If this test reports the .bval/.bvec siblings too, the schema \
         patch in crates/bids-schema-codegen/src/patches.rs is not in \
         effect. See `spoiling-type-extension-filter`."
    );
}

#[test]
fn spoiling_type_does_not_fire_when_state_absent() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // Same fixture but without `SpoilingState: true` — the rule's
    // gating selector fails, so no SpoilingType issue should fire.
    let files = discover(
        &root,
        &[
            (
                "/dataset_description.json",
                br#"{"Name": "ds", "BIDSVersion": "1.10.0"}"#,
            ),
            ("/sub-01/dwi/sub-01_dwi.json", br#"{}"#),
            ("/sub-01/dwi/sub-01_dwi.nii.gz", b"fake-nii"),
            ("/sub-01/dwi/sub-01_dwi.bval", b"0 0 0"),
            ("/sub-01/dwi/sub-01_dwi.bvec", b"0 0 0"),
        ],
    );
    let index = build_index(&files);
    let policy = ContentPolicy::new(ContentMode::Parity);
    let cache = bids_core::content::ContentCache::new(policy);
    let ctx = DatasetContext {
        root,
        files,
        index,
        dataset_type: DatasetType::Raw,
        dataset_modalities: Vec::new(),
        dataset_subjects: DatasetSubjects {
            sub_dirs: Vec::new(),
            participant_id: None,
            phenotype: None,
        },
        ignore_nifti_headers: false,
        cache,
    };
    let result = validate_dataset(&ctx);
    let locs = locations_with_subcode(
        &result.issues.issues,
        "SIDECAR_KEY_RECOMMENDED",
        "SpoilingType",
    );
    assert!(
        locs.is_empty(),
        "SpoilingType warning fired without SpoilingState=true: {locs:?}"
    );
}

// Silence unused-import warning when --no-default-features etc.
#[allow(dead_code)]
fn _api_marker() -> PathBuf {
    PathBuf::new()
}
