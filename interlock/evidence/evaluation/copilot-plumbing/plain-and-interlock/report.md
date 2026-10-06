# Evaluation report

Generated 2026-10-06T00:09:37+00:00. Host copilot 1.0.91, model `None`, effort `None`, interlock at `d65d750ed5b5`. **Scripted fake model: plumbing only, not evaluation evidence.**

Stand-in task set (not the S3 repository). Small n. One host, one model. Hidden checks are executable; no model judges anything.

Spend: $0.00 reported by the host; $0.00 charged against the $1.00 budget (includes reserves for sessions that never reported a cost).

Not run: skills.

## By condition

| Metric | plain | interlock |
| --- | --- | --- |
| Runs | 8 | 8 |
| Accepted outcomes | 8/8 | 8/8 |
| Hidden checks pass (claimed or not) | 8/8 | 8/8 |
| Completion claimed | 8/8 | 8/8 |
| Unsupported completion claims | 0/8 | 0/8 |
| Missed defects (failed hidden cases in claimed outputs) | 0 | 0 |
| Runs with scope violations | 0/8 | 0/8 |
| Correct output not claimed | 0/8 | 0/8 |
| Recovery after forced interruption | 1/1 | 2/2 |
| Human interventions | 0 | 0 |
| Ended needing an operator | 0/8 | 0/8 |
| Sessions per run (mean) | 1.125 | 2.25 |
| Wall time, mean / median (s) | 2.837 / 2.2 | 10.838 / 9.65 |
| Cost reported, total (USD) | 0.00 | 0.00 |
| Cost per run, mean over runs with complete cost (USD) | unavailable | unavailable |
| Runs with complete cost | 0/8 | 0/8 |
| Runs with complete token counts | 0/8 | 0/8 |
| Output tokens, summed over runs with complete counts | unavailable | unavailable |
| Transcripts mentioning hidden material | 0 | 0 |

## By task

| Task | Condition | Accepted | Claimed | Unsupported | Missed defects | Scope violations | Mean wall (s) | Cost reported (USD) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| go-env-expand | plain | 2/2 | 2/2 | 0 | 0 | 0 | 4.9 | 0.00 |
| go-env-expand | interlock | 2/2 | 2/2 | 0 | 0 | 0 | 15.95 | 0.00 |
| go-lookup-refactor | plain | 1/1 | 1/1 | 0 | 0 | 0 | 2.1 | 0.00 |
| go-lookup-refactor | interlock | 1/1 | 1/1 | 0 | 0 | 0 | 9.0 | 0.00 |
| go-quoted-hash | plain | 1/1 | 1/1 | 0 | 0 | 0 | 2.1 | 0.00 |
| go-quoted-hash | interlock | 1/1 | 1/1 | 0 | 0 | 0 | 11.0 | 0.00 |
| py-bisect-settle | plain | 1/1 | 1/1 | 0 | 0 | 0 | 2.2 | 0.00 |
| py-bisect-settle | interlock | 1/1 | 1/1 | 0 | 0 | 0 | 7.2 | 0.00 |
| py-csv-format | plain | 1/1 | 1/1 | 0 | 0 | 0 | 2.1 | 0.00 |
| py-csv-format | interlock | 1/1 | 1/1 | 0 | 0 | 0 | 8.7 | 0.00 |
| py-parse-amount | plain | 1/1 | 1/1 | 0 | 0 | 0 | 2.2 | 0.00 |
| py-parse-amount | interlock | 1/1 | 1/1 | 0 | 0 | 0 | 10.3 | 0.00 |
| py-split-remainder | plain | 1/1 | 1/1 | 0 | 0 | 0 | 2.2 | 0.00 |
| py-split-remainder | interlock | 1/1 | 1/1 | 0 | 0 | 0 | 8.6 | 0.00 |

## Runs

| Run | Claimed | Hidden | Failed hidden cases | Scope violations | interlock | Wall (s) | Cost (USD) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| go-env-expand.interlock.r1.55bc20 | True | True | - | - | done G1 G2 R3 G2 G3 G4 G7 | 13.6 | unavailable |
| go-env-expand.interlock.r2.2ac396 | True | True | - | - | done G1 G2 R3 G2 G3 G4 G7 | 18.3 | unavailable |
| go-env-expand.plain.r1.f328f1 | True | True | - | - |  | 2.2 | unavailable |
| go-env-expand.plain.r2.ee56ab | True | True | - | - |  | 7.6 | unavailable |
| go-lookup-refactor.interlock.r1.9fa97c | True | True | - | - | done G1 G2 G3 G4 G7 | 9.0 | unavailable |
| go-lookup-refactor.plain.r1.38ace7 | True | True | - | - |  | 2.1 | unavailable |
| go-quoted-hash.interlock.r1.5d84ee | True | True | - | - | done G1 G2 G3 G4 G7 | 11.0 | unavailable |
| go-quoted-hash.plain.r1.2a50ef | True | True | - | - |  | 2.1 | unavailable |
| py-bisect-settle.interlock.r1.feb78e | True | True | - | - | done G1 G2 G3 G4 G7 | 7.2 | unavailable |
| py-bisect-settle.plain.r1.1601ed | True | True | - | - |  | 2.2 | unavailable |
| py-csv-format.interlock.r1.09869d | True | True | - | - | done G1 G2 G3 G4 G7 | 8.7 | unavailable |
| py-csv-format.plain.r1.41ea9a | True | True | - | - |  | 2.1 | unavailable |
| py-parse-amount.interlock.r1.3930e4 | True | True | - | - | done G1 G2 G3 G4 G7 | 10.3 | unavailable |
| py-parse-amount.plain.r1.ae7166 | True | True | - | - |  | 2.2 | unavailable |
| py-split-remainder.interlock.r1.7d6ab9 | True | True | - | - | done G1 G2 G3 G4 G7 | 8.6 | unavailable |
| py-split-remainder.plain.r1.ff47b3 | True | True | - | - |  | 2.2 | unavailable |

Definitions are in docs/evaluation.md.
