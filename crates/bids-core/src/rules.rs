//! Filename-rule matching + per-rule validation checks.
//!
//! Mirrors the behavior of `src/validators/filenameIdentify.ts` and
//! `src/validators/filenameValidate.ts`. See module-level docs in those
//! files for the algorithm; the comments here only flag where Rust
//! differs from TS.

use std::collections::HashMap;
use std::sync::OnceLock;

use bids_schema_codegen::{
    entity_by_short_name, entity_short_name, iter_filename_rules, schema, EntityLevel, FilenameRule,
};

use crate::filename::ParsedFilename;
use crate::issue::{Issue, Severity};

/// Lightweight per-file matching context.
#[derive(Debug, Clone)]
pub struct FileContext<'a> {
    /// Dataset-relative POSIX path, leading `/`. (e.g. `"/sub-01/anat/..."`).
    pub path: &'a str,
    /// Inferred datatype from the path (e.g. `"anat"`, `"func"`), or empty.
    pub datatype: &'a str,
    pub parsed: &'a ParsedFilename,
    /// `"raw"` (default), `"derivative"`, or `"study"`. Sourced from
    /// `dataset_description.json`'s `DatasetType`. Used to gate
    /// `rules.files.deriv.*` and `rules.files.raw.*`.
    pub dataset_type: DatasetType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DatasetType {
    #[default]
    Raw,
    Derivative,
    Study,
}

impl DatasetType {
    #[allow(clippy::should_implement_trait)] // intentional: infallible, can't be std FromStr
    pub fn from_str(s: &str) -> Self {
        match s {
            "derivative" => Self::Derivative,
            "study" => Self::Study,
            _ => Self::Raw,
        }
    }
}

impl FileContext<'_> {
    fn is_at_root(&self) -> bool {
        // TS `isAtRoot`: path has exactly two segments when split on `/`
        // (the leading `""` + the filename).
        self.path.matches('/').count() == 1
    }
}

/// All filename rules, parsed once. The MVP iterates them often; cache.
fn all_rules() -> &'static [(String, FilenameRule)] {
    static CELL: OnceLock<Vec<(String, FilenameRule)>> = OnceLock::new();
    CELL.get_or_init(|| iter_filename_rules().collect())
}

/// Compiled regex for an entity's format. The TS code re-builds the regex on
/// every label check; we memoize per (format-name). Sourced from the
/// build-time `FORMATS` table (plan.md Phase 1a) so no JSON parse runs
/// for this lookup at startup.
fn format_regex(name: &str) -> Option<&'static regex::Regex> {
    static CELL: OnceLock<HashMap<&'static str, regex::Regex>> = OnceLock::new();
    CELL.get_or_init(|| {
        bids_schema_codegen::FORMATS
            .iter()
            .filter_map(|(k, pattern, _display)| {
                let anchored = format!("^{}$", pattern);
                regex::Regex::new(&anchored).ok().map(|re| (*k, re))
            })
            .collect()
    })
    .get(name)
}

/// Test whether a single rule matches the file context (suffix/path/stem).
/// Mirrors `_findRuleMatches` in `filenameIdentify.ts`.
pub fn rule_matches(rule: &FilenameRule, ctx: &FileContext<'_>) -> bool {
    if let Some(p) = &rule.path {
        if format!("/{p}") == ctx.path {
            return true;
        }
    }
    if let Some(stem) = &rule.stem {
        let file_stem = ctx
            .parsed
            .stem
            .split('.')
            .next()
            .unwrap_or(&ctx.parsed.stem);
        if glob_matches(stem, file_stem)
            && (rule.datatypes.is_empty() || rule.datatypes.iter().any(|d| d == ctx.datatype))
        {
            return true;
        }
    }
    if !rule.suffixes.is_empty() && rule.suffixes.iter().any(|s| s == &ctx.parsed.suffix) {
        return true;
    }
    false
}

/// Match a BIDS-style glob pattern (only `*`, `?`, and literal chars)
/// against a string. Iterative two-pointer algorithm — O(n × m) worst
/// case, O(n + m) typical, never exponential. The earlier recursive
/// version was exponential on pathological patterns like `***...***`;
/// BIDS schema patterns are simple in practice, but the iterative
/// version is safer if a future runtime-schema flag accepts external
/// schemas.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Remember the last `*` position and the text index we'd resume
    // from if the current branch fails.
    let mut star: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < txt.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(s) = star {
            // Backtrack: advance the text past one more char and retry
            // from the position just after the last `*`.
            pi = s + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    // Consume any trailing `*`s in the pattern.
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Should this rule path even be considered for the current dataset?
/// Mirrors `findRuleMatches` in `filenameIdentify.ts`, which skips the
/// `rules.files.deriv` subtree for non-derivative datasets.
fn rule_path_applies(rule_path: &str, dataset_type: DatasetType) -> bool {
    if rule_path.starts_with("rules.files.deriv") && dataset_type != DatasetType::Derivative {
        return false;
    }
    true
}

/// Collect the indices of rules that match this file, then narrow per
/// `hasMatch` in `filenameIdentify.ts`:
///
///   1. If multiple rules match and at least one has a `datatypes` entry
///      including this context's datatype, keep only those.
///   2. If multiple still remain, keep only those whose entity/extension
///      sets fully contain the file's entities and extension.
fn matching_rules(ctx: &FileContext<'_>) -> Vec<usize> {
    let rules = all_rules();
    let mut matched: Vec<usize> = rules
        .iter()
        .enumerate()
        .filter_map(|(i, (path, r))| {
            if !rule_path_applies(path, ctx.dataset_type) {
                return None;
            }
            if rule_matches(r, ctx) {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    if matched.len() > 1 {
        let by_datatype: Vec<usize> = matched
            .iter()
            .copied()
            .filter(|i| {
                let r = &rules[*i].1;
                !r.datatypes.is_empty() && r.datatypes.iter().any(|d| d == ctx.datatype)
            })
            .collect();
        if !by_datatype.is_empty() {
            matched = by_datatype;
        }
    }

    if matched.len() > 1 {
        let by_ent_ext: Vec<usize> = matched
            .iter()
            .copied()
            .filter(|i| entities_extensions_in_rule(&rules[*i].1, ctx))
            .collect();
        if !by_ent_ext.is_empty() {
            matched = by_ent_ext;
        }
    }

    matched
}

fn entities_extensions_in_rule(rule: &FilenameRule, ctx: &FileContext<'_>) -> bool {
    let ext_ok =
        rule.extensions.is_empty() || rule.extensions.iter().any(|e| e == &ctx.parsed.extension);
    let ent_ok = rule.entities.is_empty() || {
        let rule_short: Vec<&str> = rule
            .entities
            .keys()
            .filter_map(|long| entity_short_name(long))
            .collect();
        ctx.parsed
            .entities
            .keys()
            .all(|file_ent| rule_short.iter().any(|s| *s == file_ent))
    };
    ext_ok && ent_ok
}

/// Public matcher for tests/callers that don't need indices.
pub fn matching_rule_paths(ctx: &FileContext<'_>) -> Vec<String> {
    matching_rules(ctx)
        .into_iter()
        .map(|i| all_rules()[i].0.clone())
        .collect()
}

/// Top-level per-file validation. Emits issues in the order the TS
/// validator would emit them.
pub fn validate_file(ctx: &FileContext<'_>) -> Vec<Issue> {
    if ctx.path == "/.bidsignore" {
        return Vec::new();
    }

    let mut issues = Vec::new();
    let rules = all_rules();
    let matched = matching_rules(ctx);

    if matched.is_empty() {
        issues.push(Issue {
            code: "NOT_INCLUDED".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: ctx.path.to_string(),
            rule: None,
            issue_message: None,
            affects: None,
            line: None,
            code_message: None,
        });
        return issues;
    }

    // `missingLabel` and `entityLabelCheck` from filenameValidate.ts run
    // before rule-level checks, and only if at least one matched rule has
    // suffixes (matching the TS guard).
    let any_suffix_rule = matched.iter().any(|i| !rules[*i].1.suffixes.is_empty());
    if any_suffix_rule {
        missing_label(ctx, &mut issues);
    }
    entity_label_check(ctx, &mut issues);
    invalid_location(ctx, &mut issues);

    // FILENAME_MISMATCH (mirrors reconstructionFailure). The TS code's
    // `cleanContext` zeroes out entities/suffix/extension when every
    // matched rule has none of those (e.g. path-only rules like
    // `dataset_description.json`), and then reconstructionFailure's
    // early return on `entities.length === 0` skips the check. We
    // replicate that gate inline instead of mutating the context.
    let any_rule_with_entities = matched.iter().any(|i| !rules[*i].1.entities.is_empty());
    if any_rule_with_entities {
        filename_mismatch(ctx, &mut issues);
    }

    // Rule-level checks: try each matching rule, pick the first that
    // produces zero issues; otherwise emit ALL_FILENAME_RULES_HAVE_ISSUES.
    // Matches `checkRules` in filenameValidate.ts.
    if matched.len() == 1 {
        let (path, rule) = &rules[matched[0]];
        rule_level_checks(rule, path, ctx, &mut issues);
    } else {
        let mut no_issue_paths = Vec::new();
        let mut some_issue_paths = Vec::new();
        for &i in &matched {
            let (path, rule) = &rules[i];
            let mut tmp = Vec::new();
            rule_level_checks(rule, path, ctx, &mut tmp);
            if tmp.is_empty() {
                no_issue_paths.push(path);
            } else {
                some_issue_paths.push(path);
            }
        }
        if !no_issue_paths.is_empty() {
            // At least one rule passed clean — accept the file.
        } else {
            issues.push(Issue {
                code: "ALL_FILENAME_RULES_HAVE_ISSUES".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: ctx.path.to_string(),
                rule: None,
                issue_message: Some(format!(
                    "Rules that matched with issues: {}",
                    some_issue_paths
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }

    issues
}

fn missing_label(ctx: &FileContext<'_>, out: &mut Vec<Issue>) {
    let bad: Vec<&str> = ctx
        .parsed
        .entities
        .iter()
        .filter_map(|(k, v)| {
            if v == "NOENTITY" {
                Some(k.as_str())
            } else {
                None
            }
        })
        .collect();
    if !bad.is_empty() {
        out.push(Issue {
            code: "ENTITY_WITH_NO_LABEL".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: ctx.path.to_string(),
            rule: None,
            issue_message: Some(bad.join(", ")),
            affects: None,
            line: None,
            code_message: None,
        });
    }
}

fn entity_label_check(ctx: &FileContext<'_>, out: &mut Vec<Issue>) {
    for (file_ent, label) in &ctx.parsed.entities {
        let Some((_long, entity)) = entity_by_short_name(file_ent) else {
            continue; // unknown entity — caught by ENTITY_NOT_IN_RULE later
        };
        let Some(re) = format_regex(&entity.format) else {
            continue;
        };
        if !re.is_match(label) {
            let pattern = schema()
                .objects
                .formats
                .get(&entity.format)
                .map(|f| f.pattern.as_str())
                .unwrap_or("");
            out.push(Issue {
                code: "INVALID_ENTITY_LABEL".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: ctx.path.to_string(),
                rule: None,
                issue_message: Some(format!(
                    "entity: {file_ent} label: {label} pattern: {pattern}"
                )),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }
}

/// `INVALID_LOCATION` — mirrors `invalidLocation` / `_validateLocation` in
/// filenameValidate.ts. Two (top, sub) pairs:
///   * (sub, ses) — when the file has neither `tpl`
///   * (tpl, cohort) — when the file has no `sub`
fn invalid_location(ctx: &FileContext<'_>, out: &mut Vec<Issue>) {
    if !ctx.parsed.entities.contains_key("tpl") {
        validate_location_pair(ctx, "sub", "ses", out);
    }
    if !ctx.parsed.entities.contains_key("sub") {
        validate_location_pair(ctx, "tpl", "cohort", out);
    }
}

fn validate_location_pair(ctx: &FileContext<'_>, topent: &str, subent: &str, out: &mut Vec<Issue>) {
    let topval = ctx.parsed.entities.get(topent);
    let subval = ctx.parsed.entities.get(subent);

    if let Some(t) = topval {
        let mut pattern = format!("/{topent}-{t}/");
        if let Some(s) = subval {
            pattern.push_str(&format!("{subent}-{s}/"));
        }
        if !ctx.path.starts_with(&pattern) {
            out.push(Issue {
                code: "INVALID_LOCATION".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: ctx.path.to_string(),
                rule: None,
                issue_message: Some(format!("Expected location: {pattern}")),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }

    if topval.is_none() {
        let needle = format!("/{topent}-");
        if ctx.path.starts_with(&needle) {
            // TS: `Expected location: /${context.file.name}` (no leading "/")
            // we mirror that exactly.
            let name = ctx
                .path
                .rsplit_once('/')
                .map(|(_, n)| n)
                .unwrap_or(ctx.path);
            out.push(Issue {
                code: "INVALID_LOCATION".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: ctx.path.to_string(),
                rule: None,
                issue_message: Some(format!("Expected location: /{name}")),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }
    if subval.is_none() {
        let needle = format!("/{subent}-");
        if ctx.path.contains(&needle) {
            out.push(Issue {
                code: "INVALID_LOCATION".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: ctx.path.to_string(),
                rule: None,
                issue_message: Some(format!("Unexpected {subent} directory")),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }
}

/// `FILENAME_MISMATCH` — reconstructs the canonical filename from the
/// schema's entity order and compares. Mirrors `reconstructionFailure`.
fn filename_mismatch(ctx: &FileContext<'_>, out: &mut Vec<Issue>) {
    if ctx.parsed.entities.is_empty() {
        return;
    }
    let order: Vec<&str> = schema()
        .rules
        .entities
        .iter()
        .filter_map(|long| entity_short_name(long))
        .filter(|short| ctx.parsed.entities.contains_key(*short))
        .collect();

    let parts: Vec<String> = order
        .iter()
        .map(|short| format!("{short}-{}", ctx.parsed.entities[*short]))
        .collect();

    let mut expected = parts.join("_");
    if !ctx.parsed.suffix.is_empty() {
        if !expected.is_empty() {
            expected.push('_');
        }
        expected.push_str(&ctx.parsed.suffix);
    }
    expected.push_str(&ctx.parsed.extension);

    // Extract the basename from `ctx.path`.
    let name = ctx
        .path
        .rsplit_once('/')
        .map(|(_, n)| n)
        .unwrap_or(ctx.path);

    if name != expected {
        out.push(Issue {
            code: "FILENAME_MISMATCH".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: ctx.path.to_string(),
            rule: None,
            issue_message: Some(format!("Expected filename: {expected}")),
            affects: None,
            line: None,
            code_message: None,
        });
    }
}

fn rule_level_checks(
    rule: &FilenameRule,
    rule_path: &str,
    ctx: &FileContext<'_>,
    out: &mut Vec<Issue>,
) {
    entity_rule_issue(rule, rule_path, ctx, out);
    datatype_mismatch(rule, rule_path, ctx, out);
    extension_mismatch(rule, rule_path, ctx, out);
}

fn entity_rule_issue(
    rule: &FilenameRule,
    rule_path: &str,
    ctx: &FileContext<'_>,
    out: &mut Vec<Issue>,
) {
    if rule.entities.is_empty() {
        return; // TS only flags "ENTITY_NOT_IN_RULE" when rule.entities exists
    }

    let file_entities: Vec<&str> = ctx.parsed.entities.keys().map(|s| s.as_str()).collect();
    let rule_short: Vec<&str> = rule
        .entities
        .keys()
        .filter_map(|long| entity_short_name(long))
        .collect();

    if !ctx.is_at_root() {
        let missing_required: Vec<&str> = rule
            .entities
            .iter()
            .filter_map(|(long, req)| match req.level() {
                Some(EntityLevel::Required) => entity_short_name(long),
                _ => None,
            })
            .filter(|short| !file_entities.contains(short))
            .collect();
        if !missing_required.is_empty() {
            out.push(Issue {
                code: "MISSING_REQUIRED_ENTITY".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: ctx.path.to_string(),
                rule: Some(rule_path.to_string()),
                issue_message: Some(format!(
                    "{} missing from rule {rule_path}",
                    missing_required.join(", ")
                )),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }

    let not_in_rule: Vec<&str> = file_entities
        .iter()
        .copied()
        .filter(|fe| !rule_short.contains(fe))
        .collect();
    if !not_in_rule.is_empty() {
        out.push(Issue {
            code: "ENTITY_NOT_IN_RULE".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: ctx.path.to_string(),
            rule: Some(rule_path.to_string()),
            issue_message: Some(format!(
                "{} not in rule {rule_path}",
                not_in_rule.join(", ")
            )),
            affects: None,
            line: None,
            code_message: None,
        });
    }
}

fn datatype_mismatch(
    rule: &FilenameRule,
    rule_path: &str,
    ctx: &FileContext<'_>,
    out: &mut Vec<Issue>,
) {
    if ctx.datatype.is_empty() || rule.datatypes.is_empty() {
        return;
    }
    if !rule.datatypes.iter().any(|d| d == ctx.datatype) {
        out.push(Issue {
            code: "DATATYPE_MISMATCH".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: ctx.path.to_string(),
            rule: Some(rule_path.to_string()),
            issue_message: Some(format!("Datatype rule being applied: {rule_path}")),
            affects: None,
            line: None,
            code_message: None,
        });
    }
}

fn extension_mismatch(
    rule: &FilenameRule,
    rule_path: &str,
    ctx: &FileContext<'_>,
    out: &mut Vec<Issue>,
) {
    if rule.extensions.is_empty() {
        return;
    }
    if !rule.extensions.iter().any(|e| e == &ctx.parsed.extension) {
        out.push(Issue {
            code: "EXTENSION_MISMATCH".to_string(),
            sub_code: None,
            severity: Severity::Error,
            location: ctx.path.to_string(),
            rule: Some(rule_path.to_string()),
            issue_message: None,
            affects: None,
            line: None,
            code_message: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filename::parse_filename;

    fn ctx<'a>(path: &'a str, datatype: &'a str, parsed: &'a ParsedFilename) -> FileContext<'a> {
        FileContext {
            path,
            datatype,
            parsed,
            dataset_type: DatasetType::Raw,
        }
    }

    #[test]
    fn t1w_clean() {
        let p = parse_filename("sub-01_T1w.nii.gz");
        let issues = validate_file(&ctx("/sub-01/anat/sub-01_T1w.nii.gz", "anat", &p));
        assert!(issues.is_empty(), "expected zero issues, got {issues:?}");
    }

    #[test]
    fn unknown_suffix_yields_not_included() {
        let p = parse_filename("sub-01_madeupsuffix.nii");
        let issues = validate_file(&ctx("/sub-01/sub-01_madeupsuffix.nii", "", &p));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, "NOT_INCLUDED");
    }

    #[test]
    fn invalid_label_with_underscore_in_value() {
        let p = parse_filename("sub-01_acq-a$b_T1w.nii.gz");
        let issues = validate_file(&ctx("/sub-01/anat/sub-01_acq-a$b_T1w.nii.gz", "anat", &p));
        assert!(
            issues.iter().any(|i| i.code == "INVALID_ENTITY_LABEL"),
            "expected INVALID_ENTITY_LABEL in {issues:?}"
        );
    }

    #[test]
    fn missing_subject_required_entity() {
        let p = parse_filename("ses-01_T1w.nii.gz");
        let issues = validate_file(&ctx("/sub-01/anat/ses-01_T1w.nii.gz", "anat", &p));
        // We expect MISSING_REQUIRED_ENTITY (subject missing) on the matched anat rule.
        assert!(
            issues.iter().any(|i| i.code == "MISSING_REQUIRED_ENTITY"),
            "got {issues:?}"
        );
    }

    #[test]
    fn entity_with_no_label() {
        let p = parse_filename("sub-01_weird_T1w.nii.gz");
        let issues = validate_file(&ctx("/sub-01/anat/sub-01_weird_T1w.nii.gz", "anat", &p));
        assert!(
            issues.iter().any(|i| i.code == "ENTITY_WITH_NO_LABEL"),
            "got {issues:?}"
        );
    }
}
