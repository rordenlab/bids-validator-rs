//! JSON metadata checks against `rules.sidecars.*`.
//!
//! Mirrors `evalJsonCheck` in `src/schema/applyRules.ts` for sidecar rules:
//!
//! For each data file:
//!   1. Build a selector context from filename parts + merged sidecar.
//!   2. For each sidecar rule (`rules.sidecars.*` leaf):
//!      a. Skip if any selector evaluates to falsy.
//!      b. For each (field, requirement) in `rule.fields`:
//!         - Look up the metadata field name from `schema.objects.metadata`.
//!         - If absent from sidecar:
//!           - Severity = required → SIDECAR_KEY_REQUIRED
//!           - Severity = recommended → SIDECAR_KEY_RECOMMENDED
//!           - Optional / prohibited → no issue.
//!
//! Implemented here: missing sidecar keys, field-level JSON Schema
//! validation, per-key dedup across files, `level_addendum` severity
//! bumps, and derivative-context skipping for raw sidecar rules.

use std::sync::OnceLock;

use std::collections::HashMap;

use bids_expr::{eval_with, EvalContext, Expr, Value};
use bids_schema_codegen::{
    iter_json_rules, iter_sidecar_rules, modality_of, schema, EntityLevel, FieldRequirement,
    SidecarRule,
};
use indexmap::IndexMap;
use jsonschema::error::{TypeKind, ValidationErrorKind};
use jsonschema::{ValidationError, Validator};
use serde_json::Value as J;

use crate::filename::ParsedFilename;
use crate::issue::{Issue, Severity};
use crate::rules::DatasetType;
use crate::tsv::TsvColumns;

#[derive(Debug, Default, Clone)]
pub struct DatasetSubjects {
    pub sub_dirs: Vec<String>,
    pub participant_id: Option<Vec<String>>,
    pub phenotype: Option<Vec<String>>,
}

/// Decide the issue severity for a missing field, taking
/// `level_addendum` into account. Mirrors TS `getFieldSeverity` in
/// `src/schema/applyRules.ts`.
///
/// Returns `None` when the field should be silently ignored.
///
/// Quirk preserved from TS:
///   * any level not present in `levelToSeverity` (`deprecated`,
///     missing level, unknown shape) is ignored.
fn severity_with_addendum(req: &FieldRequirement, sidecar: &J) -> Option<Severity> {
    use FieldRequirement::*;
    let initial = match req {
        Plain(EntityLevel::Required) => Some(Severity::Error),
        Plain(EntityLevel::Recommended) => Some(Severity::Warning),
        Plain(_) => None,
        Detailed { level, .. } => match level {
            Some(EntityLevel::Required) => Some(Severity::Error),
            Some(EntityLevel::Recommended) => Some(Severity::Warning),
            Some(EntityLevel::Optional)
            | Some(EntityLevel::Prohibited)
            | Some(EntityLevel::Deprecated)
            | None => None,
        },
    };

    // Apply `level_addendum` override: when the sidecar key matches the
    // value, replace severity with the addendum's level. TS regex:
    // `^(required|recommended) if \`(\w+)\` is \`(\w+)\`$`.
    if let Some(addendum) = req.level_addendum() {
        if let Some((addendum_level, key, value)) = parse_level_addendum(addendum) {
            if let Some(actual) = sidecar.get(key).and_then(|v| v.as_str()) {
                if actual == value {
                    return Some(match addendum_level {
                        "required" => Severity::Error,
                        _ => Severity::Warning,
                    });
                }
            }
        }
    }

    initial
}

/// Parse a `level_addendum` string. Returns `(level, key, value)` if
/// the input matches the regex. Cached static regex.
fn parse_level_addendum(s: &str) -> Option<(&str, &str, &str)> {
    use regex::Regex;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(required|recommended) if `(\w+)` is `(\w+)`")
            .expect("level_addendum regex compiles")
    });
    let caps = re.captures(s)?;
    Some((
        caps.get(1)?.as_str(),
        caps.get(2)?.as_str(),
        caps.get(3)?.as_str(),
    ))
}

/// Pre-parsed selector ASTs for every sidecar rule, plus the rule's
/// dotted path and the field requirements. Parsed once.
struct CompiledSidecarRule {
    path: String,
    selectors: Vec<Expr>,
    selectors_include_derivative: bool,
    rule: SidecarRule,
}

fn compiled_rules() -> &'static [CompiledSidecarRule] {
    static CELL: OnceLock<Vec<CompiledSidecarRule>> = OnceLock::new();
    CELL.get_or_init(|| compile_rules(iter_sidecar_rules()))
}

fn compiled_json_rules() -> &'static [CompiledSidecarRule] {
    static CELL: OnceLock<Vec<CompiledSidecarRule>> = OnceLock::new();
    CELL.get_or_init(|| compile_rules(iter_json_rules()))
}

fn compile_rules<I: IntoIterator<Item = (String, SidecarRule)>>(
    rules: I,
) -> Vec<CompiledSidecarRule> {
    rules
        .into_iter()
        .filter_map(|(path, rule)| {
            // Drops the WHOLE rule on any parse failure (audit H14 fix);
            // before this change the loop dropped per-selector, which can
            // flip a narrowing rule into a fires-on-everything rule.
            let selectors = crate::compile::parse_all_or_skip(&path, "selector", &rule.selectors)?;
            Some(CompiledSidecarRule {
                path,
                selectors,
                selectors_include_derivative: rule
                    .selectors
                    .iter()
                    .any(|s| s == r#"dataset.dataset_description.DatasetType == "derivative""#),
                rule,
            })
        })
        .collect()
}

/// Per-file context for sidecar rule evaluation. Doesn't borrow anything
/// from the schema; the schema is global via OnceLock.
pub struct SidecarContext<'a> {
    pub bids_path: &'a str,
    pub datatype: &'a str,
    pub parsed: &'a ParsedFilename,
    pub merged_sidecar: &'a J,
    /// The file's own parsed JSON, if this is a `.json` file. Used by
    /// `rules.json.*` and `rules.dataset_metadata.*` rules (via the
    /// selector identifier `json`).
    pub file_json: &'a J,
    pub dataset_type: DatasetType,
    pub dataset_modalities: &'a [String],
    /// `sidecar_key -> bids_path of contributing sidecar file`. Used to
    /// route JSON_SCHEMA_VALIDATION_ERROR locations to the sidecar that
    /// actually defined the offending value (TS `context.sidecarKeyOrigin`).
    pub sidecar_key_origin: &'a HashMap<String, String>,
    /// Parsed NIfTI header, exposed to selectors/checks as
    /// `nifti_header`. `J::Null` when the file isn't NIfTI, the header
    /// couldn't be read, or `--ignoreNiftiHeaders` was requested.
    pub nifti_header: &'a J,
    pub gzip: &'a J,
    pub columns: &'a TsvColumns,
    pub associations: &'a J,
    pub ome: &'a J,
    pub tiff: &'a J,
    pub file_size: u64,
    pub dataset_subjects: &'a DatasetSubjects,
    /// Dataset file set (normalized, leading `/` stripped) used by
    /// `bids_expr::exists(...)` selectors. Plumbed through the context
    /// rather than a thread-local so parallel validation workers each
    /// see the right set without setup ceremony.
    pub dataset_files: &'a std::collections::HashSet<String>,
}

/// Dataset-wide dedup state for JSON_SCHEMA_VALIDATION_ERROR emissions.
/// Mirrors TS `BIDSContextDataset.sidecarKeyValidated`: a set of
/// `"{sidecar_location}:{key}"` strings indicating which sidecar+key
/// pairs have already been validated this run. Without this, the same
/// invalid sidecar key produces duplicate issues for every data file
/// that inherits the sidecar.
#[derive(Default, Debug)]
pub struct SidecarKeyValidated(std::collections::HashSet<String>);

impl SidecarKeyValidated {
    pub fn new() -> Self {
        Self::default()
    }
    /// Returns true if `(location, key)` was already validated. Marks
    /// the pair as seen as a side-effect, so the second call returns
    /// true. Empty `location` is a no-op (acts as "not yet seen"),
    /// matching TS: `if (sidecarRule && location) { ... add(...) }`.
    pub fn check_and_record(&mut self, location: &str, key: &str) -> bool {
        if location.is_empty() {
            return false;
        }
        let address = format!("{location}:{key}");
        !self.0.insert(address)
    }
}

/// Build the `Value::Object` that selectors evaluate against.
pub fn build_eval_context(c: &SidecarContext<'_>) -> Value {
    let mut top = IndexMap::new();

    // Filename parts.
    top.insert(
        "datatype".to_string(),
        Value::String(c.datatype.to_string()),
    );
    top.insert("suffix".to_string(), Value::String(c.parsed.suffix.clone()));
    top.insert(
        "extension".to_string(),
        Value::String(c.parsed.extension.clone()),
    );
    top.insert(
        "modality".to_string(),
        modality_of(c.datatype)
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null),
    );

    // Entities: object of short-name → label.
    let entities = c
        .parsed
        .entities
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    top.insert("entities".to_string(), Value::Object(entities));

    // Sidecar: merged JSON as Value::Object.
    top.insert("sidecar".to_string(), c.merged_sidecar.clone().into());

    top.insert("associations".to_string(), c.associations.clone().into());
    top.insert("columns".to_string(), columns_value(c.columns));
    top.insert("json".to_string(), c.file_json.clone().into());
    top.insert("path".to_string(), Value::String(c.bids_path.to_string()));
    top.insert("size".to_string(), Value::Number(c.file_size as f64));
    top.insert("gzip".to_string(), c.gzip.clone().into());
    top.insert("nifti_header".to_string(), c.nifti_header.clone().into());
    top.insert("ome".to_string(), c.ome.clone().into());
    top.insert("tiff".to_string(), c.tiff.clone().into());

    // dataset.dataset_description.DatasetType + dataset.modalities
    let mut dataset = IndexMap::new();
    let dt_str = match c.dataset_type {
        DatasetType::Raw => "raw",
        DatasetType::Derivative => "derivative",
        DatasetType::Study => "study",
    };
    let mut dataset_description = IndexMap::new();
    dataset_description.insert("DatasetType".to_string(), Value::String(dt_str.to_string()));
    dataset.insert(
        "dataset_description".to_string(),
        Value::Object(dataset_description),
    );
    let modalities = c
        .dataset_modalities
        .iter()
        .map(|s| Value::String(s.clone()))
        .collect::<Vec<_>>();
    dataset.insert("modalities".to_string(), Value::Array(modalities));
    let mut subjects = IndexMap::new();
    subjects.insert(
        "sub_dirs".to_string(),
        Value::Array(
            c.dataset_subjects
                .sub_dirs
                .iter()
                .cloned()
                .map(Value::String)
                .collect(),
        ),
    );
    if let Some(ids) = &c.dataset_subjects.participant_id {
        subjects.insert(
            "participant_id".to_string(),
            Value::Array(ids.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(ids) = &c.dataset_subjects.phenotype {
        subjects.insert(
            "phenotype".to_string(),
            Value::Array(ids.iter().cloned().map(Value::String).collect()),
        );
    }
    dataset.insert("subjects".to_string(), Value::Object(subjects));
    top.insert("dataset".to_string(), Value::Object(dataset));

    // Schema constants accessed in selectors like
    // `intersects(schema.objects.enums._StandardTemplateCoordSys.enum, [...])`.
    // Built once per process and cloned per-file — the subtree is identical
    // every call and the recompute (~JSON Value of all schema enums) was
    // measurable in the per-file profile on ds005016.
    top.insert("schema".to_string(), schema_value_static().clone());

    Value::Object(top)
}

pub fn set_eval_context_associations(ctx: &mut Value, associations: &J) {
    if let Value::Object(map) = ctx {
        map.insert("associations".to_string(), associations.clone().into());
    }
}

/// Static (process-lifetime) view of `schema.{objects.enums, meta.versions}`
/// in `bids-expr`'s `Value` shape. Used by selectors that reference
/// `schema.objects.enums.*.enum` or `schema.meta.versions`. The body
/// never changes for a given schema version, so cache once.
fn schema_value_static() -> &'static Value {
    static CELL: OnceLock<Value> = OnceLock::new();
    CELL.get_or_init(|| {
        serde_json::to_value(serde_json::json!({
            "objects": { "enums": schema_enums() },
            "meta":    { "versions": schema().meta.versions },
        }))
        .expect("schema constants always serializable")
        .into()
    })
}

fn columns_value(columns: &TsvColumns) -> Value {
    let mut map = IndexMap::new();
    for (name, values) in &columns.columns {
        map.insert(
            name.clone(),
            Value::Array(values.iter().cloned().map(Value::String).collect()),
        );
    }
    Value::Object(map)
}

/// Per-metadata-key compiled JSON Schema validator, looked up lazily.
/// Keyed by the `objects.metadata.<KEY>` rule key (e.g. `"RepetitionTime"`,
/// `"ScanningSequence__mrs"`) — NOT by the `.name` sidecar field, so
/// that variants with the same name (`"ScanningSequence"` vs
/// `"ScanningSequence__mrs"`) get their own validator.
///
/// On `valid_headers`-sized datasets, eagerly building all ~200 metadata
/// validators at first access cost ~150 ms. The lazy form builds only
/// the validators a dataset actually triggers (one per sidecar key
/// present), which collapses the 200-validator startup cost on small
/// datasets.
fn with_metadata_validator<R>(key: &str, f: impl FnOnce(Option<&Validator>) -> R) -> R {
    use std::sync::{Arc, RwLock};
    // Cache holds `Arc<Validator>` so we can clone the handle out under
    // a read lock, drop the lock, and run the (potentially expensive)
    // user closure with no lock held. Mirrors the audit_temp.md High #3
    // fix: the previous design held a `Mutex` guard across the closure,
    // serializing every JSON-schema validation across threads when
    // Phase 2 parallelism lands.
    static CACHE: OnceLock<RwLock<HashMap<String, Option<Arc<Validator>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    // Fast path: read lock, clone the Arc out, drop the lock.
    {
        let guard = cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = guard.get(key) {
            return f(slot.as_deref());
        }
    }

    // Slow path: build under the write lock and insert. Another thread
    // may have inserted in the meantime; do the standard "double-check"
    // to avoid duplicate work.
    {
        let mut guard = cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.contains_key(key) {
            let built = match build_field_validator(key) {
                Ok(v) => Some(Arc::new(v)),
                Err(e) => {
                    eprintln!("warn: failed to build validator for metadata.{key}: {e}");
                    None
                }
            };
            guard.insert(key.to_string(), built);
        }
    }

    // Re-read with the lock dropped before running the closure.
    let slot = cache
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(key)
        .cloned()
        .flatten();
    f(slot.as_deref())
}

fn build_field_validator(
    field_key: &str,
) -> Result<Validator, jsonschema::ValidationError<'static>> {
    // Fetch the field's raw JSON body by the rule key, not by .name.
    let raw = raw_metadata_definition(field_key)
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let cleaned = strip_bids_keys(raw);
    let mut opts = jsonschema::options();
    for (name, re) in anchored_format_regexes() {
        let re = re.clone();
        opts = opts.with_format(name.clone(), move |s: &str| re.is_match(s));
    }
    opts.build(&cleaned).map_err(|e| e.to_owned())
}

fn ajv_compatible_message(err: &ValidationError<'_>) -> String {
    match err.kind() {
        ValidationErrorKind::Type {
            kind: TypeKind::Single(type_),
        } => format!("must be {type_}"),
        ValidationErrorKind::Type {
            kind: TypeKind::Multiple(types),
        } => {
            let types = types
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("must be {types}")
        }
        ValidationErrorKind::Enum { .. } => {
            "must be equal to one of the allowed values".to_string()
        }
        ValidationErrorKind::Minimum { limit } => format!("must be >= {}", json_scalar(limit)),
        ValidationErrorKind::Maximum { limit } => format!("must be <= {}", json_scalar(limit)),
        ValidationErrorKind::ExclusiveMinimum { limit } => {
            format!("must be > {}", json_scalar(limit))
        }
        ValidationErrorKind::ExclusiveMaximum { limit } => {
            format!("must be < {}", json_scalar(limit))
        }
        ValidationErrorKind::Pattern { pattern } => {
            format!(r#"must match pattern "{pattern}""#)
        }
        ValidationErrorKind::Required { property } => {
            let property = property
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| property.to_string());
            format!("must have required property '{property}'")
        }
        ValidationErrorKind::AdditionalProperties { .. } => {
            "must NOT have additional properties".to_string()
        }
        ValidationErrorKind::MinItems { limit } => {
            format!("must NOT have fewer than {limit} items")
        }
        ValidationErrorKind::MaxItems { limit } => {
            format!("must NOT have more than {limit} items")
        }
        ValidationErrorKind::MinLength { limit } => {
            format!("must NOT have fewer than {limit} characters")
        }
        ValidationErrorKind::MaxLength { limit } => {
            format!("must NOT have more than {limit} characters")
        }
        ValidationErrorKind::Format { format } => {
            format!(r#"must match format "{format}""#)
        }
        _ => err.to_string(),
    }
}

fn json_scalar(value: &J) -> String {
    match value {
        J::String(s) => s.clone(),
        _ => value.to_string(),
    }
}

/// Per-format anchored `^…$` regexes derived once from
/// `schema.objects.formats`. Previously rebuilt inside every
/// `build_field_validator` call — for ~200 metadata fields that
/// meant ~3,600 `Regex::new(...)` invocations at startup of the
/// first sidecar check. Caching them once shaves ~50–150 ms on
/// small datasets (audit M12).
fn anchored_format_regexes() -> &'static IndexMap<String, std::sync::Arc<regex::Regex>> {
    static CELL: OnceLock<IndexMap<String, std::sync::Arc<regex::Regex>>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut out = IndexMap::new();
        // Sourced from build-time `FORMATS` (plan.md Phase 1a) — zero
        // JSON parse cost for this set.
        for (name, pattern, _display) in bids_schema_codegen::FORMATS {
            let anchored = format!("^{}$", pattern);
            if let Ok(re) = regex::Regex::new(&anchored) {
                out.insert((*name).to_string(), std::sync::Arc::new(re));
            }
        }
        out
    })
}

/// Build the raw JSON shape of a metadata field for jsonschema validator
/// construction. Sources from `schema().objects.metadata[key]` — the
/// flattened `raw` map captures every field outside of
/// `name`/`display_name`/`description`, which are stripped by
/// `strip_bids_keys` anyway. Cached per key so repeated lookups during
/// validator construction don't rebuild the map.
fn raw_metadata_definition(field_key: &str) -> Option<&'static serde_json::Value> {
    static CELL: OnceLock<HashMap<String, serde_json::Value>> = OnceLock::new();
    let map = CELL.get_or_init(|| {
        let mut out = HashMap::new();
        for (k, m) in &schema().objects.metadata {
            // `raw` already excludes the three BIDS-only fields that
            // `strip_bids_keys` would remove next anyway.
            out.insert(k.clone(), serde_json::Value::Object(m.raw.clone()));
        }
        out
    });
    map.get(field_key)
}

/// Remove keys that are documentation/BIDS-specific and not part of the
/// JSON Schema vocabulary. Recursive — applies to nested `properties`
/// and `items` so the same stripping happens inside object/array schemas.
fn strip_bids_keys(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value as V;
    const STRIP: &[&str] = &[
        "name",
        "display_name",
        "description",
        "description_addendum",
        "unit",
        "recommended",
    ];
    match v {
        V::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if STRIP.contains(&k.as_str()) {
                    continue;
                }
                out.insert(k, strip_bids_keys(val));
            }
            V::Object(out)
        }
        V::Array(xs) => V::Array(xs.into_iter().map(strip_bids_keys).collect()),
        other => other,
    }
}

/// Subset of `schema.objects.enums` we expose to selectors. Sourced
/// from the typed schema's raw `enums` field — no separate parse.
fn schema_enums() -> &'static serde_json::Value {
    &schema().objects.enums
}

/// A `JSON_SCHEMA_VALIDATION_ERROR` issue tagged with the `(origin, key)`
/// pair that gates it. Used by the per-file validator: emit candidates
/// unconditionally, and let the deterministic merge step in
/// [`crate::run::validate_dataset`] decide which observer wins (by file
/// index, lowest wins). Mirrors TS `dataset.sidecarKeyValidated` without
/// the run-order coupling that prevented parallel validation.
#[derive(Debug, Clone)]
pub struct DedupCandidate {
    /// `(sidecar_location, sidecar_key)`. Multiple data files can
    /// inherit the same sidecar key; only the lowest-file-index one
    /// emits.
    pub key: (String, String),
    /// Issues to emit if this is the first observer of `key` in file
    /// index order. Empty if validation succeeded.
    pub issues: Vec<Issue>,
}

/// Per-file output from [`check_sidecars`]. `plain` issues fire
/// unconditionally; `dedup` candidates need the merge step to pick
/// winners.
#[derive(Default, Debug)]
pub struct SidecarCheckResult {
    pub plain: Vec<Issue>,
    pub dedup: Vec<DedupCandidate>,
}

/// Run all sidecar-rule checks against this file and emit the issues.
///
/// Mirrors `evalJsonCheck`'s sidecar branch: skip files whose extension
/// is in `{".json","","md",".txt",".rst",".cff"}` (sidecars themselves
/// don't have sidecars).
pub fn check_sidecars(c: &SidecarContext<'_>, ctx_value: &Value) -> SidecarCheckResult {
    const SKIP_EXTENSIONS: &[&str] = &[".json", "", ".md", ".txt", ".rst", ".cff"];
    if SKIP_EXTENSIONS.contains(&c.parsed.extension.as_str()) {
        return SidecarCheckResult::default();
    }

    let mut out = SidecarCheckResult::default();
    let ec = EvalContext::with_files(ctx_value, c.dataset_files);

    for r in compiled_rules() {
        // Skip the rule if any selector evaluates falsy.
        let all_pass = r
            .selectors
            .iter()
            .all(|expr| eval_with(expr, &ec).map(|v| v.is_truthy()).unwrap_or(false));
        if !all_pass {
            continue;
        }

        for (field_key, req) in &r.rule.fields {
            // Resolve the schema-side metadata definition to get the
            // canonical sidecar key name (TS reads `schema.objects.metadata[key].name`).
            let meta = match schema().objects.metadata.get(field_key) {
                Some(m) => m,
                None => continue,
            };
            let key_name = meta.name.as_str();

            // Field present in merged sidecar? Always run JSON Schema
            // validation (regardless of required/optional level —
            // matches TS, which validates whenever value !== undefined).
            // Field absent? Map severity and maybe emit SIDECAR_KEY_*.
            let value = c.merged_sidecar.as_object().and_then(|m| m.get(key_name));

            if let Some(val) = value {
                // Per-key dedup: each file emits a `DedupCandidate`
                // tagged with (origin, key_name); the deterministic
                // merge step in `validate_dataset` keeps only the
                // lowest-file-index observer. Empty origin is the
                // TS no-dedup case — emit unconditionally as a plain
                // issue.
                let origin = c
                    .sidecar_key_origin
                    .get(key_name)
                    .cloned()
                    .unwrap_or_default();
                let origin_for_issue = if origin.is_empty() {
                    c.bids_path.to_string()
                } else {
                    origin.clone()
                };
                let validator_issues = with_metadata_validator(field_key, |validator| {
                    let mut issues = Vec::new();
                    if let Some(v) = validator {
                        for err in v.iter_errors(val) {
                            // TS routes sidecar-rule schema errors to
                            // the *sidecar* path that contributed the
                            // key, and records the data file in
                            // `affects`.
                            issues.push(Issue {
                                code: "JSON_SCHEMA_VALIDATION_ERROR".to_string(),
                                sub_code: Some(key_name.to_string()),
                                severity: Severity::Error,
                                location: origin_for_issue.clone(),
                                rule: Some(r.path.clone()),
                                issue_message: Some(format!(
                                    "{}\n\nField description: {}",
                                    ajv_compatible_message(&err),
                                    meta.description
                                )),
                                affects: Some(vec![c.bids_path.to_string()]),
                                line: None,
                                code_message: None,
                            });
                        }
                    }
                    issues
                });
                if origin.is_empty() {
                    out.plain.extend(validator_issues);
                } else {
                    out.dedup.push(DedupCandidate {
                        key: (origin, key_name.to_string()),
                        issues: validator_issues,
                    });
                }
                continue;
            }

            // Field is absent — decide severity for emission.
            // Mirror TS `getFieldSeverity`: both string and detailed
            // `deprecated` levels are absent from `levelToSeverity`, so
            // they are ignored. `AcquisitionDuration` relies on this.
            // `level_addendum` (TS `getFieldSeverity`) can promote
            // ignore → required/recommended when the sidecar's gating
            // key matches. Deno passes `context.sidecar` (the merged
            // sidecar) into `getFieldSeverity`; this is the data-file
            // path of `check_sidecars`, so the right input is
            // `c.merged_sidecar`, not `c.file_json` (which is `Null`
            // for non-JSON files). Audited as High #4 (2026-05-17).
            let severity = match severity_with_addendum(req, c.merged_sidecar) {
                Some(s) => s,
                None => continue,
            };
            if c.dataset_type == DatasetType::Derivative && !r.selectors_include_derivative {
                continue;
            }

            // Custom `issue: {code, message}` override (mirrors TS).
            if let Some(custom) = req.custom_issue() {
                out.plain.push(Issue {
                    code: custom.code.clone(),
                    sub_code: Some(key_name.to_string()),
                    severity,
                    location: c.bids_path.to_string(),
                    rule: Some(r.path.clone()),
                    issue_message: Some(format!("Field description: {}", meta.description)),
                    affects: None,
                    line: None,
                    code_message: Some(custom.message.clone()),
                });
                continue;
            }

            let code = match severity {
                Severity::Error => "SIDECAR_KEY_REQUIRED",
                Severity::Warning => "SIDECAR_KEY_RECOMMENDED",
            };
            out.plain.push(Issue {
                code: code.to_string(),
                sub_code: Some(key_name.to_string()),
                severity,
                location: c.bids_path.to_string(),
                rule: Some(r.path.clone()),
                issue_message: Some(format!("Field description: {}", meta.description)),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }

    out
}

/// Run `rules.json.*` and `rules.dataset_metadata.*` checks against this
/// file's own JSON contents (NOT the merged sidecar).
///
/// Mirrors `evalJsonCheck`'s non-sidecar branch in `applyRules.ts`:
/// emits `JSON_KEY_REQUIRED` / `JSON_KEY_RECOMMENDED` for missing fields,
/// and `JSON_SCHEMA_VALIDATION_ERROR` (without `affects`) for present
/// fields that fail their schema. Location is always the file itself.
pub fn check_json(c: &SidecarContext<'_>, ctx_value: &Value) -> Vec<Issue> {
    // Only `.json` files have own-JSON content to validate against
    // these rules. Non-JSON file extensions can't satisfy the
    // `path == "/x.json"` / `extension == ".json"` selectors anyway.
    if c.parsed.extension != ".json" {
        return Vec::new();
    }

    let mut out = Vec::new();
    let ec = EvalContext::with_files(ctx_value, c.dataset_files);

    for r in compiled_json_rules() {
        let all_pass = r
            .selectors
            .iter()
            .all(|expr| eval_with(expr, &ec).map(|v| v.is_truthy()).unwrap_or(false));
        if !all_pass {
            continue;
        }

        for (field_key, req) in &r.rule.fields {
            let meta = match schema().objects.metadata.get(field_key) {
                Some(m) => m,
                None => continue,
            };
            let key_name = meta.name.as_str();

            // Validate against the file's own JSON, not the merged sidecar.
            // Validation runs regardless of `required`/`optional` level —
            // matches TS which validates whenever value !== undefined.
            let value = c.file_json.as_object().and_then(|m| m.get(key_name));

            if let Some(val) = value {
                let validator_issues = with_metadata_validator(field_key, |validator| {
                    let mut issues = Vec::new();
                    if let Some(v) = validator {
                        for err in v.iter_errors(val) {
                            // JSON-rule errors are located on the file
                            // itself; no `affects` (the file IS the source).
                            issues.push(Issue {
                                code: "JSON_SCHEMA_VALIDATION_ERROR".to_string(),
                                sub_code: Some(key_name.to_string()),
                                severity: Severity::Error,
                                location: c.bids_path.to_string(),
                                rule: Some(r.path.clone()),
                                issue_message: Some(format!(
                                    "{}\n\nField description: {}",
                                    ajv_compatible_message(&err),
                                    meta.description
                                )),
                                affects: None,
                                line: None,
                                code_message: None,
                            });
                        }
                    }
                    issues
                });
                out.extend(validator_issues);
                continue;
            }

            // Field is absent — decide severity for emission.
            // check_json validates a file's OWN JSON body; the
            // `level_addendum` gating key, if any, references another
            // key in the same file (e.g. `DatasetType`-gated
            // requirements on `dataset_description.json`). So we pass
            // `c.file_json`, not the inherited merged sidecar.
            let severity = match severity_with_addendum(req, c.file_json) {
                Some(s) => s,
                None => continue,
            };
            if c.dataset_type == DatasetType::Derivative && !r.selectors_include_derivative {
                continue;
            }

            // Custom `issue: {code, message}` overrides the generic
            // JSON_KEY_* emission. Mirrors `requirement.issue` in
            // evalJsonCheck.
            if let Some(custom) = req.custom_issue() {
                out.push(Issue {
                    code: custom.code.clone(),
                    sub_code: Some(key_name.to_string()),
                    severity,
                    location: c.bids_path.to_string(),
                    rule: Some(r.path.clone()),
                    issue_message: Some(format!("Field description: {}", meta.description)),
                    affects: None,
                    line: None,
                    code_message: Some(custom.message.clone()),
                });
                continue;
            }

            let code = match severity {
                Severity::Error => "JSON_KEY_REQUIRED",
                Severity::Warning => "JSON_KEY_RECOMMENDED",
            };
            out.push(Issue {
                code: code.to_string(),
                sub_code: Some(key_name.to_string()),
                severity,
                location: c.bids_path.to_string(),
                rule: Some(r.path.clone()),
                issue_message: Some(format!("Field description: {}", meta.description)),
                affects: None,
                line: None,
                code_message: None,
            });
        }
    }

    out
}

#[cfg(test)]
mod message_tests {
    use super::*;
    use bids_schema_codegen::CustomIssue;
    use serde_json::json;

    fn first_ajv_message(schema: J, instance: J) -> String {
        let validator = jsonschema::validator_for(&schema).unwrap();
        let err = validator.iter_errors(&instance).next().unwrap();
        ajv_compatible_message(&err)
    }

    #[test]
    fn formats_common_json_schema_errors_like_ajv() {
        assert_eq!(
            first_ajv_message(json!({"type": "array"}), json!("")),
            "must be array"
        );
        assert_eq!(
            first_ajv_message(json!({"enum": ["foo", "bar"]}), json!("baz")),
            "must be equal to one of the allowed values"
        );
        assert_eq!(
            first_ajv_message(json!({"minimum": 0}), json!(-1)),
            "must be >= 0"
        );
        assert_eq!(
            first_ajv_message(json!({"maximum": 89}), json!(90)),
            "must be <= 89"
        );
        assert_eq!(
            first_ajv_message(json!({"pattern": "^[0-9]+$"}), json!("abc")),
            r#"must match pattern "^[0-9]+$""#
        );
        assert_eq!(
            first_ajv_message(json!({"required": ["Name"]}), json!({})),
            "must have required property 'Name'"
        );
        assert_eq!(
            first_ajv_message(
                json!({"additionalProperties": false, "properties": {"a": true}}),
                json!({"a": 1, "b": 2})
            ),
            "must NOT have additional properties"
        );
    }

    #[test]
    fn deprecated_metadata_fields_are_suppressed_like_deno() {
        assert!(severity_with_addendum(
            &FieldRequirement::Plain(EntityLevel::Deprecated),
            &json!({})
        )
        .is_none());
        assert!(severity_with_addendum(
            &FieldRequirement::Detailed {
                level: Some(EntityLevel::Deprecated),
                issue: Option::<CustomIssue>::None,
                level_addendum: None,
            },
            &json!({})
        )
        .is_none());
    }
}
