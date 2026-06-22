//! Compile-time-embedded view of the BIDS schema.
#![forbid(unsafe_code)]
//!
//! Hybrid model (plan.md Phase 1a in progress):
//!
//!   * The bulk of the schema is still lazily deserialized from the
//!     bundled `schema-<version>.json` via `serde_json::from_str` into
//!     a typed [`Schema`], cached in `OnceLock`. This keeps the typed
//!     API stable while we migrate subtrees one at a time.
//!   * Selected subtrees are emitted as `&'static` Rust slices at
//!     build time by [`build.rs`](../build.rs) and included via
//!     `include!`. These have zero runtime parse cost.
//!
//! Subtrees migrated so far:
//!   * `objects.formats` → [`FORMATS`] (`(rule-key, pattern, display)`).
//!   * `objects.entities` → [`ENTITIES`] (`(long-key, short-name, format)`).
//!   * `rules.modalities` → [`MODALITY_OF`] (`(datatype, modality)`).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use indexmap::IndexMap;
use serde::Deserialize;

mod patches;

/// Embedded schema text. Pinned by `rust/schemas/schema-<version>.json`
/// symlink. Used only for the *runtime-parse* fallback path; the
/// build-time codegen reads the same file in `build.rs`.
pub const BUNDLED_SCHEMA_JSON: &str = include_str!("../../../schemas/schema-1.2.1.json");

// Build-time-generated static tables. Adding a new table is one
// edit each here, in `build.rs::write_*`, and at the consuming
// callsites.
include!(concat!(env!("OUT_DIR"), "/generated_formats.rs"));
include!(concat!(env!("OUT_DIR"), "/generated_entities.rs"));
include!(concat!(env!("OUT_DIR"), "/generated_modalities.rs"));

/// Subset of the BIDS schema we actually need today.
#[derive(Debug, Deserialize)]
pub struct Schema {
    pub schema_version: String,
    pub bids_version: String,
    #[serde(default)]
    pub meta: Meta,
    pub objects: Objects,
    pub rules: Rules,
}

#[derive(Debug, Deserialize, Default)]
pub struct Meta {
    #[serde(default)]
    pub associations: serde_json::Value,
    #[serde(default)]
    pub versions: Vec<String>,
    /// Reference cases for the expression evaluator. Each case has an
    /// `expression` string and the expected JS-evaluated `result`.
    /// Exposed so `bids-expr` can use this as a parity oracle in tests.
    #[serde(default)]
    pub expression_tests: Vec<ExpressionTest>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ExpressionTest {
    pub expression: String,
    #[serde(default)]
    pub result: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct Objects {
    /// Long-name → entity definition (e.g. `"subject"` → `{ name: "sub", ... }`).
    pub entities: IndexMap<String, Entity>,
    pub formats: IndexMap<String, Format>,
    /// `metadata.<FieldName>` → field metadata (name, description, type, ...).
    /// Keys may be `"CamelCase"` aliases; the canonical name is in `name`.
    #[serde(default)]
    pub metadata: IndexMap<String, MetadataField>,
    /// `columns.<RuleKey>` → column schema (e.g. `"name__channels"` →
    /// `{ name: "name", type: "string", ... }`). Rule keys may carry
    /// `__suffix` discriminators while `name` is the on-disk header.
    #[serde(default)]
    pub columns: IndexMap<String, ColumnSchema>,
    /// Raw `objects.enums` — preserved as-is so selectors can dereference
    /// into it (e.g. `schema.objects.enums._StandardTemplateCoordSys.enum`)
    /// without re-parsing the embedded schema.
    #[serde(default)]
    pub enums: serde_json::Value,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MetadataField {
    /// Sidecar JSON key (e.g. `"RepetitionTime"`).
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// All other top-level keys on the metadata field (type, format,
    /// properties, items, etc). Captured via `#[serde(flatten)]` so the
    /// JSON-Schema validator can build a per-field validator without
    /// re-parsing the embedded schema.
    #[serde(flatten)]
    pub raw: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Entity {
    /// Short name as it appears in filenames (`"sub"`, `"acq"`, `"ce"`).
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    /// `"label"` or `"index"`, naming a `Format`.
    pub format: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Format {
    pub pattern: String,
    #[serde(default)]
    pub display_name: String,
}

/// A column schema as it appears in `objects.columns.<RuleKey>`.
///
/// One of three shapes:
///   * Plain typed: `type` + optional `format`/`pattern`/`enum`/`unit`/
///     `minimum`/`maximum`.
///   * `anyOf`: union of typed shapes (we collect the inner formats).
///   * `definition`: the column is fully overridable via a sidecar
///     `ColumnDefinition` block; the embedded `definition` is the
///     default fallback when no sidecar override is present.
#[derive(Debug, Deserialize, Clone)]
pub struct ColumnSchema {
    /// On-disk header name (`name__channels` rule key → `"name"` header).
    /// Optional because `anyOf` inner schemas (nested `ColumnSchema`)
    /// carry only `type`/`format`, not a header name. Top-level entries
    /// in `objects.columns` always have one — `column_by_key().name`
    /// callers can `.expect("top-level column missing name")` safely.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(rename = "type", default)]
    pub type_: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default, rename = "enum")]
    pub enum_: Option<Vec<String>>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub minimum: Option<f64>,
    #[serde(default)]
    pub maximum: Option<f64>,
    #[serde(default, rename = "anyOf")]
    pub any_of: Option<Vec<ColumnSchema>>,
    /// When present, the column is "conventional" — its type, levels,
    /// units, etc. are sourced from the sidecar's `ColumnDefinition` for
    /// this column at validation time (with `definition` as default).
    /// Mirrors the `'definition' in schemaObject` branch in TS
    /// `tables.ts::getValueSignature`.
    #[serde(default)]
    pub definition: Option<ColumnDefinition>,
}

/// Sidecar-side column definition (and the in-schema default for
/// `definition`-bearing columns). Mirrors TS `ColumnDefinition`.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ColumnDefinition {
    #[serde(default, rename = "Format")]
    pub format: Option<String>,
    #[serde(default, rename = "Levels")]
    pub levels: Option<IndexMap<String, serde_json::Value>>,
    #[serde(default, rename = "Units")]
    pub units: Option<String>,
    #[serde(default, rename = "Minimum")]
    pub minimum: Option<f64>,
    #[serde(default, rename = "Maximum")]
    pub maximum: Option<f64>,
}

/// `rules.tabular_data.*` leaf: a selectors list plus column rules.
#[derive(Debug, Deserialize, Clone)]
pub struct TabularRule {
    #[serde(default)]
    pub selectors: Vec<String>,
    #[serde(default)]
    pub columns: IndexMap<String, ColumnRequirement>,
    #[serde(default)]
    pub initial_columns: Vec<String>,
    #[serde(default)]
    pub index_columns: Vec<String>,
    /// `"allowed" | "allowed_if_defined" | "not_allowed"`. Absent means
    /// no opinion (defaults to `"n/a"` in TS `getRequirement`).
    #[serde(default)]
    pub additional_columns: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum ColumnRequirement {
    Plain(String),
    Detailed {
        level: String,
        // We accept and ignore `level_addendum` / `description_addendum`.
    },
}

impl ColumnRequirement {
    pub fn level(&self) -> &str {
        match self {
            Self::Plain(s) => s,
            Self::Detailed { level, .. } => level,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Rules {
    /// `files.raw.<datatype>.<rule_name>` and `files.deriv.<...>` mostly. We
    /// keep the full tree as `serde_json::Value` and flatten at access time.
    pub files: serde_json::Value,
    /// Canonical entity order (long-key names), used to reconstruct the
    /// expected filename for `FILENAME_MISMATCH`.
    #[serde(default)]
    pub entities: Vec<String>,
    /// `rules.sidecars` tree — each leaf has `selectors` + `fields`.
    /// Kept as `serde_json::Value` and flattened at access time.
    #[serde(default)]
    pub sidecars: serde_json::Value,
    /// `rules.json` tree — leaves emit `JSON_KEY_*` against the file's
    /// own JSON contents (not the merged sidecar).
    #[serde(default)]
    pub json: serde_json::Value,
    /// `rules.dataset_metadata` — top-level metadata rules (also emit
    /// `JSON_KEY_*` against the file's own contents).
    #[serde(default)]
    pub dataset_metadata: serde_json::Value,
    /// `rules.modalities.<modality>.datatypes` — maps each modality to
    /// the datatypes it covers, used to derive `modality` from `datatype`.
    #[serde(default)]
    pub modalities: serde_json::Value,
    /// `rules.tabular_data.*` tree — leaves emit TSV column issues.
    #[serde(default)]
    pub tabular_data: serde_json::Value,
    /// `rules.checks.*` tree — leaves are general "if selectors match,
    /// run checks; if any check fails, emit issue" rules. Used heavily
    /// for NIfTI header consistency.
    #[serde(default)]
    pub checks: serde_json::Value,
}

/// A filename rule as it appears in `rules.files.*`.
///
/// Rules come in three matcher flavors:
///   * `suffixes` — most common; match if file's suffix is in the list.
///   * `path` — exact dataset-relative path (e.g. `"CHANGES"`, `"dataset_description.json"`).
///   * `stem` — glob against the file stem (e.g. `"README"`, `"participants"`),
///     usually paired with `extensions`.
///
/// A leaf in `rules.files.*` has at least one of these three.
#[derive(Debug, Deserialize, Clone)]
pub struct FilenameRule {
    #[serde(default)]
    pub suffixes: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub datatypes: Vec<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub stem: Option<String>,
    /// Long-key → `"required"`/`"optional"`/`"recommended"`/`"prohibited"`.
    /// Some rules embed extra structure under each entity; we accept either.
    #[serde(default)]
    pub entities: IndexMap<String, EntityRequirement>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum EntityRequirement {
    Plain(EntityLevel),
    Detailed {
        #[serde(default)]
        level: Option<EntityLevel>,
    },
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntityLevel {
    Required,
    Optional,
    Recommended,
    Prohibited,
    /// `deprecated` appears on some sidecar fields. Treat as optional
    /// from a check-emission standpoint; the TS validator's
    /// `getFieldSeverity` returns `ignore` for anything not in
    /// required/recommended.
    Deprecated,
}

impl EntityRequirement {
    pub fn level(&self) -> Option<EntityLevel> {
        match self {
            EntityRequirement::Plain(l) => Some(*l),
            EntityRequirement::Detailed { level } => *level,
        }
    }
}

/// Global typed schema accessor. The embedded JSON is parsed to a
/// `serde_json::Value`, the local patch layer ([`patches`]) is applied,
/// then the result is deserialized into `Schema` — once, behind a
/// `OnceLock`. Subtree "raw" views needed by downstream crates
/// (metadata-field bodies, enums) are captured *inside* the typed struct
/// via `#[serde(flatten)]` / `serde_json::Value` fields. The extra
/// `Value` materialization is the price of keeping the on-disk JSON
/// byte-verbatim (see [`patches`]); it runs once at startup.
pub fn schema() -> &'static Schema {
    static CELL: OnceLock<Schema> = OnceLock::new();
    CELL.get_or_init(|| {
        // Parse the raw schema into `serde_json::Value` first, run
        // local patches (see `patches.rs` for the audit trail and
        // upstream-fix detectors), then deserialize into the typed
        // `Schema`. The raw bytes returned by `BUNDLED_SCHEMA_JSON`
        // and `bundled_schema_value()` remain unmodified — patches
        // only affect the typed `schema()` view that the validator
        // engine reads from.
        let mut value: serde_json::Value = serde_json::from_str(BUNDLED_SCHEMA_JSON)
            .expect("embedded schema.json failed to parse");
        patches::apply_all(&mut value);
        serde_json::from_value::<Schema>(value)
            .expect("embedded schema failed to deserialize after patching")
    })
}

fn bundled_schema_value() -> &'static serde_json::Value {
    static CELL: OnceLock<serde_json::Value> = OnceLock::new();
    CELL.get_or_init(|| {
        serde_json::from_str(BUNDLED_SCHEMA_JSON).expect("embedded schema.json failed to parse")
    })
}

/// Schema source selected by the CLI. The validation engine is still
/// generated from the bundled schema, so runtime schemas are accepted
/// only when they are structurally identical to the bundled schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaSource {
    Bundled,
    LocalPath(PathBuf),
}

/// Active schema metadata reported by callers. This is intentionally
/// small: all validation rule access still goes through the generated
/// bundled schema tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaContext {
    pub source: SchemaSource,
    pub schema_version: String,
    pub bids_version: String,
}

impl SchemaContext {
    pub fn source_label(&self) -> String {
        match &self.source {
            SchemaSource::Bundled => "bundled".to_string(),
            SchemaSource::LocalPath(path) => path.display().to_string(),
        }
    }
}

#[derive(Debug)]
pub enum SchemaLoadError {
    UnsupportedRemote(String),
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Json {
        label: String,
        source: serde_json::Error,
    },
    Incompatible {
        label: String,
        schema_version: Option<String>,
        bids_version: Option<String>,
    },
}

impl fmt::Display for SchemaLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRemote(spec) => write!(
                f,
                "remote schema loading is not supported by the Rust beta yet: {spec}"
            ),
            Self::Read { path, source } => {
                write!(f, "failed to read schema {}: {source}", path.display())
            }
            Self::Json { label, source } => write!(f, "failed to parse schema {label}: {source}"),
            Self::Incompatible {
                label,
                schema_version,
                bids_version,
            } => write!(
                f,
                "runtime schema {label} is not compatible with this binary's generated schema tables (schema_version={:?}, bids_version={:?}; bundled schema_version={}, bids_version={})",
                schema_version,
                bids_version,
                schema().schema_version,
                schema().bids_version
            ),
        }
    }
}

impl std::error::Error for SchemaLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub fn bundled_schema_context() -> SchemaContext {
    let s = schema();
    SchemaContext {
        source: SchemaSource::Bundled,
        schema_version: s.schema_version.clone(),
        bids_version: s.bids_version.clone(),
    }
}

pub fn load_schema_context(spec: Option<&str>) -> Result<SchemaContext, SchemaLoadError> {
    let Some(spec) = spec else {
        return Ok(bundled_schema_context());
    };
    if spec.starts_with("http://") || spec.starts_with("https://") {
        return Err(SchemaLoadError::UnsupportedRemote(spec.to_string()));
    }
    if matches_bundled_schema_tag(spec) {
        return Ok(bundled_schema_context());
    }
    if looks_like_remote_schema_tag(spec) {
        return Err(SchemaLoadError::UnsupportedRemote(spec.to_string()));
    }
    let path = Path::new(spec);
    let text = std::fs::read_to_string(path).map_err(|source| SchemaLoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    validate_runtime_schema_text(&text, SchemaSource::LocalPath(path.to_path_buf()))
}

fn matches_bundled_schema_tag(spec: &str) -> bool {
    spec == schema().schema_version
        || spec == format!("v{}", schema().schema_version)
        || spec == schema().bids_version
}

fn looks_like_remote_schema_tag(spec: &str) -> bool {
    matches!(spec, "stable" | "latest")
        || spec.starts_with('v')
        || spec
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
            && spec.matches('.').count() >= 1
}

pub fn validate_runtime_schema_text(
    text: &str,
    source: SchemaSource,
) -> Result<SchemaContext, SchemaLoadError> {
    let label = match &source {
        SchemaSource::Bundled => "bundled".to_string(),
        SchemaSource::LocalPath(path) => path.display().to_string(),
    };
    let runtime_value: serde_json::Value =
        serde_json::from_str(text).map_err(|source| SchemaLoadError::Json {
            label: label.clone(),
            source,
        })?;
    let runtime_schema: Schema =
        serde_json::from_value(runtime_value.clone()).map_err(|source| SchemaLoadError::Json {
            label: label.clone(),
            source,
        })?;
    if &runtime_value != bundled_schema_value() {
        return Err(SchemaLoadError::Incompatible {
            label,
            schema_version: Some(runtime_schema.schema_version),
            bids_version: Some(runtime_schema.bids_version),
        });
    }

    Ok(SchemaContext {
        source,
        schema_version: runtime_schema.schema_version,
        bids_version: runtime_schema.bids_version,
    })
}

/// Look up an entity definition by its short name (the form that appears in
/// filenames, e.g. `"sub"`, `"acq"`, `"ce"`). Mirrors
/// `getEntityByLiteral` in `src/validators/filenameValidate.ts`.
pub fn entity_by_short_name(short: &str) -> Option<(&'static str, &'static Entity)> {
    schema()
        .objects
        .entities
        .iter()
        .find(|(_, e)| e.name == short)
        .map(|(k, v)| (k.as_str(), v))
}

/// Look up an entity's short filename name by its schema long key (e.g.
/// `"subject"` → `"sub"`). Mirrors `lookupEntityLiteral`. Backed by the
/// build-time `ENTITIES` static (plan.md Phase 1a) — no JSON access.
pub fn entity_short_name(long: &str) -> Option<&'static str> {
    ENTITIES
        .iter()
        .find(|(l, _, _)| *l == long)
        .map(|(_, short, _)| *short)
}

/// Look up an entity's format-name by its short (filename) form. Used
/// by `bids-core::rules` to anchor the entity-label regex. Backed by
/// the build-time `ENTITIES` static.
pub fn entity_format_by_short_name(short: &str) -> Option<&'static str> {
    ENTITIES
        .iter()
        .find(|(_, s, _)| *s == short)
        .map(|(_, _, format)| *format)
}

/// A sidecar rule: a `selectors` list and a `fields` dict.
#[derive(Debug, Deserialize, Clone)]
pub struct SidecarRule {
    #[serde(default)]
    pub selectors: Vec<String>,
    #[serde(default)]
    pub fields: IndexMap<String, FieldRequirement>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum FieldRequirement {
    Plain(EntityLevel),
    Detailed {
        #[serde(default)]
        level: Option<EntityLevel>,
        /// Custom diagnostic emitted in place of the generic
        /// `*_KEY_REQUIRED` / `*_KEY_RECOMMENDED` code. Mirrors
        /// `requirement.issue?.code/message` in
        /// `src/schema/applyRules.ts::evalJsonCheck`.
        #[serde(default)]
        issue: Option<CustomIssue>,
        /// Conditional severity bump. TS regex:
        /// `^(required|recommended) if \`(\w+)\` is \`(\w+)\`$`. When the
        /// referenced sidecar key matches the value, the requirement
        /// level is replaced. Mirrors `getFieldSeverity` in
        /// `src/schema/applyRules.ts`.
        #[serde(default)]
        level_addendum: Option<String>,
        // `description_addendum` is accepted but ignored.
    },
}

#[derive(Debug, Deserialize, Clone)]
pub struct CustomIssue {
    pub code: String,
    #[serde(default)]
    pub message: String,
}

impl FieldRequirement {
    pub fn level(&self) -> Option<EntityLevel> {
        match self {
            Self::Plain(l) => Some(*l),
            Self::Detailed { level, .. } => *level,
        }
    }

    pub fn custom_issue(&self) -> Option<&CustomIssue> {
        match self {
            Self::Plain(_) => None,
            Self::Detailed { issue, .. } => issue.as_ref(),
        }
    }

    pub fn level_addendum(&self) -> Option<&str> {
        match self {
            Self::Plain(_) => None,
            Self::Detailed { level_addendum, .. } => level_addendum.as_deref(),
        }
    }
}

/// Walk `rules.sidecars` and yield each `(dotted_path, rule)` leaf.
pub fn iter_sidecar_rules() -> impl Iterator<Item = (String, SidecarRule)> {
    let mut out = Vec::new();
    walk_rules_with_fields(
        &schema().rules.sidecars,
        String::from("rules.sidecars"),
        &mut out,
    );
    out.into_iter()
}

/// Walk `rules.json` + `rules.dataset_metadata` and yield each
/// `(dotted_path, rule)` leaf. These are the rules that produce
/// `JSON_KEY_REQUIRED` / `JSON_KEY_RECOMMENDED` rather than
/// `SIDECAR_KEY_*` — they validate the file's own JSON content rather
/// than the merged sidecar.
pub fn iter_json_rules() -> impl Iterator<Item = (String, SidecarRule)> {
    let mut out = Vec::new();
    walk_rules_with_fields(&schema().rules.json, String::from("rules.json"), &mut out);
    walk_rules_with_fields(
        &schema().rules.dataset_metadata,
        String::from("rules.dataset_metadata"),
        &mut out,
    );
    out.into_iter()
}

fn walk_rules_with_fields(
    node: &serde_json::Value,
    path: String,
    out: &mut Vec<(String, SidecarRule)>,
) {
    let Some(obj) = node.as_object() else { return };

    // Detect a leaf: presence of `fields` (with `selectors` optional but typical).
    if obj.contains_key("fields") {
        match serde_json::from_value::<SidecarRule>(node.clone()) {
            Ok(rule) => out.push((path, rule)),
            Err(e) => panic!("schema rule at {path} failed to parse: {e}"),
        }
        return;
    }

    for (k, v) in obj {
        walk_rules_with_fields(v, format!("{path}.{k}"), out);
    }
}

/// Map a datatype name (e.g. `"anat"`) to its modality (e.g. `"mri"`).
/// Backed by the build-time `MODALITY_OF` static (plan.md Phase 1a)
/// — no JSON access, no runtime IndexMap allocation.
pub fn modality_of(datatype: &str) -> Option<&'static str> {
    MODALITY_OF
        .iter()
        .find(|(dt, _)| *dt == datatype)
        .map(|(_, modality)| *modality)
}

/// `rules.checks.*` leaf: selectors + checks + an `issue` template
/// describing what to emit when any check fails.
#[derive(Debug, Deserialize, Clone)]
pub struct CheckRule {
    #[serde(default)]
    pub selectors: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    pub issue: CheckIssue,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CheckIssue {
    pub code: String,
    #[serde(default)]
    pub message: String,
    /// `"error"` (default) or `"warning"`. We accept any string; the
    /// caller maps it to its own `Severity`.
    #[serde(default = "default_level")]
    pub level: String,
}

fn default_level() -> String {
    "error".to_string()
}

/// `meta.associations.*` leaf. Associations are not validation issues by
/// themselves; they populate `context.associations` for `rules.checks.*`.
#[derive(Debug, Deserialize, Clone)]
pub struct AssociationRule {
    #[serde(default)]
    pub selectors: Vec<String>,
    pub target: AssociationTarget,
    #[serde(default = "default_true")]
    pub inherit: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
pub struct AssociationTarget {
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub extension: StringOrVec,
    #[serde(default)]
    pub entities: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(untagged)]
pub enum StringOrVec {
    One(String),
    Many(Vec<String>),
    #[default]
    None,
}

impl StringOrVec {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s],
            Self::Many(xs) => xs,
            Self::None => Vec::new(),
        }
    }
}

/// Walk `meta.associations` and yield each `(association_key, rule)`.
pub fn iter_association_rules() -> impl Iterator<Item = (String, AssociationRule)> {
    let mut out = Vec::new();
    let Some(obj) = schema().meta.associations.as_object() else {
        return out.into_iter();
    };
    for (key, value) in obj {
        match serde_json::from_value::<AssociationRule>(value.clone()) {
            Ok(rule) => out.push((key.clone(), rule)),
            Err(e) => panic!("schema association rule {key} failed to parse: {e}"),
        }
    }
    out.into_iter()
}

/// Walk `rules.checks` and yield each `(dotted_path, rule)` leaf.
pub fn iter_check_rules() -> impl Iterator<Item = (String, CheckRule)> {
    let mut out = Vec::new();
    walk_check_rules(
        &schema().rules.checks,
        String::from("rules.checks"),
        &mut out,
    );
    out.into_iter()
}

fn walk_check_rules(node: &serde_json::Value, path: String, out: &mut Vec<(String, CheckRule)>) {
    let Some(obj) = node.as_object() else { return };
    if obj.contains_key("checks") && obj.contains_key("issue") {
        match serde_json::from_value::<CheckRule>(node.clone()) {
            Ok(rule) => out.push((path, rule)),
            Err(e) => panic!("schema check rule at {path} failed to parse: {e}"),
        }
        return;
    }
    for (k, v) in obj {
        walk_check_rules(v, format!("{path}.{k}"), out);
    }
}

/// Walk `rules.tabular_data` and yield each `(dotted_path, rule)` leaf.
pub fn iter_tabular_rules() -> impl Iterator<Item = (String, TabularRule)> {
    let mut out = Vec::new();
    walk_tabular(
        &schema().rules.tabular_data,
        String::from("rules.tabular_data"),
        &mut out,
    );
    out.into_iter()
}

fn walk_tabular(node: &serde_json::Value, path: String, out: &mut Vec<(String, TabularRule)>) {
    let Some(obj) = node.as_object() else { return };
    // A leaf has `columns` (and typically `selectors`).
    if obj.contains_key("columns") {
        match serde_json::from_value::<TabularRule>(node.clone()) {
            Ok(rule) => out.push((path, rule)),
            Err(e) => panic!("schema tabular rule at {path} failed to parse: {e}"),
        }
        return;
    }
    for (k, v) in obj {
        walk_tabular(v, format!("{path}.{k}"), out);
    }
}

/// Look up a column schema by its `objects.columns` rule key
/// (e.g. `"name__channels"`). Returns `None` for unknown keys — the
/// caller is responsible for treating that as a schema bug.
pub fn column_by_key(key: &str) -> Option<&'static ColumnSchema> {
    schema().objects.columns.get(key)
}

impl ColumnSchema {
    /// On-disk header name. Top-level entries in `objects.columns` always
    /// supply one; `anyOf` inner entries do not but should never be looked
    /// up by name. Falls back to `""` if absent.
    pub fn header_name(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }
}

/// Walk `rules.files` and yield every leaf `FilenameRule` plus its dotted path
/// (e.g. `"rules.files.raw.anat.nonparametric"`). A node is considered a rule
/// if it has any of the rule shape keys.
pub fn iter_filename_rules() -> impl Iterator<Item = (String, FilenameRule)> {
    let mut out = Vec::new();
    walk(&schema().rules.files, String::from("rules.files"), &mut out);
    out.into_iter()
}

fn walk(node: &serde_json::Value, path: String, out: &mut Vec<(String, FilenameRule)>) {
    let Some(obj) = node.as_object() else { return };

    let is_rule = obj.contains_key("suffixes")
        || obj.contains_key("extensions")
        || obj.contains_key("datatypes")
        || obj.contains_key("entities")
        || obj.contains_key("path")
        || obj.contains_key("stem");
    if is_rule {
        match serde_json::from_value::<FilenameRule>(node.clone()) {
            Ok(rule) => out.push((path, rule)),
            Err(e) => panic!("schema filename rule at {path} failed to parse: {e}"),
        }
        return;
    }

    for (k, v) in obj {
        walk(v, format!("{path}.{k}"), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_parses() {
        let s = schema();
        assert!(!s.objects.entities.is_empty());
        assert!(s.objects.entities.contains_key("subject"));
        assert_eq!(s.objects.entities["subject"].name, "sub");
    }

    #[test]
    fn bundled_runtime_schema_context_is_accepted() {
        let ctx = validate_runtime_schema_text(BUNDLED_SCHEMA_JSON, SchemaSource::Bundled)
            .expect("bundled schema must validate as runtime schema");
        assert_eq!(ctx.schema_version, schema().schema_version);
        assert_eq!(ctx.bids_version, schema().bids_version);
    }

    #[test]
    fn modified_runtime_schema_is_rejected() {
        let mut value: serde_json::Value = serde_json::from_str(BUNDLED_SCHEMA_JSON).unwrap();
        value["schema_version"] = serde_json::Value::String("9.9.9".to_string());
        let text = serde_json::to_string(&value).unwrap();
        let err = validate_runtime_schema_text(&text, SchemaSource::Bundled).unwrap_err();
        assert!(matches!(err, SchemaLoadError::Incompatible { .. }));
    }

    #[test]
    fn url_schema_is_rejected_for_beta() {
        let err = load_schema_context(Some("https://example.test/schema.json")).unwrap_err();
        assert!(matches!(err, SchemaLoadError::UnsupportedRemote(_)));
    }

    #[test]
    fn enumerates_tabular_rules() {
        let rules: Vec<_> = iter_tabular_rules().collect();
        assert!(
            rules.len() >= 20,
            "expected ≥20 tabular rules, got {}",
            rules.len()
        );
        // Spot-check a familiar leaf shape.
        let (p, r) = rules
            .iter()
            .find(|(p, _)| p.ends_with(".EEGChannels"))
            .expect("EEGChannels rule missing");
        assert!(!r.selectors.is_empty(), "rule {p} has no selectors");
        assert!(r.columns.contains_key("name__channels"));
    }

    #[test]
    fn looks_up_column_schemas() {
        let age = column_by_key("age").expect("age column");
        assert_eq!(age.header_name(), "age");
        assert!(age.definition.is_some(), "age must be definition-bearing");
        let duration = column_by_key("duration").expect("duration column");
        assert_eq!(duration.minimum, Some(0.0));
        let component = column_by_key("component").expect("component column");
        assert!(component.enum_.as_ref().unwrap().contains(&"x".to_string()));
        // `group__emg` uses anyOf — inner entries deserialize without a `name`.
        let group = column_by_key("group__emg").expect("group__emg column");
        let any_of = group.any_of.as_ref().expect("group__emg should have anyOf");
        assert_eq!(any_of.len(), 2);
        assert!(any_of[0].name.is_none(), "anyOf inner has no name");
    }

    #[test]
    fn enumerates_filename_rules() {
        let rules: Vec<_> = iter_filename_rules().collect();
        assert!(
            rules.len() > 100,
            "expected >100 filename rules, got {}",
            rules.len()
        );
        // Spot-check anat raw is present.
        assert!(
            rules
                .iter()
                .any(|(p, _)| p.starts_with("rules.files.raw.anat")),
            "expected at least one raw/anat rule"
        );
    }
}
