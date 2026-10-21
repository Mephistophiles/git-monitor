# baseline-v0

Initial measurements for v0.1.0. These are observations on one host, not performance guarantees.

| Scenario | Mean | 95% confidence interval |
| --- | --- | --- |
| end_to_end/incremental_fetch_to_mime | 400.700 ms | 399.823–401.545 ms |
| native_syntax_diff_90_lines | 1.860 ms | 1.830–1.896 ms |
| native_text_diff_90_lines | 1.000 ms | 0.979–1.027 ms |

CPU: AMD EPYC 9V74 80-Core Processor. Release profile, local Git transport. Fixture setup and baseline fetching are excluded from the incremental measurement. SMTP is not contacted.

The microbenchmarks compare 90 lines of C and include native highlighting. The end-to-end benchmark runs the actual binary through incremental fetch, selection, worker analysis, HTML, and MIME outbox creation.

Source SHA-256: `644794f30d344b581da2cf55bc591081119e4c711316260eb7a79c48485c0fdd`. Tool versions, environment, raw samples, and Criterion estimates are in [results.json](results.json). Observation timestamps use the wall clock; the OS cache is not controlled. Do not interpret these as cold-disk measurements.
