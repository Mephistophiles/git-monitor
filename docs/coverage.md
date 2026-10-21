# Coverage and review regressions

Measured on 2026-10-21 with Rust 1.99.1 / LLVM 23.1.1 on Linux, using
`python3 scripts/coverage.py --offline`. All 49 tests passed (43 monitoring/CLI
integration tests, four process tests, and two syntax tests). These results are
a snapshot; the CI artifact contains the measurement for each subsequent run.

| Source | Line coverage |
| --- | ---: |
| config.rs | 97.67% |
| diff.rs | 95.82% |
| git.rs | 97.37% |
| lib.rs | 96.15% |
| mail.rs | 90.69% |
| main.rs | 90.29% |
| model.rs | 100.00% |
| monitor.rs | 93.60% |
| process.rs | 89.35% |
| render.rs | 96.61% |
| store.rs | 94.63% |
| **Total** | **94.61% (2895 / 3060)** |

Function coverage is **94.38% (235 / 249)** and region coverage is
**89.17% (4866 / 5457)**. Thresholds are 90% lines, 90% functions, 85% regions,
and 85% lines for each individual source file. Tests, benchmarks, dependencies,
and standard-library sources are excluded from these totals.

## Behavioral checks

The four review reproductions failed before the fixes. Their regression cases
now check the following invariants:

| Area | Checks |
| --- | --- |
| Changing selectors | Narrowing, equivalent glob changes, unrestricted selectors and added exclusions retain changes in the continuous scope. Expansion baselines only newly selected paths. |
| Persistence and caching | The selector intersection survives a restart; different previous scopes cannot share an incorrect cache entry; subsequent identical scopes share analysis again; legacy tasks still deserialize. |
| Process deadlines | Deadlines cover inherited stdout/stderr and blocked stdin after parent exit; SIGTERM terminates remaining pipe holders; successful input/output is byte-exact; output limits and exit codes remain visible. |
| Revocation and report windows | Removed recipients and watches are revoked before the byte budget; eligible rows fit in the same call; later pending matches survive budget limits; oversized items are explicit. |
| Syntax classification | Python block nesting changes are not labeled formatting-only; harmless whitespace/comments in Python, C, Rust and JSON retain the formatting classification. |
| Recovery and delivery | Failed analysis is uncached and can be retried after raising limits; status/preview/export work; polling respects backoff; changed addresses rebuild the pending outbox. |
| Non-text files | Binary, executable-mode and gitlink changes are reported explicitly. |

## Measurement boundaries

Each instrumented process writes a unique `%p-%m.profraw` profile. The script
merges profiles from the test binaries and spawned application/worker binaries;
no SMTP service outside the local test fixture is contacted. Profiles are cleared
before each run, and LLVM tools come from the same Rust toolchain as the build.

This stable-toolchain run measures lines, functions and regions, **not branch
coverage or MC/DC**. A high percentage does not prove every error combination is
tested. Some uncovered paths are OS/SQLite errors and post-fork `pre_exec` code
(its counters are discarded by `exec`); abruptly killed processes cannot flush
their counters. The tests assert the resulting shutdown/recovery behavior.
Browser JavaScript/CSS is outside this Rust coverage report; the existing browser
checks remain a separate validation step.
