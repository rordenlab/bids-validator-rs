//! Dataset filesystem discovery: walks the tree, applies `.bidsignore`,
//! recognizes datatypes/modalities, and registers derivative roots.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use walkdir::WalkDir;

use bids_core::filename::parse_filename;
use bids_core::run::DiscoveredFile;
use bids_schema_codegen::modality_of;

use crate::cli::LinkMode;

pub(crate) struct Discovery {
    pub(crate) files: Vec<DiscoveredFile>,
    pub(crate) modalities_seen: BTreeSet<String>,
    pub(crate) root_dirs: BTreeSet<String>,
}

pub(crate) fn derivative_roots(root: &Path, link_mode: LinkMode) -> Vec<(String, PathBuf)> {
    let derivatives = root.join("derivatives");
    let Ok(entries) = fs::read_dir(&derivatives) else {
        return Vec::new();
    };
    let Ok(canonical_derivatives) = derivatives.canonicalize() else {
        return Vec::new();
    };
    if !canonical_derivatives.starts_with(root) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() && !link_mode.follows_dirs() {
            continue;
        }
        if !path.is_dir() || !path.join("dataset_description.json").is_file() {
            continue;
        }
        let Ok(canonical_path) = path.canonicalize() else {
            continue;
        };
        if !canonical_path.starts_with(&canonical_derivatives) {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push((name.to_string(), canonical_path));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Enumerate files and gather summary inputs for sidecar inheritance and
/// dataset-level selectors.
pub(crate) fn discover_dataset(
    root: &Path,
    bidsignore: Option<&Gitignore>,
    link_mode: LinkMode,
) -> Discovery {
    let mut files: Vec<DiscoveredFile> = Vec::new();
    let mut modalities_seen: BTreeSet<String> = BTreeSet::new();
    let mut root_dirs: BTreeSet<String> = BTreeSet::new();

    for entry in WalkDir::new(root)
        .follow_links(link_mode.follows_dirs())
        .into_iter()
        .filter_entry(|e| !is_skip_dir(e.path(), root))
    {
        // walkdir reports broken symlinks as `Err` when following links.
        // Recover the path and register the file anyway so filename checks
        // and sidecar lookup still see it. Cycles and other errors without a
        // path are skipped silently.
        let (entry_path, file_type) = match entry {
            Ok(e) => (e.path().to_path_buf(), Some(e.file_type())),
            Err(err) => match err.path() {
                Some(p) => (p.to_path_buf(), None),
                None => continue,
            },
        };
        if let Some(ft) = file_type {
            if ft.is_dir() {
                if entry_path.parent() == Some(root) {
                    if let Some(name) = entry_path.file_name().and_then(|s| s.to_str()) {
                        root_dirs.insert(name.to_string());
                    }
                }
                continue;
            }
            if ft.is_symlink() && is_symlink_to_dir(&entry_path) {
                if !link_mode.registers_symlinked_dirs() {
                    continue;
                }
                // In Deno-compatible parity mode, symlinked directories
                // are seen as path entries but not traversed. Register
                // them for filename validation (for example NOT_INCLUDED
                // on `/sub-01`) without adding them to subject root dirs.
                if link_mode.follows_dirs() {
                    continue;
                }
            } else if !ft.is_file() && !ft.is_symlink() {
                continue;
            }
            // No further metadata check: ft (from walkdir's lstat) is
            // already the symlink's own type. Symlinks to directories
            // were handled at the `is_symlink_to_dir` branch above; any
            // remaining symlink targets a file or is broken, and either
            // way should register for filename validation. A redundant
            // `symlink_metadata()` here was costing one syscall per
            // entry without changing the outcome.
        }

        // Reject entries that don't sit under the canonical dataset root.
        // walkdir normally yields only descendants, but error recovery can
        // expose resolved target paths. Without this guard, downstream
        // validators could reason about absolute or ../-relative bids paths.
        let rel = match entry_path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let bids_path = format!("/{rel}");

        if let Some(gi) = bidsignore {
            if gi.matched_path_or_any_parents(&rel, false).is_ignore() {
                continue;
            }
        }

        let datatype = infer_datatype(&bids_path);
        if let Some(m) = modality_of(&datatype) {
            // Match Deno's `Summary.modalities`, which maps lowercase
            // modality codes to pretty names (mri→MRI, pet→PET, …) before
            // exposing them via `dataset.modalities`. The selector
            // `intersects(dataset.modalities, ["pet"])` is checking for
            // these pretty names, not the raw codes.
            modalities_seen.insert(pretty_modality(m).to_string());
        }
        let name = bids_path
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(bids_path.as_str());
        let parsed = parse_filename(name);
        files.push(DiscoveredFile {
            bids_path,
            fs_path: entry_path,
            datatype,
            parsed,
        });
    }

    Discovery {
        files,
        modalities_seen,
        root_dirs,
    }
}

fn is_symlink_to_dir(path: &Path) -> bool {
    path.metadata().map(|md| md.is_dir()).unwrap_or(false)
}

/// Skip directories that don't contribute to validation in default mode:
///   * `.git`, `.datalad` — version-control metadata
///   * `derivatives` — Deno validator strips these from the main dataset
///     tree and, when `--recursive` is set, validates conformant derivative
///     datasets as separate results. Rust mirrors that by always pruning the
///     root `derivatives/` directory here and discovering derivative roots in
///     `derivative_roots`.
fn is_skip_dir(p: &Path, root: &Path) -> bool {
    if p == root {
        return false;
    }
    let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    // Only prune `derivatives` at the dataset root, not deep matches.
    if name == "derivatives" && p.parent() == Some(root) {
        return true;
    }
    matches!(name, ".git" | ".datalad")
}

/// Mirrors `modalityPrettyLookup` in `src/summary/summary.ts`. Modalities
/// not in the table fall through unchanged (matches the TS map-or-id
/// behavior). TS schema selectors like `intersects(dataset.modalities,
/// ["pet"])` use *lowercase* codes, while `Summary.modalities` produces
/// *pretty* names. `context.dataset.modalities` is currently never
/// populated from the summary at selector-eval time in 2.4.1 (audited
/// 2026-05-18 against the pinned vendor), so every such selector
/// evaluates against `[]` in Deno. Our pretty-name population is
/// parity-neutral today; revisit if upstream wires up
/// `dsContext.modalities` from `Summary.modalities`.
fn pretty_modality(m: &str) -> &str {
    match m {
        "mri" => "MRI",
        "pet" => "PET",
        "meg" => "MEG",
        "eeg" => "EEG",
        "ieeg" => "iEEG",
        "micro" => "Microscopy",
        other => other,
    }
}

/// Build the ignore matcher: the BIDS default ignores (matching
/// `src/files/ignore.ts::defaultIgnores`) plus any `<root>/.bidsignore` lines.
/// Always returns a matcher (defaults make it useful even without
/// `.bidsignore`).
pub(crate) fn load_bidsignore(root: &Path) -> Option<Gitignore> {
    const DEFAULTS: &[&str] = &[".git**", ".*", "sourcedata/", "code/", "stimuli/", "log/"];
    let mut builder = GitignoreBuilder::new(root);
    for pat in DEFAULTS {
        let _ = builder.add_line(None, pat);
    }
    let path = root.join(".bidsignore");
    if path.exists() {
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                let _ = builder.add_line(None, line);
            }
        }
    }
    builder.build().ok()
}

/// Best-effort datatype inference: BIDS datatypes appear as the directory
/// component immediately containing the file (e.g. `/sub-01/anat/...nii.gz` →
/// `"anat"`). Returns `""` if not recognized. Used only for tie-breaking in
/// rule matching today.
pub(crate) fn infer_datatype(bids_path: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[test]
    fn infer_anat() {
        assert_eq!(infer_datatype("/sub-01/anat/sub-01_T1w.nii.gz"), "anat");
        assert_eq!(
            infer_datatype("/sub-01/ses-02/func/sub-01_ses-02_task-x_bold.nii"),
            "func"
        );
        assert_eq!(infer_datatype("/dataset_description.json"), "");
        assert_eq!(infer_datatype("/sub-01/emg/sub-01_channels.tsv"), "emg");
    }

    #[cfg(unix)]
    fn make_symlinked_subject_dataset(tmp: &Path) -> PathBuf {
        let root = tmp.join("ds");
        let target_subject = tmp.join("external").join("sub-01");
        let target_anat = target_subject.join("anat");
        fs::create_dir_all(&target_anat).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(target_anat.join("sub-01_T1w.nii.gz"), b"").unwrap();
        symlink(&target_subject, root.join("sub-01")).unwrap();
        root
    }

    fn discovered_paths(discovery: &Discovery) -> Vec<String> {
        discovery
            .files
            .iter()
            .map(|f| f.bids_path.clone())
            .collect()
    }

    #[test]
    #[cfg(unix)]
    fn parity_registers_symlinked_subject_dir_without_traversing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = make_symlinked_subject_dataset(tmp.path());

        let discovery = discover_dataset(&root, None, LinkMode::Parity);
        let paths = discovered_paths(&discovery);
        assert!(paths.contains(&"/sub-01".to_string()));
        assert!(!paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
        assert!(!discovery.root_dirs.contains("sub-01"));
    }

    #[test]
    #[cfg(unix)]
    fn follow_traverses_symlinked_subject_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = make_symlinked_subject_dataset(tmp.path());

        let discovery = discover_dataset(&root, None, LinkMode::Follow);
        let paths = discovered_paths(&discovery);
        assert!(paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
        assert!(discovery.root_dirs.contains("sub-01"));
    }

    #[test]
    #[cfg(unix)]
    fn no_follow_skips_symlinked_subject_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = make_symlinked_subject_dataset(tmp.path());

        let discovery = discover_dataset(&root, None, LinkMode::NoFollow);
        let paths = discovered_paths(&discovery);
        assert!(!paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
        assert!(!discovery.root_dirs.contains("sub-01"));
    }

    #[test]
    #[cfg(unix)]
    fn no_follow_registers_broken_symlink_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("ds");
        let anat = root.join("sub-01").join("anat");
        fs::create_dir_all(&anat).unwrap();
        symlink(
            tmp.path().join("missing.nii.gz"),
            anat.join("sub-01_T1w.nii.gz"),
        )
        .unwrap();

        let discovery = discover_dataset(&root, None, LinkMode::NoFollow);
        let paths = discovered_paths(&discovery);
        assert!(paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
        assert!(discovery.root_dirs.contains("sub-01"));
    }
}
