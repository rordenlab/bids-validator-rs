//! Tree-walking interpreter. Built-in functions live here too, so the
//! evaluator and the function table stay close.

use std::collections::HashSet;
use std::sync::OnceLock;

use indexmap::IndexMap;

use crate::ast::{BinaryOp, Expr, UnaryOp};
use crate::value::Value;
use crate::EvalError;

/// Evaluation context. Carries the variable scope (`value`) and, when
/// the validator's `exists(...)` selectors need it, the dataset file
/// set (`dataset_files`). The file set is explicit-plumbed rather than
/// stored in a thread-local so per-worker parallel validation cannot
/// see leaked state from a previous run on the same thread (the prior
/// `thread_local!` design was a documented blocker for M4 parallelism).
pub struct EvalContext<'a> {
    pub value: &'a Value,
    pub dataset_files: Option<&'a HashSet<String>>,
}

impl<'a> EvalContext<'a> {
    pub fn new(value: &'a Value) -> Self {
        Self {
            value,
            dataset_files: None,
        }
    }

    pub fn with_files(value: &'a Value, dataset_files: &'a HashSet<String>) -> Self {
        Self {
            value,
            dataset_files: Some(dataset_files),
        }
    }
}

/// Evaluate an expression against a bare `Value` context. Equivalent to
/// `eval_with(expr, &EvalContext::new(ctx))`. Callers that need `exists`
/// support should use [`eval_with`] with an explicit [`EvalContext`].
pub fn eval(expr: &Expr, ctx: &Value) -> Result<Value, EvalError> {
    eval_with(expr, &EvalContext::new(ctx))
}

/// Evaluate with an explicit [`EvalContext`]. This is the entry point
/// the validator uses for selector evaluation when the dataset's file
/// set must be visible to `exists(...)`.
pub fn eval_with(expr: &Expr, ec: &EvalContext) -> Result<Value, EvalError> {
    match expr {
        Expr::Null => Ok(Value::Null),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Number(n) => Ok(Value::Number(*n)),
        Expr::String(s) => Ok(Value::String(s.clone())),
        Expr::Array(xs) => {
            let mut out = Vec::with_capacity(xs.len());
            for x in xs {
                out.push(eval_with(x, ec)?);
            }
            Ok(Value::Array(out))
        }
        Expr::Ident(name) => Ok(lookup_top(ec.value, name)),
        Expr::Member(base, name) => {
            let b = eval_with(base, ec)?;
            Ok(lookup_member(&b, name))
        }
        Expr::Call(name, args) => {
            let evaled: Result<Vec<_>, _> = args.iter().map(|a| eval_with(a, ec)).collect();
            let evaled = evaled?;
            call_builtin(name, &evaled, ec)
        }
        Expr::In(needle, haystack) => {
            let n = eval_with(needle, ec)?;
            let h = eval_with(haystack, ec)?;
            Ok(in_op(&n, &h))
        }
        Expr::Index(base, idx) => {
            let b = eval_with(base, ec)?;
            let i = eval_with(idx, ec)?;
            Ok(index_into(&b, &i))
        }
        Expr::Unary(UnaryOp::Not, inner) => {
            let v = eval_with(inner, ec)?;
            Ok(Value::Bool(!v.is_truthy()))
        }
        Expr::Unary(UnaryOp::Neg, inner) => match eval_with(inner, ec)? {
            Value::Number(n) => Ok(Value::Number(-n)),
            other => Err(EvalError::TypeError(format!(
                "unary `-` on non-number: {other}"
            ))),
        },
        Expr::Binary(op, l, r) => {
            // Short-circuit && / ||
            match op {
                BinaryOp::And => {
                    let lv = eval_with(l, ec)?;
                    if !lv.is_truthy() {
                        return Ok(lv);
                    }
                    return eval_with(r, ec);
                }
                BinaryOp::Or => {
                    let lv = eval_with(l, ec)?;
                    if lv.is_truthy() {
                        return Ok(lv);
                    }
                    return eval_with(r, ec);
                }
                _ => {}
            }
            let lv = eval_with(l, ec)?;
            let rv = eval_with(r, ec)?;
            Ok(match op {
                BinaryOp::Eq => Value::Bool(lv.loose_eq(&rv)),
                BinaryOp::Neq => Value::Bool(!lv.loose_eq(&rv)),
                BinaryOp::Lt => cmp(&lv, &rv, |a, b| a < b),
                BinaryOp::Le => cmp(&lv, &rv, |a, b| a <= b),
                BinaryOp::Gt => cmp(&lv, &rv, |a, b| a > b),
                BinaryOp::Ge => cmp(&lv, &rv, |a, b| a >= b),
                BinaryOp::Add => add_op(&lv, &rv),
                BinaryOp::Sub => arith(&lv, &rv, |a, b| a - b),
                BinaryOp::Mul => arith(&lv, &rv, |a, b| a * b),
                BinaryOp::Div => arith(&lv, &rv, |a, b| a / b),
                BinaryOp::Mod => arith(&lv, &rv, |a, b| {
                    // JS `%` is `rem`, not Euclidean mod.
                    a - (a / b).trunc() * b
                }),
                BinaryOp::Pow => arith(&lv, &rv, f64::powf),
                BinaryOp::And | BinaryOp::Or => unreachable!(),
            })
        }
    }
}

fn lookup_top(ctx: &Value, name: &str) -> Value {
    match ctx {
        Value::Object(m) => m.get(name).cloned().unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    }
}

fn lookup_member(base: &Value, name: &str) -> Value {
    match base {
        Value::Object(m) => m.get(name).cloned().unwrap_or(Value::Undefined),
        // Pseudo-property: `.length` on string/array, matching JS.
        Value::Array(a) if name == "length" => Value::Number(a.len() as f64),
        Value::String(s) if name == "length" => Value::Number(s.chars().count() as f64),
        _ => Value::Undefined,
    }
}

fn index_into(base: &Value, idx: &Value) -> Value {
    match (base, idx) {
        (Value::Array(xs), Value::Number(n)) => match float_to_index(*n) {
            Some(i) => xs.get(i).cloned().unwrap_or(Value::Undefined),
            None => Value::Undefined,
        },
        (Value::String(s), Value::Number(n)) => match float_to_index(*n) {
            Some(i) => s
                .chars()
                .nth(i)
                .map(|c| Value::String(c.to_string()))
                .unwrap_or(Value::Undefined),
            None => Value::Undefined,
        },
        // JS allows object[string] as bracket access; we support that for parity
        // with selector code that might use it.
        (Value::Object(m), Value::String(k)) => m.get(k).cloned().unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    }
}

/// Safely coerce a float to a `usize` index. Returns `None` for
/// negative, non-integer, NaN, or out-of-`usize`-range values. Casting
/// such values directly with `as usize` is undefined behavior in Rust
/// (per the language reference, before 1.45 it was UB; from 1.45 it
/// saturates, but the saturated value is semantically nonsense for
/// indexing).
fn float_to_index(n: f64) -> Option<usize> {
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
        return None;
    }
    // The largest f64 that round-trips to a usize on 64-bit platforms.
    if n > (usize::MAX as f64) {
        return None;
    }
    Some(n as usize)
}

fn arith(l: &Value, r: &Value, f: impl Fn(f64, f64) -> f64) -> Value {
    Value::Number(f(as_f64(l), as_f64(r)))
}

/// JS-flavored `+`: if either operand is a string, concatenate (with
/// string coercion on the other). Otherwise numeric add.
///
/// Schema oracle test 41 (`"cat" + "dog"` → `"catdog"`) requires the
/// string path; pre-fix we always coerced to f64 and returned `NaN`.
/// The full ECMA `+` involves ToPrimitive on objects/arrays — selectors
/// don't exercise that, so this implementation handles the cases that
/// actually appear: string ± string, string ± non-string, number ±
/// number.
fn add_op(l: &Value, r: &Value) -> Value {
    if matches!(l, Value::String(_)) || matches!(r, Value::String(_)) {
        return Value::String(format!(
            "{}{}",
            l.to_display_string(),
            r.to_display_string()
        ));
    }
    Value::Number(as_f64(l) + as_f64(r))
}

fn cmp(l: &Value, r: &Value, f: impl Fn(f64, f64) -> bool) -> Value {
    match (l, r) {
        (Value::Number(a), Value::Number(b)) => Value::Bool(f(*a, *b)),
        (Value::String(a), Value::String(b)) => {
            // Lexical comparison via cmp.
            let ord = a.cmp(b) as i8 as f64;
            Value::Bool(f(ord, 0.0))
        }
        _ => Value::Bool(false),
    }
}

fn in_op(needle: &Value, haystack: &Value) -> Value {
    // Schema oracle: `"x" in null` → null (test case 32). The same
    // propagates for undefined (lookups against absent identifiers).
    if matches!(haystack, Value::Null | Value::Undefined) {
        return Value::Null;
    }
    Value::Bool(match (needle, haystack) {
        (Value::String(s), Value::Object(m)) => m.contains_key(s),
        (Value::String(s), Value::Array(xs)) => {
            xs.iter().any(|v| matches!(v, Value::String(t) if t == s))
        }
        (Value::Number(n), Value::Array(xs)) => {
            xs.iter().any(|v| matches!(v, Value::Number(m) if *m == *n))
        }
        _ => false,
    })
}

// ---------- Built-in function table ----------

type BuiltinFn = fn(&[Value], &EvalContext) -> Result<Value, EvalError>;

fn builtins() -> &'static IndexMap<&'static str, BuiltinFn> {
    static CELL: OnceLock<IndexMap<&'static str, BuiltinFn>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut m: IndexMap<&'static str, BuiltinFn> = IndexMap::new();
        m.insert("match", builtin_match);
        m.insert("intersects", builtin_intersects);
        m.insert("type", builtin_type);
        m.insert("length", builtin_length);
        // `len` is an alias used by some `rules.checks.*` expressions
        // (e.g. `len(sidecar.EchoTime)`). TS treats it the same.
        m.insert("len", builtin_length);
        m.insert("count", builtin_count);
        m.insert("index", builtin_index);
        m.insert("substr", builtin_substr);
        m.insert("sorted", builtin_sorted);
        m.insert("allequal", builtin_allequal);
        m.insert("min", builtin_min);
        m.insert("max", builtin_max);
        m.insert("unique", builtin_unique);
        m.insert("exists", builtin_exists);
        m
    })
}

fn call_builtin(name: &str, args: &[Value], ec: &EvalContext) -> Result<Value, EvalError> {
    match builtins().get(name) {
        Some(f) => f(args, ec),
        None => Err(EvalError::UnknownFunction(name.to_string())),
    }
}

fn arity(name: &str, args: &[Value], expected: usize) -> Result<(), EvalError> {
    if args.len() != expected {
        return Err(EvalError::BadFunctionCall {
            name: name.to_string(),
            reason: format!("expected {expected} args, got {}", args.len()),
        });
    }
    Ok(())
}

fn arity_range(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), EvalError> {
    if args.len() < min || args.len() > max {
        return Err(EvalError::BadFunctionCall {
            name: name.to_string(),
            reason: format!("expected {min}..={max} args, got {}", args.len()),
        });
    }
    Ok(())
}

fn builtin_match(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("match", args, 2)?;
    // Schema oracle:
    //   * `match(null, 'pattern')`    → null   (test 15: nullish target)
    //   * `match('string', null)`     → false  (test 16: nullish pattern)
    // i.e. the target's nullishness propagates, but a nullish pattern
    // means "no match against a real string."
    if matches!(&args[0], Value::Null | Value::Undefined) {
        return Ok(Value::Null);
    }
    let (target, pat) = match (&args[0], &args[1]) {
        (Value::String(t), Value::String(p)) => (t.as_str(), p.as_str()),
        _ => return Ok(Value::Bool(false)),
    };
    match regex::Regex::new(pat) {
        Ok(re) => Ok(Value::Bool(re.is_match(target))),
        Err(_) => Ok(Value::Bool(false)),
    }
}

fn as_array_loose(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(xs) => xs.clone(),
        _ => vec![v.clone()],
    }
}

fn builtin_intersects(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("intersects", args, 2)?;
    let a = as_array_loose(&args[0]);
    let b = as_array_loose(&args[1]);
    if a.is_empty() || b.is_empty() {
        return Ok(Value::Bool(false));
    }
    let intersection: Vec<Value> = a
        .iter()
        .filter(|x| b.iter().any(|y| x.loose_eq(y)))
        .cloned()
        .collect();
    if intersection.is_empty() {
        Ok(Value::Bool(false))
    } else {
        Ok(Value::Array(intersection))
    }
}

fn builtin_type(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("type", args, 1)?;
    Ok(Value::String(
        match &args[0] {
            Value::Null | Value::Undefined => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
        .into(),
    ))
}

fn builtin_length(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("length", args, 1)?;
    Ok(match &args[0] {
        Value::Array(xs) => Value::Number(xs.len() as f64),
        Value::String(s) => Value::Number(s.chars().count() as f64),
        _ => Value::Null,
    })
}

fn builtin_count(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("count", args, 2)?;
    let list = as_array_loose(&args[0]);
    let needle = &args[1];
    Ok(Value::Number(
        list.iter().filter(|x| x.loose_eq(needle)).count() as f64,
    ))
}

fn builtin_index(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("index", args, 2)?;
    let list = as_array_loose(&args[0]);
    let needle = &args[1];
    match list.iter().position(|x| x.loose_eq(needle)) {
        Some(i) => Ok(Value::Number(i as f64)),
        None => Ok(Value::Null),
    }
}

fn builtin_substr(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("substr", args, 3)?;
    // Mirrors JS `String.prototype.substring(start, end)` with the
    // schema oracle's nullish behavior overlaid (tests 17–19, 44, 45):
    //   * Any nullish argument propagates → null.
    //   * Numeric NaN / non-finite → null (defensive; not a JS exact
    //     match — JS would treat NaN as 0, but bids selectors don't
    //     exercise that and "be explicit about garbage" is safer).
    //   * Negative indices clamp to 0; over-length indices clamp to
    //     `s.chars().count()`. start > end → swap.
    //   * Indexing is by CHARACTER (not byte). The previous
    //     implementation sliced raw UTF-8 bytes and panicked on
    //     non-ASCII boundaries — `s[1..4]` on a string starting with
    //     a 2-byte UTF-8 code point. Audited as High #6 (2026-05-17).
    let s = match &args[0] {
        Value::String(s) => s,
        Value::Null | Value::Undefined => return Ok(Value::Null),
        _ => return Ok(Value::Null),
    };
    let start = match &args[1] {
        Value::Number(n) if n.is_finite() => *n,
        _ => return Ok(Value::Null),
    };
    let end = match &args[2] {
        Value::Number(n) if n.is_finite() => *n,
        _ => return Ok(Value::Null),
    };
    let len_chars = s.chars().count();
    let mut start = start.trunc().max(0.0).min(len_chars as f64) as usize;
    let mut end = end.trunc().max(0.0).min(len_chars as f64) as usize;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    // Walk chars instead of bytes — safe for any Unicode boundary.
    let out: String = s.chars().skip(start).take(end - start).collect();
    Ok(Value::String(out))
}

fn builtin_sorted(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity_range("sorted", args, 1, 2)?;
    let mut xs = as_array_loose(&args[0]);
    let method = args
        .get(1)
        .and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
        .unwrap_or("auto");
    match method {
        "numeric" => xs.sort_by(|a, b| {
            let na = as_f64(a);
            let nb = as_f64(b);
            na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
        }),
        "lexical" => xs.sort_by_key(|a| a.to_display_string()),
        _ => xs.sort_by(|a, b| {
            // "auto": Number<>Number compare numerically; otherwise lexically.
            match (a, b) {
                (Value::Number(x), Value::Number(y)) => {
                    x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
                }
                _ => a.to_display_string().cmp(&b.to_display_string()),
            }
        }),
    }
    Ok(Value::Array(xs))
}

fn builtin_allequal(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("allequal", args, 2)?;
    let a = as_array_loose(&args[0]);
    let b = as_array_loose(&args[1]);
    if a.len() != b.len() {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(
        a.iter().zip(b.iter()).all(|(x, y)| x.loose_eq(y)),
    ))
}

fn builtin_min(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("min", args, 1)?;
    if matches!(args[0], Value::Null | Value::Undefined) {
        return Ok(Value::Null);
    }
    let xs = as_array_loose(&args[0]);
    let nums: Vec<f64> = xs.iter().map(as_f64).filter(|n| !n.is_nan()).collect();
    Ok(Value::Number(
        nums.iter().copied().fold(f64::INFINITY, f64::min),
    ))
}

fn builtin_max(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("max", args, 1)?;
    if matches!(args[0], Value::Null | Value::Undefined) {
        return Ok(Value::Null);
    }
    let xs = as_array_loose(&args[0]);
    let nums: Vec<f64> = xs.iter().map(as_f64).filter(|n| !n.is_nan()).collect();
    Ok(Value::Number(
        nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    ))
}

fn builtin_unique(args: &[Value], _ec: &EvalContext) -> Result<Value, EvalError> {
    arity("unique", args, 1)?;
    // Schema oracle (test 23): `unique(null) → null`. Without the
    // guard, `as_array_loose(null)` would wrap it into `[null]` and
    // return `[null]` instead of propagating nullishness.
    if matches!(&args[0], Value::Null | Value::Undefined) {
        return Ok(Value::Null);
    }
    let xs = as_array_loose(&args[0]);
    let mut out: Vec<Value> = Vec::new();
    for x in xs {
        if !out.iter().any(|y| x.loose_eq(y)) {
            out.push(x);
        }
    }
    Ok(Value::Array(out))
}

/// `exists(list, rule="dataset")` — checks how many of the given paths
/// exist under the dataset / file / stimuli / subject tree, returning
/// the count. Mirrors `expressionLanguage.ts::exists`.
///
/// Implementation reads the dataset file set from the [`EvalContext`].
/// If `dataset_files` is `None`, falls back to `0` (parity-safe — the
/// only schema selectors that use `exists` use the negated form, so
/// `!exists(...)` is `true` when no set is installed).
fn builtin_exists(args: &[Value], ec: &EvalContext) -> Result<Value, EvalError> {
    arity_range("exists", args, 1, 2)?;
    let rule = match args.get(1) {
        Some(Value::String(s)) => s.as_str(),
        _ => "dataset",
    };
    let list: Vec<String> = match &args[0] {
        Value::Array(xs) => xs
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        Value::String(s) => vec![s.clone()],
        Value::Null | Value::Undefined => return Ok(Value::Number(0.0)),
        _ => return Ok(Value::Number(0.0)),
    };
    let count = exists::count(&list, rule, ec);
    Ok(Value::Number(count as f64))
}

/// `exists(...)` helpers. The dataset file set is plumbed through the
/// [`EvalContext`] (`ec.dataset_files`) rather than thread-local
/// storage so per-worker parallel validation (M4) sees a consistent
/// set without setup ceremony in every worker.
///
/// Build the set with [`normalize_dataset_files`] once per
/// `validate_dataset` call. Paths are stored *without* a leading `/`
/// so they match the format the schema's `exists(...)` calls use
/// (`"CITATION.cff"`, `"genetic_info.json"`); absolute-path arguments
/// to `exists` are rejected at lookup time (see [`count`]) to mirror
/// a TS quirk.
pub mod exists {
    use super::EvalContext;
    use crate::value::Value;
    use std::collections::HashSet;

    /// Normalize dataset paths into the form `exists(...)` expects:
    /// strip any leading `/` so e.g. `/CITATION.cff` becomes
    /// `CITATION.cff`. Use this once per validation run to build the
    /// `HashSet` you pass to [`EvalContext::with_files`].
    pub fn normalize_dataset_files(paths: impl IntoIterator<Item = String>) -> HashSet<String> {
        paths
            .into_iter()
            .map(|p| p.trim_start_matches('/').to_string())
            .collect()
    }

    /// Returns how many of `paths` exist in the dataset file set
    /// (passed via `ec.dataset_files`). If no set is installed,
    /// returns 0 (parity-safe — `!exists(...)` evaluates true).
    ///
    /// Matches a TS quirk in `expressionLanguage.ts::exists`: the TS
    /// implementation does `path.split('/')` and walks a `FileTree`,
    /// which for an absolute path like `/sub-01/...` produces
    /// `["", "sub-01", ...]` — the leading empty element causes the
    /// lookup to silently fail. So absolute-path arguments to
    /// `exists(..., "dataset")` always return 0 in TS, even when the
    /// file exists. We mirror that here to preserve parity (otherwise
    /// rules like `DuplicateFiles` over-fire).
    pub(crate) fn count(paths: &[String], rule: &str, ec: &EvalContext) -> usize {
        if rule == "bids-uri" {
            return paths.iter().filter(|p| p.starts_with("bids:")).count();
        }
        let Some(set) = ec.dataset_files else {
            return 0;
        };
        let ctx = ec.value;
        paths
            .iter()
            .filter(|p| {
                if p.starts_with('/') {
                    return false;
                }
                match rule {
                    "dataset" => set.contains(p.as_str()),
                    "stimuli" => set.contains(format!("stimuli/{p}").as_str()),
                    "subject" => {
                        let sub = ctx_member(ctx, &["entities", "sub"]).and_then(value_string);
                        match sub {
                            Some(sub) => set.contains(format!("sub-{sub}/{p}").as_str()),
                            None => false,
                        }
                    }
                    "file" => {
                        let current = ctx_member(ctx, &["path"]).and_then(value_string);
                        match current.and_then(|p| parent_path(&p)) {
                            Some(parent) if parent.is_empty() => set.contains(p.as_str()),
                            Some(parent) => set.contains(format!("{parent}/{p}").as_str()),
                            None => false,
                        }
                    }
                    _ => false,
                }
            })
            .count()
    }

    fn ctx_member<'a>(ctx: &'a Value, path: &[&str]) -> Option<&'a Value> {
        let mut cur = ctx;
        for key in path {
            let Value::Object(map) = cur else {
                return None;
            };
            cur = map.get(*key)?;
        }
        Some(cur)
    }

    fn value_string(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    fn parent_path(path: &str) -> Option<String> {
        let trimmed = path.trim_start_matches('/');
        match trimmed.rsplit_once('/') {
            Some((parent, _)) => Some(parent.to_string()),
            None => Some(String::new()),
        }
    }
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Number(n) => *n,
        Value::Bool(b) => *b as i64 as f64,
        Value::String(s) => s.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn obj(pairs: &[(&str, Value)]) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert(k.to_string(), v.clone());
        }
        Value::Object(m)
    }

    fn ctx() -> Value {
        obj(&[
            ("modality", "mri".into()),
            ("extension", ".nii.gz".into()),
            ("datatype", "anat".into()),
            ("suffix", "T1w".into()),
            ("path", "/sub-01/func/sub-01_task-rest_events.tsv".into()),
            (
                "entities",
                obj(&[("sub", "01".into()), ("task", "rest".into())]),
            ),
            ("sidecar", obj(&[("RepetitionTime", Value::Number(2.0))])),
            (
                "dataset",
                obj(&[(
                    "modalities",
                    Value::Array(vec!["mri".into(), "micr".into()]),
                )]),
            ),
        ])
    }

    fn ev(src: &str) -> Value {
        let e = parse(src).unwrap();
        eval(&e, &ctx()).unwrap()
    }

    fn ev_bool(src: &str) -> bool {
        match ev(src) {
            Value::Bool(b) => b,
            other => panic!("expected bool, got {other:?} for {src}"),
        }
    }

    fn ev_num(src: &str) -> f64 {
        match ev(src) {
            Value::Number(n) => n,
            other => panic!("expected number, got {other:?} for {src}"),
        }
    }

    #[test]
    fn equality_and_in() {
        assert!(ev_bool(r#"modality == "mri""#));
        assert!(ev_bool(r#""sub" in entities"#));
        assert!(!ev_bool(r#""sub" in sidecar"#));
        assert!(ev_bool(r#"!("IntendedFor" in sidecar)"#));
    }

    #[test]
    fn match_regex() {
        assert!(ev_bool(r#"match(extension, "^\\.nii(\\.gz)?$")"#));
        assert!(!ev_bool(r#"match(extension, "^\\.txt$")"#));
    }

    #[test]
    fn intersects_arrays() {
        // Per JS reference, `intersects` returns the intersection array
        // (truthy) or `false` (falsy) — same as a boolean for selector use.
        assert!(ev(r#"intersects(["T1w","T2w"], [suffix])"#).is_truthy());
        assert!(!ev(r#"intersects(["dseg","probseg"], [suffix])"#).is_truthy());
        assert!(ev_bool(r#""micr" in dataset.modalities"#));
    }

    #[test]
    fn short_circuit_and_or() {
        assert!(ev_bool(r#"true && true"#));
        assert!(!ev_bool(r#"true && false"#));
        // Returns the actual operand on the truthy side, JS-style.
        match ev(r#"sidecar.MissingKey || "fallback""#) {
            Value::String(s) => assert_eq!(s, "fallback"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn comparison_numeric() {
        assert!(ev_bool(r#"sidecar.RepetitionTime == 2"#));
        assert!(ev_bool(r#"sidecar.RepetitionTime > 1"#));
        assert!(!ev_bool(r#"sidecar.RepetitionTime > 5"#));
    }

    #[test]
    fn min_max_empty_match_js() {
        assert!(ev_num("min([])").is_infinite());
        assert!(ev_num("min([])").is_sign_positive());
        assert!(ev_num("max([])").is_infinite());
        assert!(ev_num("max([])").is_sign_negative());
    }

    #[test]
    fn exists_modes_use_supplied_file_set() {
        let files = exists::normalize_dataset_files([
            "CITATION.cff".to_string(),
            "stimuli/face.png".to_string(),
            "sub-01/phenotype.tsv".to_string(),
            "sub-01/func/local.json".to_string(),
        ]);
        let value = ctx();
        let ec = EvalContext::with_files(&value, &files);
        let ev = |src: &str| -> f64 {
            let v = eval_with(&parse(src).unwrap(), &ec).unwrap();
            match v {
                Value::Number(n) => n,
                other => panic!("expected number, got {other:?}"),
            }
        };

        assert_eq!(ev(r#"exists("CITATION.cff", "dataset")"#), 1.0);
        assert_eq!(ev(r#"exists("face.png", "stimuli")"#), 1.0);
        assert_eq!(ev(r#"exists("phenotype.tsv", "subject")"#), 1.0);
        assert_eq!(ev(r#"exists("local.json", "file")"#), 1.0);
        assert_eq!(
            ev(r#"exists(["bids::sub-01/path", "plain/path"], "bids-uri")"#),
            1.0
        );
        assert_eq!(ev(r#"exists("/CITATION.cff", "dataset")"#), 0.0);
    }

    /// Regression: pre-fix `substr` byte-sliced raw UTF-8 and panicked
    /// when start/end fell mid-codepoint. The eight-byte string
    /// "ééééé" is five chars but ten bytes; `substr("ééééé", 1, 4)`
    /// at the old byte indices `s[1..4]` straddled boundaries. Audited
    /// as High #6 (2026-05-17).
    #[test]
    fn substr_non_ascii_does_not_panic() {
        let c = ctx();
        // 5 chars, 10 bytes. char-indexed slice [1..4] = "ééé".
        let v = eval(&parse(r#"substr("ééééé", 1, 4)"#).unwrap(), &c).unwrap();
        let got = match v {
            Value::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        // 3 char-units from a 5-char string starting at index 1.
        assert_eq!(got.chars().count(), 3, "got {got:?}");
        assert!(got.chars().all(|c| c == 'é'), "got {got:?}");
    }

    #[test]
    fn substr_clamps_negative_and_oversize() {
        let c = ctx();
        let v = eval(&parse(r#"substr("string", -3, 20)"#).unwrap(), &c).unwrap();
        assert!(matches!(&v, Value::String(s) if s == "string"));
    }

    #[test]
    fn substr_swaps_when_start_greater_than_end() {
        let c = ctx();
        let v = eval(&parse(r#"substr("string", 4, 1)"#).unwrap(), &c).unwrap();
        assert!(matches!(&v, Value::String(s) if s == "tri"));
    }
}
