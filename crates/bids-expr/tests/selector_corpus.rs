//! Parse every selector that appears anywhere in the embedded schema and
//! report any that don't parse. Intentionally a single test so cargo
//! prints one consolidated failure block.

use bids_expr::parse;
use serde_json::Value as J;

#[test]
fn parses_every_schema_selector() {
    let schema: J = serde_json::from_str(bids_schema_codegen::BUNDLED_SCHEMA_JSON).unwrap();
    let selectors = collect_selectors(&schema);
    let mut failed: Vec<(String, String)> = Vec::new();
    for selector in &selectors {
        if let Err(e) = parse(selector) {
            failed.push((selector.clone(), e.to_string()));
        }
    }

    if !failed.is_empty() {
        eprintln!(
            "Failed to parse {} of {} selectors:",
            failed.len(),
            selectors.len()
        );
        for (s, e) in &failed {
            eprintln!("  {s}");
            eprintln!("    -> {e}");
        }
        panic!("{} selector(s) failed to parse", failed.len());
    } else {
        eprintln!("Parsed all {} selectors successfully", selectors.len());
    }
}

fn collect_selectors(value: &J) -> Vec<String> {
    let mut out = Vec::new();
    collect_selectors_into(value, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_selectors_into(value: &J, out: &mut Vec<String>) {
    match value {
        J::Object(map) => {
            for (key, child) in map {
                if key == "selectors" {
                    if let J::Array(items) = child {
                        out.extend(items.iter().filter_map(|item| {
                            item.as_str()
                                .filter(|selector| !selector.trim().is_empty())
                                .map(ToOwned::to_owned)
                        }));
                    }
                }
                collect_selectors_into(child, out);
            }
        }
        J::Array(items) => {
            for item in items {
                collect_selectors_into(item, out);
            }
        }
        _ => {}
    }
}
