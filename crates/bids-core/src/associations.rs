//! `meta.associations` context builder.
//!
//! This mirrors `src/schema/associations.ts`: for each file, schema-defined
//! association rules locate nearby files via the BIDS inheritance walk and
//! load just enough structured data for `rules.checks.*` expressions.

use std::collections::HashSet;
use std::sync::OnceLock;

use bids_expr::{eval_with, EvalContext, Expr, Value};
use bids_schema_codegen::{iter_association_rules, AssociationRule};
use serde_json::Value as J;

use crate::filename::ParsedFilename;
use crate::inheritance::{read_sidecars_with_issues, DirIndex};
use crate::issue::{Issue, Severity};
use crate::run::DiscoveredFile;
use crate::tsv::{load_tsv, TsvColumns};

struct CompiledAssociationRule {
    key: String,
    selectors: Vec<Expr>,
    rule: AssociationRule,
}

fn compiled_rules() -> &'static [CompiledAssociationRule] {
    static CELL: OnceLock<Vec<CompiledAssociationRule>> = OnceLock::new();
    CELL.get_or_init(|| {
        iter_association_rules()
            .filter_map(|(key, rule)| {
                let selectors =
                    crate::compile::parse_all_or_skip(&key, "selector", &rule.selectors)?;
                Some(CompiledAssociationRule {
                    key,
                    selectors,
                    rule,
                })
            })
            .collect()
    })
}

pub fn build_associations(
    ctx_value: &Value,
    arena: &[DiscoveredFile],
    source: &DiscoveredFile,
    index: &DirIndex,
    cache: &crate::content::ContentCache,
    dataset_files: &HashSet<String>,
) -> (J, Vec<Issue>) {
    let mut out = serde_json::Map::new();
    let mut issues = Vec::new();
    let ec = EvalContext::with_files(ctx_value, dataset_files);

    for rule in compiled_rules() {
        let all_pass = rule
            .selectors
            .iter()
            .all(|expr| eval_with(expr, &ec).map(|v| v.is_truthy()).unwrap_or(false));
        if !all_pass {
            continue;
        }

        let extensions = rule.rule.target.extension.clone().into_vec();
        let target_suffix = rule
            .rule
            .target
            .suffix
            .as_deref()
            .unwrap_or(source.parsed.suffix.as_str());
        let (candidates, mut candidate_issues) = association_candidates(
            arena,
            source,
            index,
            rule.rule.inherit,
            &extensions,
            target_suffix,
            &rule.rule.target.entities,
        );
        issues.append(&mut candidate_issues);

        if candidates.is_empty() {
            continue;
        }

        let value = if rule.rule.target.entities.is_empty() {
            load_one(
                &rule.key,
                &arena[candidates[0]],
                arena,
                index,
                &mut issues,
                cache,
            )
        } else {
            let resolved: Vec<&DiscoveredFile> = candidates.iter().map(|&i| &arena[i]).collect();
            load_many(&rule.key, &resolved, cache)
        };
        if !value.is_null() {
            out.insert(rule.key.clone(), value);
        }
    }

    (J::Object(out), issues)
}

fn association_candidates(
    arena: &[DiscoveredFile],
    source: &DiscoveredFile,
    index: &DirIndex,
    inherit: bool,
    extensions: &[String],
    target_suffix: &str,
    target_entities: &[String],
) -> (Vec<usize>, Vec<Issue>) {
    let mut issues = Vec::new();
    let mut dir = parent_dir(&source.bids_path);
    loop {
        if let Some(file_idxs) = index.get(&dir) {
            let candidates: Vec<usize> = file_idxs
                .iter()
                .copied()
                .filter(|&i| {
                    let f = &arena[i];
                    extensions.iter().any(|e| e == &f.parsed.extension)
                        && f.parsed.suffix == target_suffix
                        && association_entities_match(&f.parsed, &source.parsed, target_entities)
                })
                .collect();
            if candidates.len() > 1 {
                if let Some(exact) = candidates.iter().copied().find(|&i| {
                    exact_association_match(&arena[i].parsed, &source.parsed, target_entities)
                }) {
                    return (vec![exact], issues);
                }
                if !target_entities.is_empty() {
                    return (candidates, issues);
                }
                let mut paths: Vec<String> = candidates
                    .iter()
                    .map(|&i| arena[i].bids_path.clone())
                    .collect();
                paths.sort();
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
                return (Vec::new(), issues);
            }
            if candidates.len() == 1 {
                return (candidates, issues);
            }
        }
        if !inherit || dir == "/" {
            break;
        }
        dir = parent_dir(&dir);
    }
    (Vec::new(), issues)
}

fn association_entities_match(
    candidate: &ParsedFilename,
    source: &ParsedFilename,
    target_entities: &[String],
) -> bool {
    candidate.entities.iter().all(|(entity, value)| {
        source.entities.get(entity) == Some(value) || target_entities.iter().any(|e| e == entity)
    })
}

fn exact_association_match(
    candidate: &ParsedFilename,
    source: &ParsedFilename,
    target_entities: &[String],
) -> bool {
    source
        .entities
        .keys()
        .chain(target_entities.iter())
        .all(|entity| candidate.entities.get(entity) == source.entities.get(entity))
}

fn parent_dir(bids_path: &str) -> String {
    let p = bids_path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    if p.is_empty() {
        "/".to_string()
    } else {
        p.to_string()
    }
}

fn load_one(
    key: &str,
    file: &DiscoveredFile,
    arena: &[DiscoveredFile],
    index: &DirIndex,
    issues: &mut Vec<Issue>,
    cache: &crate::content::ContentCache,
) -> J {
    match key {
        "events" => {
            let cols = load_tsv_if_allowed(file, cache);
            serde_json::json!({
                "path": file.bids_path,
                "onset": column(&cols, "onset"),
                "sidecar": construct_sidecar(arena, file, index, issues, cache),
            })
        }
        "aslcontext" => {
            let cols = load_tsv_if_allowed(file, cache);
            let volume_type = column(&cols, "volume_type");
            serde_json::json!({
                "path": file.bids_path,
                "n_rows": volume_type.len(),
                "volume_type": volume_type,
            })
        }
        "bval" => {
            let rows = read_bval_bvec(file, cache);
            serde_json::json!({
                "path": file.bids_path,
                "n_cols": rows.first().map(|r| r.len()).unwrap_or(0),
                "n_rows": rows.len(),
                "values": rows.first().cloned().unwrap_or_default(),
            })
        }
        "bvec" => {
            let rows = read_bval_bvec(file, cache);
            if rows
                .iter()
                .any(|row| row.len() != rows.first().map(|r| r.len()).unwrap_or(0))
            {
                issues.push(Issue {
                    code: "BVEC_ROW_LENGTH".to_string(),
                    sub_code: None,
                    severity: Severity::Error,
                    location: file.bids_path.clone(),
                    rule: None,
                    issue_message: None,
                    affects: None,
                    line: None,
                    code_message: None,
                });
                return J::Null;
            }
            serde_json::json!({
                "path": file.bids_path,
                "n_cols": rows.first().map(|r| r.len()).unwrap_or(0),
                "n_rows": rows.len(),
            })
        }
        "channels" => {
            let cols = load_tsv_if_allowed(file, cache);
            serde_json::json!({
                "path": file.bids_path,
                "type": nullable_column(&cols, "type"),
                "short_channel": nullable_column(&cols, "short_channel"),
                "sampling_frequency": nullable_column(&cols, "sampling_frequency"),
            })
        }
        "physio" => serde_json::json!({
            "path": file.bids_path,
            "sidecar": construct_sidecar(arena, file, index, issues, cache),
        }),
        _ => serde_json::json!({ "path": file.bids_path }),
    }
}

fn load_many(key: &str, files: &[&DiscoveredFile], cache: &crate::content::ContentCache) -> J {
    match key {
        "coordsystems" => {
            let mut parents = Vec::new();
            for f in files {
                let Some(json) = cache.read_json(&f.fs_path) else {
                    continue;
                };
                if let Some(s) = json.get("ParentCoordinateSystem").and_then(|v| v.as_str()) {
                    parents.push(s.to_string());
                }
            }
            serde_json::json!({
                "paths": files.iter().map(|f| f.bids_path.clone()).collect::<Vec<_>>(),
                "spaces": files.iter().filter_map(|f| f.parsed.entities.get("space").cloned()).collect::<Vec<_>>(),
                "ParentCoordinateSystems": parents,
            })
        }
        _ => J::Array(
            files
                .iter()
                .map(|f| serde_json::json!({"path": f.bids_path}))
                .collect(),
        ),
    }
}

fn construct_sidecar(
    arena: &[DiscoveredFile],
    file: &DiscoveredFile,
    index: &DirIndex,
    issues: &mut Vec<Issue>,
    cache: &crate::content::ContentCache,
) -> J {
    let (mut sidecars, mut inherit_issues) = read_sidecars_with_issues(arena, file, index, cache);
    issues.append(&mut inherit_issues);
    sidecars.reverse();
    crate::inheritance::merge_sidecars(sidecars.into_iter().map(|(_, json)| json))
}

fn column(cols: &TsvColumns, name: &str) -> Vec<String> {
    cols.columns.get(name).cloned().unwrap_or_default()
}

fn nullable_column(cols: &TsvColumns, name: &str) -> J {
    match cols.columns.get(name) {
        Some(values) => J::Array(values.iter().cloned().map(J::String).collect()),
        None => J::Null,
    }
}

fn load_tsv_if_allowed(file: &DiscoveredFile, cache: &crate::content::ContentCache) -> TsvColumns {
    // TSV bodies are not cached (size-bounded but potentially large);
    // the cache only provides the underlying policy decision here.
    load_tsv(&file.fs_path, &cache.policy()).0
}

fn read_bval_bvec(file: &DiscoveredFile, cache: &crate::content::ContentCache) -> Vec<Vec<String>> {
    let Some(text) = cache.read_to_string(&file.fs_path) else {
        return Vec::new();
    };
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .filter(|row| row.chars().any(|c| !c.is_whitespace()))
        .map(|row| row.split_whitespace().map(|s| s.to_string()).collect())
        .collect()
}
