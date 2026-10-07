# Live tests that wait on a GitHub token

Two parts of the design can only be proven against GitHub itself, and this development container has no working GitHub token. Every other live test runs on Claude Code as the coding agent; these two test the integration with GitHub and with Copilot's own model, which no other agent can stand in for. Both are ready to run, and neither has been run yet.

[TESTING.md](../TESTING.md) runs both with one command each (`testkit/run.sh github` and `testkit/run.sh copilot`), with the token set up as below; this page has the details.

## What they need

A fine-grained GitHub token, added as `GH_TOKEN` in the environment's settings (never pasted into a chat), with:

| For | Repository access | Permissions |
| --- | --- | --- |
| Live delivery | only a separate, empty test repository (for example `interlock-sandbox`) | Contents: read and write; Pull requests: read and write; Metadata: read |
| Copilot with its real model | (account level) | Copilot Requests, on an account with a Copilot plan |

A new session picks the token up. Never point the delivery test at this project's repository; the test refuses to.

## Live delivery on GitHub

```bash
INTERLOCK_LIVE_GITHUB_REPO=<owner>/<test-repo> INTERLOCK_LIVE_OUT=evidence/forge/live-github \
  cargo test -p interlock-cli --test live_github -- --ignored --nocapture
```

`crates/interlock-cli/tests/live_github.rs` pushes a scratch base branch `interlock-live-<time>` holding a bug to the test repository, verifies a fix by hand (no model calls), and runs `interlock integrate run`. That means G5, the push to `interlock/<task>`, a pull request, readiness, the merge pinned with `--match-head-commit`, and G6. It then checks that the base branch holds exactly the verified tree, and deletes both branches; the merged pull request stays in the repository's history. Set `INTERLOCK_FORGE_METHOD=squash` or `rebase` if the repository forbids merge commits.

The test pushes with `git`, which needs the token too: run `gh auth setup-git` first, or use `testkit/run.sh github`, which hands `git` the token for that run only.

What this adds to the fake-`gh` tests ([forge.md](forge.md), "What a live run would add"): GitHub's real responses, timing, mergeability states and branch protection.

## Copilot with its real model

With the token in place, the export-retry walkthrough runs on Copilot exactly as it does on Claude Code:

```bash
cp -r examples/export-retry /tmp/er && cd /tmp/er
git init -q && git add -A && git commit -qm "Exporter with retry"
interlock init && interlock task create task.toml
interlock run export-retry --host copilot          # unset COPILOT_OFFLINE and COPILOT_PROVIDER_* first
interlock task log export-retry
```

The guided path is the same: `interlock setup --host copilot`, then a Copilot session using the `interlock-route` skill, with `interlock verify export-retry --host copilot` for the independent verifier. Expect `G1 G2 G3 G4 G7`. Record the runs under `evidence/` as the Claude Code runs are.
