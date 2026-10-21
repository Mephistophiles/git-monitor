# selected-fetch-v1

Measurements for v0.1.0. These are observations on one host, not performance guarantees.

| Scenario | Mean | 95% confidence interval |
| --- | --- | --- |
| end_to_end/incremental_fetch_to_mime | 406.927 ms | 402.783–412.151 ms |
| end_to_end/incremental_selected_fetch_to_mime | 568.376 ms | 566.715–570.052 ms |
| native_syntax_diff_90_lines | 1.707 ms | 1.696–1.720 ms |
| native_text_diff_90_lines | 1.085 ms | 1.036–1.139 ms |

CPU: AMD EPYC 9V74 80-Core Processor. Release profile, local Git transport. Fixture setup and baseline fetching are excluded from the incremental measurement. SMTP is not contacted.

The microbenchmarks compare 90 lines of C and include native highlighting. The end-to-end benchmark runs the actual binary through incremental fetch, selection, worker analysis, HTML, and MIME outbox creation.

Both end-to-end scenarios use the same one-branch history. Selected fetching adds discovery, a persisted manifest, and checked atomic publication, so this fixture measures its fixed overhead rather than savings on a remote with many unrelated branches. Integration tests separately verify that unselected orphan tips are not downloaded and that a no-change selected poll skips object downloading.

Source SHA-256: `eeafa8f850eb8ae71b7c51a4fae0d929ded0aa451ddd532552297555e5fdc4e9`. Tool versions, environment, raw samples, and Criterion estimates are in [results.json](results.json). Observation timestamps use the wall clock; the OS cache is not controlled. Do not interpret these as cold-disk measurements.
