//! Oracle test for the expression evaluator.
//!
//! Loads `meta.expression_tests` from the embedded BIDS schema and runs
//! each case against the Rust evaluator, comparing against the
//! JS-evaluated reference result. Any drift between Rust and the schema
//! oracle is a parity bug — failing tests must be fixed (or, for cases
//! the schema explicitly out-of-scopes us, listed under
//! `KNOWN_PARSE_FAILURES`).
//!
//! This guards against silent regressions when the schema bumps and
//! introduces new test cases or operators.

use bids_expr::{parse_and_eval, Value};
use bids_schema_codegen::schema;
use indexmap::IndexMap;
use serde_json::Value as J;

/// Cases the parser can't accept yet. Documented in `audit_response.md`
/// (High #5 — object literals). Each entry is the raw oracle
/// `expression` string. Empty `{}` is the only case in schema 1.2.1;
/// when these become parseable, drop the entry and the test will start
/// asserting on them.
const KNOWN_PARSE_FAILURES: &[&str] = &["type({})"];

#[test]
fn schema_expression_tests_pass() {
    let tests = &schema().meta.expression_tests;
    assert!(
        !tests.is_empty(),
        "embedded schema has no expression_tests; schema bump regression?"
    );

    let ctx = Value::Object(IndexMap::new());
    let mut failures: Vec<String> = Vec::new();
    let mut parse_skips: Vec<String> = Vec::new();

    for case in tests {
        if KNOWN_PARSE_FAILURES.contains(&case.expression.as_str()) {
            parse_skips.push(case.expression.clone());
            continue;
        }
        match parse_and_eval(&case.expression, &ctx) {
            Ok(actual) => {
                if !json_matches_value(&case.result, &actual) {
                    failures.push(format!(
                        "  {expr:<50}  expected {expected}  got {actual}",
                        expr = case.expression,
                        expected = case.result,
                        actual = display(&actual),
                    ));
                }
            }
            Err(e) => failures.push(format!(
                "  {expr:<50}  parse/eval error: {e}",
                expr = case.expression,
            )),
        }
    }

    // Skipped cases must still appear in the schema (catches stale
    // KNOWN_PARSE_FAILURES entries). The parse-skip set is the
    // intersection of expected-skips with the schema oracle.
    for skip in &parse_skips {
        assert!(
            tests.iter().any(|t| &t.expression == skip),
            "KNOWN_PARSE_FAILURES contains {skip:?} which is no longer in the schema oracle"
        );
    }

    if !failures.is_empty() {
        panic!(
            "schema expression_tests: {} of {} cases failed (skipped {} known parse-failures)\n{}",
            failures.len(),
            tests.len() - parse_skips.len(),
            parse_skips.len(),
            failures.join("\n"),
        );
    }
}

/// Compare a schema oracle's expected JSON to a runtime [`Value`].
///
/// - Schema `null` matches `Value::Null` and `Value::Undefined`
///   (selectors that look up missing top-level identifiers yield
///   `Undefined`; the oracle never distinguishes the two).
/// - Numbers compare with strict equality (the oracle uses small
///   exact values: 0, 1, -1, 3, 1.5, 729, etc).
/// - Arrays/objects compare recursively, in order.
fn json_matches_value(expected: &J, actual: &Value) -> bool {
    match (expected, actual) {
        (J::Null, Value::Null | Value::Undefined) => true,
        (J::Bool(b), Value::Bool(v)) => b == v,
        (J::Number(n), Value::Number(v)) => n.as_f64() == Some(*v),
        (J::String(s), Value::String(v)) => s == v,
        (J::Array(xs), Value::Array(vs)) => {
            xs.len() == vs.len()
                && xs
                    .iter()
                    .zip(vs.iter())
                    .all(|(x, v)| json_matches_value(x, v))
        }
        (J::Object(map), Value::Object(vm)) => {
            map.len() == vm.len()
                && map
                    .iter()
                    .all(|(k, x)| vm.get(k).is_some_and(|v| json_matches_value(x, v)))
        }
        _ => false,
    }
}

fn display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Undefined => "undefined".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.is_finite() {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        Value::String(s) => format!("{s:?}"),
        Value::Array(xs) => {
            let parts: Vec<String> = xs.iter().map(display).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{k:?}: {}", display(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}
