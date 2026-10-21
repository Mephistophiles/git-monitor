#!/usr/bin/env python3
"""Generate a real offline report from deterministic, local Git histories."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

PROJECT = Path(__file__).resolve().parents[1]


def command(*args, cwd=None, env=None):
    return subprocess.run(args, cwd=cwd, env=env, check=True, text=True, capture_output=True).stdout.strip()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/debug/git-monitor")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.exists():
        raise SystemExit("Build the binary first: cargo build")
    with tempfile.TemporaryDirectory(prefix="git-monitor-demo-") as temporary:
        root = Path(temporary)
        work = root / "work"
        work.mkdir()
        git = lambda *args: command("git", "-C", str(work), *args)
        git("init", "-q", "-b", "main")
        git("config", "user.name", "Runtime Team")
        git("config", "user.email", "runtime@example.invalid")
        env = dict(os.environ, GIT_AUTHOR_DATE="2026-10-21T09:00:00+08:00", GIT_COMMITTER_DATE="2026-10-21T09:00:00+08:00")

        def write(path, text):
            target = work / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)

        def commit(subject):
            git("add", ".")
            command("git", "-C", str(work), "commit", "-qm", subject, env=env)

        before_c = """#include <stddef.h>

int validate_allocation(size_t size, size_t alignment) {
    if (size == 0)
        return 0;
    return size % alignment == 0;
}

void poison_region(char *begin, size_t length) {
    for (size_t i = 0; i < length; ++i)
        begin[i] = 0;
}
"""
        before_rs = """pub fn should_instrument(path: &str) -> bool {
    path.ends_with(".c") || path.ends_with(".cpp")
}

pub fn normalize_symbol(symbol: &str) -> &str {
    symbol.trim()
}
"""
        write("compiler-rt/lib/asan/allocation.c", before_c)
        write("tools/coverage/src/filter.rs", before_rs)
        write("libcxx/include/thread_guard.hpp", """#pragma once

class thread_guard {
public:
    explicit thread_guard(int &state) : state_(state) {}
    ~thread_guard() { state_ = 0; }

private:
    int &state_;
};
""")
        commit("Initial runtime snapshot")
        remotes = {}
        for name in ["origin", "upstream"]:
            remote = root / f"{name}.git"
            command("git", "init", "--bare", "-q", str(remote))
            git("remote", "add", name, str(remote))
            git("push", "-q", name, "main")
            remotes[name] = remote
        git("push", "-q", "origin", "main:refs/heads/release/22.x")
        config = root / "monitor.toml"
        config.write_text(f"""version = 1
[storage]
repositories_dir = "repos"
state_dir = "state"
[defaults]
digest_every = "0s"
[recipients.runtime-team]
to = ["runtime@example.invalid"]
[repositories.llvm-project.remotes.origin]
url = "{remotes['origin']}"
[repositories.llvm-project.remotes.upstream]
url = "{remotes['upstream']}"
[[watches]]
id = "runtime-safety"
repository = "llvm-project"
remotes = ["origin", "upstream"]
recipients = ["runtime-team"]
branches = {{ all = true }}
paths = ["compiler-rt/**", "libcxx/**", "tools/coverage/**"]
""")

        def run():
            result = command(str(binary), "--config", str(config), "run", "--once", "--force")
            print(result)

        run()
        write("compiler-rt/lib/asan/allocation.c", before_c.replace("if (size == 0)", "if (size == 0 || alignment == 0)").replace("return size % alignment == 0;", "return (alignment & (alignment - 1)) == 0\n        && size % alignment == 0;"))
        commit("Guard allocation alignment before checking the remainder")
        git("push", "-q", "origin", "main")
        git("push", "-q", "upstream", "main")
        write("tools/coverage/src/filter.rs", before_rs.replace('path.ends_with(".c") || path.ends_with(".cpp")', 'matches!(path.rsplit(\'.\').next(), Some("c" | "cpp" | "cc" | "rs"))').replace("symbol.trim()", 'symbol.trim().trim_start_matches("__wrap_")'))
        commit("Recognize Rust sources and normalize wrapped symbols")
        git("push", "-q", "origin", "main")
        write("libcxx/include/thread_guard.hpp", (work / "libcxx/include/thread_guard.hpp").read_text().replace("~thread_guard()", "thread_guard(const thread_guard &) = delete;\n    thread_guard &operator=(const thread_guard &) = delete;\n\n    ~thread_guard()"))
        commit("Make thread_guard ownership explicit")
        git("push", "-q", "origin", "main:refs/heads/release/22.x")
        run()
        docs = PROJECT / "docs"
        docs.mkdir(exist_ok=True)
        shutil.copyfile(root / "state/artifacts/runtime-team-latest.html", docs / "example-report.html")
        print(f"Generated {docs / 'example-report.html'} from real Git events")


if __name__ == "__main__":
    main()
