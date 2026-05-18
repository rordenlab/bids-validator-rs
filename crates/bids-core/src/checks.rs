//! `rules.checks.*` evaluator.
//!
//! Mirrors `_evalRuleChecks` in `src/schema/applyRules.ts`. Each rule has:
//!
//!   * `selectors` — all must evaluate truthy for the rule to apply.
//!   * `checks` — if any evaluates falsy, emit `rule.issue.code` with
//!     the configured `level`. The first failing check halts the rule.
//!   * `issue: {code, message, level}` — output template.
//!
//! The selector grammar is the same as for `rules.sidecars` / etc., so
//! we reuse the bids-expr evaluator and the eval context built by
//! `metadata::build_eval_context`.

use std::sync::OnceLock;

use bids_expr::{eval_with, parse, EvalContext, Expr, Value};
use bids_schema_codegen::{iter_check_rules, CheckRule};

use crate::compile::parse_all_or_skip;
use crate::issue::{Issue, Severity};
use crate::metadata::SidecarContext;

/// Pre-parsed selectors + checks for every `rules.checks.*` leaf.
struct CompiledCheckRule {
    path: String,
    selectors: Vec<Expr>,
    checks: Vec<Expr>,
    rule: CheckRule,
}

fn compiled_rules() -> &'static [CompiledCheckRule] {
    static CELL: OnceLock<Vec<CompiledCheckRule>> = OnceLock::new();
    CELL.get_or_init(|| {
        iter_check_rules()
            .filter_map(|(path, rule)| {
                let selectors = parse_all_or_skip(&path, "selector", &rule.selectors)?;
                let checks = parse_all_or_skip(&path, "check", &rule.checks)?;
                Some(CompiledCheckRule {
                    path,
                    selectors,
                    checks,
                    rule,
                })
            })
            .collect()
    })
}

/// Evaluate every `rules.checks.*` leaf against this file. Emits one
/// issue per rule whose selectors all pass but whose checks fail.
///
/// `ctx_value` is the prebuilt selector-evaluation context from
/// [`crate::metadata::build_eval_context`]; callers build it once per
/// file and share it across `check_sidecars`, `check_json`,
/// `check_rules`, and `check_tables` to avoid four redundant builds.
pub fn check_rules(c: &SidecarContext<'_>, ctx_value: &Value) -> Vec<Issue> {
    let mut out = Vec::new();
    let ec = EvalContext::with_files(ctx_value, c.dataset_files);
    for r in compiled_rules() {
        if r.checks.is_empty() {
            continue;
        }
        // Skip the rule if any selector evaluates falsy or errors out.
        let all_selectors_pass = r
            .selectors
            .iter()
            .all(|expr| eval_with(expr, &ec).map(|v| v.is_truthy()).unwrap_or(false));
        if !all_selectors_pass {
            continue;
        }
        let all_checks_pass = r
            .checks
            .iter()
            .all(|expr| eval_with(expr, &ec).map(|v| v.is_truthy()).unwrap_or(false));
        if !all_checks_pass {
            let severity = match r.rule.issue.level.as_str() {
                "warning" => Severity::Warning,
                _ => Severity::Error,
            };
            out.push(Issue {
                code: r.rule.issue.code.clone(),
                sub_code: None,
                severity,
                location: c.bids_path.to_string(),
                rule: Some(r.path.clone()),
                issue_message: None,
                affects: None,
                line: None,
                code_message: Some(format_schema_message(&r.rule.issue.message, &ec)),
            });
        }
    }
    out
}

fn format_schema_message(template: &str, ec: &EvalContext) -> String {
    if !template.contains('{') {
        return template.to_string();
    }

    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];
        let Some(end) = after_open.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expr = &after_open[..end];
        let value = parse(expr)
            .ok()
            .and_then(|parsed| eval_with(&parsed, ec).ok())
            .map(|v| v.to_display_string())
            .unwrap_or_else(|| "undefined".to_string());
        out.push_str(&value);
        rest = &after_open[end + 1..];
    }
    out.push_str(rest);
    out
}
