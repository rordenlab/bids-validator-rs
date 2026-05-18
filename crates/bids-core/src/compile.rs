//! Shared rule-compilation utility.
//!
//! Five sites in this crate compile schema-rule selectors (and checks)
//! into `bids_expr::Expr` ASTs at first access:
//!
//!   * [`metadata::compiled_rules`] (`rules.sidecars.*`)
//!   * [`metadata::compiled_json_rules`] (`rules.json` + `rules.dataset_metadata`)
//!   * [`tables::compiled_rules`] (`rules.tabular_data.*`)
//!   * [`checks::compiled_rules`] (`rules.checks.*`)
//!   * [`associations::compiled_rules`] (`meta.associations.*`)
//!
//! Each one has the same shape: iterate every selector source string,
//! parse it, and either keep the rule or drop it. This module exposes
//! that shape as a single helper so all five sites share the same
//! defensive semantics — **drop the whole rule on any parse failure**.
//!
//! Audit response H14: three of the five sites used to drop only the
//! offending selector (`filter_map`), which can silently flip a
//! narrowing rule into a fires-on-everything rule when a future schema
//! bump introduces an unparseable form. The shared helper removes that
//! footgun.

use bids_expr::{parse, Expr};

/// Parse every selector source. Returns `Some(asts)` if every string
/// parsed, or `None` (with one `eprintln!` per failure) if any did not.
///
/// `label` is logged with the failing source — typically the rule's
/// dotted path or its key. `kind` distinguishes selector / check / etc.
pub fn parse_all_or_skip<S: AsRef<str>>(
    label: &str,
    kind: &str,
    sources: impl IntoIterator<Item = S>,
) -> Option<Vec<Expr>> {
    let mut out = Vec::new();
    for src in sources {
        let s = src.as_ref();
        match parse(s) {
            Ok(ast) => out.push(ast),
            Err(e) => {
                eprintln!("warn: rule {label} {kind} {s:?} failed to parse: {e}");
                return None;
            }
        }
    }
    Some(out)
}
