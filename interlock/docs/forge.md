# Forge: delivering verified work to GitHub

A verified task that needs delivery lands through `interlock-forge`. interlock lands exactly one commit, the verified head, and merges only through GitHub's own pin: `gh pr merge --match-head-commit`. Every call that changes GitHub is preceded by an operation row. A restarted controller settles any operation whose outcome it never heard, from what GitHub and git show. G6 is applied only when the merge that landed is exactly what the evidence was for.

This covers the design's §2 (merge not pinned to a head SHA), §3 and §4 (the forge adapter and `interlock-forge`), §5 (operation records), §7 invariant 7, §8 (landing authority, and the core checking its own forge actions), §11 step 7, and the delivery half of §12 P4.

**Live GitHub was not reachable while this was built.** `gh` is installed, but `gh auth status` reports that the container's GH_TOKEN is invalid. Every test runs the real `GhForge` code, with its real `gh` and `git` command lines, against a stateful fake `gh` and a local bare repository. [What a live run would add](#what-a-live-run-would-add) lists what that leaves unproven.

## Who merges

Landing authority comes from the operator's grants (design §8). It decides how far interlock goes:

| Landing authority | What interlock does |
| --- | --- |
| `none` | G5 is refused: the task is blocked at verified, with the reason "no landing authority is granted for this task". Nothing reaches the forge |
| `coordinator`, `owner` | interlock opens the pull request pinned at the verified head and merges it itself (`interlock run`, `interlock integrate run`) |
| `operator` | interlock opens the pull request pinned at the verified head, but never merges or arms auto-merge. It stops with "waiting for the operator to merge pull request #n at `<head>`". G6 comes only from reconcile seeing the operator's merge, checked like any other |

The core enforces the last row too: starting a `merge` or `arm_auto_merge` operation under `operator` authority is refused.

## The path

`interlock integrate run <task>`, or `interlock run` once a task is verified:

1. **G5.** The core checks that the task requires integration and that an active grant gives landing authority. One transaction moves the task to integrating and writes the landing operation, `planned`, holding the verified head it may land. Nothing reaches the forge before G5 passes, and no ref is written.
2. **Push and open the pull request** under an `open_pr` operation. interlock pushes the verified head to `interlock/<task>` with `--force-with-lease`, then runs `gh pr create`. A pull request that already holds the head is reused. Right after its own push, GitHub may still show the previous head. interlock reads again, up to five times, and if it is still behind, leaves the operation in flight and stops rather than blocking.
3. **Readiness.** `gh pr view --json ...` reads the head, base branch, base commit, checks and mergeability. interlock polls until the pull request is ready, or until the wait runs out (`--wait`, 15 minutes by default).
4. **The pinned merge.** interlock reads the base branch with `git ls-remote` right before the call. GitHub's `baseRefOid` can lag, and git's answer must still be the snapshot base. Then the landing operation is marked `started` (see below) and interlock runs `gh pr merge <n> --merge --match-head-commit <verified head>`.
5. **G6.** interlock settles the operation from what the forge and git show after the call, not from the call's exit code. G6 needs all of the conditions in [When a merge counts](#when-a-merge-counts).

Each pass begins with the evidence. If it went stale while the task was integrating, for example because `interlock task tree` recorded a new tree, the pass applies R2 instead of continuing. The supervisor does the same before every delivery step. A head that changed after G5 is R2, never a quiet re-plan.

### The verified head

The verified head is `git commit-tree <task.current_tree> -p <snapshot base>`. Author, committer and date are fixed (the date is the task's creation time), and it is never signed. So the same tree on the same base gives the same commit id in any clone, at any time, and a restarted controller rebuilds exactly the head it pinned.

A test (`the_verified_head_is_the_same_commit_in_another_clone_a_second_later_and_never_signed`) computes it in a repository, then again more than a second later in a clone of it. Both have commit signing switched on and pointed at a program that always fails. The test checks that both give the same id and that the head carries no signature. Undoing parts of the fix shows what the test catches (`evidence/forge/mutations/`):

- Removing the fixed dates makes the test fail.
- Asking `commit-tree` to sign (`-S`) makes it fail.
- Removing `--no-gpg-sign` does not. git 2.43's `commit-tree` signs only when given `-S` and does not apply `commit.gpgsign`, so the flag is a guard, not a fix.

From G5 until the task ends, the ref `refs/interlock/<task>/head` keeps the head from garbage collection. Once a task is done, failed or cancelled, the next delivery pass or reconcile deletes its `refs/interlock/<task>/*` refs. No branch moves, and neither does the index or the worktree.

`interlock/<task>` escapes characters git does not allow with `%`, which task ids never contain, so two tasks never share a branch. A property test checks that it reverses.

## Operations

| Kind | Calls it covers | Action class |
| --- | --- | --- |
| `open_pr` | `git push` of the verified head to `interlock/<task>`, then `gh pr create` | external, reversible |
| `merge` | `gh pr merge --match-head-commit` | landing |
| `arm_auto_merge` | `gh pr merge --auto --match-head-commit` while readiness says to wait; a direct pinned merge if the pull request is already ready | landing |
| `disarm_auto_merge` | `gh pr merge --disable-auto`, when landing authority ended while auto-merge was armed | external, reversible |

Each operation goes `planned` → `started` → `confirmed`, `failed` or `unknown`, and every step is its own transaction:

- **`planned`** is written when the decision is made. The landing operation is written at G5. A landing still planned when R2 applies was never called. It stays `planned` (`integrate operations` shows it) until the next G5 fails it with "superseded before any call by <the new operation>".
- **`started`** is committed right before the call. In that same transaction the store checks:
  - the task's evidence still passes for its current tree;
  - the head was built from that tree;
  - landing authority still holds (§8: "the core separately checks every action it performs itself").

  Stale evidence or a different tree fails the operation without a call and applies R2. A revoked or expired grant fails it and blocks the task. Only one operation per task may be in flight, except a disarm while its auto-merge is armed.
- **Settled.** After the call, interlock asks the forge what happened and records the verdict and the observation in the operation's `outcome`. Confirmed and failed are final: the store refuses to settle them again.

A test proves the ordering from the outside. The fake `gh` reads interlock's SQLite store each time it is called: `gh pr create` finds its `open_pr` row already `started`, and `gh pr merge` finds the `merge` row already `started`.

Reads (`gh pr view`, `gh pr list`, `git ls-remote`, `git fetch`) change nothing on the forge and get no operation row.

## Readiness

| What `gh pr view` shows | What interlock does |
| --- | --- |
| The verified head, targeting the pinned branch, on the snapshot base, checks pass, mergeable | Read the base with git, then merge, pinned |
| Checks pending, mergeability unknown, draft, or blocked by branch protection | Wait until `--wait`, then stop with the task integrating. With `--auto-merge` (and coordinator or owner authority), arm auto-merge pinned to the verified head instead |
| No checks at all, less than `--checks-settle` (default 30 s) after the push | Wait: CI may not have reported yet |
| A check failed, conflicts, the pull request was closed, or **it targets another branch** | Fail the landing operation and block the task, with the reason |
| **The head is not the verified head** | Do not merge. R2, with the reason. A head interlock itself pushed earlier is read as GitHub lagging, and interlock waits |
| **The base moved** (GitHub's `baseRefOid`, or git right before the merge) | The evidence is about a tree that would no longer land. See below |
| Merged | Settled by the rules in [When a merge counts](#when-a-merge-counts) |

### A changed base invalidates the evidence

When the base branch has moved past the snapshot base, interlock fetches it into `refs/interlock/<task>/base`. It then computes the tree that merging the verified head would produce, with `git merge-tree --write-tree`. In one transaction it records that tree and the new base on the task, fails the landing operation, and applies R2. Evidence about the old tree stops counting, so the task cannot pass G4 again until something verifies the new tree. The next verified head is that tree on the new base. Pushing it updates the same pull request, under a lease on the head interlock pushed before. If the change conflicts with the new base, the task is blocked instead.

### A moved head is never overwritten

`--match-head-commit` makes GitHub refuse the merge if anyone pushed to `interlock/<task>`, even mid-request. interlock then applies R2. If the evidence still covers the verified tree, the next step advances straight back to verified with no verifier session, and the next push will not overwrite a commit interlock did not push: every push carries a lease on a head interlock pushed for this task. The task is blocked instead: "the branch interlock/<task> holds X, which interlock did not push; delete it or reset it, then unblock the task".

## When a merge counts

G6 is applied only when all of these hold:

| Condition | Read from | Otherwise |
| --- | --- | --- |
| The merged head is the pinned head (full or abbreviated id) | `gh pr view` | Merge recorded; task blocked: "merged at X, but interlock pinned Y" |
| It was merged into the pinned branch | `gh pr view` (`baseRefName`) | Merge recorded; task blocked: "merged into X, not Y" |
| The base branch contains the merge commit | `git fetch` of the base branch, then `git merge-base --is-ancestor` | Unknown, or for an operation interlock never started, blocked: the forge's word alone is not enough |
| The merge's first parent is the snapshot base | `git rev-parse <merge>^1` | Merge recorded; task blocked: "landed on a base nobody verified" |
| The tree it left is the verified tree | `git rev-parse <merge>^{tree}` | Merge recorded; task blocked: "landed a tree nobody verified" |
| The evidence still passes for the task's current tree | the store, in the settling transaction | Merge recorded; task blocked |
| Landing authority held when the forge merged (`mergedAt`) | the grants, in the settling transaction | Merge recorded; task blocked: "merged at T, when no landing authority was granted" |

"Merge recorded" means the operation is confirmed, because the forge did act, while the task is blocked with the reason for the operator. Nothing here undoes a merge on the forge.

## Reconcile

`interlock reconcile [<task>]` runs automatically at the start of every `interlock run`, and at the start of every `interlock integrate run`. It asks the forge about each operation in `started` or `unknown` state. It also asks about each planned landing tied to a pull request, which is how the operator's merge is found. Then:

| What the forge shows | Verdict | Move |
| --- | --- | --- |
| A merge that meets every condition above | confirmed | G6 |
| A merge that does not | confirmed (the forge acted) | blocked, with the reason |
| Open, head moved | failed | R2 |
| Open at the expected head, queued for auto-merge | still started | none; watched again later |
| Open at the expected head, not queued | failed: the merge did not happen | none; integration merges again |
| Open, retargeted to another branch, or closed | failed | blocked |
| `open_pr`: a pull request holds the expected head | confirmed | none |
| `open_pr`: no such pull request | failed | none; integration opens it again |
| No answer | unknown | blocked: "the outcome of a forge operation is unknown: ..." |
| A planned landing whose pull request is still open, or no answer | nothing recorded | none: no call was made |

An armed auto-merge whose landing authority has ended is disarmed first, under a `disarm_auto_merge` operation, and the task is blocked with the reason. If the forge already merged, the merge is settled as seen, and G6 still needs authority at the time of the merge.

A task blocked on an unknown outcome resumes by itself once the forge answers. A block the operator set is left alone.

The same rules apply right after a call, with one more: a failed merge call is `failed` only when GitHub plainly refused (for example "is not mergeable" or "Head branch was modified"). Any other failure, including a timeout or a dropped connection while the pull request still shows open at the head, leaves the operation `unknown` for reconcile, because the request may still land.

## Agents cannot drive the forge

- **The forge commands are the operator's.** `integrate run`, `integrate begin`, `integrate confirm`, `reconcile` and `run` refuse to run inside an attempt (when `INTERLOCK_ATTEMPT` is set), whatever the hooks let through. `integrate operations` only reads, so it runs anywhere.
- **The hook's classifier agrees.** It classifies those commands as operator-only, so a headless session is denied and an interactive one asks.
  - A `git push` to anything but an `interlock/...` branch, including a bare `git push`, is landing.
  - So are `gh api` merges and GraphQL merge mutations.
  - Setting `INTERLOCK_GH_BIN`, `INTERLOCK_FORGE_*` or `INTERLOCK_DB` on a command line is operator-only.
- **Inside an attempt, interlock ignores those settings anyway.**
- **The store directory is out of bounds.** The hook denies any command that names interlock's directory (`.interlock/`: the store, the controller lock, the hooks plugin), apart from attempt worktrees under it. This is a string check, not a sandbox: a path built at run time gets past it, as with the rest of the classifier.

## Commands and configuration

```bash
interlock integrate run <task> [--remote origin] [--base main] [--repo OWNER/REPO]
                               [--method merge|squash|rebase] [--auto-merge] [--wait 15m] [--poll 15s]
                               [--checks-settle 30s] [--call-timeout 120s]
interlock integrate operations <task>
interlock reconcile [<task>]
```

`integrate run` exits 0 when the task is done and 5 otherwise, like `interlock run`. Both commands, and the reconcile at the start of `interlock run`, take the controller lock `.interlock/supervisor.lock`.

`interlock run` reads the same settings from the environment. Inside an attempt, all of them are ignored.

| Variable | Default |
| --- | --- |
| `INTERLOCK_GH_BIN` | `gh` |
| `INTERLOCK_FORGE_REMOTE` | `origin` |
| `INTERLOCK_FORGE_BASE` | the remote's default branch (`git ls-remote --symref`) |
| `INTERLOCK_FORGE_REPO` | none: `gh` infers the repository from the remote |
| `INTERLOCK_FORGE_METHOD` | `merge` |
| `INTERLOCK_FORGE_AUTO_MERGE` | off |
| `INTERLOCK_FORGE_WAIT`, `INTERLOCK_FORGE_POLL` | `15m`, `15s` |
| `INTERLOCK_FORGE_CHECKS_SETTLE` | `30s` |
| `INTERLOCK_FORGE_CALL_TIMEOUT` | `120s`: one `gh` or `git` call that takes longer has an unknown outcome |

Output a call leaves open after it exits (a child holding the pipe) is cut off two seconds later.

In the supervisor, a verified task without integration still goes straight to done (G7), and one that needs integration goes through G5 and the path above. Before starting a verifier session for a task awaiting verification, the supervisor advances first. After a moved-head R2 the evidence is usually still current, so no session is spent.

## Tests

All tests are deterministic and need no network. The fake `gh` (`crates/interlock-forge/src/fake_gh.py`) is stateful and file-backed. Branches live in a real bare git repository, which interlock pushes to with plain `git`. The fake merges with real merge commits, and it emulates `gh`'s flags, JSON fields and error messages for `pr create`, `pr list`, `pr view` and `pr merge`. Faults are set per subcommand, alongside a few state knobs:

| Fault or knob | Effect |
| --- | --- |
| `move_head` | Someone pushes to the head branch while the merge request is in flight |
| `refuse` | Branch policy refuses the merge |
| `crash` | The fake merges, then SIGKILLs its caller (interlock) before it hears back |
| `die` | The fake merges, then fails without an answer |
| `vanish` | The fake merges, then stops answering anything |
| `queue`, `queue_then_drop` | The forge accepts the merge and completes it a few calls later; the second also drops the call's connection |
| `hang`, `orphan` | No answer for a while; or an answer with a child left holding the output open |
| `unreachable` | No effect, and an answer like a network error |
| `checks` | `pass`, `pending`, `fail` or `none` |
| `lag`, `base_oid`, `lie_merged` | A stale `headRefOid` for a few reads; a stale `baseRefOid`; a pull request reported merged that nothing merged |

| Requirement | Test |
| --- | --- |
| Invariant 7: a crash right after a merge call; restart finds the merged SHA | `forge_cli.rs`: the fake kills `interlock integrate run` mid-merge. `interlock reconcile` then finds the merged head and applies G6, and so does a restarted `interlock run`, with no session. `deliver_copilot.rs`: the same through a whole `interlock run` on Copilot CLI, and the second run makes no model call. `operations.rs` (store): a started row is still there after the store is closed and reopened, and reconcile settles it |
| P4: a moved head refuses the merge | `deliver.rs`, `forge_cli.rs`: the head moves during the merge, GitHub's pin refuses it, R2 applies and main never moves. A later push will not overwrite the unverified commit, and `interlock run` gets there with no verifier session |
| P4: a changed base invalidates evidence | `deliver.rs`, `forge_cli.rs`: the base moves after verification. The tree that would land is recorded, the old evidence goes stale, and R2 applies with no merge attempted. After re-verification, the new head lands on the new base. `regressions.rs`: the same when only git, not GitHub's API, shows the move |
| G5: without landing authority the task blocks at verified, with the reason | `deliver.rs`, `forge_cli.rs`, and `deliver_copilot.rs` through `interlock run`. No `gh` call is made and no ref is written |
| §8: the core checks its own forge actions | `deliver.rs`, `operations.rs`: a grant revoked after G5 stops the merge before the call. `regressions.rs`: a grant revoked while auto-merge is armed disarms it; a merge made after the grant ended is not G6 |
| Operator authority | `regressions.rs`: interlock opens the pull request and waits; the operator's merge lands the task through reconcile; an operator merge onto a moved base, or at another head, does not |
| Operation rows are written before each call | `deliver.rs`: the fake `gh` reads the store when it is called |
| §11 step 7, end to end | `deliver_copilot.rs`: a bug-fix task with `integration_required = true` and a coordinator grant with landing authority. It runs through `interlock run` on the real Copilot CLI (offline, scripted model), G1–G6. Main gains a merge of exactly the verified head, made with `--match-head-commit`. The user's branch and files are untouched |
| Reconcile rules | `core/tests/delivery.rs`: each forge answer; property tests that only the pinned head (full or abbreviated) ever lands, and that branch names are injective |
| The October 6 review's findings | `regressions.rs`, `forge_cli.rs`, `operations.rs`, `classify.rs`, `hook.rs`. `evidence/forge/mutations/` shows each covering test failing with its fix undone |

Two live runs on Claude Code with a real model delivered the export-retry walkthrough through G5 and G6 on the same fake `gh`: one before the review and one after it, with a coordinator grant. See `evidence/forge/README.md`.

## What a live run would add

The fake `gh` was written from `gh` 2.89's help, its JSON field list (checked with `gh pr view --json` in this container), and GitHub's documented behavior. A run against a real repository would establish:

- **GitHub's own messages and timing.** That `--match-head-commit` refuses a moved head with the message and exit code interlock expects, and that interlock's list of plain refusals matches what GitHub says. How long `headRefOid` lags a push, and whether five re-reads suffice. That `mergeStateStatus`, `mergedAt` and `baseRefOid` read as modeled.
- **Auto-merge and merge queues.** Whether `gh pr merge --auto` on a pull request that is already clean merges or errors; interlock avoids the question by merging directly when ready. Merge-queue repositories may not set `autoMergeRequest`. interlock then treats a merge request GitHub accepted, but has not finished, as in flight until the wait runs out, and reconcile retries it later.
- **Branch protection.** Required reviews (`BLOCKED`) and the "require branches to be up to date" rule. interlock does not read the protection rules (that needs `gh api` and, for classic protection, admin rights), so it cannot refuse to arm auto-merge where they are missing.
- **Credentials.** That `git push` and `git fetch` work with the user's credential helper and that `gh` resolves the repository from the remote. Neither is needed with a local bare remote.

## Limits

- **The merge pins the head, not the base.** `gh` has no `--match-base-commit`. Some window always remains:
  - **Direct merge.** interlock reads the base with `git ls-remote` immediately before `gh pr merge`, so the window is the time between that read and GitHub performing the merge, about one `gh` call.
  - **Armed auto-merge.** The window lasts as long as auto-merge stays armed, because GitHub merges whenever its requirements pass.

  In either case GitHub may merge the verified head onto a base that moved. interlock cannot stop that merge. It detects it afterwards, as above: the merge is recorded, G6 is withheld, and the task is blocked with "landed on a base nobody verified". The base branch then holds a merge no evidence covered, until the operator acts. GitHub's "require branches to be up to date" rule closes the window on GitHub's side. interlock does not check that the rule is set.
- **Only GitHub, and only branches in the same repository.** Forks are not supported.
- **Rebases are squashed.** When the base moves, the next verified head is one commit holding the merged tree, not a replay of the original commits.
- **The snapshot base should be on the base branch.** If the snapshot base is not the tip of the base branch (for example, unpushed local commits), interlock treats that as a moved base. What lands is the merge of those commits and the change.
- **One controller per checkout.** `integrate run`, `reconcile` and `run` share a lock file. It is not a distributed lock.
- **A failing CI check blocks the task.** It is not sent back for rework: the core has no move from integrating to ready.
- **The classifier and the store-directory check read the command text.** They do not parse shell grammar, so containment still rests on the host's tool restrictions and the operating system.
