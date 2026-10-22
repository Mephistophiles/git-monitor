# Architecture implementation checkpoint

Restored and published on 2026-10-22. The architecture specification is unchanged.

| Block | Implementation | Review |
| --- | --- | --- |
| Reproducibility | Frozen Git/dependency/analyzer/model profiles, explicit function rules, SHA-256 repositories, source epochs and offline doctor probes | [PR #3](https://github.com/Mephistophiles/git-monitor/pull/3) |
| Resource bounds and retention | Isolated bounded HTML/MIME workers, exact-byte continuations, MIME diagnostics, protected queue objects and maintenance/GC | [PR #4](https://github.com/Mephistophiles/git-monitor/pull/4) |
| Diff correctness | Byte reconstruction and selected-function scope proofs, canonical histogram exports, line/grapheme alignment, moved blocks, properties and fuzz target | [PR #5](https://github.com/Mephistophiles/git-monitor/pull/5) |
| Reuse and measurements | Checksummed immutable input, range and render caches; batched blobs; per-cache count/byte limits; counters and shared-vs-naive checks | [PR #6](https://github.com/Mephistophiles/git-monitor/pull/6) |
| Scheduling and recovery | Repository/host concurrency limits, stable poll jitter, dedicated SQLite command worker, shared computations, independent watch bindings, generation/token fencing and live/restart lease recovery | [PR #7](https://github.com/Mephistophiles/git-monitor/pull/7) |
| Structural model and reports | Cached AST descriptors, verified subtree anchors, parent/order matches, leaves/delimiters; typed hunks/line pairs/byte spans; structural controls, folding, local navigation, filters and ref provenance | [PR #8](https://github.com/Mephistophiles/git-monitor/pull/8) |

The final block also adds Tokio process I/O, signals and timers; independent persistent fetch/analysis/report/SMTP clocks; explicit new-branch base comparisons; ordinary aggregate comparisons; C++ overload/custom-rule and octopus-merge fixtures; and signal/recovery tests. A blocked analysis no longer stops subsequent fetch observations. `run --once` deliberately completes phases in fetch → analysis → report → SMTP order.

## Current verification

- 89 tests pass, including the 1000-binding shared-computation case, stale-token/restart/expiry tests, cached-binding migration, selected-byte projection properties, CR/invalid-UTF-8 regressions, syntax corpus and Git/analysis/SMTP signal cases.
- `cargo test --all-targets --offline --locked`, including Criterion smoke scenarios: pass.
- `cargo fmt --check` and `cargo clippy --all-targets --offline --locked -- -D warnings`: pass.
- Source coverage includes CLI and analysis/render/SMTP children: **94.93% lines, 96.86% functions, 90.40% regions**. Gates remain 90% aggregate lines/functions, 85% regions and 85% lines in every source file.
- LLVM reports mismatched data for 9 function records in the multi-executable profile merge; this warning remains visible and is not represented as a clean warning-free coverage merge.
- The separate fuzz workspace compiles. The target exercises raw patch parsing, byte reconstruction, escaping and splitting. A sustained coverage-guided fuzz campaign is separate from these checks.
- GitHub MSRV 1.90 and architecture-isolation jobs have passed. Final CI status belongs to the PR's exact head SHA.

## Browser verification

The browser script generates and opens a fresh self-contained report with HTTP blocked. It checks layouts, structural annotations, exact byte downloads, native folding, visible hunk navigation, event/aggregate filters, theme, mobile width and no-JS readability. The CI workflow installs Chromium before running it and retains the report/screenshots as artifacts.

A local browser pass is **not claimed**: Chromium is absent in this environment, and the browser download returned an HTML “Site Unavailable” page instead of an archive. The generated HTML is updated; refreshed screenshots require a successful browser run. Browser CI must pass before merging the report changes.

## Interpretation

Canonical Git histogram patches coexist with normalized Myers byte-reconstruction edits. Verified equal anchors determine contiguous edit gaps; matcher cursor hints are not blindly used as reconstruction coordinates. Segmentation is not claimed identical to Git's patch segmentation.

Structural correspondences have confidence/evidence labels and do not prove behavior equivalence. They supplement complete textual edits and exact selected-source exports. Current schema 3 reports read typed byte ranges; legacy rows remain a compatibility projection.

The 1/10/100/1000 repeated/distinct microbenchmarks measure selector grouping. The ten-binding end-to-end benchmark and 1000-binding integration test exercise real sharing; microbenchmark timings are not claimed as full Git timings for 1000 distinct analyses. Host measurements are observations, not performance guarantees or cold-disk measurements.
