# Testing interlock yourself

The build is tested: 350 automated tests pass, and live runs on Claude Code are recorded under `evidence/`. What is left needs things only you have: a GitHub token, a Copilot plan, a real repository for the baseline, and a network share. This kit runs each of those tests with one command and keeps what it saw, so the result can be checked afterwards.

Every command is `testkit/run.sh <test>`. It writes its evidence to `testkit-results/<test>-<time>/` and ends with one line:

- `RESULT: PASS` means the test did what the design says.
- `RESULT: FAIL` means it did not, and the line says where to look.
- `RESULT: NOT RUN` means something it needs is missing, and the line says what.

When you are done, `testkit/run.sh pack` zips every result for you to send back (step 10).

## What is left, and what each test proves

| Step | Test | What it proves | You need | Cost |
| --- | --- | --- | --- | --- |
| 2 | `suite` | The 350 tests pass on your machine, not only in the build container | Docker | none |
| 4 | `github` | Delivery on real GitHub: a pull request, the merge pinned to the verified commit, and the base branch holding exactly the verified files | A GitHub token and an empty test repository | none |
| 5 | `copilot` | A whole task, worker and independent verifier, on Copilot CLI with its real model | A Copilot plan | about 3 premium requests |
| 6 | `guided` | A person drives Copilot, the skills guide it, and interlock holds it to evidence | A Copilot plan, about 10 minutes at the keyboard | a few premium requests |
| 7 | `eval` | Plain Copilot against Copilot with the skills against Copilot with skills and interlock, judged by hidden checks | A Copilot plan | about 20 to 30 premium requests |
| 8 | `eval` on your own tasks | Whether interlock improves outcomes on tasks nobody tuned it for: the real S3 baseline | A repository with 10 to 15 closed issues | an afternoon, and more premium requests |
| 9 | `netfs` | The store refuses a real network filesystem | An NFS, SMB or sshfs mount on a Linux machine | none |

GitHub counts Copilot CLI use in premium requests, about one per prompt or headless session, times the model's rate. Your usage is at <https://github.com/settings/copilot>.

The design's spike S2 (a Rust SDK for Copilot) is not a test. The CLI adapter was kept for v1; the README says why.

## Step 1: a Linux machine

interlock runs on Linux only: it reads `/proc` to know which processes belong to an agent's session. On a Mac or Windows machine, use Docker, which gives you the same Linux environment the build was tested in.

**With Docker** (any computer). Install Docker Desktop and give it at least 4 CPUs, 8 GB of memory and 20 GB of disk (Settings, Resources); compiling Rust needs the memory. Then, in a terminal:

```bash
unzip interlock-test-kit.zip
cd interlock
docker build -t interlock-testkit -f testkit/Dockerfile .       # 10 to 20 minutes, about 4 GB
docker run -it --name interlock-testkit interlock-testkit
```

You are now inside the container, in `/home/tester/interlock`, with Rust, Python, Go, Node, Copilot CLI 1.0.91, Claude Code and the GitHub CLI installed and interlock built. Every command below runs in there.

- To leave: `exit`. To come back to the same container, with your results: `docker start -ai interlock-testkit`.
- Environment variables you `export` (step 3) are lost when you leave. Export them again when you come back.
- The image was built and tested on an x86_64 machine. On an Apple Silicon Mac, Docker builds the arm64 version with the same steps; that has not been tried.

**Without Docker** (a Linux machine, such as Ubuntu 24.04): install Rust 1.89 or later (<https://rustup.rs>), Python 3.11 or later, git, Go 1.22 or later, Node 22 or later, and then:

```bash
npm install -g @github/copilot@1.0.91 @anthropic-ai/claude-code
# the GitHub CLI: https://cli.github.com
cd interlock
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

Either way, check what is ready:

```bash
testkit/run.sh doctor
```

## Step 2: the whole suite

```bash
testkit/run.sh suite
```

This builds interlock and runs every test with both hosts required, so no test can pass by skipping. It needs no account and makes no model calls: Copilot and Claude Code run offline against scripted models. It takes 10 to 30 minutes.

Expected: `RESULT: PASS: 350 passed, 0 failed, 2 ignored ...`. The two ignored tests are the opt-in live ones (step 4 runs one of them).

## Step 3: a test repository and a token

For steps 4 to 8.

1. **Make a test repository.** At <https://github.com/new>: name it `interlock-sandbox`, make it private, and tick **Add a README file** (the test needs a repository with at least one commit). Use nothing else for this: the delivery test opens and merges pull requests in it.
2. **Make a fine-grained token.** At <https://github.com/settings/personal-access-tokens/new>:
   - Expiration: 7 days.
   - Repository access: **Only select repositories**, then `interlock-sandbox`.
   - Repository permissions: **Contents**: Read and write; **Pull requests**: Read and write. (Metadata: Read is added for you.)
   - Account permissions: **Copilot Requests**: Read, if it is offered. Without it, sign Copilot in separately (below).
3. **Give it to the container, without it showing on screen or in your history.** Inside the container:

   ```bash
   read -rs GH_TOKEN && export GH_TOKEN          # paste the token, press Enter; nothing is shown
   export INTERLOCK_LIVE_GITHUB_REPO=<your-github-name>/interlock-sandbox
   testkit/run.sh doctor                         # "gh signed in" and "test repository" should now say ok
   ```

Never paste the token into a chat or save it in a file in this kit. `run.sh` writes only the names of the variables you set into its results, never their values, and `pack` removes any value that slipped into a file.

**Signing Copilot in.** Copilot CLI uses a token in `COPILOT_GITHUB_TOKEN`, `GH_TOKEN` or `GITHUB_TOKEN`, in that order, before any login. If your token has the Copilot Requests permission, there is nothing more to do. Otherwise run `copilot login` (it shows a code to enter at github.com), and run the Copilot tests with the token hidden from it, for example `env -u GH_TOKEN testkit/run.sh copilot`.

The token reaches the Copilot sessions interlock starts, and an agent can run shell commands there. That is why it should reach only the test repository.

## Step 4: delivery on real GitHub

```bash
testkit/run.sh github
```

What happens in `interlock-sandbox`: the test pushes a branch `interlock-live-<time>` holding a small bug, verifies a fix without any model, and runs `interlock integrate run`. That pushes `interlock/live-<time>`, opens a pull request into the first branch, waits until GitHub says it can merge, and merges it pinned to the verified commit (`gh pr merge --match-head-commit`). The test then checks that the branch holds exactly the verified files, and deletes both branches. The merged pull request stays in the repository's history.

Expected: `RESULT: PASS: delivered to ... through G5 and G6 (pull request #1 MERGED)`. If your repository allows only squash or rebase merges, run `INTERLOCK_FORGE_METHOD=squash testkit/run.sh github`.

## Step 5: Copilot with its real model

```bash
testkit/run.sh copilot                  # or: testkit/run.sh copilot --model <a model your plan offers>
```

This makes a fresh repository from `examples/export-retry` (a bug that writes rows twice after a retry), checks with one small request that Copilot is signed in, and runs `interlock run export-retry --host copilot`. interlock first runs the checks on the original code (the reproduction must fail there), then starts a Copilot worker session to fix the bug, then a separate Copilot verifier session that runs the checks again on exactly the worker's files.

Expected: `RESULT: PASS: export-retry is done on copilot's real model, moves G1 G2 G3 G4 G7`. The fix is in `verified-output.diff`, and every check interlock ran is in the store it keeps.

## Step 6: a guided session that uses the skills

```bash
testkit/run.sh guided
```

This prepares a repository with the same bug and installs interlock's skills and hooks for Copilot (`interlock setup --host copilot`), then prints what to do. In short:

1. Start Copilot in that repository with the command it prints.
2. Ask, in your own words, for the bug to be fixed, without naming a skill. The printed request is a good one.
3. Approve what Copilot asks to run. Note whether it uses the interlock skills (it invokes a skill and runs `interlock` commands) or just edits the file.
4. If it did not use them, type `/interlock-route Fix task export-retry, described in task.toml`.
5. When it says the task is done, or stops, leave with `/exit` and run `testkit/run.sh guided-collect`.

Expected: `RESULT: PASS: export-retry is done in a guided Copilot session, moves G1 G2 G3 G4 G7; skills invoked: ...; attempts bound as verifier: interlock_launched, worker: session`.

Whether Copilot reached for a skill on its own in point 2 is the open question here; no live model has yet. Either answer is a result. Tell me which happened.

## Step 7: the evaluation on Copilot

```bash
testkit/run.sh eval                     # 4 tasks x 3 conditions, 1 repeat: 12 runs
testkit/run.sh eval --repeats 2         # 24 runs, in ABBA order
testkit/run.sh eval --all-tasks         # all 11 stand-in tasks: 33 runs
testkit/run.sh eval --help
```

Each task is run three ways: plain Copilot (`plain`), Copilot with interlock's skills but no runtime (`skills`), and `interlock run` with the skills (`interlock`). Each output is judged by hidden checks the agents never see. The default tasks are the four interlock did worst on before its last prompt change.

The result line gives the accepted runs for each condition and how many runs invoked a skill. These are findings, not a pass or fail: `PASS` here only means the evaluation ran. `results/report.md` has every metric; `export/` is the same without the output trees.

With a dozen runs per condition, no difference between conditions will be statistically significant. It is the first evidence on Copilot's real model, not a verdict.

## Step 8: the real baseline, on your own repository

The stand-in tasks were written for this harness, and interlock's last prompt change was made after seeing them, so they cannot show whether interlock improves outcomes. The design's spike S3 asks for 10 to 15 closed issues from a real repository, each with a known fix and tests.

1. Pick a repository you know, and 10 to 15 closed issues whose fix commit added or changed tests.
2. For each, make `eval/taskset/tasks/<id>/` as [docs/evaluation.md](docs/evaluation.md#swapping-in-the-real-s3-set) describes: the issue text as the prompt, the fix commit's parent as the start, the tests the fix added as the hidden checks.
3. `python3 eval/harness.py build --update-lock --state-dir testkit-results/eval-state`, then `testkit/run.sh eval --tasks <your ids> --repeats 2`.

If you tell me which repository and issues, I can write the task directories for you.

For a final run, use a fresh container for it: agents run as the same user as the harness, so a determined agent could read the hidden checks. docs/evaluation.md, "Isolation", says what the harness does about that and what it cannot do.

## Step 9: a real network filesystem

interlock refuses to keep its store on a network filesystem, where two machines could both believe they hold its lock. That check has only run against local mounts dressed up as network ones. On a Linux machine with a real NFS, SMB or sshfs mount (not inside Docker Desktop, which hands mounts to the container as a local filesystem):

```bash
testkit/run.sh netfs /path/on/the/mount
```

Expected: `RESULT: PASS: interlock refused a store on ... (nfs)`. It also records that `INTERLOCK_ALLOW_NETWORK_FS=1` lets you override the refusal.

## Step 10: send the results back

```bash
testkit/run.sh pack
```

It writes `testkit-results-<time>.zip` in the interlock directory, after removing the value of any credential in your environment from every file. From outside the container, copy it to your computer:

```bash
docker cp interlock-testkit:/home/tester/interlock/testkit-results-<time>.zip .
```

It holds the transcripts of the agent sessions, interlock's records, and the versions of every tool. Send it to me, or just the `RESULT:` lines, and I will check the evidence and update the README's "What is not proven".

## When something goes wrong

| What you see | What to do |
| --- | --- |
| `Copilot could not sign in` | A token in `GH_TOKEN` without the Copilot Requests permission takes precedence over `copilot login`. Run `env -u GH_TOKEN testkit/run.sh copilot`, or add the permission to the token |
| Copilot says its version is no longer supported | Rebuild with `docker build --build-arg COPILOT_VERSION=latest ...`. The results record which version ran |
| `this token cannot read <repo>` | The token's repository access does not include the test repository |
| `has no commits yet` | The test repository was made without a README. Add any file to it on github.com |
| The Docker build stops while compiling | Give Docker more memory (8 GB) |
| A network that inspects HTTPS | Build a base image that trusts your network's certificate and pass it with `--build-arg BASE=<that image>` |
| Anything else | Run `testkit/run.sh pack` and send the zip: the `.err` and `.txt` files in each result say what happened |

## What I checked before handing this over

So far, in a fresh container built from `testkit/Dockerfile`: the image builds with every tool at its pinned version, `doctor` reports them, and `github` refuses to start without a test repository, with this project's repository, or without a working token, leaving no token value in its results. `testkit/run.sh` passes ShellCheck. The rest of the kit is being checked now, and this section will say what was checked when the zip is made. What I cannot check is what needs your account: GitHub delivery, and Copilot with its real model.
