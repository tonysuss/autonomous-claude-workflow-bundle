# Bug fix playbook

Be scientific. Every line you ship traces to runtime evidence. A change that "might help" is a hypothesis and does not ship; when evidence refutes a hypothesis, revert what it motivated.

1. **Reproduce it on the input snapshot.** Have interlock run the reproduction there (`interlock check run --criterion repro --target base`). Read its output: it must fail for the reported reason. If it cannot reproduce, tighten the conditions or instrument until it does; do not fix what you have not seen fail.
2. **Find the cause.** List the candidate causes, then rule them out one by one with runtime evidence: run the code, add a print, read it. Take the test that eliminates the most candidates each time. Confirm the surviving mechanism with evidence before writing the fix. Use `references/fix-root-causes.md`.
3. **Make the smallest fix** at the cause, inside the scope, in the worktree. Fix the same mistake wherever else it appears in scope.
4. **Add a regression test** when the project has a cheap way to test this path. It must fail without the fix and pass with it (`references/test-behavior-not-implementation.md`).
5. **Verify on the same surface.** Have interlock run every check on your files (`interlock check run --criterion <c>`). The reproduction now passes and the regression guard still passes. "Inconclusive" or a different surface is not a pass; say so.
6. **Claim and submit** (implement steps 4 and 5).

In the result summary: what was broken, the root cause, the fix, and the failing-then-passing output of the reproduction, quoted.
