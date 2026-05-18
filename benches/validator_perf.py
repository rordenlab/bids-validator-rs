#!/usr/bin/env python3
"""Benchmark the optimized Rust BIDS Validator on real-world datasets.

Correctness is covered by the test and parity harnesses. This script is
for performance work: it measures wall time, peak RSS, and issue count
for the release Rust binary. Deno timing is opt-in because large
DataLad datasets are intentionally slow in the TypeScript validator.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
UPSTREAM = ROOT / "vendor" / "bids-validator-2.4.1"
DEFAULT_RUST_BIN = ROOT / "target" / "release" / "bids-validator"
DEFAULT_DENO_BIN = Path(os.environ.get("DENO_BIN") or shutil.which("deno") or "/Users/chris/.deno/bin/deno")
LOCAL_DATASETS = {
    "pet002": ROOT / "data" / "pet002-tiny",
    "ds005016": ROOT / "data" / "ds005016",
}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("datasets", nargs="*", help="benchmark dataset alias or explicit path")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--warmups", type=int, default=0)
    parser.add_argument("--rust-bin", type=Path, default=DEFAULT_RUST_BIN)
    parser.add_argument("--deno-bin", type=Path, default=DEFAULT_DENO_BIN)
    parser.add_argument("--upstream", type=Path, default=UPSTREAM)
    parser.add_argument("--content-mode", choices=["parity", "thorough"], default="parity")
    parser.add_argument("--link-mode", choices=["parity", "no-follow", "follow"], default="parity")
    parser.add_argument("--include-deno", action="store_true", help="also measure the pinned Deno validator")
    parser.add_argument("--out", type=Path, default=ROOT / "target" / "validator_perf.json")
    args = parser.parse_args()

    require_release_binary(args.rust_bin)
    deno_entry = args.upstream / "src" / "bids-validator.ts"
    if args.include_deno and not deno_entry.exists():
        raise SystemExit(f"missing upstream checkout: {args.upstream}; run scripts/fetch_upstream.py")

    datasets = select_datasets(args.datasets)
    results = []
    for name, path in datasets:
        print(f"=== {name} ===")
        for _ in range(args.warmups):
            run_validator("rust", args.rust_bin, args.deno_bin, deno_entry, path, args.content_mode, args.link_mode)
        rust_runs = [
            run_validator("rust", args.rust_bin, args.deno_bin, deno_entry, path, args.content_mode, args.link_mode)
            for _ in range(args.runs)
        ]
        rust = summarize(rust_runs)
        row = {"dataset": name, "path": str(path), "rust": rust}
        print(f"rust {rust['best_ms']:.1f} ms / {rust['max_rss_mib']:.1f} MiB; "
              f"issues={rust['issue_count']}")
        if args.include_deno:
            deno_runs = [
                run_validator("deno", args.rust_bin, args.deno_bin, deno_entry, path, args.content_mode, args.link_mode)
                for _ in range(args.runs)
            ]
            deno = summarize(deno_runs)
            row["deno"] = deno
            if deno["best_ms"] > 0:
                row["speedup_best"] = deno["best_ms"] / rust["best_ms"]
            print(f"deno {deno['best_ms']:.1f} ms / {deno['max_rss_mib']:.1f} MiB; "
                  f"issues={deno['issue_count']}")
        results.append(row)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({
        "metadata": {
            "generated_at": dt.datetime.now(dt.UTC).isoformat(),
            "rust_bin": str(args.rust_bin),
            "rust_version": read_version([str(args.rust_bin), "-V"]),
            "deno_version": read_version([str(args.deno_bin), "--version"]) if args.include_deno else None,
            "rust_cmd_mode": {
                "content_mode": args.content_mode,
                "link_mode": args.link_mode,
            },
            "deno_included": args.include_deno,
            "runs": args.runs,
            "warmups": args.warmups,
        },
        "results": results,
    }, indent=2) + "\n")
    print(f"wrote {args.out}")
    return 0


def read_version(cmd: list[str]) -> str | None:
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=False)
    except OSError:
        return None
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def require_release_binary(path: Path) -> None:
    if not path.exists():
        raise SystemExit(f"missing Rust release binary: {path}; run cargo build --release --locked")
    resolved = path.resolve()
    target_root = ROOT / "target"
    if target_root in resolved.parents and "release" not in resolved.parts:
        raise SystemExit(f"benchmark requires an optimized release binary, got: {path}")


def select_datasets(tokens: list[str]) -> list[tuple[str, Path]]:
    if not tokens:
        tokens = [name for name, path in LOCAL_DATASETS.items() if path.is_dir()]
        if not tokens:
            raise SystemExit("missing benchmark data; run scripts/fetch_bench_data.py pet002 and ds005016")
    selected = []
    for token in tokens:
        path = Path(token).expanduser()
        if path.is_dir():
            selected.append((path.name, path))
        elif token in LOCAL_DATASETS:
            selected.append((token, LOCAL_DATASETS[token]))
        else:
            selected.append((token, path))
    missing = [str(p) for _, p in selected if not p.is_dir()]
    if missing:
        hint = "; run scripts/fetch_bench_data.py pet002 or scripts/fetch_bench_data.py ds005016"
        raise SystemExit("missing dataset(s): " + ", ".join(missing) + hint)
    return selected


def run_validator(
    kind: str,
    rust_bin: Path,
    deno_bin: Path,
    deno_entry: Path,
    dataset: Path,
    content_mode: str,
    link_mode: str,
) -> dict:
    if kind == "rust":
        cmd = [str(rust_bin), "--content-mode", content_mode, "--link-mode", link_mode, str(dataset)]
    else:
        cmd = [str(deno_bin), "run", "-A", str(deno_entry), "--json", str(dataset)]
    start = time.perf_counter()
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stdout, tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stderr:
        proc = subprocess.Popen(cmd, stdout=stdout, stderr=stderr, text=True)
        _pid, status, usage = os.wait4(proc.pid, 0)
        elapsed_ms = (time.perf_counter() - start) * 1000
        stdout.seek(0)
        stderr.seek(0)
        out = stdout.read()
        err = stderr.read()
    returncode = os.waitstatus_to_exitcode(status)
    if returncode > 1 and not out.strip():
        raise RuntimeError(f"{kind} failed on {dataset}: {err[:500]}")
    try:
        parsed = json.loads(out)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"{kind} did not emit JSON on {dataset}: {out[:500]}") from exc
    return {
        "cmd": cmd,
        "returncode": returncode,
        "real_ms": elapsed_ms,
        "max_rss_mib": maxrss_mib(usage.ru_maxrss),
        "json": parsed,
    }


def maxrss_mib(maxrss: int) -> float:
    if platform.system() == "Darwin":
        return maxrss / (1024 * 1024)
    return maxrss / 1024


def summarize(runs: list[dict]) -> dict:
    return {
        "best_ms": min(r["real_ms"] for r in runs),
        "median_ms": statistics.median(r["real_ms"] for r in runs),
        "max_rss_mib": max(r["max_rss_mib"] for r in runs),
        "issue_count": len(runs[0]["json"].get("issues", {}).get("issues", [])),
    }


if __name__ == "__main__":
    raise SystemExit(main())
