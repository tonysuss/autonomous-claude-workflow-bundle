# Prove it works

Check the real thing directly. Proxies feel cheaper and are wrong often enough to cost more: file timestamps, output that looks fresh, an agent's summary, a cached screenshot, a green build.

- **Same surface, before and after.** Reproduce the problem where it was reported (the CLI, the API, the page), then show the same steps succeed after the change. interlock records both: a run of the check on the input snapshot and a run on your files.
- **Read the actual value,** not something derived from it. Read the output a check printed, not just its exit code. A run that tested nothing proves nothing, and interlock will not count it.
- **Inconclusive is not a pass.** A check that could not run is `blocked`. A check on the wrong surface proves the wrong thing. Say so.
- **Unit tests show branches, not the absence of the bug.** They guard against regressions; the reproduction proves the fix.
- **When a check disagrees with you, suspect how you observed first,** then the system.
- **Script the check.** A command a reviewer can rerun beats a description of what you saw. Put it in the criterion's `check` so interlock runs it.
