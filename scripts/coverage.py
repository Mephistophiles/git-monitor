#!/usr/bin/env python3
"""Measure Rust source coverage, including CLI/analysis/SMTP child processes.

Requires the matching toolchain's llvm-tools-preview component. Reports live in
target/coverage; tests and dependencies are excluded from the source totals.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--min-lines", type=float, default=90)
    parser.add_argument("--min-functions", type=float, default=90)
    parser.add_argument("--min-regions", type=float, default=85)
    parser.add_argument("--min-file-lines", type=float, default=85)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    target = root / "target" / "coverage"
    profiles = target / "profiles"
    if profiles.exists():
        shutil.rmtree(profiles)
    profiles.mkdir(parents=True)
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    host = next(line.split(": ", 1)[1] for line in subprocess.check_output(
        ["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    tools = sysroot / "lib" / "rustlib" / host / "bin"
    cov, profdata = tools / "llvm-cov", tools / "llvm-profdata"
    if not cov.exists() or not profdata.exists():
        parser.error("install the matching tools: rustup component add llvm-tools-preview")
    env = dict(os.environ)
    # Encoded flags would override RUSTFLAGS, so use one explicit coverage profile.
    env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    env.update(CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL="0",
               RUSTFLAGS=env.get("RUSTFLAGS", "") + " -C instrument-coverage",
               LLVM_PROFILE_FILE=str(profiles / "%p-%m.profraw"))
    command = ["cargo", "test", "--tests", "--locked"]
    if args.offline:
        command.append("--offline")
    build = subprocess.check_output(command + ["--no-run", "--message-format=json"],
                                    cwd=root, env=env, text=True)
    objects = set()
    for line in build.splitlines():
        artifact = json.loads(line)
        if artifact.get("reason") == "compiler-artifact" and artifact.get("executable"):
            objects.add(artifact["executable"])
    if not objects:
        raise SystemExit("No instrumented executables found")
    subprocess.run(command, cwd=root, env=env, check=True)
    raw = sorted(profiles.glob("*.profraw"))
    if not raw:
        raise SystemExit("No coverage profiles produced")
    inputs = target / "profiles.txt"
    inputs.write_text("\n".join(map(str, raw)) + "\n")
    merged = target / "coverage.profdata"
    subprocess.run([profdata, "merge", "-sparse", "-f", inputs, "-o", merged], check=True)
    flags = [f"--instr-profile={merged}",
             "--ignore-filename-regex=(/registry/|/rustc/|/tests/|/benches/)"]
    for obj in sorted(objects):
        flags += ["--object", obj]
    subprocess.run([cov, "report", *flags], cwd=root, check=True)
    exported = subprocess.check_output([cov, "export", *flags], cwd=root)
    (target / "coverage.json").write_bytes(exported)
    subprocess.run([cov, "show", *flags, "--format=html", f"--output-dir={target / 'html'}"],
                   cwd=root, check=True, stdout=subprocess.DEVNULL)
    data = json.loads(exported)["data"][0]
    totals = data["totals"]
    for metric, minimum in [("lines", args.min_lines), ("functions", args.min_functions),
                            ("regions", args.min_regions)]:
        value = totals[metric]["percent"]
        if not totals[metric]["count"] or value < minimum:
            raise SystemExit(f"{metric} coverage {value:.2f}% is below {minimum:.2f}%")
    for source in data["files"]:
        lines = source["summary"]["lines"]
        if lines["count"] and lines["percent"] < args.min_file_lines:
            raise SystemExit(f"{source['filename']}: line coverage {lines['percent']:.2f}% "
                             f"is below {args.min_file_lines:.2f}%")
    print(f"Coverage gates passed. HTML: {target / 'html' / 'index.html'}")


if __name__ == "__main__":
    main()
