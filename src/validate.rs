//! Validation pipeline: builds the per-dataset context, runs the core
//! validator, applies CLI-level filters, and (optionally) recurses into
//! derivative roots.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use bids_core::filename::parse_filename;
use bids_core::inheritance::{build_index, sidecar_candidates_with_issues, DirIndex};
use bids_core::issue::{Issue, IssueList, Severity, ValidationResult};
use bids_core::metadata::DatasetSubjects;
use bids_core::policy::{ContentMode, ContentPolicy};
use bids_core::rules::DatasetType;
use bids_core::run::{validate_dataset, validate_matching_files, DatasetContext, DiscoveredFile};
use bids_core::tsv::load_tsv;

use crate::discover::{derivative_roots, discover_dataset, load_bidsignore, Discovery};

const MAX_RECURSIVE_DERIVATIVE_DEPTH: usize = 16;

/// Settings for filesystem validation. Defaults match the CLI without flags.
#[derive(Clone, Debug)]
pub struct ValidationOptions {
    /// How to read content from git-annex links. Defaults to parity mode.
    pub content_mode: ContentMode,
    /// Traverse symlinked directories during discovery (default: true).
    pub follow_symlink_dirs: bool,
    /// Also validate datasets under `derivatives/` (default: false).
    pub recursive: bool,
    /// Keep only error-severity findings.
    pub ignore_warnings: bool,
    /// Skip checks that depend on reading NIfTI headers.
    pub ignore_nifti_headers: bool,
    /// Permitted dataset types (`raw`, `derivative`, `study`); empty allows all.
    pub dataset_types: Vec<String>,
    /// Report an error for modalities on this list (e.g. `MRI`).
    pub blacklist_modalities: Vec<String>,
    /// Check only these BIDS datatype directories, across subjects and sessions.
    /// `None` checks the entire dataset. An empty list or an unknown name is an error.
    /// Scoped runs also check root `dataset_description.json`, `participants.tsv`,
    /// and metadata used by the selected files. They omit whole-dataset orphan checks.
    pub include_datatypes: Option<Vec<String>>,
}

impl Default for ValidationOptions {
    fn default() -> Self {
        Self {
            content_mode: ContentMode::Parity,
            follow_symlink_dirs: true,
            recursive: false,
            ignore_warnings: false,
            ignore_nifti_headers: false,
            dataset_types: Vec::new(),
            blacklist_modalities: Vec::new(),
            include_datatypes: None,
        }
    }
}

/// Findings for a dataset, serializable in the same format as CLI JSON.
#[derive(Debug, Serialize)]
pub struct ValidationReport {
    /// Issues in this dataset, excluding its derivatives.
    pub issues: IssueList,
    /// Reports keyed by derivative directory name when recursion is enabled.
    #[serde(
        rename = "derivativesSummary",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub derivatives_summary: BTreeMap<String, ValidationReport>,
}

impl ValidationReport {
    /// Whether this dataset or any recursively validated derivative has errors.
    pub fn has_errors(&self) -> bool {
        self.issues
            .issues
            .iter()
            .any(|issue| issue.severity == Severity::Error)
            || self.derivatives_summary.values().any(Self::has_errors)
    }
}

/// Validate a dataset root on disk using the bundled BIDS schema.
///
/// BIDS violations are returned as `Ok(report)`. `Err` means validation could
/// not run to completion, for example because `root` could not be resolved.
pub fn validate_path(
    root: impl AsRef<Path>,
    options: &ValidationOptions,
) -> Result<ValidationReport> {
    if let Some(datatypes) = &options.include_datatypes {
        if datatypes.is_empty() {
            bail!("include_datatypes must contain at least one BIDS datatype");
        }
        for datatype in datatypes {
            if !bids_schema_codegen::DATATYPES.contains(&datatype.as_str()) {
                bail!("unknown BIDS datatype: {datatype:?}");
            }
        }
    }
    let root = root.as_ref();
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("dataset path not found: {}", root.display()))?;
    let mut visited = HashSet::new();
    validate_path_inner(&canonical_root, options, &mut visited, 0)
}

fn validate_path_inner(
    root: &Path,
    options: &ValidationOptions,
    visited: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<ValidationReport> {
    if !visited.insert(root.to_path_buf()) {
        bail!("recursive derivative cycle detected at {}", root.display());
    }

    let mut result = validate_single_dataset(root, options);

    if options.recursive {
        if depth >= MAX_RECURSIVE_DERIVATIVE_DEPTH {
            bail!(
                "recursive derivative depth exceeds {} at {}",
                MAX_RECURSIVE_DERIVATIVE_DEPTH,
                root.display()
            );
        }
        for (name, path) in derivative_roots(root, options.follow_symlink_dirs) {
            if visited.contains(&path) {
                continue;
            }
            let derivative = validate_path_inner(&path, options, visited, depth + 1)
                .with_context(|| format!("failed to validate derivative {name}"))?;
            result.derivatives_summary.insert(name, derivative);
        }
    }

    if options.ignore_warnings {
        retain_errors_only(&mut result);
    }

    Ok(result)
}

fn validate_single_dataset(root: &Path, options: &ValidationOptions) -> ValidationReport {
    let policy = ContentPolicy::new(options.content_mode);
    let cache = bids_core::content::ContentCache::new(policy);
    let bidsignore = load_bidsignore(root);
    let dataset_type = load_dataset_type(root, &cache);

    let Discovery {
        files,
        modalities_seen,
        root_dirs,
        mut link_issues,
    } = discover_dataset(
        root,
        bidsignore.as_ref(),
        options.follow_symlink_dirs,
        dataset_type_name(dataset_type),
    );

    let index = build_index(&files);
    let dataset_modalities: Vec<String> = modalities_seen.into_iter().collect();
    let dataset_subjects = load_subjects(root, root_dirs, &policy);

    // CLI filter that needs the file list runs *before* we hand the
    // files to the dataset context, so we can move-instead-of-clone.
    // (Order matters: the blacklist emission used to run *after*
    // validate_dataset and needed a separate borrow of `files`, which
    // forced a clone.)
    let mut blacklist_issues = Vec::new();
    apply_blacklist_modalities(&mut blacklist_issues, &files, &options.blacklist_modalities);

    let ctx = DatasetContext {
        root: root.to_path_buf(),
        files,
        index,
        dataset_type,
        dataset_modalities,
        dataset_subjects,
        ignore_nifti_headers: options.ignore_nifti_headers,
        cache,
    };
    let mut result = if let Some(selected) = &options.include_datatypes {
        // These findings are emitted outside the per-file loop and their location
        // may itself be a datatype directory (including an unresolvable link).
        blacklist_issues
            .retain(|issue| scoped_relative_path(&issue.location, selected) == Some(""));
        validate_datatypes(ctx, selected, &mut link_issues)
    } else {
        validate_dataset(&ctx)
    };
    apply_dataset_type_filter(
        &mut result.issues.issues,
        dataset_type,
        &options.dataset_types,
    );
    result.issues.issues.extend(blacklist_issues);
    result.issues.issues.extend(link_issues);

    ValidationReport {
        issues: result.issues,
        derivatives_summary: BTreeMap::new(),
    }
}

fn validate_datatypes(
    mut ctx: DatasetContext,
    selected: &[String],
    links: &mut Vec<Issue>,
) -> ValidationResult {
    let matches = |file: &DiscoveredFile| {
        scoped_relative_path(&file.bids_path, selected).is_some_and(|relative| !relative.is_empty())
    };
    let (result, dependencies) = validate_matching_files(&ctx, matches);
    let in_scope = |link: &Issue| scoped_relative_path(&link.location, selected).is_some();
    let json_links = links.iter().filter(|link| link.location.ends_with(".json"));
    if json_links.clone().all(in_scope) {
        links.retain(in_scope);
        return result;
    }

    // Replay filename-only inheritance in directories with broken JSON links. This
    // happens after validation so unresolved links can never satisfy an exists() check.
    let file_count = ctx.files.len();
    let mut candidates = DirIndex::new();
    for link in json_links {
        let (dir, name) = link.location.rsplit_once('/').expect("absolute BIDS path");
        let dir = if dir.is_empty() { "/" } else { dir };
        // Include real siblings: an exact entity match takes precedence.
        candidates
            .entry(dir.to_string())
            .or_insert_with(|| ctx.index.remove(dir).unwrap_or_default())
            .push(ctx.files.len());
        ctx.files.push(DiscoveredFile {
            parsed: parse_filename(name),
            fs_path: ctx.root.join(link.location.trim_start_matches('/')),
            bids_path: link.location.clone(),
            datatype: String::new(),
        });
    }
    let inherited: HashSet<_> = ctx.files[..file_count]
        .iter()
        .filter(|file| matches(file) || dependencies.contains(&file.fs_path))
        .flat_map(|file| sidecar_candidates_with_issues(&ctx.files, file, &candidates).0)
        .filter(|&i| i >= file_count)
        .map(|i| ctx.files[i].bids_path.as_str())
        .collect();
    links.retain(|link| in_scope(link) || inherited.contains(link.location.as_str()));
    result
}

/// Path within a selected datatype directory (empty for the directory itself),
/// or within the dataset root for shared metadata.
fn scoped_relative_path<'a>(path: &'a str, datatypes: &[String]) -> Option<&'a str> {
    let relative = path.trim_start_matches('/');
    if matches!(path, "/dataset_description.json" | "/participants.tsv") {
        return Some(relative);
    }
    let (root, rest) = relative.split_once('/').unwrap_or((relative, ""));
    let after_optional_group = |prefix| match rest.strip_prefix(prefix) {
        Some(group) => group.split_once('/').map(|(_, tail)| tail),
        None => Some(rest),
    };
    let datatype_path = match root {
        "phenotype" => relative,
        _ if root.starts_with("sub-") => after_optional_group("ses-")?,
        _ if root.starts_with("tpl-") => after_optional_group("cohort-")?,
        _ => return None,
    };
    let (datatype, relative) = datatype_path.split_once('/').unwrap_or((datatype_path, ""));
    datatypes.iter().any(|d| d == datatype).then_some(relative)
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

fn retain_errors_only(result: &mut ValidationReport) {
    result
        .issues
        .issues
        .retain(|issue| issue.severity == Severity::Error);
    for derivative in result.derivatives_summary.values_mut() {
        retain_errors_only(derivative);
    }
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

        let no_recursive_result = validate_path(root, &ValidationOptions::default()).unwrap();
        assert!(no_recursive_result.derivatives_summary.is_empty());

        let recursive = ValidationOptions {
            recursive: true,
            ..Default::default()
        };
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

        let recursive = ValidationOptions {
            recursive: true,
            ..Default::default()
        };
        let result = validate_path(root, &recursive).unwrap();
        assert!(result.derivatives_summary.is_empty());
    }

    #[test]
    fn ignore_warnings_removes_warning_only_issues() {
        let mut result = ValidationReport {
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
        assert!(result.has_errors());
    }
}
