#!/usr/bin/env python3
"""Small Deno-compatible message snapshot check for the Rust beta.

This intentionally avoids the large external parity datasets. It locks
representative user-facing fields that are easy to regress:

* `issues.codeMessages`
* schema custom issue messages
* `rules.checks` messages that belong in `codeMessages`
* filename issue `issueMessage` / `rule`

Run after `cargo build --release`.
"""

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
RUST_BIN = ROOT / "rust/target/release/bids-validator"
EXPECTED = Path(__file__).with_name("expected.json")

DATASETS = {
    "valid_headers": ROOT / "tests/data/valid_headers",
    "no_t1w": ROOT / "tests/data/no_t1w",
}


def run_rust(dataset_name):
    result = subprocess.run(
        [str(RUST_BIN), str(DATASETS[dataset_name])],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 and not result.stdout.strip():
        raise RuntimeError(result.stderr[-1000:])
    return json.loads(result.stdout)


def find_issue(issues, expected):
    for issue in issues:
        if issue.get("code") != expected["code"]:
            continue
        if issue.get("location") != expected["location"]:
            continue
        if "subCode" in expected and issue.get("subCode") != expected["subCode"]:
            continue
        return issue
    return None


def main():
    expected = json.loads(EXPECTED.read_text())
    ok = True
    for dataset_name, snapshots in expected.items():
        payload = run_rust(dataset_name)
        issues = payload["issues"]["issues"]
        code_messages = payload["issues"].get("codeMessages", {})
        for snapshot in snapshots:
            issue = find_issue(issues, snapshot)
            label = f"{dataset_name}:{snapshot['code']}:{snapshot['location']}"
            if issue is None:
                print(f"missing issue {label}", file=sys.stderr)
                ok = False
                continue

            for field in ("issueMessage", "rule"):
                if field in snapshot and issue.get(field) != snapshot[field]:
                    print(
                        f"{label} {field} mismatch: {issue.get(field)!r} != {snapshot[field]!r}",
                        file=sys.stderr,
                    )
                    ok = False

            got_code_message = code_messages.get(snapshot["code"])
            if got_code_message != snapshot["codeMessage"]:
                print(
                    f"{label} codeMessage mismatch: {got_code_message!r} != {snapshot['codeMessage']!r}",
                    file=sys.stderr,
                )
                ok = False

    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
