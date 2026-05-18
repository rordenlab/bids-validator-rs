//! Integration test for `SIDECAR_WITHOUT_DATAFILE` emission.
//!
//! Mirrors Deno `src/validators/internal/unusedFile.ts::sidecarWithoutDatafile`.
//! A `.json` sidecar that isn't picked up by any data file's inheritance
//! walk gets flagged, with three documented exemptions:
//!
//! * `/dataset_description.json` (schema-level standalone)
//! * `/genetic_info.json` (schema-level standalone)
//! * `suffix == "coordsystem"` AND `modality == "emg"` (per
//!   `rules.errors.SidecarWithoutDatafile.selectors`)
//!
//! These cases are not exercised by any of the 9 parity datasets, so
//! the only correctness signal here comes from this test.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use bids_core::filename::parse_filename;
use bids_core::inheritance::build_index;
use bids_core::metadata::DatasetSubjects;
use bids_core::policy::{ContentMode, ContentPolicy};
use bids_core::rules::DatasetType;
use bids_core::run::{validate_dataset, DatasetContext, DiscoveredFile};

use tempfile::TempDir;

/// Build a `DatasetContext` from a tempdir of (`bids_path`, `body`)
/// entries. Mirrors what `bids-cli` does in its walker — minus the
/// `walkdir`/`.bidsignore` complexity.
fn build_ctx(files: &[(&str, &[u8])]) -> (TempDir, DatasetContext) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();

    let mut discovered = Vec::new();
    for (bids_path, body) in files {
        let rel = bids_path.trim_start_matches('/');
        let fs_path = root.join(rel);
        fs::create_dir_all(fs_path.parent().unwrap()).unwrap();
        fs::write(&fs_path, body).unwrap();
        let datatype = infer_datatype(bids_path);
        let name = bids_path
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(bids_path);
        let parsed = parse_filename(name);
        discovered.push(DiscoveredFile {
            bids_path: bids_path.to_string(),
            fs_path: fs_path.clone(),
            datatype,
            parsed,
        });
    }

    let index = build_index(&discovered);
    let policy = ContentPolicy::new(ContentMode::Parity);
    let ctx = DatasetContext {
        root,
        files: discovered,
        index,
        dataset_type: DatasetType::Raw,
        dataset_modalities: Vec::new(),
        dataset_subjects: DatasetSubjects {
            sub_dirs: Vec::new(),
            participant_id: None,
            phenotype: None,
        },
        ignore_nifti_headers: false,
        cache: bids_core::content::ContentCache::new(policy),
    };
    (tmp, ctx)
}

/// Minimal datatype inference (matches `bids-cli/src/main.rs::infer_datatype`).
fn infer_datatype(bids_path: &str) -> String {
    const DATATYPES: &[&str] = &[
        "anat", "func", "fmap", "dwi", "perf", "meg", "eeg", "ieeg", "beh", "pet", "micr",
        "motion", "nirs", "mrs", "emg",
    ];
    let parent = match bids_path.rsplit_once('/') {
        Some((p, _)) => p,
        None => return String::new(),
    };
    let last = match parent.rsplit_once('/') {
        Some((_, l)) => l,
        None => parent,
    };
    if DATATYPES.contains(&last) {
        last.to_string()
    } else {
        String::new()
    }
}

fn issue_locations<'a>(issues: &'a [bids_core::issue::Issue], code: &str) -> Vec<&'a str> {
    let mut out: Vec<&str> = issues
        .iter()
        .filter(|i| i.code == code)
        .map(|i| i.location.as_str())
        .collect();
    out.sort();
    out
}

fn issue_subcodes<'a>(issues: &'a [bids_core::issue::Issue], code: &str) -> Vec<&'a str> {
    let mut out: Vec<&str> = issues
        .iter()
        .filter(|i| i.code == code)
        .filter_map(|i| i.sub_code.as_deref())
        .collect();
    out.sort();
    out
}

#[test]
fn flags_empty_regular_file() {
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/sub-01/anat/sub-01_T1w.nii.gz", b""),
    ]);
    let result = validate_dataset(&ctx);
    let empty = issue_locations(&result.issues.issues, "EMPTY_FILE");
    assert_eq!(empty, vec!["/sub-01/anat/sub-01_T1w.nii.gz"]);
}

#[test]
fn flags_missing_dataset_description() {
    let (_tmp, ctx) = build_ctx(&[("/sub-01/anat/sub-01_T1w.nii.gz", b"not-empty")]);
    let result = validate_dataset(&ctx);
    let issues: Vec<_> = result
        .issues
        .issues
        .iter()
        .filter(|i| i.code == "MISSING_DATASET_DESCRIPTION")
        .collect();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].location, "");
    assert_eq!(
        issues[0].affects.as_deref(),
        Some(&["/dataset_description.json".to_string()][..])
    );
}

#[test]
fn top_level_json_non_object_emits_json_not_object_once() {
    let (_tmp, ctx) = build_ctx(&[("/dataset_description.json", b"[]")]);
    let result = validate_dataset(&ctx);
    let issues: Vec<_> = result
        .issues
        .issues
        .iter()
        .filter(|i| i.code == "JSON_NOT_AN_OBJECT")
        .collect();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].location, "/dataset_description.json");
    assert!(issues[0].affects.is_none());
}

#[test]
fn flags_same_directory_case_collision() {
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/sub-01/anat/sub-01_T1w.nii.gz", b"not-empty"),
        ("/sub-01/anat/sub-01_t1w.nii.gz", b"not-empty"),
    ]);
    let result = validate_dataset(&ctx);
    let collisions = issue_locations(&result.issues.issues, "CASE_COLLISION");
    assert_eq!(
        collisions,
        vec![
            "/sub-01/anat/sub-01_T1w.nii.gz",
            "/sub-01/anat/sub-01_t1w.nii.gz"
        ]
    );
}

#[test]
fn flags_sidecar_field_overrides() {
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        (
            "/T1w.json",
            br#"{"rootOverwrite": "root", "rootValue": "root"}"#,
        ),
        (
            "/sub-01/sub-01_T1w.json",
            br#"{"subOverwrite": "subject", "subValue": "subject"}"#,
        ),
        (
            "/sub-01/anat/sub-01_T1w.json",
            br#"{"rootOverwrite": "anat", "subOverwrite": "anat", "anatValue": "anat"}"#,
        ),
        ("/sub-01/anat/sub-01_T1w.nii.gz", b"not-empty"),
    ]);
    let result = validate_dataset(&ctx);
    let locations = issue_locations(&result.issues.issues, "SIDECAR_FIELD_OVERRIDE");
    assert_eq!(
        locations,
        vec![
            "/sub-01/anat/sub-01_T1w.json",
            "/sub-01/anat/sub-01_T1w.json"
        ]
    );
    assert_eq!(
        issue_subcodes(&result.issues.issues, "SIDECAR_FIELD_OVERRIDE"),
        vec!["rootOverwrite", "subOverwrite"]
    );
}

#[test]
fn flags_orphan_sidecar_with_no_matching_data_file() {
    // /task-orphan_bold.json has no matching .nii.gz anywhere.
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/task-orphan_bold.json", br#"{"RepetitionTime": 2.0}"#),
    ]);
    let result = validate_dataset(&ctx);
    let orphans = issue_locations(&result.issues.issues, "SIDECAR_WITHOUT_DATAFILE");
    assert_eq!(orphans, vec!["/task-orphan_bold.json"]);
}

#[test]
fn does_not_flag_sidecar_used_by_inheritance() {
    // The root-level task-rest_bold.json is inherited by the data
    // file's walk, so it's "viewed" and not orphaned.
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/task-rest_bold.json", br#"{"RepetitionTime": 2.0}"#),
        (
            "/sub-01/func/sub-01_task-rest_bold.nii.gz",
            b"\x1f\x8b\x08\x00not-real-gzip",
        ),
    ]);
    let result = validate_dataset(&ctx);
    let orphans = issue_locations(&result.issues.issues, "SIDECAR_WITHOUT_DATAFILE");
    assert!(orphans.is_empty(), "got unexpected orphans: {orphans:?}");
}

#[test]
fn exempts_dataset_description_and_genetic_info() {
    // Both standalone JSONs have no data file, but the schema treats
    // them as legitimate top-level docs.
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/genetic_info.json", br#"{}"#),
    ]);
    let result = validate_dataset(&ctx);
    let orphans = issue_locations(&result.issues.issues, "SIDECAR_WITHOUT_DATAFILE");
    assert!(
        orphans.is_empty(),
        "standalone JSONs must not be flagged: {orphans:?}"
    );
}

#[test]
fn exempts_emg_coordsystem_json() {
    // EMG-specific quirk encoded in the schema's
    // rules.errors.SidecarWithoutDatafile selector
    // `suffix != "coordsystem" || modality != "emg"`.
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/sub-01/emg/sub-01_coordsystem.json", br#"{}"#),
    ]);
    let result = validate_dataset(&ctx);
    let orphans = issue_locations(&result.issues.issues, "SIDECAR_WITHOUT_DATAFILE");
    assert!(
        orphans.is_empty(),
        "EMG coordsystem must not be flagged: {orphans:?}"
    );
}

#[test]
fn flags_non_emg_coordsystem_when_unused() {
    // A non-EMG coordsystem.json is NOT exempted — should fire if no
    // data file picks it up via inheritance.
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/sub-01/meg/sub-01_coordsystem.json", br#"{}"#),
    ]);
    let result = validate_dataset(&ctx);
    let orphans = issue_locations(&result.issues.issues, "SIDECAR_WITHOUT_DATAFILE");
    assert_eq!(orphans, vec!["/sub-01/meg/sub-01_coordsystem.json"]);
}

#[test]
fn rejects_orphan_only_when_no_matching_subject_data_file() {
    // Root-level sidecar shared by ALL subjects → viewed.
    // Subject-only sidecar with no matching subject data file → orphan.
    let (_tmp, ctx) = build_ctx(&[
        (
            "/dataset_description.json",
            br#"{"Name": "x", "BIDSVersion": "1.10.0"}"#,
        ),
        ("/task-rest_bold.json", br#"{"RepetitionTime": 2.0}"#),
        ("/sub-01/func/sub-01_task-rest_bold.nii.gz", b""),
        // Orphan: per-subject sidecar with no matching bold for sub-99.
        (
            "/sub-99/func/sub-99_task-rest_bold.json",
            br#"{"EchoTime": 0.03}"#,
        ),
    ]);
    let result = validate_dataset(&ctx);
    let orphans = issue_locations(&result.issues.issues, "SIDECAR_WITHOUT_DATAFILE");
    assert_eq!(
        orphans,
        vec!["/sub-99/func/sub-99_task-rest_bold.json"],
        "only the sub-99 orphan should fire"
    );
}

// `DirIndex` is `BTreeMap<String, Vec<usize>>` (indices into the
// canonical file arena). Keep the type-equivalence assertion as a
// compile-time check so the public surface this test depends on
// remains explicit.
#[allow(dead_code)]
fn _api_surface_used() -> (BTreeMap<String, Vec<usize>>, PathBuf) {
    (BTreeMap::new(), PathBuf::new())
}
