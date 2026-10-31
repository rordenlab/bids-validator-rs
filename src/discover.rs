//! Dataset filesystem discovery: walks the tree, applies `.bidsignore`,
//! recognizes datatypes/modalities, and registers derivative roots.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use walkdir::WalkDir;

use bids_core::filename::parse_filename;
use bids_core::issue::{Issue, Severity};
use bids_core::run::DiscoveredFile;
use bids_schema_codegen::modality_of;

use crate::cli::LinkMode;

pub(crate) struct Discovery {
    pub(crate) files: Vec<DiscoveredFile>,
    pub(crate) modalities_seen: BTreeSet<String>,
    pub(crate) root_dirs: BTreeSet<String>,
    /// `SYMLINK_BROKEN` / `SYMLINK_CYCLE` for unresolvable links (Deno 3.x
    /// `walk.ts`); such links are not registered as files.
    pub(crate) link_issues: Vec<Issue>,
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
    dataset_type: &str,
) -> Discovery {
    let opaque = opaque_dirs(dataset_type);
    let mut files: Vec<DiscoveredFile> = Vec::new();
    let mut modalities_seen: BTreeSet<String> = BTreeSet::new();
    let mut root_dirs: BTreeSet<String> = BTreeSet::new();
    let mut link_issues: Vec<Issue> = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(link_mode.follows_dirs())
        .into_iter()
        .filter_entry(|e| !is_skip_dir(e.path(), root, &opaque))
    {
        // walkdir reports broken symlinks (and, when following, directory
        // loops) as `Err`. Recover the path and classify the link below.
        // Errors without a path are skipped silently.
        let (entry_path, file_type, dir_loop) = match entry {
            Ok(e) => (e.path().to_path_buf(), Some(e.file_type()), false),
            Err(err) => match err.path() {
                Some(p) => (p.to_path_buf(), None, err.loop_ancestor().is_some()),
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
            // `no-follow` sees the link itself: drop symlinked directories.
            if ft.is_symlink() && is_symlink_to_dir(&entry_path) {
                continue;
            }
            if !ft.is_file() && !ft.is_symlink() {
                continue;
            }
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

        let maybe_link = file_type.is_none_or(|ft| ft.is_symlink());
        if let Some(code) = maybe_link
            .then(|| unresolved_link_code(&entry_path, dir_loop))
            .flatten()
        {
            let target = fs::read_link(&entry_path).unwrap_or_default();
            link_issues.push(Issue {
                code: code.to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: bids_path,
                rule: None,
                issue_message: Some(format!("Symlink target: {}", target.display())),
                affects: None,
                line: None,
                code_message: None,
            });
            continue;
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
        link_issues,
    }
}

/// Classify an unresolvable symlink like Deno 3.x `readFileTree`: git-annex
/// pointers (target basename is an annex key) are files even when their
/// content is absent; other dangling links are `SYMLINK_BROKEN`, ELOOP is
/// `SYMLINK_CYCLE`. A walkdir directory loop is reported as a cycle at the
/// first looping link (Deno only fails once the kernel's symlink depth limit
/// is hit, deeper in the repeated tree).
fn unresolved_link_code(path: &Path, dir_loop: bool) -> Option<&'static str> {
    if dir_loop {
        return Some("SYMLINK_CYCLE");
    }
    let target = fs::read_link(path).ok()?;
    let err = fs::metadata(path).err()?;
    if target
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(is_annex_key)
    {
        return None;
    }
    if err.kind() == std::io::ErrorKind::NotFound {
        return Some("SYMLINK_BROKEN");
    }
    (err.raw_os_error() == Some(ELOOP)).then_some("SYMLINK_CYCLE")
}

#[cfg(target_os = "linux")]
const ELOOP: i32 = 40;
#[cfg(not(target_os = "linux"))]
const ELOOP: i32 = 62; // macOS / BSD

/// `annexKeyRegex` in upstream `src/files/repo.ts`:
/// `^[A-Z0-9]+-s\d+--[0-9a-fA-F]+`.
fn is_annex_key(name: &str) -> bool {
    let Some((backend, rest)) = name.split_once("-s") else {
        return false;
    };
    let Some((size, digest)) = rest.split_once("--") else {
        return false;
    };
    !backend.is_empty()
        && backend
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && !size.is_empty()
        && size.bytes().all(|b| b.is_ascii_digit())
        && digest.bytes().next().is_some_and(|b| b.is_ascii_hexdigit())
}

fn is_symlink_to_dir(path: &Path) -> bool {
    path.metadata().map(|md| md.is_dir()).unwrap_or(false)
}

/// Skip directories that don't contribute to validation in default mode:
///   * `.git`, `.datalad` — version-control metadata
///   * root-level opaque directories (`rules.directories.<DatasetType>`,
///     e.g. `docs/`, `logs/`) — Deno validates them as one unit and does
///     not descend. This includes `derivatives`: Deno strips it from the
///     main tree and, with `--recursive`, validates conformant derivative
///     datasets separately (see `derivative_roots`).
fn is_skip_dir(p: &Path, root: &Path, opaque: &BTreeSet<String>) -> bool {
    if p == root {
        return false;
    }
    let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if p.parent() == Some(root) && (name == "derivatives" || opaque.contains(name)) {
        return true;
    }
    matches!(name, ".git" | ".datalad")
}

fn opaque_dirs(dataset_type: &str) -> BTreeSet<String> {
    let Some(rules) = bids_schema_codegen::schema()
        .rules
        .directories
        .get(dataset_type)
        .and_then(|v| v.as_object())
    else {
        return BTreeSet::new();
    };
    rules
        .values()
        .filter(|r| r.get("opaque").and_then(|v| v.as_bool()) == Some(true))
        .filter_map(|r| r.get("name").and_then(|v| v.as_str()))
        .map(str::to_string)
        .collect()
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

/// Build the ignore matcher: the BIDS default ignores and prunes (matching
/// `src/files/ignore.ts::ignoreDefaults`; `.*` stands in for the `.**`
/// prune) plus any `<root>/.bidsignore` lines. Always returns a matcher
/// (defaults make it useful even without `.bidsignore`).
pub(crate) fn load_bidsignore(root: &Path) -> Option<Gitignore> {
    const DEFAULTS: &[&str] = &[
        ".*",
        "/sourcedata/",
        "/derivatives/",
        "/code/",
        "/stimuli/",
        "/log/",
        "/doc/",
    ];
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
    fn parity_traverses_symlinked_subject_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = make_symlinked_subject_dataset(tmp.path());

        let discovery = discover_dataset(&root, None, LinkMode::Parity, "raw");
        let paths = discovered_paths(&discovery);
        assert!(paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
        assert!(discovery.root_dirs.contains("sub-01"));
    }

    #[test]
    #[cfg(unix)]
    fn follow_traverses_symlinked_subject_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = make_symlinked_subject_dataset(tmp.path());

        let discovery = discover_dataset(&root, None, LinkMode::Follow, "raw");
        let paths = discovered_paths(&discovery);
        assert!(paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
        assert!(discovery.root_dirs.contains("sub-01"));
    }

    #[test]
    #[cfg(unix)]
    fn no_follow_skips_symlinked_subject_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = make_symlinked_subject_dataset(tmp.path());

        let discovery = discover_dataset(&root, None, LinkMode::NoFollow, "raw");
        let paths = discovered_paths(&discovery);
        assert!(!paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
        assert!(!discovery.root_dirs.contains("sub-01"));
    }

    #[test]
    #[cfg(unix)]
    fn broken_symlink_is_reported_not_registered() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("ds");
        let anat = root.join("sub-01").join("anat");
        fs::create_dir_all(&anat).unwrap();
        symlink(
            tmp.path().join("missing.nii.gz"),
            anat.join("sub-01_T1w.nii.gz"),
        )
        .unwrap();
        symlink("loop.nii.gz", anat.join("loop.nii.gz")).unwrap();
        // Absent git-annex content is still a file, as in Deno.
        symlink(
            "../../.git/annex/objects/Xx/Yy/MD5E-s4--0123abcd.nii.gz/MD5E-s4--0123abcd.nii.gz",
            anat.join("sub-01_T2w.nii.gz"),
        )
        .unwrap();

        for mode in [LinkMode::Parity, LinkMode::NoFollow] {
            let discovery = discover_dataset(&root, None, mode, "raw");
            let paths = discovered_paths(&discovery);
            assert!(!paths.contains(&"/sub-01/anat/sub-01_T1w.nii.gz".to_string()));
            assert!(paths.contains(&"/sub-01/anat/sub-01_T2w.nii.gz".to_string()));
            let mut links = discovery
                .link_issues
                .iter()
                .map(|i| (i.code.as_str(), i.location.as_str()))
                .collect::<Vec<_>>();
            links.sort();
            assert_eq!(
                links,
                vec![
                    ("SYMLINK_BROKEN", "/sub-01/anat/sub-01_T1w.nii.gz"),
                    ("SYMLINK_CYCLE", "/sub-01/anat/loop.nii.gz"),
                ],
                "{mode:?}"
            );
        }
    }

    /// Deno 3.x: ignore defaults are root-anchored (`/code/`, `/doc/`, …)
    /// and root opaque directories (`docs/`, `logs/`) are not descended.
    #[test]
    fn root_anchored_ignores_and_opaque_dirs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        for rel in [
            "doc/a.txt",
            "docs/a.txt",
            "logs/a.txt",
            "code/a.txt",
            "sub-01/code/a.txt",
            "sub-01/docs/a.txt",
            ".hidden/a.txt",
        ] {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        let gi = load_bidsignore(root);
        let discovery = discover_dataset(root, gi.as_ref(), LinkMode::Parity, "raw");
        let mut paths = discovered_paths(&discovery);
        paths.sort();
        assert_eq!(paths, vec!["/sub-01/code/a.txt", "/sub-01/docs/a.txt"]);
    }

    #[test]
    fn annex_key_matches_upstream_regex() {
        assert!(is_annex_key("SHA256E-s1234--abcdef.nii.gz"));
        assert!(is_annex_key("MD5-s0--0a"));
        assert!(!is_annex_key("SHA256E-s1234-m99--abcdef"));
        assert!(!is_annex_key("sha256e-s1--ab"));
        assert!(!is_annex_key("missing.nii.gz"));
    }
}
