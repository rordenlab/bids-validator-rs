//! Runtime value type for the expression evaluator. Mirrors the small
//! set of JSON-shaped values selector/check expressions actually traffic
//! in: strings, numbers, booleans, arrays, objects, and null/undefined.
//!
//! `Null` and `Undefined` are distinct (the TS code reads `entities.task`
//! as `undefined` when missing — we propagate that), but most operations
//! treat them identically (truthiness, `==` against each other, etc.).

use std::fmt;

use indexmap::IndexMap;
use serde_json::Value as J;

#[derive(Debug, Clone)]
pub enum Value {
    Null,
    /// Missing identifier / property access on a non-object. Distinguished
    /// from `Null` for parity with JS semantics in selectors like
    /// `!("X" in sidecar)`.
    Undefined,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(IndexMap<String, Value>),
}

impl Value {
    /// JS-style truthiness. Per ECMA-262: `false`, `0`, `NaN`, `""`,
    /// `null`, `undefined` are falsy. Everything else (including `[]`
    /// and `{}`) is truthy.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Null | Self::Undefined => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::String(s) => !s.is_empty(),
            Self::Array(_) | Self::Object(_) => true,
        }
    }

    /// JS `==` (loose equality). We implement the cases that selectors
    /// actually exercise; "implausible" comparisons (e.g. array vs
    /// number) fall through to `false`.
    pub fn loose_eq(&self, other: &Self) -> bool {
        use Value::*;
        match (self, other) {
            (Null | Undefined, Null | Undefined) => true,
            (Bool(a), Bool(b)) => a == b,
            (Number(a), Number(b)) => a == b,
            (String(a), String(b)) => a == b,
            (Number(a), String(b)) | (String(b), Number(a)) => b.parse::<f64>() == Ok(*a),
            (Bool(b), Number(n)) | (Number(n), Bool(b)) => (*b as i64 as f64) == *n,
            (Array(_), Array(_)) | (Object(_), Object(_)) => false, // reference equality in JS
            _ => false,
        }
    }

    /// Coerce to a JSON-ish string for display.
    pub fn to_display_string(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Undefined => "undefined".into(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => {
                if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e16 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Self::String(s) => s.clone(),
            Self::Array(_) => "[array]".into(),
            Self::Object(_) => "[object]".into(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_display_string())
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<serde_json::Value> for Value {
    fn from(j: serde_json::Value) -> Self {
        match j {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(b),
            J::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
            J::String(s) => Value::String(s),
            J::Array(xs) => Value::Array(xs.into_iter().map(Into::into).collect()),
            J::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, v.into())).collect()),
        }
    }
}
