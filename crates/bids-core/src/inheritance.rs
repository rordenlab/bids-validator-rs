//! Sidecar inheritance walker — finds JSON sidecars that apply to a data
//! file via the BIDS Inheritance Principle.
//!
//! Mirrors `walkBack` + `readSidecars` in `src/files/inheritance.ts`.
//! The CLI walks the dataset once and builds a directory → files map;
//! this module then, for each data file, walks up its ancestor
//! directories looking for sidecars whose entities are a subset of the
//! source file's and whose suffix matches.

use std::collections::BTreeMap;

use serde_json::Value as J;

use crate::filename::ParsedFilename;
use crate::issue::{Issue, Severity};
use crate::run::DiscoveredFile;

/// Indexed view of the dataset's tree of files, grouped by parent
/// directory (POSIX dataset-relative path, e.g. `/sub-01/anat`).
/// Values are indices into the canonical `Vec<DiscoveredFile>` arena
/// (`DatasetContext.files`). Index lookup is O(1) and indices are
/// `Copy` — friendly to future parallel access.
pub type DirIndex = BTreeMap<String, Vec<usize>>;

pub struct SidecarReadResult {
    pub loaded: Vec<(std::path::PathBuf, J)>,
    pub issues: Vec<Issue>,
    pub candidates: Vec<std::path::PathBuf>,
}

/// Build a `DirIndex` from the canonical file arena. Each entry's
/// position in `arena` becomes its index value in the map. The arena's
/// `bids_path` field already carries the dataset-relative path; no
/// strings are duplicated into the index.
pub fn build_index(arena: &[DiscoveredFile]) -> DirIndex {
    let mut idx: DirIndex = BTreeMap::new();
    for (i, f) in arena.iter().enumerate() {
        let parent = f
            .bids_path
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap_or("/");
        idx.entry(parent.to_string()).or_default().push(i);
    }
    idx
}

/// Walk up from `source` collecting sidecar candidate arena indices
/// plus any `MULTIPLE_INHERITABLE_FILES` issues produced during the
/// walk (no I/O). Returned in closest-first order. Mirrors
/// the TS validator's `walkBack` throw in `src/files/inheritance.ts`:
/// when a directory level has multiple sidecar candidates and no
/// exact-entity match, emit one issue with `location` = the lex-first
/// candidate path and `affects` = the source data file path.
pub fn sidecar_candidates_with_issues(
    arena: &[DiscoveredFile],
    source: &DiscoveredFile,
    index: &DirIndex,
) -> (Vec<usize>, Vec<Issue>) {
    let target_suffix = &source.parsed.suffix;
    let mut out: Vec<usize> = Vec::new();
    let mut issues: Vec<Issue> = Vec::new();

    let mut dir = parent_dir(&source.bids_path);
    loop {
        if let Some(file_idxs) = index.get(&dir) {
            let mut candidates: Vec<usize> = file_idxs
                .iter()
                .copied()
                .filter(|&i| {
                    let f = &arena[i];
                    f.parsed.extension == ".json"
                        && f.parsed.suffix == *target_suffix
                        && entities_subset(&f.parsed, &source.parsed)
                })
                .collect();

            let chosen: Option<usize> = if candidates.len() > 1 {
                if let Some(exact) = candidates
                    .iter()
                    .copied()
                    .find(|&i| arena[i].parsed.entities == source.parsed.entities)
                {
                    Some(exact)
                } else {
                    // Sort by path for a stable issue `location`.
                    candidates.sort_by(|&a, &b| arena[a].bids_path.cmp(&arena[b].bids_path));
                    let paths: Vec<String> = candidates
                        .iter()
                        .map(|&i| arena[i].bids_path.clone())
                        .collect();
                    issues.push(Issue {
                        code: "MULTIPLE_INHERITABLE_FILES".to_string(),
                        sub_code: None,
                        severity: Severity::Error,
                        location: paths[0].clone(),
                        rule: None,
                        issue_message: Some(format!("Candidate files: {}", paths.join(","))),
                        affects: Some(vec![source.bids_path.clone()]),
                        line: None,
                        code_message: None,
                    });
                    // Continue with the lex-first candidate (TS aborts the
                    // walk via throw; for our purposes the issue is the
                    // important output, and continuing yields more useful
                    // downstream sidecar coverage).
                    candidates.first().copied()
                }
            } else {
                candidates.first().copied()
            };

            if let Some(i) = chosen {
                out.push(i);
            }
        }
        if dir == "/" {
            break;
        }
        dir = parent_dir(&dir);
    }

    (out, issues)
}

/// Read + parse each candidate sidecar's JSON content, gated by
/// `policy` (sidecars that are git-annex symlinks under parity mode
/// are skipped to match Deno's `loadJSON` catch-arm). Surfaces both
/// inheritance-walker issues (currently `MULTIPLE_INHERITABLE_FILES`)
/// and per-sidecar `JSON_NOT_AN_OBJECT` issues (audit A-L3): pre-fix,
/// an inherited sidecar whose body was `[]` / `42` / null was
/// silently dropped, leaving downstream required-field checks unable
/// to tell whether the bad sidecar would have supplied the missing
/// key. Mirrors `readSidecars` in `src/files/inheritance.ts`.
pub fn read_sidecars_with_issues(
    arena: &[DiscoveredFile],
    source: &DiscoveredFile,
    index: &DirIndex,
    cache: &crate::content::ContentCache,
) -> (Vec<(std::path::PathBuf, J)>, Vec<Issue>) {
    let result = read_sidecars_with_candidates(arena, source, index, cache);
    (result.loaded, result.issues)
}

/// Read sidecars and also return every selected candidate path, including
/// unreadable, invalid, and non-object JSON files. The orphan-sidecar pass
/// needs candidate selection, not successful parse, to decide whether a
/// sidecar corresponds to a data file.
pub fn read_sidecars_with_candidates(
    arena: &[DiscoveredFile],
    source: &DiscoveredFile,
    index: &DirIndex,
    cache: &crate::content::ContentCache,
) -> SidecarReadResult {
    let (candidate_idxs, mut issues) = sidecar_candidates_with_issues(arena, source, index);
    let mut loaded = Vec::with_capacity(candidate_idxs.len());
    let candidate_paths = candidate_idxs
        .iter()
        .map(|&i| arena[i].fs_path.clone())
        .collect::<Vec<_>>();
    for &i in &candidate_idxs {
        let c = &arena[i];
        if c.fs_path == source.fs_path {
            continue;
        }
        let Some(parsed) = cache.read_json(&c.fs_path) else {
            // Read failure, parse failure, or policy refusal — silent,
            // matches Deno's `loadJSON` catch-arm. `JSON_INVALID` is
            // emitted only when the file is the direct target of
            // validation (run.rs), not for inherited sidecars.
            continue;
        };
        if !parsed.is_object() {
            // A non-object body would otherwise be silently dropped by
            // `merge_with_origins` / `merge_sidecars` (which only
            // descend into `J::Object`). Surface it as JSON_NOT_AN_OBJECT
            // at the *inherited* sidecar's path, with the data file
            // recorded in `affects`.
            issues.push(Issue {
                code: "JSON_NOT_AN_OBJECT".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: c.bids_path.clone(),
                rule: None,
                issue_message: None,
                affects: Some(vec![source.bids_path.clone()]),
                line: None,
                code_message: None,
            });
            continue;
        }
        loaded.push((c.fs_path.clone(), (*parsed).clone()));
    }
    SidecarReadResult {
        loaded,
        issues,
        candidates: candidate_paths,
    }
}

/// Merge sidecars in inheritance order: caller passes them with the closest
/// ancestor LAST so it wins. (`read_sidecars` returns them closest-first;
/// reverse before passing.)
pub fn merge_sidecars(sidecars: impl IntoIterator<Item = J>) -> J {
    let mut merged = serde_json::Map::new();
    for s in sidecars {
        if let J::Object(map) = s {
            for (k, v) in map {
                merged.insert(k, v);
            }
        }
    }
    J::Object(merged)
}

/// Merge sidecars and also track which file contributed each top-level key.
/// Returns `(merged_value, key_origin)`. `sidecars` should be ordered with
/// the closest ancestor LAST.
pub fn merge_with_origins(
    sidecars: impl IntoIterator<Item = (String, J)>,
) -> (J, std::collections::HashMap<String, String>) {
    let mut merged = serde_json::Map::new();
    let mut origin: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (bids_path, s) in sidecars {
        if let J::Object(map) = s {
            for (k, v) in map {
                origin.insert(k.clone(), bids_path.clone());
                merged.insert(k, v);
            }
        }
    }
    (J::Object(merged), origin)
}

/// Merge sidecars from closest to farthest ancestor and emit
/// `SIDECAR_FIELD_OVERRIDE` warnings for conflicting inherited keys.
///
/// Mirrors `BIDSContext.loadSidecar` in TS:
/// - closest sidecar values win;
/// - `sidecarKeyOrigin` remains the closest file that defined a key;
/// - every farther sidecar that defines a different value for an
///   already-defined key emits a warning located at the closest origin.
pub fn merge_with_origins_and_override_issues(
    sidecars_closest_first: impl IntoIterator<Item = (String, J)>,
) -> (J, std::collections::HashMap<String, String>, Vec<Issue>) {
    let mut merged = serde_json::Map::new();
    let mut origin: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut issues = Vec::new();

    for (bids_path, s) in sidecars_closest_first {
        let J::Object(map) = s else {
            continue;
        };

        for (k, v) in &map {
            if let Some(existing) = merged.get(k) {
                if js_strict_not_equal(v, existing) {
                    let override_location = origin.get(k).cloned().unwrap_or_default();
                    issues.push(Issue {
                        code: "SIDECAR_FIELD_OVERRIDE".to_string(),
                        sub_code: Some(k.clone()),
                        severity: Severity::Warning,
                        location: override_location.clone(),
                        rule: None,
                        issue_message: Some(format!(
                            "Sidecar key defined in {bids_path} overrides previous value ({}) from {override_location}",
                            json_value_for_message(v)
                        )),
                        affects: None,
                        line: None,
                        code_message: None,
                    });
                }
            }
        }

        for (k, v) in map {
            origin.entry(k.clone()).or_insert_with(|| bids_path.clone());
            merged.entry(k).or_insert(v);
        }
    }

    (J::Object(merged), origin, issues)
}

fn js_strict_not_equal(a: &J, b: &J) -> bool {
    !match (a, b) {
        (J::Null, J::Null) => true,
        (J::Bool(x), J::Bool(y)) => x == y,
        (J::Number(x), J::Number(y)) => x.as_f64() == y.as_f64(),
        (J::String(x), J::String(y)) => x == y,
        // JSON arrays/objects are parsed from separate files, so TS
        // `!==` treats them as different object identities even when
        // their serialized contents match.
        (J::Array(_), J::Array(_)) | (J::Object(_), J::Object(_)) => false,
        _ => false,
    }
}

fn json_value_for_message(v: &J) -> String {
    match v {
        J::String(s) => s.clone(),
        J::Array(xs) => xs
            .iter()
            .map(json_value_for_message)
            .collect::<Vec<_>>()
            .join(","),
        J::Object(_) => "[object Object]".to_string(),
        other => other.to_string(),
    }
}

fn entities_subset(child: &ParsedFilename, parent: &ParsedFilename) -> bool {
    child
        .entities
        .iter()
        .all(|(k, v)| parent.entities.get(k) == Some(v))
}

fn parent_dir(bids_path: &str) -> String {
    let p = bids_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    if p.is_empty() {
        "/".to_string()
    } else {
        p.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(bids: &str) -> DiscoveredFile {
        let name = bids.rsplit_once('/').map(|(_, n)| n).unwrap_or(bids);
        DiscoveredFile {
            bids_path: bids.to_string(),
            fs_path: PathBuf::from(bids),
            datatype: String::new(),
            parsed: crate::filename::parse_filename(name),
        }
    }

    #[test]
    fn subset_walk_finds_root_and_subject_sidecars() {
        let arena = vec![
            entry("/task-rhyme_bold.json"),
            entry("/sub-01/sub-01_task-rhyme_bold.json"),
            entry("/sub-01/func/sub-01_task-rhyme_bold.nii.gz"),
        ];
        let idx = build_index(&arena);
        let source = arena
            .iter()
            .find(|f| f.bids_path.ends_with(".nii.gz"))
            .unwrap();
        // Use the pure-logic helper here — no actual files on disk.
        let (candidate_idxs, _issues) = sidecar_candidates_with_issues(&arena, source, &idx);
        let paths: Vec<_> = candidate_idxs
            .iter()
            .map(|&i| arena[i].bids_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "/sub-01/sub-01_task-rhyme_bold.json",
                "/task-rhyme_bold.json",
            ]
        );
    }

    #[test]
    fn merge_lets_closer_override() {
        let root = serde_json::json!({"A": 1, "B": 2});
        let close = serde_json::json!({"B": 99, "C": 3});
        // `merge_sidecars` expects closest-LAST.
        let merged = merge_sidecars(vec![root, close]);
        let m = merged.as_object().unwrap();
        assert_eq!(m["A"], 1);
        assert_eq!(m["B"], 99);
        assert_eq!(m["C"], 3);
    }
}
