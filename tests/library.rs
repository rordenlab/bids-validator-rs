use std::fs;
use std::path::Path;
use std::process::Command;

use bids_validator_rs::{validate_path, ValidationOptions, ValidationReport};
use serde_json::Value;
use tempfile::tempdir;

fn write(root: &Path, bids_path: &str, contents: &str) {
    let path = root.join(bids_path.trim_start_matches('/'));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn library_and_cli_report_the_same_findings() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    write(
        root,
        "dataset_description.json",
        r#"{"Name":"test","BIDSVersion":"1.11.1"}"#,
    );
    write(root, "derivatives/broken/dataset_description.json", "{");

    let options = ValidationOptions {
        recursive: true,
        ignore_warnings: true,
        ..ValidationOptions::default()
    };
    assert!(validate_path(root.join("missing"), &options).is_err());
    let report = validate_path(root, &options).unwrap();
    assert!(report.has_errors());
    let output = Command::new(env!("CARGO_BIN_EXE_bids-validator"))
        .args(["--recursive", "--ignoreWarnings"])
        .arg(root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    );
}

fn scoped(datatypes: &[&str]) -> ValidationOptions {
    ValidationOptions {
        include_datatypes: Some(datatypes.iter().map(|s| s.to_string()).collect()),
        ..Default::default()
    }
}

fn locations<'a>(report: &'a ValidationReport, code: &str) -> Vec<&'a str> {
    let mut paths: Vec<_> = report
        .issues
        .issues
        .iter()
        .filter(|issue| issue.code == code)
        .map(|issue| issue.location.as_str())
        .collect();
    paths.sort_unstable();
    paths
}

#[test]
fn datatype_selection_checks_targets_and_shared_metadata_in_dataset_context() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let anat = "/sub-01/ses-01/anat/sub-01_ses-01_T1w.nii";
    let perf = "/sub-01/ses-01/perf/sub-01_ses-01_asl.nii";
    let func = "/sub-01/ses-01/func/sub-01_ses-01_task-rest_bold.nii";
    let other_anat = "/sub-02/anat/sub-02_T1w.nii";
    let orphan = "/sub-02/anat/sub-02_T2w.json";
    for path in [anat, perf, func, other_anat, "/participants.tsv"] {
        write(root, path, "");
    }
    write(root, "dataset_description.json", "{");
    write(root, "asl.json", "{");
    // The inherited ASL context table has a sidecar of its own.
    write(
        root,
        "sub-01/sub-01_aslcontext.tsv",
        "volume_type\ncontrol\n",
    );
    write(root, "sub-01/sub-01_aslcontext.json", "{");
    write(root, "task-rest_bold.json", "{");
    write(root, orphan, "{}");
    write(
        root,
        "T1w.json",
        r#"{"IntendedFor":"ses-01/func/sub-01_ses-01_task-rest_bold.nii"}"#,
    );
    write(root, ".bidsignore", "ignored.json\n");
    write(root, "sub-01/ses-01/perf/ignored.json", "{");

    let full = validate_path(root, &ValidationOptions::default()).unwrap();
    assert!(locations(&full, "SIDECAR_WITHOUT_DATAFILE").contains(&orphan));

    let report = validate_path(root, &scoped(&["anat", "perf"])).unwrap();
    assert_eq!(
        locations(&report, "EMPTY_FILE"),
        ["/participants.tsv", anat, perf, other_anat]
    );
    assert_eq!(
        locations(&report, "JSON_INVALID"),
        [
            "/asl.json",
            "/dataset_description.json",
            "/sub-01/sub-01_aslcontext.json"
        ]
    );
    // Both subjects inherit T1w.json. Only sub-01 has the referenced func file.
    assert_eq!(locations(&report, "INTENDED_FOR"), [other_anat]);
    assert!(locations(&report, "SIDECAR_WITHOUT_DATAFILE").is_empty());
}

#[test]
fn scoped_discovery_findings_stay_scoped_in_derivatives() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let derivative = root.join("derivatives/example");
    write(root, "dataset_description.json", "{}");
    write(
        &derivative,
        "dataset_description.json",
        r#"{"DatasetType":"derivative"}"#,
    );
    let anat = "/sub-01/anat/sub-01_T1w.nii";
    let nested = "/sub-01/anat/func/notes.txt";
    let template = "/tpl-Test/cohort-1/anat/tpl-Test_cohort-1_T1w.nii";
    for path in [anat, nested, template, "/sub-01/func/anat/notes.txt"] {
        write(&derivative, path, "");
    }
    #[cfg(unix)]
    for dir in ["sub-01/anat", "sub-01/func/anat"] {
        std::os::unix::fs::symlink("absent", derivative.join(dir).join("missing.nii")).unwrap();
    }
    let options = ValidationOptions {
        recursive: true,
        blacklist_modalities: vec!["MRI".into()],
        ..scoped(&["anat"])
    };
    let report = validate_path(root, &options).unwrap();
    let child = &report.derivatives_summary["example"];
    assert_eq!(
        locations(child, "BLACKLISTED_MODALITY"),
        ["/sub-01/anat", "/tpl-Test/cohort-1/anat"]
    );
    assert_eq!(locations(child, "EMPTY_FILE"), [nested, anat, template]);
    #[cfg(unix)]
    assert_eq!(
        locations(child, "SYMLINK_BROKEN"),
        ["/sub-01/anat/missing.nii"]
    );
}

#[cfg(unix)]
#[test]
fn scoped_run_reports_broken_inherited_sidecars() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    write(root, "sub-01/anat/sub-01_T1w.nii", "");
    write(root, "sub-01/sub-01_events.tsv", "onset\tduration\n0\t1\n");
    write(root, "sub-02/func/sub-02_task-rest_bold.nii", "");
    // The exact match shadows the broken subject-level T1w.json, not the root one.
    write(root, "sub-01/sub-01_T1w.json", "{}");
    for sidecar in [
        "T1w.json",
        "T2w.json",
        "acq-other_T1w.json",
        "sub-01/T1w.json",
        "sub-02/T1w.json",
        "sub-01/sub-01_events.json",
    ] {
        std::os::unix::fs::symlink("absent", root.join(sidecar)).unwrap();
    }

    let report = validate_path(root, &scoped(&["anat"])).unwrap();
    assert_eq!(
        locations(&report, "SYMLINK_BROKEN"),
        ["/T1w.json", "/sub-01/sub-01_events.json"]
    );
}

#[test]
fn selection_requires_known_names_but_not_matching_files() {
    let tmp = tempdir().unwrap();
    assert!(validate_path(tmp.path(), &scoped(&[])).is_err());
    assert!(validate_path(tmp.path(), &scoped(&["anat", "MRI"])).is_err());
    assert!(validate_path(tmp.path(), &scoped(&["func"])).is_ok());
}
