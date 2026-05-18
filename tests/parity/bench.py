#!/usr/bin/env python3
"""Benchmark Rust against cached Deno baselines.

Default mode is iteration-friendly:

  * run the Rust release binary on the 9 parity datasets,
  * compare Rust output to cached Deno `{code, location, subCode}` multisets,
  * write timing/RSS/output summaries to `cache/benchmarks.json`,
  * do not run Deno unless explicitly requested.

Use `--refresh-deno-output` when Deno output must be re-cached, and
`--refresh-deno-bench` when Deno wall/RSS numbers must be re-measured.
For the local `/Users/chris/src/bidsui/datasets` tree, pass
`--external-all`.

Rust defaults to `--content-mode parity --link-mode parity`. Use
`--content-mode thorough` to benchmark opt-in local annex/symlink
content reads, `--link-mode follow` to traverse symlinked directories,
or `--link-mode no-follow` to skip symlinked directories entirely. Deno
output comparison is skipped outside the default modes.
"""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import run as parity

BENCHMARK_CACHE = parity.CACHE_DIR / "benchmarks.json"
EXTERNAL_ROOT = Path("/Users/chris/src/bidsui/datasets")


def external_datasets(root: Path) -> dict[str, Path]:
    if not root.is_dir():
        return {}
    return {
        p.name: p
        for p in sorted(root.iterdir(), key=lambda x: x.name)
        if p.is_dir() and not p.name.startswith(".")
    }


def selected_datasets(args: argparse.Namespace) -> dict[str, Path]:
    catalog = {}
    if args.external_all:
        catalog.update(external_datasets(args.external_root))
    else:
        catalog.update(parity.DATASETS)
        catalog.update(external_datasets(args.external_root))

    if not args.datasets:
        return catalog if args.external_all else parity.DATASETS

    selected = {}
    for token in args.datasets:
        path = Path(token).expanduser()
        if path.is_dir():
            selected[path.name] = path
            continue
        if token not in catalog:
            raise SystemExit(f"unknown dataset {token!r}")
        selected[token] = catalog[token]
    return selected


def validator_cmd(
    kind: str,
    dataset_path: Path,
    rust_content_mode: str,
    rust_link_mode: str,
) -> list[str]:
    if kind == "rust":
        return [
            str(parity.RUST_BIN),
            "--content-mode",
            rust_content_mode,
            "--link-mode",
            rust_link_mode,
            str(dataset_path),
        ]
    if kind == "deno":
        return [
            str(parity.DENO_BIN),
            "run",
            "-A",
            str(parity.DENO_ENTRY),
            "--json",
            str(dataset_path),
        ]
    raise ValueError(kind)


def maxrss_to_bytes(maxrss: int) -> int | None:
    if maxrss <= 0:
        return None
    # getrusage/wait4 units are platform-specific: bytes on Darwin, KiB on Linux.
    if platform.system() == "Darwin":
        return maxrss
    return maxrss * 1024


def wait_with_usage(proc: subprocess.Popen, timeout_s: float | None):
    if timeout_s is None:
        return (*os.wait4(proc.pid, 0), False)

    deadline = time.perf_counter() + timeout_s
    while True:
        pid, status, usage = os.wait4(proc.pid, os.WNOHANG)
        if pid:
            return pid, status, usage, False
        if time.perf_counter() >= deadline:
            proc.kill()
            pid, status, usage = os.wait4(proc.pid, 0)
            return pid, status, usage, True
        time.sleep(0.05)


def run_json(
    kind: str,
    dataset_path: Path,
    timeout_s: float | None,
    rust_content_mode: str,
    rust_link_mode: str,
) -> tuple[dict, str, dict]:
    cmd = validator_cmd(kind, dataset_path, rust_content_mode, rust_link_mode)
    start = time.perf_counter()

    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stdout_file, tempfile.TemporaryFile(
        mode="w+", encoding="utf-8"
    ) as stderr_file:
        proc = subprocess.Popen(
            cmd,
            stdout=stdout_file,
            stderr=stderr_file,
            text=True,
        )
        _pid, status, usage, timed_out = wait_with_usage(proc, timeout_s)
        returncode = os.waitstatus_to_exitcode(status)
        proc.returncode = returncode

        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read()
        stderr = stderr_file.read()

    elapsed_ms = (time.perf_counter() - start) * 1000
    if timed_out:
        raise TimeoutError(f"{kind} timed out after {timeout_s}s on {dataset_path}")

    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as e:
        raise RuntimeError(f"{kind} produced invalid JSON: {e}; stderr={stderr!r}") from e

    # Deno returns 1 when validation issues are present; that is expected.
    if kind == "deno":
        failed = returncode > 1 and not stdout.strip()
    else:
        failed = returncode != 0 and not stdout.strip()
    if failed:
        raise RuntimeError(f"{kind} failed with exit {returncode}: {stderr[-1000:]}")

    metrics = {
        "real_ms": round(elapsed_ms, 3),
        "max_rss_mib": rss_mib(maxrss_to_bytes(usage.ru_maxrss)),
        "exit_code": returncode,
    }
    return payload, stdout, metrics


def measure(
    kind: str,
    dataset_path: Path,
    runs: int,
    warmups: int,
    timeout_s: float | None,
    rust_content_mode: str,
    rust_link_mode: str,
) -> dict:
    if runs < 1:
        raise ValueError("runs must be >= 1")

    for _ in range(warmups):
        run_json(kind, dataset_path, timeout_s, rust_content_mode, rust_link_mode)

    run_metrics = []
    last_payload = None
    last_stdout = ""
    for _ in range(runs):
        last_payload, last_stdout, metrics = run_json(
            kind, dataset_path, timeout_s, rust_content_mode, rust_link_mode
        )
        run_metrics.append(metrics)

    wall = [r["real_ms"] for r in run_metrics]
    rss_values = [r["max_rss_mib"] for r in run_metrics if r["max_rss_mib"] is not None]
    return {
        "runs": run_metrics,
        "median_ms": round(statistics.median(wall), 3),
        "best_ms": round(min(wall), 3),
        "max_rss_mib": round(max(rss_values), 3) if rss_values else None,
        "last_payload": last_payload,
        "last_stdout": last_stdout,
    }


def rss_mib(rss_bytes: int | None) -> float | None:
    if rss_bytes is None:
        return None
    return round(rss_bytes / (1024 * 1024), 3)


def deno_cache_path(name: str) -> Path:
    return parity.CACHE_DIR / f"{name}.deno.json"


def load_deno_multiset(name: str) -> collections.Counter | None:
    path = deno_cache_path(name)
    if not path.exists():
        return None
    return parity.deserialize_multiset(path.read_text())


def write_deno_multiset(name: str, ms) -> None:
    parity.CACHE_DIR.mkdir(parents=True, exist_ok=True)
    deno_cache_path(name).write_text(parity.serialize_multiset(ms))


def hash_multiset(ms) -> str:
    return hashlib.sha256(parity.serialize_multiset(ms).encode()).hexdigest()


def contract_hash() -> str:
    payload = {
        "implemented_codes": sorted(parity.RUST_CODES),
        "known_divergences": sorted([list(x) for x in parity.KNOWN_DIVERGENCES]),
        "non_contract_rust_codes": sorted(parity.NON_CONTRACT_RUST_CODES),
    }
    return hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()


def output_summary(payload: dict | None, ms, stdout: str | None = None) -> dict:
    summary = {
        "implemented_issue_count": int(sum(ms.values())) if ms is not None else None,
        "implemented_multiset_sha256": hash_multiset(ms) if ms is not None else None,
        "implemented_code_count": len(parity.RUST_CODES),
        "non_contract_codes": sorted(parity.NON_CONTRACT_RUST_CODES),
    }
    if payload is not None:
        all_issues = parity.multiset(payload, None)
        summary["all_issue_count"] = int(sum(all_issues.values()))
    else:
        summary["all_issue_count"] = None
    if stdout:
        summary["raw_stdout_sha256"] = hashlib.sha256(stdout.encode()).hexdigest()
    else:
        summary["raw_stdout_sha256"] = None
    return summary


def strip_payload(measured: dict) -> dict:
    return {
        k: v
        for k, v in measured.items()
        if k not in {"last_payload", "last_stdout"}
    }


def load_benchmark_cache() -> dict:
    if BENCHMARK_CACHE.exists():
        return json.loads(BENCHMARK_CACHE.read_text())
    return {"schema_version": 1, "datasets": {}}


def save_benchmark_cache(data: dict) -> None:
    parity.CACHE_DIR.mkdir(parents=True, exist_ok=True)
    BENCHMARK_CACHE.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")


def benchmark_document(args: argparse.Namespace, datasets: dict) -> dict:
    return {
        "schema_version": 1,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": platform.python_version(),
        },
        "config": {
            "runs": args.runs,
            "warmups": args.warmups,
            "rust_bin": str(parity.RUST_BIN),
            "deno_bin": str(parity.DENO_BIN),
            "deno_entry": str(parity.DENO_ENTRY),
            "rust_content_mode": args.content_mode,
            "rust_link_mode": args.link_mode,
            "implemented_codes": sorted(parity.RUST_CODES),
            "known_divergences": sorted([list(x) for x in parity.KNOWN_DIVERGENCES]),
            "non_contract_rust_codes": sorted(parity.NON_CONTRACT_RUST_CODES),
            "contract_sha256": contract_hash(),
        },
        "datasets": datasets,
    }


def format_ms(ms: float | None) -> str:
    return "n/a" if ms is None else f"{ms:.1f} ms"


def format_rss(mib: float | None) -> str:
    return "n/a" if mib is None else f"{mib:.1f} MiB"


def benchmark_dataset(
    name: str,
    dataset_path: Path,
    args: argparse.Namespace,
    existing: dict,
) -> tuple[dict, bool]:
    if not dataset_path.is_dir():
        return {
            "path": str(dataset_path),
            "status": "missing",
            "rust": None,
            "deno": None,
        }, args.allow_missing_deno

    print(f"=== {name} ===", flush=True)

    rust_measured = measure(
        "rust",
        dataset_path,
        args.runs,
        args.warmups,
        args.timeout_s,
        args.content_mode,
        args.link_mode,
    )
    rust_payload = rust_measured["last_payload"]
    rust_stdout = rust_measured["last_stdout"]
    rust_ms = parity.multiset(rust_payload, parity.RUST_CODES)

    deno_ms = None
    deno_payload = None
    deno_stdout = None
    deno_measured = None
    deno_block = existing.get(name, {}).get("deno") or {}

    need_deno_run = args.refresh_deno_output or args.refresh_deno_bench
    if need_deno_run:
        deno_runs = args.runs if args.refresh_deno_bench else 1
        deno_warmups = args.warmups if args.refresh_deno_bench else 0
        print("  refreshing Deno baseline...", flush=True)
        try:
            deno_measured = measure(
                "deno",
                dataset_path,
                deno_runs,
                deno_warmups,
                args.timeout_s,
                args.content_mode,
                args.link_mode,
            )
            deno_payload = deno_measured["last_payload"]
            deno_stdout = deno_measured["last_stdout"]
            deno_ms = parity.multiset(deno_payload, parity.RUST_CODES)
            write_deno_multiset(name, deno_ms)
            deno_block = strip_payload(deno_measured) | {
                "source": "measured",
                "output": output_summary(deno_payload, deno_ms, deno_stdout),
                "cache_file": str(deno_cache_path(name).relative_to(parity.CACHE_DIR.parent)),
            }
        except Exception as e:
            print(f"  Deno refresh failed: {e}", flush=True)
            deno_ms = load_deno_multiset(name)
            deno_block = deno_block | {
                "source": deno_block.get("source", "cached-after-refresh-failure"),
                "output": output_summary(None, deno_ms),
                "cache_file": str(deno_cache_path(name).relative_to(parity.CACHE_DIR.parent)),
                "refresh_error": str(e),
            }
    else:
        deno_ms = load_deno_multiset(name)
        if deno_ms is not None:
            deno_block = deno_block | {
                "source": deno_block.get("source", "cached"),
                "output": output_summary(None, deno_ms),
                "cache_file": str(deno_cache_path(name).relative_to(parity.CACHE_DIR.parent)),
            }
        else:
            deno_block = {
                "source": "missing",
                "runs": [],
                "median_ms": None,
                "best_ms": None,
                "max_rss_mib": None,
                "output": output_summary(None, None),
                "cache_file": str(deno_cache_path(name).relative_to(parity.CACHE_DIR.parent)),
            }

    unexpected = parity.unexpected_rust_codes(rust_payload)
    if unexpected:
        print(f"  unexpected Rust issue codes outside contract: {unexpected}", flush=True)

    compare_to_deno = args.content_mode == "parity" and args.link_mode == "parity"
    comparable_rust_ms = parity.strip_known_divergences(name, rust_ms)
    comparable_deno_ms = (
        parity.strip_known_divergences(name, deno_ms) if deno_ms is not None else None
    )
    matches_deno = (
        compare_to_deno
        and comparable_deno_ms is not None
        and comparable_rust_ms == comparable_deno_ms
        and not unexpected
    )
    missing_deno_ok = compare_to_deno and deno_ms is None and args.allow_missing_deno
    ok = (matches_deno or missing_deno_ok or not compare_to_deno) and not unexpected

    rust_block = strip_payload(rust_measured) | {
        "content_mode": args.content_mode,
        "link_mode": args.link_mode,
        "output": output_summary(rust_payload, rust_ms, rust_stdout),
        "matches_deno_cache": matches_deno if compare_to_deno and deno_ms is not None else None,
    }

    deno_median = deno_block.get("median_ms")
    deno_rss = deno_block.get("max_rss_mib")
    if not compare_to_deno:
        output_state = "Deno comparison skipped"
    elif matches_deno:
        output_state = "output OK"
    elif deno_ms is None:
        output_state = "Deno output missing"
    else:
        output_state = "MISMATCH"

    print(
        "  "
        f"rust {format_ms(rust_block['median_ms'])}, {format_rss(rust_block['max_rss_mib'])}; "
        f"deno {format_ms(deno_median)}, {format_rss(deno_rss)}; "
        f"{output_state}",
        flush=True,
    )

    if compare_to_deno and deno_ms is not None and not matches_deno:
        rust_only = comparable_rust_ms - comparable_deno_ms
        deno_only = comparable_deno_ms - comparable_rust_ms
        if rust_only:
            print(f"  rust-only ({sum(rust_only.values())}):", flush=True)
            for k, n in rust_only.most_common(10):
                print(f"    {n}x {k}", flush=True)
        if deno_only:
            print(f"  deno-only ({sum(deno_only.values())}):", flush=True)
            for k, n in deno_only.most_common(10):
                print(f"    {n}x {k}", flush=True)

    return {
        "path": str(dataset_path),
        "status": "ok",
        "rust": rust_block,
        "deno": deno_block,
    }, ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("datasets", nargs="*", help="Dataset names or paths to benchmark.")
    parser.add_argument("--runs", type=int, default=1, help="Timed runs per validator.")
    parser.add_argument("--warmups", type=int, default=0, help="Untimed warmup runs.")
    parser.add_argument(
        "--timeout-s",
        type=float,
        default=None,
        help="Optional per-run timeout for either validator.",
    )
    parser.add_argument(
        "--refresh-deno-output",
        action="store_true",
        help="Run Deno and rewrite cache/<dataset>.deno.json.",
    )
    parser.add_argument(
        "--refresh-deno-bench",
        action="store_true",
        help="Run Deno for timing/RSS instead of reusing cached Deno metrics.",
    )
    parser.add_argument(
        "--external-all",
        action="store_true",
        help="Benchmark every directory under --external-root.",
    )
    parser.add_argument(
        "--external-root",
        type=Path,
        default=EXTERNAL_ROOT,
        help="Local dataset root for --external-all and name resolution.",
    )
    parser.add_argument(
        "--allow-missing-deno",
        action="store_true",
        help="Do not fail when a dataset has no cached Deno output.",
    )
    parser.add_argument(
        "--content-mode",
        choices=["parity", "thorough"],
        default="parity",
        help="Rust content-read mode. 'parity' mimics Deno annex-symlink read gaps; "
        "'thorough' reads local annex targets when available.",
    )
    parser.add_argument(
        "--link-mode",
        choices=["parity", "follow", "no-follow"],
        default="parity",
        help="Rust discovery symlink mode. 'parity' registers but does not traverse "
        "symlinked directories; 'follow' traverses them; 'no-follow' skips them.",
    )
    args = parser.parse_args()

    if args.runs < 1:
        raise SystemExit("--runs must be >= 1")
    if args.warmups < 0:
        raise SystemExit("--warmups must be >= 0")
    if args.timeout_s is not None and args.timeout_s <= 0:
        raise SystemExit("--timeout-s must be > 0")

    datasets = selected_datasets(args)
    cache = load_benchmark_cache()
    old_datasets = cache.get("datasets", {})
    new_datasets = dict(old_datasets)

    overall_ok = True
    for name, path in datasets.items():
        try:
            entry, ok = benchmark_dataset(name, path, args, old_datasets)
        except Exception as e:
            print(f"=== {name} ===", flush=True)
            print(f"  ERROR: {e}", flush=True)
            entry = {
                "path": str(path),
                "status": "error",
                "error": str(e),
                "rust": None,
                "deno": old_datasets.get(name, {}).get("deno"),
            }
            ok = False
        new_datasets[name] = entry
        overall_ok = overall_ok and ok
        save_benchmark_cache(benchmark_document(args, new_datasets))

    cache = benchmark_document(args, new_datasets)
    save_benchmark_cache(cache)
    print(f"\nwrote {BENCHMARK_CACHE}")

    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main())
