#!/usr/bin/env python3
"""Persist an immutable Criterion snapshot and a reproducible source fingerprint."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument("--name", default="baseline-v0")
args = parser.parse_args()
if not args.name.replace("-", "").replace("_", "").isalnum():
    raise SystemExit("Invalid baseline name")
destination = root / "benchmarks/baselines" / args.name
if destination.exists():
    raise SystemExit("Refusing to overwrite a saved baseline; select a new name")
sources = sorted([root / "Cargo.toml", root / "Cargo.lock", *root.glob("src/*"), *root.glob("benches/*")])
source_hash = hashlib.sha256()
for path in sources:
    source_hash.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes() + b"\0")
results = {}
for directory in sorted((root / "target/criterion").rglob(args.name)):
    if not (directory / "estimates.json").exists():
        continue
    name = str(directory.parent.relative_to(root / "target/criterion"))
    results[name] = {"estimates_ns": json.loads((directory / "estimates.json").read_text()), "samples": json.loads((directory / "sample.json").read_text())}
if not results:
    raise SystemExit("Run cargo bench with --save-baseline first")

def output(*args):
    return subprocess.check_output(args, cwd=root, text=True).strip()

cpu = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), platform.machine())
manifest = {
    "baseline": args.name,
    "application_version": "0.1.0",
    "source_state": "pre-commit initial implementation; identified by source_sha256",
    "source_sha256": source_hash.hexdigest(),
    "cargo_lock_sha256": hashlib.sha256((root / "Cargo.lock").read_bytes()).hexdigest(),
    "rustc": output("rustc", "-Vv"),
    "cargo": output("cargo", "--version"),
    "git": output("git", "--version"),
    "os": platform.platform(),
    "cpu": cpu,
    "logical_cpus": os.cpu_count(),
    "filesystem": output("stat", "-f", "-c", "%T", str(root)),
    "profile": {"name": "release", "lto": False, "strip": True},
    "cache_conditions": "Warm application-independent OS cache; fixture and baseline-fetch setup outside measurement; no cold-disk claim.",
    "transport": "Local Git transport; MIME prepared without SMTP or external network requests.",
    "fixture": "Fixed source pairs in benches/baseline.rs; Git dates fixed to 2000-01-01T00:00:00Z. Application observation timestamps use the wall clock.",
    "criterion": results,
}
destination.mkdir(parents=True)
(destination / "results.json").write_text(json.dumps(manifest, indent=2) + "\n")
rows = []
for name, result in results.items():
    mean = result["estimates_ns"]["mean"]
    estimate = mean["point_estimate"] / 1e6
    interval = mean["confidence_interval"]
    rows.append(f"| {name} | {estimate:.3f} ms | {interval['lower_bound']/1e6:.3f}–{interval['upper_bound']/1e6:.3f} ms |")
summary = f"""# {args.name}

Initial measurements for v0.1.0. These are observations on one host, not performance guarantees.

| Scenario | Mean | 95% confidence interval |
| --- | --- | --- |
{chr(10).join(rows)}

CPU: {cpu}. Release profile, local Git transport. Fixture setup and baseline fetching are excluded from the incremental measurement. SMTP is not contacted.

The microbenchmarks compare 90 lines of C and include native highlighting. The end-to-end benchmark runs the actual binary through incremental fetch, selection, worker analysis, HTML, and MIME outbox creation.

Source SHA-256: `{source_hash.hexdigest()}`. Tool versions, environment, raw samples, and Criterion estimates are in [results.json](results.json). Observation timestamps use the wall clock; the OS cache is not controlled. Do not interpret these as cold-disk measurements.
"""
(destination / "README.md").write_text(summary)
print(summary)
