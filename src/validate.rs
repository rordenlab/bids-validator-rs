//! Validation pipeline: builds the per-dataset context, runs the core
//! validator, applies CLI-level filters, and (optionally) recurses into
//! derivative roots.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use bids_core::inheritance::build_index;
use bids_core::issue::{Issue, IssueList, Severity};
use bids_core::metadata::DatasetSubjects;
use bids_core::policy::ContentPolicy;
use bids_core::rules::DatasetType;
use bids_core::run::{validate_dataset, DatasetContext, DiscoveredFile};
use bids_core::tsv::load_tsv;
use bids_schema_codegen::SchemaContext;

use crate::cli::{Cli, ContentMode};
use crate::discover::{derivative_roots, discover_dataset, load_bidsignore, Discovery};

const MAX_RECURSIVE_DERIVATIVE_DEPTH: usize = 16;

#[derive(Debug, Serialize)]
pub(crate) struct CliValidationResult {
    pub(crate) issues: IssueList,
    #[serde(
        rename = "derivativesSummary",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub(crate) derivatives_summary: BTreeMap<String, CliValidationResult>,
}

pub(crate) fn validate_cli(
    cli: &Cli,
    _schema_context: &SchemaContext,
) -> Result<CliValidationResult> {
    validate_path(&cli.dataset, cli)
}

pub(crate) fn validate_path(root: &Path, cli: &Cli) -> Result<CliValidationResult> {
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("dataset path not found: {}", root.display()))?;
    let mut visited = HashSet::new();
    validate_path_inner(&canonical_root, cli, &mut visited, 0)
}

fn validate_path_inner(
    root: &Path,
    cli: &Cli,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<CliValidationResult> {
    if !visited.insert(root.to_path_buf()) {
        bail!("recursive derivative cycle detected at {}", root.display());
    }

    let policy = content_policy(cli.content_mode);
    let mut result = validate_single_dataset(root, cli, policy)?;

    if cli.recursive && !cli.prune {
        if depth >= MAX_RECURSIVE_DERIVATIVE_DEPTH {
            bail!(
                "recursive derivative depth exceeds {} at {}",
                MAX_RECURSIVE_DERIVATIVE_DEPTH,
                root.display()
            );
        }
        for (name, path) in derivative_roots(root, cli.link_mode) {
            if visited.contains(&path) {
                continue;
            }
            let derivative = validate_path_inner(&path, cli, visited, depth + 1)
                .with_context(|| format!("failed to validate derivative {name}"))?;
            result.derivatives_summary.insert(name, derivative);
        }
    }

    if cli.ignore_warnings {
        retain_errors_only(&mut result);
    }

    Ok(result)
}

fn validate_single_dataset(
    root: &Path,
    cli: &Cli,
    policy: ContentPolicy,
) -> Result<CliValidationResult> {
    let cache = bids_core::content::ContentCache::new(policy);
    let bidsignore = load_bidsignore(root);
    let dataset_type = load_dataset_type(root, &cache);

    let Discovery {
        files,
        modalities_seen,
        root_dirs,
    } = discover_dataset(root, bidsignore.as_ref(), cli.link_mode);

    let index = build_index(&files);
    let dataset_modalities: Vec<String> = modalities_seen.into_iter().collect();
    let dataset_subjects = load_subjects(root, root_dirs, &policy);

    // CLI filter that needs the file list runs *before* we hand the
    // files to the dataset context, so we can move-instead-of-clone.
    // (Order matters: the blacklist emission used to run *after*
    // validate_dataset and needed a separate borrow of `files`, which
    // forced a clone.)
    let mut blacklist_issues: Vec<bids_core::issue::Issue> = Vec::new();
    apply_blacklist_modalities(&mut blacklist_issues, &files, &cli.blacklist_modalities);

    let ctx = DatasetContext {
        root: root.to_path_buf(),
        files,
        index,
        dataset_type,
        dataset_modalities,
        dataset_subjects,
        ignore_nifti_headers: cli.ignore_nifti_headers,
        cache,
    };
    let mut result = validate_dataset(&ctx);
    apply_dataset_type_filter(&mut result.issues.issues, dataset_type, &cli.dataset_types);
    result.issues.issues.extend(blacklist_issues);

    Ok(CliValidationResult {
        issues: result.issues,
        derivatives_summary: BTreeMap::new(),
    })
}

fn content_policy(mode: ContentMode) -> ContentPolicy {
    ContentPolicy::new(match mode {
        ContentMode::Parity => bids_core::policy::ContentMode::Parity,
        ContentMode::Thorough => bids_core::policy::ContentMode::Thorough,
    })
}

fn apply_dataset_type_filter(
    issues: &mut Vec<Issue>,
    dataset_type: DatasetType,
    allowed: &[String],
) {
    if allowed.is_empty() {
        return;
    }
    let actual = dataset_type_name(dataset_type);
    if allowed.iter().any(|s| s.eq_ignore_ascii_case(actual)) {
        return;
    }
    issues.push(Issue {
        code: "UNSUPPORTED_DATASET_TYPE".to_string(),
        sub_code: None,
        severity: Severity::Error,
        location: "/dataset_description.json".to_string(),
        rule: None,
        issue_message: Some(format!("\"DatasetType\": \"{actual}\"")),
        affects: None,
        line: None,
        code_message: None,
    });
}

fn apply_blacklist_modalities(
    issues: &mut Vec<Issue>,
    files: &[DiscoveredFile],
    blacklist: &[String],
) {
    if blacklist.is_empty() {
        return;
    }
    let blacklisted = blacklisted_datatypes(blacklist);
    if blacklisted.is_empty() {
        return;
    }
    let mut dirs = BTreeSet::new();
    for file in files {
        if !blacklisted.contains(&file.datatype) {
            continue;
        }
        if let Some((parent, _)) = file.bids_path.rsplit_once('/') {
            if !parent.is_empty() {
                dirs.insert(parent.to_string());
            }
        }
    }
    for dir in dirs {
        issues.push(Issue {
            code: "BLACKLISTED_MODALITY".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: dir,
            rule: None,
            issue_message: None,
            affects: None,
            line: None,
            code_message: None,
        });
    }
}

fn blacklisted_datatypes(blacklist: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let modalities = &bids_schema_codegen::schema().rules.modalities;
    for modality in blacklist {
        let key = modality.to_ascii_lowercase();
        let Some(datatypes) = modalities
            .get(&key)
            .and_then(|v| v.get("datatypes"))
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        for datatype in datatypes {
            if let Some(s) = datatype.as_str() {
                out.insert(s.to_string());
            }
        }
    }
    out
}

fn dataset_type_name(dataset_type: DatasetType) -> &'static str {
    match dataset_type {
        DatasetType::Raw => "raw",
        DatasetType::Derivative => "derivative",
        DatasetType::Study => "study",
    }
}

fn retain_errors_only(result: &mut CliValidationResult) {
    result
        .issues
        .issues
        .retain(|issue| issue.severity == Severity::Error);
    for derivative in result.derivatives_summary.values_mut() {
        retain_errors_only(derivative);
    }
}

pub(crate) fn has_errors(result: &CliValidationResult) -> bool {
    result
        .issues
        .issues
        .iter()
        .any(|issue| issue.severity == Severity::Error)
        || result.derivatives_summary.values().any(has_errors)
}

/// Read `<root>/dataset_description.json`'s `DatasetType`. Returns
/// `Raw` (the BIDS default) if the file is absent, unreadable, or
/// doesn't declare a type — but matches Deno's
/// `BIDSContextDataset.set dataset_description` setter by defaulting to
/// `Derivative` when `DatasetType` is missing (or `null`) and
/// `GeneratedBy` is present. Without this, derivative datasets without
/// an explicit `DatasetType` pick the wrong rule set.
fn load_dataset_type(root: &Path, cache: &bids_core::content::ContentCache) -> DatasetType {
    let path = root.join("dataset_description.json");
    let Some(json) = cache.read_json(&path) else {
        return DatasetType::Raw;
    };
    // Treat both missing key AND explicit JSON null as "unset" so the
    // GeneratedBy-driven default applies. Deno's setter checks
    // `!('DatasetType' in description)` which only catches the missing
    // case; we extend it to null because Rust users producing
    // `{"DatasetType": null}` would otherwise get a silent downgrade
    // to Raw.
    let explicit = json
        .get("DatasetType")
        .filter(|v| !v.is_null())
        .and_then(|v| v.as_str());
    if let Some(s) = explicit {
        return DatasetType::from_str(s);
    }
    if json.get("GeneratedBy").is_some() {
        DatasetType::Derivative
    } else {
        DatasetType::Raw
    }
}

fn load_subjects(
    root: &Path,
    root_dirs: BTreeSet<String>,
    policy: &ContentPolicy,
) -> DatasetSubjects {
    let sub_dirs = root_dirs
        .into_iter()
        .filter(|name| name.starts_with("sub-"))
        .collect::<Vec<_>>();
    let participants_path = root.join("participants.tsv");
    let participant_id = if participants_path.exists() {
        let (cols, _) = load_tsv(&participants_path, policy);
        cols.columns.get("participant_id").cloned()
    } else {
        None
    };
    DatasetSubjects {
        sub_dirs,
        participant_id,
        phenotype: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;
    use std::fs;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn write_file(root: &Path, rel: &str, body: &[u8]) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn dataset_type_filter_emits_unsupported_dataset_type() {
        let mut issues = Vec::new();
        apply_dataset_type_filter(&mut issues, DatasetType::Raw, &["derivative".to_string()]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "UNSUPPORTED_DATASET_TYPE");
        assert_eq!(issues[0].location, "/dataset_description.json");
    }

    #[test]
    fn blacklist_modalities_emits_one_issue_per_datatype_directory() {
        let mk = |bids: &str, dt: &str| {
            let name = bids.rsplit_once('/').map(|(_, n)| n).unwrap_or(bids);
            DiscoveredFile {
                bids_path: bids.to_string(),
                fs_path: PathBuf::from("unused"),
                datatype: dt.to_string(),
                parsed: bids_core::filename::parse_filename(name),
            }
        };
        let files = vec![
            mk("/sub-01/anat/sub-01_T1w.nii.gz", "anat"),
            mk("/sub-01/anat/sub-01_T2w.nii.gz", "anat"),
            mk("/sub-01/func/sub-01_task-rest_bold.nii.gz", "func"),
        ];
        let mut issues = Vec::new();
        apply_blacklist_modalities(&mut issues, &files, &["MRI".to_string()]);
        let locations = issues
            .iter()
            .map(|i| i.location.as_str())
            .collect::<Vec<_>>();
        assert_eq!(locations, vec!["/sub-01/anat", "/sub-01/func"]);
        assert!(issues.iter().all(|i| i.code == "BLACKLISTED_MODALITY"));
    }

    #[test]
    fn recursive_validation_adds_derivative_summary() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "dataset_description.json",
            br#"{"Name": "root", "BIDSVersion": "1.10.0"}"#,
        );
        write_file(
            root,
            "derivatives/deriv/dataset_description.json",
            br#"{"Name": "deriv", "BIDSVersion": "1.10.0", "DatasetType": "derivative"}"#,
        );
        write_file(
            root,
            "derivatives/deriv/sub-01/anat/sub-01_T1w.nii.gz",
            b"not-empty",
        );

        let no_recursive = Cli::try_parse_from(["bids-validator", root.to_str().unwrap()]).unwrap();
        let no_recursive_result = validate_path(root, &no_recursive).unwrap();
        assert!(no_recursive_result.derivatives_summary.is_empty());

        let recursive =
            Cli::try_parse_from(["bids-validator", "--recursive", root.to_str().unwrap()]).unwrap();
        let recursive_result = validate_path(root, &recursive).unwrap();
        assert!(recursive_result.derivatives_summary.contains_key("deriv"));
    }

    #[test]
    #[cfg(unix)]
    fn recursive_validation_skips_derivative_symlink_to_ancestor() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_file(
            root,
            "dataset_description.json",
            br#"{"Name": "root", "BIDSVersion": "1.10.0"}"#,
        );
        fs::create_dir_all(root.join("derivatives")).unwrap();
        symlink(root, root.join("derivatives").join("loop")).unwrap();

        let recursive =
            Cli::try_parse_from(["bids-validator", "--recursive", root.to_str().unwrap()]).unwrap();
        let result = validate_path(root, &recursive).unwrap();
        assert!(result.derivatives_summary.is_empty());
    }

    #[test]
    fn ignore_warnings_removes_warning_only_issues() {
        let mut result = CliValidationResult {
            issues: IssueList {
                issues: vec![
                    Issue {
                        code: "JSON_KEY_RECOMMENDED".to_string(),
                        sub_code: None,
                        severity: Severity::Warning,
                        location: "/dataset_description.json".to_string(),
                        rule: None,
                        issue_message: None,
                        affects: None,
                        line: None,
                        code_message: None,
                    },
                    Issue {
                        code: "UNSUPPORTED_DATASET_TYPE".to_string(),
                        sub_code: None,
                        severity: Severity::Error,
                        location: "/dataset_description.json".to_string(),
                        rule: None,
                        issue_message: None,
                        affects: None,
                        line: None,
                        code_message: None,
                    },
                ],
            },
            derivatives_summary: BTreeMap::new(),
        };
        retain_errors_only(&mut result);
        assert_eq!(result.issues.issues.len(), 1);
        assert_eq!(result.issues.issues[0].code, "UNSUPPORTED_DATASET_TYPE");
        assert!(has_errors(&result));
    }
}
