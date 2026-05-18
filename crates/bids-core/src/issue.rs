//! Issue model, shaped to match the JS validator's JSON output.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::ser::SerializeStruct;
use serde::Serialize;

use bids_schema_codegen::{iter_check_rules, iter_json_rules, iter_sidecar_rules};

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "subCode")]
    pub sub_code: Option<String>,
    pub severity: Severity,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "issueMessage")]
    pub issue_message: Option<String>,
    /// For sidecar-key issues: the data files whose validation is
    /// affected by this sidecar key (typically the source file).
    /// Empty / `None` for issues on the file itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affects: Option<Vec<String>>,
    /// Line number in the source file (1-based). Used by TSV checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// Deno stores code-level prose in `issues.codeMessages`, not on
    /// every issue. This internal field carries schema/custom messages
    /// until `IssueList` serializes the map.
    #[serde(skip)]
    pub code_message: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Default)]
pub struct IssueList {
    pub issues: Vec<Issue>,
}

impl Serialize for IssueList {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("IssueList", 2)?;
        state.serialize_field("issues", &self.issues)?;
        state.serialize_field("codeMessages", &code_messages_for_issues(&self.issues))?;
        state.end()
    }
}

fn code_messages_for_issues(issues: &[Issue]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for issue in issues {
        if out.contains_key(&issue.code) {
            continue;
        }
        let message = issue
            .code_message
            .clone()
            .or_else(|| schema_code_messages().get(&issue.code).cloned())
            .or_else(|| non_schema_code_message(&issue.code).map(str::to_string));
        if let Some(message) = message {
            out.insert(issue.code.clone(), message);
        }
    }
    out
}

fn schema_code_messages() -> &'static BTreeMap<String, String> {
    static CELL: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut out = BTreeMap::new();
        for (_, rule) in iter_check_rules() {
            out.entry(rule.issue.code).or_insert(rule.issue.message);
        }
        for (_, rule) in iter_sidecar_rules().chain(iter_json_rules()) {
            for requirement in rule.fields.values() {
                if let Some(custom) = requirement.custom_issue() {
                    out.entry(custom.code.clone())
                        .or_insert(custom.message.clone());
                }
            }
        }
        out
    })
}

fn non_schema_code_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "INVALID_JSON_ENCODING" => "JSON files must be valid UTF-8 encoded text.",
        "JSON_INVALID" => "Not a valid JSON file.",
        "JSON_NOT_AN_OBJECT" => "Parsed JSON file does not contain an object.",
        "MISSING_DATASET_DESCRIPTION" => {
            "A dataset_description.json file is required in the root of the dataset"
        }
        "INVALID_ENTITY_LABEL" => "entity label doesn't match format found for files with this suffix",
        "ENTITY_WITH_NO_LABEL" => "Found an entity with no label.",
        "MISSING_REQUIRED_ENTITY" => "Missing required entity for files with this suffix.",
        "ENTITY_NOT_IN_RULE" => "Entity not listed as required or optional for files with this suffix",
        "DATATYPE_MISMATCH" => {
            "The datatype directory does not match datatype of found suffix and extension"
        }
        "ALL_FILENAME_RULES_HAVE_ISSUES" => {
            "Multiple filename rules were found as potential matches. All of them had at least one issue during filename validation."
        }
        "EXTENSION_MISMATCH" => "Extension used by file does not match allowed extensions for its suffix",
        "INVALID_LOCATION" => {
            "The file has a valid name, but is located in an invalid directory."
        }
        "FILENAME_MISMATCH" => {
            "The filename is not formatted correctly. This could result from entity duplication or reordering."
        }
        "JSON_KEY_REQUIRED" => "A JSON flle is missing a key listed as required.",
        "JSON_KEY_RECOMMENDED" => "A JSON file is missing a key listed as recommended.",
        "SIDECAR_KEY_REQUIRED" => {
            "A data file's JSON sidecar is missing a key listed as required."
        }
        "SIDECAR_KEY_RECOMMENDED" => {
            "A data file's JSON sidecar is missing a key listed as recommended."
        }
        "JSON_SCHEMA_VALIDATION_ERROR" => {
            "Invalid JSON sidecar file. The sidecar is not formatted according the schema."
        }
        "TSV_ERROR" => "generic place holder for errors from tsv files",
        "TSV_COLUMN_HEADER_DUPLICATE" => {
            "Two elements in the first row of a TSV are the same. Each column header must be unique."
        }
        "TSV_EQUAL_ROWS" => "All rows must have the same number of columns as there are headers.",
        "TSV_EMPTY_LINE" => "An empty line was found in the TSV file.",
        "TSV_COLUMN_MISSING" => "A required column is missing",
        "TSV_COLUMN_ORDER_INCORRECT" => "Some TSV columns are in the incorrect order",
        "TSV_ADDITIONAL_COLUMNS_NOT_ALLOWED" => {
            "A TSV file has extra columns which are not allowed for its file type"
        }
        "TSV_ADDITIONAL_COLUMNS_MUST_DEFINE" => {
            "Additional TSV columns must be defined in the associated JSON sidecar for this file type"
        }
        "TSV_ADDITIONAL_COLUMNS_UNDEFINED" => {
            "A TSV file has extra columns which are not defined in its associated JSON sidecar"
        }
        "TSV_INDEX_VALUE_NOT_UNIQUE" => {
            "An index column(s) was specified for the tsv file and not all of the values for it are unique."
        }
        "TSV_VALUE_INCORRECT_TYPE" => {
            "A value in a column did not match the acceptable type for that column headers specified format."
        }
        "TSV_COLUMN_TYPE_REDEFINED" => {
            "A column required in a TSV file has been redefined in a sidecar file. This redefinition is being ignored."
        }
        "TSV_PSEUDO_AGE_DEPRECATED" => {
            "Use of the value \"89+\" in column \"age\" is deprecated. Use 89 for all ages 89 and over."
        }
        "INVALID_GZIP" => "The gzip file could not be decompressed. It may be corrupt or misnamed.",
        "MULTIPLE_INHERITABLE_FILES" => {
            "Multiple files in a directory were found to be valid candidates for inheritance."
        }
        "NIFTI_HEADER_UNREADABLE" => {
            "We were unable to parse header data from this NIfTI file. Please ensure it is not corrupted or mislabeled."
        }
        "AMBIGUOUS_AFFINE" => {
            "The image orientation could not be determined. This may be the result of an invalid affine matrix."
        }
        "CHECK_ERROR" => "generic place holder for errors from failed `checks` evaluated from schema.",
        "NOT_INCLUDED" => {
            "Files with such naming scheme are not part of BIDS specification. This error is most commonly caused by typos in file names that make them not BIDS compatible. Please consult the specification and make sure your files are named correctly. If this is not a file naming issue (for example when including files not yet covered by the BIDS specification) you should include a \".bidsignore\" file in your dataset (see https://github.com/bids-standard/bids-validator#bidsignore for details). Please note that derived (processed) data should be placed in /derivatives folder and source data (such as DICOMS or behavioural logs in proprietary formats) should be placed in /sourcedata folder."
        }
        "EMPTY_FILE" => "Empty files not allowed.",
        "UNUSED_STIMULUS" => {
            "There are files in the /stimuli directory that are not utilized in any _events.tsv file."
        }
        "SIDECAR_WITHOUT_DATAFILE" => {
            "A json sidecar file was found without a corresponding data file"
        }
        "SIDECAR_FIELD_OVERRIDE" => "Sidecar files should not override values assigned at a higher level.",
        "BLACKLISTED_MODALITY" => {
            "The modality in this file is blacklisted through validator configuration."
        }
        "UNSUPPORTED_DATASET_TYPE" => {
            "The dataset type is not allowed by validator configuration."
        }
        "CITATION_CFF_VALIDATION_ERROR" => {
            "The file does not pass validation using the citation.cff standard's schema.https://github.com/citation-file-format/citation-file-format/blob/main/schema-guide.md"
        }
        "FILE_READ" => "We were unable to read this file.",
        "HED_ERROR" => "The validation on this HED string returned an error.",
        "HED_WARNING" => "The validation on this HED string returned a warning.",
        "CASE_COLLISION" => "Files with the same name but different casing have been found.",
        "INVALID_FILE_ENCODING" => "File encoding is not valid UTF-8.",
        "SYMLINK_BROKEN" => "Symbolic link target does not exist.",
        "SYMLINK_CYCLE" => "Symbolic link chain contains a cycle or exceeds maximum depth.",
        "SYMLINK_OUT_OF_TREE" => "Symbolic link target escapes the repository root.",
        "SYMLINK_IN_SUBMODULE" => {
            "Symbolic link target lies within an uninitialized git submodule."
        }
        "BVEC_ROW_LENGTH" => "Rows in this bvec file do not have the same length.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(code: &str, code_message: Option<&str>) -> Issue {
        Issue {
            code: code.to_string(),
            sub_code: None,
            severity: Severity::Warning,
            location: "/dataset_description.json".to_string(),
            rule: None,
            issue_message: None,
            affects: None,
            line: None,
            code_message: code_message.map(str::to_string),
        }
    }

    #[test]
    fn serializes_deno_style_code_messages() {
        let list = IssueList {
            issues: vec![
                issue("JSON_KEY_RECOMMENDED", None),
                issue("TOO_FEW_AUTHORS", None),
            ],
        };

        let value = serde_json::to_value(list).unwrap();
        assert_eq!(
            value["codeMessages"]["JSON_KEY_RECOMMENDED"],
            json!("A JSON file is missing a key listed as recommended.")
        );
        assert!(value["codeMessages"]["TOO_FEW_AUTHORS"]
            .as_str()
            .unwrap()
            .starts_with("The 'Authors' field of 'dataset_description.json'"));
    }

    #[test]
    fn internal_code_message_overrides_catalog_message() {
        let list = IssueList {
            issues: vec![issue("JSON_KEY_RECOMMENDED", Some("schema-specific text"))],
        };

        let value = serde_json::to_value(list).unwrap();
        assert_eq!(
            value["codeMessages"]["JSON_KEY_RECOMMENDED"],
            json!("schema-specific text")
        );
        assert!(value["issues"][0].get("code_message").is_none());
    }
}

/// Top-level JSON result, matching the shape `resultToJSONStr` produces.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ValidationResult {
    pub issues: IssueList,
}
