# Example: export retry

The bug from the design's walkthrough. `export` retries the whole batch after a transient failure, so rows written before the failure are written twice: the check prints `expected [1, 2, 3], got [1, 2, 1, 2, 3]`.

| Criterion | Check | Needs |
| --- | --- | --- |
| `repro`: retrying after a transient failure writes no duplicates | `sh checks/export-retry.sh` | `observed`, from an independent verifier |
| `regression`: the existing unit tests pass | `python3 -m unittest discover -s tests -q` | `tested`, from the worker or a verifier |

## Run it

Copy this directory somewhere, make it a git repository, then:

```bash
git init -q && git add -A && git commit -qm "Exporter with retry"
interlock init
interlock task create task.toml
interlock run export-retry --host claude-code      # or --host copilot
```

## Recorded run, October 5, 2026

Claude Code 2.1.289, conservative profile, one worker session and one verifier session. The task finished on the first attempt, with this transition log:

| Signal | Move | Reason |
| --- | --- | --- |
| G1 | pending → ready | input snapshot recorded |
| G2 | ready → running | worker attempt opened at epoch 1 |
| G3 | running → awaiting verification | result accepted (worker session, 21 s) |
| G4 | awaiting verification → verified | every criterion has current passing evidence (verifier session, 14 s) |
| G7 | verified → done | the workflow needs no delivery |

The worker's change made `export` resume after the last row written instead of restarting the batch:

```diff
 def export(rows, sink, retries=1):
     """Write every row to the sink. On a transient failure, retry the batch."""
+    written = 0
     for attempt in range(retries + 1):
         try:
-            for row in rows:
-                sink.write(row)
+            while written < len(rows):
+                sink.write(rows[written])
+                written += 1
             return len(rows)
```

All four evidence records point at the same output tree. The verifier also ran the reproduction on the base commit to confirm it catches the bug. The fix exists only as that tree and a dangling commit; the repository's branch was not touched.

Both agents noticed, independently, that the original regression command, `python3 -m unittest -q`, finds no tests here (no `tests/__init__.py`), so it passes without checking anything. They used `unittest discover` instead and said so in their notes. The task file now uses the corrected command. A check that runs nothing should never count as evidence; see "Next" in the main README.
