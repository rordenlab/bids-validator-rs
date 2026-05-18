//! TSV column / initial-column / index-column checks.
//!
//! Mirrors `src/schema/tables.ts`. Three independent evaluators run against
//! each matching `rules.tabular_data.*` leaf:
//!
//!   * [`eval_columns`] — column presence, allowed-extras, type checks.
//!   * [`eval_initial_columns`] — required initial column order.
//!   * [`eval_index_columns`] — uniqueness across an index tuple.
//!
//! All three short-circuit unless the file is `.tsv`/`.tsv.gz`. The caller
//! is responsible for filtering on selectors (we reuse the sidecar-rule
//! selector evaluator); each evaluator only sees rules that already
//! matched.
//!
//! Type-check parity uses the same anchored regexes the TS validator
//! builds in `compileSignature`: `format` patterns are taken from
//! `schema.objects.formats`, joined by `|` when a column has multiple
//! formats (via `anyOf`), and surrounded by `^…$`. Per-cell logic
//! mirrors `checkValue`:
//!
//!   1. `n/a` is always valid.
//!   2. Regex must match the whole value.
//!   3. If `levels` is set, the value must be in the enum.
//!   4. If `minimum`/`maximum` are set, the value parses as f64 and lies
//!      in the range.

use std::collections::HashSet;
use std::sync::OnceLock;

use indexmap::IndexMap;
use regex::Regex;

use bids_expr::{eval_with, EvalContext, Expr, Value};
use bids_schema_codegen::{
    column_by_key, iter_tabular_rules, ColumnDefinition, ColumnSchema, TabularRule,
};
use serde_json::Value as J;

use crate::issue::{Issue, Severity};
use crate::metadata::SidecarContext;
use crate::tsv::TsvColumns;

/// Per-file inputs for the column evaluators. Bundled to keep the public
/// surface small and stable as new fields appear.
pub struct TabularContext<'a> {
    pub bids_path: &'a str,
    pub extension: &'a str,
    pub columns: &'a TsvColumns,
    /// Merged sidecar (used for `ColumnDefinition` overrides on
    /// `definition`-bearing columns).
    pub merged_sidecar: &'a J,
    /// Per-sidecar-key origin map, mirroring TS `context.sidecarKeyOrigin`.
    /// Used as the `location` for `TSV_COLUMN_TYPE_REDEFINED` so the issue
    /// points at the sidecar file that supplied the bad definition.
    pub sidecar_key_origin: &'a std::collections::HashMap<String, String>,
}

/// Pre-parsed selector ASTs for every tabular rule. Parsed once.
struct CompiledTabularRule {
    path: String,
    selectors: Vec<Expr>,
    rule: TabularRule,
}

fn compiled_rules() -> &'static [CompiledTabularRule] {
    static CELL: OnceLock<Vec<CompiledTabularRule>> = OnceLock::new();
    CELL.get_or_init(|| {
        iter_tabular_rules()
            .filter_map(|(path, rule)| {
                let selectors =
                    crate::compile::parse_all_or_skip(&path, "selector", &rule.selectors)?;
                Some(CompiledTabularRule {
                    path,
                    selectors,
                    rule,
                })
            })
            .collect()
    })
}

/// Top-level orchestrator: for every `rules.tabular_data.*` rule whose
/// selectors all evaluate truthy for this file, run the three column
/// evaluators and collect their issues.
///
/// Caller is responsible for having produced `columns` via [`crate::tsv`].
/// If TSV parsing failed (parse issue already emitted), pass an empty
/// `TsvColumns` — matches TS's "catch error, return empty Map" behavior.
pub fn check_tables(
    sc: &SidecarContext<'_>,
    ctx_value: &Value,
    columns: &TsvColumns,
) -> Vec<Issue> {
    let ext = sc.parsed.extension.as_str();
    if !is_tsv_ext(ext) {
        return Vec::new();
    }
    let tctx = TabularContext {
        bids_path: sc.bids_path,
        extension: ext,
        columns,
        merged_sidecar: sc.merged_sidecar,
        sidecar_key_origin: sc.sidecar_key_origin,
    };
    let mut out = Vec::new();
    let ec = EvalContext::with_files(ctx_value, sc.dataset_files);
    for r in compiled_rules() {
        let all_pass = r
            .selectors
            .iter()
            .all(|expr| eval_with(expr, &ec).map(|v| v.is_truthy()).unwrap_or(false));
        if !all_pass {
            continue;
        }
        out.extend(eval_columns(&r.rule, &r.path, &tctx));
        out.extend(eval_initial_columns(&r.rule, &r.path, &tctx));
        out.extend(eval_index_columns(&r.rule, &r.path, &tctx));
    }
    out
}

/// `evalColumns` parity. Returns issues in TS emission order.
pub fn eval_columns(rule: &TabularRule, rule_path: &str, ctx: &TabularContext<'_>) -> Vec<Issue> {
    let mut out = Vec::new();
    if !is_tsv_ext(ctx.extension) || rule.columns.is_empty() {
        return out;
    }

    // rule's column rule-keys (`name__channels`) → on-disk header (`name`)
    // Preserve rule-defined order — IndexMap.
    let mut column_lookup: IndexMap<&str, &str> = IndexMap::new();
    for rule_key in rule.columns.keys() {
        let Some(col) = column_by_key(rule_key) else {
            continue; // unknown column key in schema — schema bug
        };
        column_lookup.insert(col.header_name(), rule_key.as_str());
    }

    // Extra on-disk columns: names in the TSV not covered by the rule.
    let mut additional: Vec<&str> = Vec::new();
    for name in &ctx.columns.headers {
        if !column_lookup.contains_key(name.as_str()) {
            additional.push(name.as_str());
        }
    }

    // Iterate rule-defined names first (rule order), then TSV-only extras
    // (file order). Mirrors `new Set([...Object.keys(columnLookup),
    // ...additionalColumns])` — JS Set preserves insertion order. Both
    // `column_lookup.keys()` and `additional` are already unique against
    // each other by construction, so no dedup HashSet is needed.
    let order: Vec<&str> = column_lookup
        .keys()
        .copied()
        .chain(additional.iter().copied())
        .collect();
    for name in order {
        let rule_header = column_lookup.get(name).copied();
        let requirement = get_column_requirement(rule, rule_header);
        let column_values = ctx.columns.columns.get(name);
        let sidecar_def = sidecar_column_definition(ctx.merged_sidecar, name);

        // Branch 1: additional / extras.
        if rule_header.is_none() {
            let code = match requirement.as_deref() {
                Some("not_allowed") => Some("TSV_ADDITIONAL_COLUMNS_NOT_ALLOWED"),
                _ if sidecar_def.is_none() => match requirement.as_deref() {
                    Some("allowed_if_defined") => Some("TSV_ADDITIONAL_COLUMNS_MUST_DEFINE"),
                    _ => Some("TSV_ADDITIONAL_COLUMNS_UNDEFINED"),
                },
                _ => None,
            };
            if let Some(code) = code {
                let severity = if code == "TSV_ADDITIONAL_COLUMNS_UNDEFINED" {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                out.push(Issue {
                    code: code.to_string(),
                    sub_code: Some(name.to_string()),
                    severity,
                    location: ctx.bids_path.to_string(),
                    rule: Some(rule_path.to_string()),
                    issue_message: None,
                    affects: None,
                    line: None,
                    code_message: None,
                });
                continue;
            }
            // Allowed-with-definition: type-check below using the sidecar
            // definition as the signature.
            let Some(def) = sidecar_def else { continue };
            let signature = extract_definition(&def);
            if signature.is_trivial() {
                continue;
            }
            if let Some(values) = column_values {
                check_values(values, &signature, name, ctx, rule_path, &mut out);
            }
            continue;
        }

        // Branch 2: rule-defined column.
        let rule_header = rule_header.unwrap();
        let Some(column_object) = column_by_key(rule_header) else {
            continue;
        };

        let Some(values) = column_values else {
            // Column missing.
            let is_initial = rule.initial_columns.iter().any(|c| c == rule_header);
            if requirement.as_deref() == Some("required") && !is_initial {
                out.push(Issue {
                    code: "TSV_COLUMN_MISSING".to_string(),
                    sub_code: Some(name.to_string()),
                    severity: Severity::Error,
                    location: ctx.bids_path.to_string(),
                    rule: Some(rule_path.to_string()),
                    issue_message: None,
                    affects: None,
                    line: None,
                    code_message: None,
                });
            }
            continue;
        };

        // Build the effective signature (schema + possible sidecar refinement).
        let signature = match build_value_signature(column_object, sidecar_def.as_ref()) {
            Ok(sig) => sig,
            Err(redef_msg) => {
                // TSV_COLUMN_TYPE_REDEFINED: located at the sidecar that
                // contributed the offending definition.
                let location = ctx
                    .sidecar_key_origin
                    .get(name)
                    .cloned()
                    .unwrap_or_default();
                out.push(Issue {
                    code: "TSV_COLUMN_TYPE_REDEFINED".to_string(),
                    sub_code: Some(name.to_string()),
                    severity: Severity::Warning,
                    location,
                    rule: Some(rule_path.to_string()),
                    issue_message: Some(redef_msg),
                    affects: None,
                    line: None,
                    code_message: None,
                });
                // Fall back to the schema-only signature (TS does the same).
                extract_schema(column_object)
            }
        };

        if signature.is_trivial() {
            continue;
        }

        check_values(values, &signature, name, ctx, rule_path, &mut out);
    }

    out
}

/// `evalInitialColumns` parity. Required initial columns must appear at
/// the correct index; missing/required ones emit `TSV_COLUMN_MISSING`,
/// out-of-order ones emit `TSV_COLUMN_ORDER_INCORRECT`.
pub fn eval_initial_columns(
    rule: &TabularRule,
    rule_path: &str,
    ctx: &TabularContext<'_>,
) -> Vec<Issue> {
    let mut out = Vec::new();
    if !is_tsv_ext(ctx.extension) || rule.initial_columns.is_empty() || rule.columns.is_empty() {
        return out;
    }
    let headers: &[String] = &ctx.columns.headers;

    // Resolve initial rule keys → on-disk names + presence index.
    struct Item<'a> {
        name: &'a str,
        requirement: Option<String>,
        present_index: Option<usize>,
    }
    let mut items: Vec<Item<'_>> = Vec::new();
    for rule_key in &rule.initial_columns {
        let Some(col) = column_by_key(rule_key) else {
            continue;
        };
        let req = get_column_requirement(rule, Some(rule_key));
        let idx = headers.iter().position(|h| h == col.header_name());
        if req.as_deref() != Some("required") && idx.is_none() {
            continue;
        }
        items.push(Item {
            name: col.header_name(),
            requirement: req,
            present_index: idx,
        });
    }

    for (target_index, item) in items.iter().enumerate() {
        match item.present_index {
            None => {
                out.push(Issue {
                    code: "TSV_COLUMN_MISSING".to_string(),
                    sub_code: Some(item.name.to_string()),
                    severity: Severity::Error,
                    location: ctx.bids_path.to_string(),
                    rule: Some(rule_path.to_string()),
                    issue_message: Some(format!(
                        "Required initial column {} not found.",
                        target_index + 1
                    )),
                    affects: None,
                    line: None,
                    code_message: None,
                });
            }
            Some(idx) if idx != target_index => {
                out.push(Issue {
                    code: "TSV_COLUMN_ORDER_INCORRECT".to_string(),
                    sub_code: Some(item.name.to_string()),
                    severity: Severity::Error,
                    location: ctx.bids_path.to_string(),
                    rule: Some(rule_path.to_string()),
                    issue_message: Some(format!(
                        "Initial column {} found at index {}.",
                        target_index + 1,
                        idx + 1
                    )),
                    affects: None,
                    line: None,
                    code_message: None,
                });
            }
            _ => {}
        }
        let _ = item.requirement.as_ref(); // currently unused; kept for parity
    }

    out
}

/// `evalIndexColumns` parity. Duplicate index tuples emit
/// `TSV_INDEX_VALUE_NOT_UNIQUE` keyed by the file path.
pub fn eval_index_columns(
    rule: &TabularRule,
    rule_path: &str,
    ctx: &TabularContext<'_>,
) -> Vec<Issue> {
    let mut out = Vec::new();
    if !is_tsv_ext(ctx.extension) || rule.index_columns.is_empty() || rule.columns.is_empty() {
        return out;
    }

    // Resolve index rule keys → on-disk names that are actually present.
    let resolved: Vec<&str> = rule
        .index_columns
        .iter()
        .filter_map(|k| column_by_key(k).map(|c| c.header_name()))
        .filter(|name| ctx.columns.columns.contains_key(*name))
        .collect();
    if resolved.is_empty() {
        return out;
    }

    let row_count = resolved
        .iter()
        .filter_map(|col| ctx.columns.columns.get(*col).map(|v| v.len()))
        .min()
        .unwrap_or(0);
    let mut seen: HashSet<Vec<&str>> = HashSet::new();

    for i in 0..row_count {
        let key = resolved
            .iter()
            .map(|col| {
                ctx.columns
                    .columns
                    .get(*col)
                    .and_then(|values| values.get(i))
                    .map(String::as_str)
                    .unwrap_or("")
            })
            .collect::<Vec<_>>();
        if !seen.insert(key.clone()) {
            out.push(Issue {
                code: "TSV_INDEX_VALUE_NOT_UNIQUE".to_string(),
                sub_code: None,
                severity: Severity::Error,
                location: ctx.bids_path.to_string(),
                rule: Some(rule_path.to_string()),
                issue_message: Some(format!("Row: {}, Value: {}", i + 2, key.join("\x1f"))),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_tsv_ext(ext: &str) -> bool {
    ext == ".tsv" || ext == ".tsv.gz"
}

fn get_column_requirement(rule: &TabularRule, rule_header: Option<&str>) -> Option<String> {
    // None means "additional column"; look up `additional_columns`.
    let key = match rule_header {
        Some(k) => k,
        None => return rule.additional_columns.clone().or(Some("n/a".to_string())),
    };
    rule.columns.get(key).map(|r| r.level().to_string())
}

/// Pull a `ColumnDefinition` from the merged sidecar for the named column,
/// if any (e.g. `sidecar["age"] = {Format: "number", Units: "year"}`).
fn sidecar_column_definition(sidecar: &J, name: &str) -> Option<ColumnDefinition> {
    let val = sidecar.get(name)?;
    if !val.is_object() {
        return None;
    }
    serde_json::from_value::<ColumnDefinition>(val.clone()).ok()
}

/// Value signature: collapsed view of what a column's cells must look like.
/// Mirrors TS `ValueSignature`.
#[derive(Debug, Clone, Default)]
struct ValueSignature {
    formats: Vec<String>,
    pattern: Option<String>,
    units: Option<String>,
    levels: Option<Vec<String>>,
    maximum: Option<f64>,
    minimum: Option<f64>,
}

impl ValueSignature {
    fn is_trivial(&self) -> bool {
        self.levels.is_none()
            && self.maximum.is_none()
            && self.minimum.is_none()
            && ((self.pattern.is_none() && self.formats.first().is_some_and(|s| s == "string"))
                || self.pattern.as_deref() == Some(".*"))
    }
}

/// Collapse `anyOf` recursively to the inner type/format names. Mirrors
/// `_getFormats` in TS.
fn collect_formats(col: &ColumnSchema) -> Vec<String> {
    if let Some(any_of) = &col.any_of {
        return any_of.iter().flat_map(collect_formats).collect();
    }
    vec![col
        .format
        .clone()
        .or_else(|| col.type_.clone())
        .unwrap_or_else(|| "string".to_string())]
}

fn extract_schema(col: &ColumnSchema) -> ValueSignature {
    ValueSignature {
        formats: collect_formats(col),
        pattern: col.pattern.clone(),
        units: col.unit.clone(),
        levels: col.enum_.clone(),
        maximum: col.maximum,
        minimum: col.minimum,
    }
}

fn extract_definition(def: &ColumnDefinition) -> ValueSignature {
    let format = def
        .format
        .clone()
        .or_else(|| {
            if def.units.is_some() {
                Some("number".to_string())
            } else {
                Some("string".to_string())
            }
        })
        .unwrap();
    ValueSignature {
        formats: vec![format],
        pattern: None,
        units: def.units.clone(),
        levels: def.levels.as_ref().map(|m| m.keys().cloned().collect()),
        maximum: def.maximum,
        minimum: def.minimum,
    }
}

fn format_to_type(format: &str) -> &str {
    match format {
        "integer" | "number" => "number",
        "boolean" => "boolean",
        _ => "string",
    }
}

/// Combine schema-side and sidecar-side signatures. Mirrors `refineSignature`
/// in TS, including the TSV_COLUMN_TYPE_REDEFINED error message strings.
fn refine_signature(
    base: ValueSignature,
    override_: ValueSignature,
) -> Result<ValueSignature, String> {
    // Formats: override's type must be in base's allowed type set.
    let base_types: HashSet<&str> = base.formats.iter().map(|f| format_to_type(f)).collect();
    let override_type = override_
        .formats
        .first()
        .map(|s| format_to_type(s))
        .unwrap_or("string");
    if !base_types.contains(override_type) {
        return Err(format!(
            "Format \"{}\" must be {}",
            override_.formats.first().cloned().unwrap_or_default(),
            base.formats.join(" or ")
        ));
    }

    // Enums must be a subset.
    let effective_levels = override_.levels.clone().or_else(|| base.levels.clone());
    if let (Some(base_levels), Some(override_levels)) = (&base.levels, &override_.levels) {
        if !override_levels.iter().all(|v| base_levels.contains(v)) {
            return Err(format!(
                "Levels {{{}}} is not a subset of {{{}}}.",
                override_levels.join(", "),
                base_levels.join(", ")
            ));
        }
    }

    // Units must match.
    let effective_units = override_.units.clone().or_else(|| base.units.clone());
    if let Some(base_units) = &base.units {
        if effective_units.as_ref() != Some(base_units) {
            return Err(format!(
                "Unit \"{}\" must be \"{}\"",
                effective_units.clone().unwrap_or_default(),
                base_units
            ));
        }
    }

    // Minimum: override min must be ≥ base min.
    let effective_minimum = override_.minimum.or(base.minimum);
    if let Some(base_min) = base.minimum {
        if effective_minimum.is_none_or(|m| m < base_min) {
            return Err(format!(
                "Minimum {} is less than {}",
                effective_minimum.unwrap_or(f64::NAN),
                base_min
            ));
        }
    }

    let effective_maximum = override_.maximum.or(base.maximum);
    if let Some(base_max) = base.maximum {
        if effective_maximum.is_none_or(|m| m > base_max) {
            return Err(format!(
                "Maximum {} is greater than {}",
                effective_maximum.unwrap_or(f64::NAN),
                base_max
            ));
        }
    }

    Ok(ValueSignature {
        formats: override_.formats,
        pattern: base.pattern,
        units: effective_units,
        levels: effective_levels,
        maximum: effective_maximum,
        minimum: effective_minimum,
    })
}

fn build_value_signature(
    col: &ColumnSchema,
    sidecar_def: Option<&ColumnDefinition>,
) -> Result<ValueSignature, String> {
    if col.definition.is_some() {
        // Fully overridable column: use the sidecar definition if present,
        // otherwise fall back to the schema-embedded default `definition`.
        let def = sidecar_def
            .cloned()
            .or_else(|| col.definition.clone())
            .unwrap_or_default();
        return Ok(extract_definition(&def));
    }
    let schema_sig = extract_schema(col);
    match sidecar_def {
        Some(def) => refine_signature(schema_sig, extract_definition(def)),
        None => Ok(schema_sig),
    }
}

/// Compile the per-column regex once per schema run. Built from
/// `objects.formats[fmt].pattern` joined by `|` for `anyOf`, then
/// anchored with `^…$`. Cached by canonical key.
fn compile_signature(sig: &ValueSignature) -> CompiledSpec {
    let pattern = if let Some(p) = &sig.pattern {
        p.clone()
    } else {
        // Look up patterns in the build-time `FORMATS` table (plan.md
        // Phase 1a). Linear scan is fine — ~18 entries.
        sig.formats
            .iter()
            .map(|f| {
                bids_schema_codegen::FORMATS
                    .iter()
                    .find(|(k, _, _)| *k == f.as_str())
                    .map(|(_, pat, _)| pat.to_string())
                    .unwrap_or_else(|| ".*".to_string())
            })
            .collect::<Vec<_>>()
            .join("|")
    };
    let anchored = format!("^(?:{pattern})$");
    let regex = compile_regex(&anchored);
    CompiledSpec {
        regex,
        levels: sig.levels.clone(),
        maximum: sig.maximum,
        minimum: sig.minimum,
    }
}

struct CompiledSpec {
    regex: Regex,
    levels: Option<Vec<String>>,
    maximum: Option<f64>,
    minimum: Option<f64>,
}

fn compile_regex(pattern: &str) -> Regex {
    // Pattern cache: keyed by the anchored regex source. Eager building
    // would require iterating every possible (formats, pattern) combo,
    // so we lazily insert. The cache uses `RwLock` so concurrent readers
    // don't serialize on the hot path; writes only happen for the small
    // bounded set of patterns the schema actually produces. If a pattern
    // doesn't compile in the Rust `regex` crate (e.g. ECMA-only lookahead
    // like `(?!/)` in `participant_relative`), we fall back to `.*` —
    // this is a known parity gap, see audit_response.md.
    static CELL: OnceLock<std::sync::RwLock<std::collections::HashMap<String, Regex>>> =
        OnceLock::new();
    let cache = CELL.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()));
    {
        let guard = cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(re) = guard.get(pattern) {
            return re.clone();
        }
    }
    let re = Regex::new(pattern).unwrap_or_else(|e| {
        eprintln!(
            "warn: regex unsupported by `regex` crate, falling back to `.*`: {pattern:?} ({e})"
        );
        Regex::new("^.*$").unwrap()
    });
    cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(pattern.to_string(), re.clone());
    re
}

/// Static `NA`/`nan`-hint regex used when emitting `TSV_VALUE_INCORRECT_TYPE`.
fn nan_hint_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\s*(NA|na|nan|NaN|n/a)\s*$").unwrap())
}

/// Per-cell check, mirroring TS `checkValue`.
fn check_value(value: &str, spec: &CompiledSpec) -> bool {
    if value == "n/a" {
        return true;
    }
    if !spec.regex.is_match(value) {
        return false;
    }
    if let Some(levels) = &spec.levels {
        if !levels.iter().any(|l| l == value) {
            return false;
        }
    }
    if spec.maximum.is_some() || spec.minimum.is_some() {
        if let Ok(n) = value.parse::<f64>() {
            if let Some(max) = spec.maximum {
                if n > max {
                    return false;
                }
            }
            if let Some(min) = spec.minimum {
                if n < min {
                    return false;
                }
            }
        } else {
            return false;
        }
    }
    true
}

fn check_values(
    values: &[String],
    signature: &ValueSignature,
    name: &str,
    ctx: &TabularContext<'_>,
    rule_path: &str,
    out: &mut Vec<Issue>,
) {
    let spec = compile_signature(signature);
    let age_check = name == "age";
    let mut age_warning_issued = false;

    for (i, v) in values.iter().enumerate() {
        if check_value(v, &spec) {
            continue;
        }
        // Special-case "89+" in the age column → deprecation warning,
        // only once per column. TS: `ageWarningIssued` flag.
        if age_check && v == "89+" {
            if !age_warning_issued {
                age_warning_issued = true;
                out.push(Issue {
                    code: "TSV_PSEUDO_AGE_DEPRECATED".to_string(),
                    sub_code: None,
                    severity: Severity::Warning,
                    location: ctx.bids_path.to_string(),
                    rule: None,
                    issue_message: None,
                    affects: None,
                    line: Some(i + 2),
                    code_message: None,
                });
            }
            continue;
        }
        let suffix = if nan_hint_regex().is_match(v) {
            ", did you mean 'n/a'?"
        } else {
            ""
        };
        out.push(Issue {
            code: "TSV_VALUE_INCORRECT_TYPE".to_string(),
            sub_code: Some(name.to_string()),
            severity: Severity::Error,
            location: ctx.bids_path.to_string(),
            rule: Some(rule_path.to_string()),
            issue_message: Some(format!("'{v}'{suffix}")),
            affects: None,
            line: Some(i + 2),
            code_message: None,
        });
        // TS `break` after first bad value: only one error per column.
        break;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cols_from(headers: &[&str], rows: &[&[&str]]) -> TsvColumns {
        let mut columns: IndexMap<String, Vec<String>> = IndexMap::new();
        for h in headers {
            columns.insert((*h).to_string(), Vec::new());
        }
        for r in rows {
            for (i, h) in headers.iter().enumerate() {
                columns.get_mut(*h).unwrap().push(r[i].to_string());
            }
        }
        TsvColumns {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            columns,
        }
    }

    fn rule(json: &str) -> TabularRule {
        serde_json::from_str(json).unwrap()
    }

    fn ctx<'a>(
        path: &'a str,
        cols: &'a TsvColumns,
        sidecar: &'a J,
        origin: &'a HashMap<String, String>,
    ) -> TabularContext<'a> {
        TabularContext {
            bids_path: path,
            extension: ".tsv",
            columns: cols,
            merged_sidecar: sidecar,
            sidecar_key_origin: origin,
        }
    }

    #[test]
    fn missing_required_column() {
        // Use real schema column: "name__channels" → "name".
        let r = rule(
            r#"{"selectors":[],"columns":{"name__channels":"required","type__channels":"required"},"initial_columns":["name__channels"]}"#,
        );
        let c = cols_from(&["type"], &[&["EEG"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_columns(
            &r,
            "rules.tabular_data.x",
            &ctx("/sub-01/eeg/sub-01_channels.tsv", &c, &s, &o),
        );
        // type is present, name is missing-but-initial (don't report), name__channels is required+initial so skipped.
        // But TS: name is required and ALSO in initial_columns, so TSV_COLUMN_MISSING is NOT emitted here (handled by evalInitialColumns instead).
        assert!(issues.iter().all(|i| i.code != "TSV_COLUMN_MISSING"));
    }

    #[test]
    fn additional_not_allowed() {
        let r = rule(
            r#"{"selectors":[],"columns":{"name__channels":"optional"},"additional_columns":"not_allowed"}"#,
        );
        let c = cols_from(&["name", "extra"], &[&["x", "y"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_columns(&r, "rules.tabular_data.x", &ctx("/x.tsv", &c, &s, &o));
        assert!(issues
            .iter()
            .any(|i| i.code == "TSV_ADDITIONAL_COLUMNS_NOT_ALLOWED"
                && i.sub_code.as_deref() == Some("extra")));
    }

    #[test]
    fn initial_columns_out_of_order() {
        let r = rule(
            r#"{"selectors":[],"columns":{"name__channels":"required","type__channels":"required"},"initial_columns":["name__channels","type__channels"]}"#,
        );
        let c = cols_from(&["type", "name"], &[&["EEG", "Ch1"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_initial_columns(&r, "rules.tabular_data.x", &ctx("/x.tsv", &c, &s, &o));
        assert!(
            issues
                .iter()
                .any(|i| i.code == "TSV_COLUMN_ORDER_INCORRECT"),
            "got {issues:?}"
        );
    }

    #[test]
    fn index_uniqueness() {
        let r =
            rule(r#"{"selectors":[],"columns":{"index":"required"},"index_columns":["index"]}"#);
        let c = cols_from(&["index"], &[&["1"], &["2"], &["1"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_index_columns(&r, "rules.tabular_data.x", &ctx("/x.tsv", &c, &s, &o));
        assert!(
            issues
                .iter()
                .any(|i| i.code == "TSV_INDEX_VALUE_NOT_UNIQUE"),
            "got {issues:?}"
        );
    }

    #[test]
    fn index_uniqueness_uses_tuple_not_concatenation() {
        let r = rule(
            r#"{"selectors":[],"columns":{"name__channels":"required","type__channels":"required"},"index_columns":["name__channels","type__channels"]}"#,
        );
        let c = cols_from(&["name", "type"], &[&["1", "23"], &["12", "3"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_index_columns(&r, "rules.tabular_data.x", &ctx("/x.tsv", &c, &s, &o));
        assert!(
            !issues
                .iter()
                .any(|i| i.code == "TSV_INDEX_VALUE_NOT_UNIQUE"),
            "got {issues:?}"
        );
    }

    #[test]
    fn value_type_check_n_a_passes() {
        let r = rule(r#"{"selectors":[],"columns":{"duration":"required"}}"#);
        let c = cols_from(&["duration"], &[&["1.5"], &["n/a"], &["2"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_columns(&r, "rules.tabular_data.x", &ctx("/x.tsv", &c, &s, &o));
        assert!(
            !issues.iter().any(|i| i.code == "TSV_VALUE_INCORRECT_TYPE"),
            "got {issues:?}"
        );
    }

    #[test]
    fn value_type_check_minimum() {
        // duration has minimum: 0
        let r = rule(r#"{"selectors":[],"columns":{"duration":"required"}}"#);
        let c = cols_from(&["duration"], &[&["1.0"], &["-1.0"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_columns(&r, "rules.tabular_data.x", &ctx("/x.tsv", &c, &s, &o));
        assert!(
            issues.iter().any(|i| i.code == "TSV_VALUE_INCORRECT_TYPE"),
            "got {issues:?}"
        );
    }

    #[test]
    fn age_89_plus_emits_pseudo_age_deprecated() {
        let r = rule(r#"{"selectors":[],"columns":{"age":"recommended"}}"#);
        let c = cols_from(&["age"], &[&["30"], &["89+"], &["89+"]]);
        let s = J::Object(Default::default());
        let o: HashMap<String, String> = HashMap::new();
        let issues = eval_columns(&r, "rules.tabular_data.x", &ctx("/x.tsv", &c, &s, &o));
        // exactly one pseudo-age (because TS sets ageWarningIssued = true)
        let count = issues
            .iter()
            .filter(|i| i.code == "TSV_PSEUDO_AGE_DEPRECATED")
            .count();
        assert_eq!(count, 1, "got {issues:?}");
    }
}
