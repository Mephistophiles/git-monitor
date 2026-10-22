# restored-architecture-v3

Measurements for v0.1.0. These are observations on one host, not performance guarantees.

| Scenario | Mean | 95% confidence interval |
| --- | --- | --- |
| canonical_filter_grouping/distinct_n_1 | 0.001 ms | 0.001–0.001 ms |
| canonical_filter_grouping/distinct_n_10 | 0.011 ms | 0.009–0.013 ms |
| canonical_filter_grouping/distinct_n_100 | 0.115 ms | 0.108–0.127 ms |
| canonical_filter_grouping/distinct_n_1000 | 1.036 ms | 0.991–1.092 ms |
| canonical_filter_grouping/repeated_n_1 | 0.001 ms | 0.001–0.001 ms |
| canonical_filter_grouping/repeated_n_10 | 0.010 ms | 0.010–0.010 ms |
| canonical_filter_grouping/repeated_n_100 | 0.100 ms | 0.098–0.103 ms |
| canonical_filter_grouping/repeated_n_1000 | 0.969 ms | 0.947–0.989 ms |
| end_to_end_repeated_filters/10_watch_bindings_one_analysis | 580.539 ms | 575.027–585.252 ms |
| native_syntax_diff_90_lines | 2.656 ms | 2.632–2.681 ms |
| native_text_diff_90_lines | 1.140 ms | 1.121–1.161 ms |

CPU: AMD EPYC 9V74 80-Core Processor. Release profile, local Git transport. Fixture setup and baseline fetching are excluded from the incremental measurement. SMTP is not contacted.

The native engine microbenchmarks compare 90 lines of C and include native highlighting. Selector microbenchmarks separately measure canonical grouping for 1/10/100/1000 repeated or distinct subscriptions. The end-to-end benchmark runs the actual binary through incremental fetch, selection, worker analysis, HTML, and MIME outbox creation.

Source SHA-256: `2caede927e168b4b8f798acd6c4220e0d1abb7439e5010a6e299f7bcd1114c3a`. Tool versions, environment, raw samples, and Criterion estimates are in [results.json](results.json). Observation timestamps use the wall clock; the OS cache is not controlled. Do not interpret these as cold-disk measurements.
