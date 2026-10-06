# Forge: delivering verified work to GitHub

A verified task that needs delivery lands through `interlock-forge`. interlock merges exactly one commit, the verified head, and only through GitHub's own pin: `gh pr merge --match-head-commit`. Every call that changes GitHub is preceded by an operation row, and a restarted controller settles any operation whose outcome it never heard, from what GitHub shows.

This covers the design's §2 (merge not pinned to a head SHA), §3 and §4 (the forge adapter and `interlock-forge`), §5 (operation records), §7 invariant 7, §8 (landing authority, and the core checking its own forge actions), §11 step 7, and the delivery half of §12 P4.

**Live GitHub was not reachable while this was built.** `gh` is installed, but `gh auth status` reports that the container's GH_TOKEN is invalid. Every test runs the real `GhForge` code, with its real `gh` and `git` command lines, against a stateful fake `gh` and a local bare repository. [What a live run would add](#what-a-live-run-would-add) lists what that leaves unproven.

## The path

`interlock integrate run <task>`, or `interlock run` once a task is verified:

1. **G5.** The core checks that the task requires integration and that an active grant gives landing authority. Without it, the task is blocked at verified with the reason "no landing authority is granted for this task", before anything reaches the forge. With it, one transaction moves the task to integrating and writes the landing operation, `planned`, holding the verified head it may merge.
2. **Push and open the pull request** under an `open_pr` operation. interlock pushes the verified head to `interlock/<task>` with `--force-with-lease`, then runs `gh pr create`. A pull request that already holds the head is reused.
3. **Readiness.** `gh pr view --json ...` reads the head, the base, checks and mergeability. interlock polls until the pull request is ready, or until the wait runs out (`--wait`, 15 minutes by default).
4. **The pinned merge.** The landing operation is marked `started` and committed. Then interlock runs `gh pr merge <n> --merge --match-head-commit <verified head>`.
5. **G6.** interlock settles the operation from what `gh pr view` shows after the call, not from the call's exit code. A merge of the verified head is G6, and the task is done.

### The verified head

The verified head is `git commit-tree <task.current_tree> -p <snapshot base>`. Author, committer and date are fixed (the date is the task's creation time), and it is never signed (`--no-gpg-sign`, since `commit.gpgsign` would otherwise apply). So the same tree on the same base always gives the same commit id, and a restarted controller rebuilds exactly the head it pinned. The ref `refs/interlock/<task>/head` keeps it from garbage collection. No branch moves, and neither does the index or the worktree: the user's checkout is never touched.

## Operations

| Kind | Calls it covers | Action class |
| --- | --- | --- |
| `open_pr` | `git push` of the verified head to `interlock/<task>`, then `gh pr create` | external, reversible |
| `merge` | `gh pr merge --match-head-commit` | landing |
| `arm_auto_merge` | `gh pr merge --auto --match-head-commit` while checks are pending; a direct pinned merge if the pull request is already ready | landing |

Each operation goes `planned` → `started` → `confirmed`, `failed` or `unknown`, and every step is its own transaction:

- **`planned`** is written when the decision is made. The landing operation is written at G5.
- **`started`** is committed right before the call. In the same transaction the core checks landing authority again (§8: "the core separately checks every action it performs itself"). If the grant was revoked or expired, the operation fails without a call and the task is blocked with the reason. Only one operation per task may be in flight.
- **Settled.** After the call, interlock asks the forge what happened and records the verdict and the observation in the operation's `outcome`.

A test proves the ordering from the outside. The fake `gh` reads interlock's SQLite store each time it is called: `gh pr create` finds its `open_pr` row already `started`, and `gh pr merge` finds the `merge` row already `started`.

Reads (`gh pr view`, `gh pr list`, `git ls-remote`, `git fetch`) change nothing on the forge and get no operation row.

## Readiness

| What `gh pr view` shows | What interlock does |
| --- | --- |
| Head is the verified head, base is the snapshot base, checks pass, mergeable | Merge, pinned |
| Checks pending, mergeability unknown, draft, or blocked by branch protection | Wait until `--wait`, then stop with the task integrating. With `--auto-merge`, arm auto-merge pinned to the verified head instead |
| A check failed, conflicts, or the pull request was closed | Fail the landing operation and block the task, with the reason |
| **The head is not the verified head** | Do not merge. R2 (integrating → awaiting verification), with the reason |
| **The base branch moved** | The evidence is about a tree that would no longer land. See below |
| Already merged at the verified head | G6 |

### A changed base invalidates the evidence

When the base branch has moved past the snapshot base, interlock fetches it into `refs/interlock/<task>/base`. It then computes the tree that merging the verified head would produce, with `git merge-tree --write-tree`. In one transaction it records that tree and the new base on the task, fails the landing operation, and applies R2. Evidence about the old tree stops counting, so the task cannot pass G4 again until something verifies the new tree. The next verified head is that tree on the new base. Pushing it updates the same pull request, under a lease on the head interlock pushed before. If the change conflicts with the new base, the task is blocked instead.

### A moved head is never overwritten

`--match-head-commit` makes GitHub refuse the merge if anyone pushed to `interlock/<task>`, even mid-request. interlock then applies R2. If the evidence still covers the verified tree, the task returns to verified, but the next push will not overwrite a commit interlock did not push: every push carries a lease on a head interlock pushed for this task. The task is blocked instead: "the branch interlock/<task> holds X, which interlock did not push; delete it or reset it, then unblock the task".

## Reconcile

`interlock reconcile [<task>]` runs automatically at the start of every `interlock run`, and at the start of every `interlock integrate run`. It asks the forge about each operation in `started` or `unknown` state:

| What the forge shows | Verdict | Move |
| --- | --- | --- |
| Merged at the expected head | confirmed | G6 |
| Merged at another head | failed | blocked: "reconcile by hand" |
| Open, head moved | failed | R2 |
| Open at the expected head, queued for auto-merge | still started | none; watched again later |
| Open at the expected head, not queued | failed: the merge did not happen | none; integration merges again |
| Closed | failed | blocked |
| `open_pr`: a pull request holds the expected head | confirmed | none |
| `open_pr`: no such pull request | failed | none; integration opens it again |
| No answer | unknown | blocked: "the outcome of a forge operation is unknown: ..." |

A task blocked on an unknown outcome resumes by itself once the forge answers. A block the operator set is left alone.

The same rules apply right after a call. A merge call that dies without an answer, or that times out, is settled from what the pull request shows. If nothing can be seen, the operation stays unknown and the task is blocked.

## Commands and configuration

```bash
interlock integrate run <task> [--remote origin] [--base main] [--repo OWNER/REPO]
                               [--method merge|squash|rebase] [--auto-merge] [--wait 15m] [--poll 15s]
interlock integrate operations <task>
interlock reconcile [<task>]
```

`integrate run` exits 0 when the task is done and 5 otherwise, like `interlock run`. Both commands, and the reconcile at the start of `interlock run`, take the controller lock `.interlock/supervisor.lock`.

`interlock run` reads the same settings from the environment:

| Variable | Default |
| --- | --- |
| `INTERLOCK_GH_BIN` | `gh` |
| `INTERLOCK_FORGE_REMOTE` | `origin` |
| `INTERLOCK_FORGE_BASE` | the remote's default branch (`git ls-remote --symref`) |
| `INTERLOCK_FORGE_REPO` | none: `gh` infers the repository from the remote |
| `INTERLOCK_FORGE_METHOD` | `merge` |
| `INTERLOCK_FORGE_AUTO_MERGE` | off |
| `INTERLOCK_FORGE_WAIT`, `INTERLOCK_FORGE_POLL` | `15m`, `15s` |

In the supervisor, a verified task without integration still goes straight to done (G7). A verified task that needs integration goes through G5. Without landing authority it is blocked at verified with the reason; with it, the delivery above runs. While checks are pending the run stops with "waiting for the forge: ...", and the next run picks up where this one stopped.

## Tests

All tests are deterministic and need no network. The fake `gh` (`crates/interlock-forge/src/fake_gh.py`) is stateful and file-backed. Branches live in a real bare git repository, which interlock pushes to with plain `git`. The fake merges with real merge commits, and it emulates `gh`'s flags, JSON fields and error messages for `pr create`, `pr list`, `pr view` and `pr merge`. Its faults are set per subcommand:

| Fault | Effect |
| --- | --- |
| `move_head` | Someone pushes to the head branch while the merge request is in flight |
| `refuse` | Branch policy refuses the merge |
| `crash` | The fake merges, then SIGKILLs its caller (interlock) before it hears back |
| `die` | The fake merges, then fails without an answer |
| `vanish` | The fake merges, then stops answering anything |
| `queue` | The forge accepts the merge and completes it a few calls later |
| `unreachable` | No effect, and an answer like a network error |
| `checks` | `pass`, `pending` or `fail` |

| Design requirement | Test |
| --- | --- |
| Invariant 7: a crash right after a merge call; restart finds the merged SHA | `forge_cli.rs`: the fake kills `interlock integrate run` mid-merge. `interlock reconcile` then finds the merged head and applies G6, and so does a restarted `interlock run`, with no session. `deliver_copilot.rs`: the same through a whole `interlock run` on Copilot CLI, and the second run makes no model call. `operations.rs` (store): a started row is still there after the store is closed and reopened, and reconcile settles it |
| P4: a moved head refuses the merge | `deliver.rs`, `forge_cli.rs`: the head moves during the merge, GitHub's pin refuses it, R2 applies and main never moves. A later push will not overwrite the unverified commit |
| P4: a changed base invalidates evidence | `deliver.rs`, `forge_cli.rs`: the base moves after verification. The tree that would land is recorded, the old evidence goes stale, and R2 applies with no merge attempted. After re-verification, the new head lands on the new base |
| G5: without landing authority the task blocks at verified, with the reason | `deliver.rs`, `forge_cli.rs`, and `deliver_copilot.rs` through `interlock run`. No `gh` call is made |
| §8: the core checks its own forge actions | `deliver.rs`, `operations.rs`: a grant revoked after G5 stops the merge before the call |
| Operation rows are written before each call | `deliver.rs`: the fake `gh` reads the store when it is called |
| §11 step 7, end to end | `deliver_copilot.rs`: a bug-fix task with `integration_required = true` and an operator grant with landing authority. It runs through `interlock run` on the real Copilot CLI (offline, scripted model), G1–G6. Main gains a merge of exactly the verified head, made with `--match-head-commit`. The user's branch and files are untouched |
| Reconcile rules | `core/tests/delivery.rs`: each forge answer, plus a property test that only the pinned head ever lands |

One live run on Claude Code with a real model delivered the export-retry walkthrough through G5 and G6 on the same fake `gh`. See `evidence/forge/README.md`.

## What a live run would add

The fake `gh` was written from `gh` 2.89's help, its JSON field list (checked with `gh pr view --json` in this container), and GitHub's documented behavior. A run against a real repository would establish:

- **GitHub's own messages and timing.** That `--match-head-commit` refuses a moved head with the message and exit code interlock expects. That a pull request's `headRefOid` follows a push quickly; interlock waits when GitHub still shows a head it pushed itself. That `mergeStateStatus` reads as modeled.
- **Auto-merge and merge queues.** Whether `gh pr merge --auto` on a pull request that is already clean merges or errors; interlock avoids the question by merging directly when ready. Merge-queue repositories may not set `autoMergeRequest`. interlock then treats a merge request GitHub accepted, but has not finished, as in flight until the wait runs out, and reconcile retries it later.
- **Branch protection.** Required reviews (`BLOCKED`) and the "require branches to be up to date" rule, against interlock's own base check.
- **Credentials.** That `git push` works with the user's credential helper and that `gh` resolves the repository from the remote. Neither is needed with a local bare remote.

## Limits

- **The merge pins the head, not the base.** `gh` has no `--match-base-commit`. interlock checks the base right before the merge, but if the base moves between that check and GitHub's merge, the merge commit contains changes no evidence covered. GitHub's "require branches to be up to date" rule closes that window. Without it, the window is the time between one `gh pr view` and one `gh pr merge`.
- **Only GitHub, and only branches in the same repository.** Forks are not supported.
- **Rebases are squashed.** When the base moves, the next verified head is one commit holding the merged tree, not a replay of the original commits.
- **The snapshot base should be on the base branch.** If the snapshot base is not the tip of the base branch (for example, unpushed local commits), interlock treats that as a moved base. What lands is the merge of those commits and the change.
- **One controller per checkout.** `integrate run`, `reconcile` and `run` share a lock file. It is not a distributed lock.
- **A failing CI check blocks the task.** It is not sent back for rework: the core has no move from integrating to ready.
