# Evidence: skills, the generator, S1, and guided sessions

October 5 and 6, 2026. Copilot CLI 1.0.91 and Claude Code 2.1.289, in the development container. What was built and what it means is in [docs/skills.md](../../docs/skills.md); this page lists what was run. S1 and row 22 ran on commit 4f7e74b; rows 23 and 24 on the integrated build, interlock-foundation at 858879e; everything else on commit d266645, after the review fixes (section "Review fixes").

Conventions in the commands below:

- `$COPILOT` is the pinned Copilot CLI run offline with a private home: `COPILOT_HOME=<empty dir> COPILOT_OFFLINE=true COPILOT_MODEL=gpt-4.1 COPILOT_AUTO_UPDATE=false NO_PROXY=127.0.0.1 copilot`. Where a model was needed, `COPILOT_PROVIDER_BASE_URL=http://127.0.0.1:<port>/v1` pointed it at a scripted model: [scripts/spike_model.py](scripts/spike_model.py) for S1 (a script per conversation, any tool), `crates/interlock-cli/tests/guided_model` for the tests.
- `claude` is Claude Code. Live runs used `--model sonnet` unless the row says otherwise. "Dead endpoint" means `ANTHROPIC_BASE_URL=http://127.0.0.1:9 CLAUDE_CODE_MAX_RETRIES=0`: the session starts, reports what it loaded in its init event, and its first model request fails, so no model call is made.
- `interlock` is this branch's binary. `$SPIKE` is the scratch directory the S1 probes ran in.
- Host tests ran with `INTERLOCK_REQUIRE_HOSTS=1`, which turns a missing host binary into a failure instead of a skip, so no result below passed by skipping.

Transcripts are trimmed by [scripts/trim.py](scripts/trim.py): strings over 2,000 to 3,000 characters are cut, streaming fragments and per-call bookkeeping events are dropped (`*_delta`, `model.call_*`, `assistant.turn_start`, `tool.execution_partial_result`, `session.background_tasks_changed`, `stream_event`), Claude Code's init event keeps only its skills, agents, plugins and slash commands, and attempt tokens and e-mail addresses are redacted. [scripts/scan.py](scripts/scan.py) compared every 64-hex string in this folder against the token hashes of every store the runs used (17 hashes) and found no raw token, credential or address. Every file is under 200 KB.

## S1: skill loading and `disable-model-invocation`

Probe skills ([s1/probes](s1/probes)): `s1-model` (plain), `s1-user` (`disable-model-invocation: true`), `s1-hidden` (`user-invocable: false`), `s1-router` with `references/routed.md`, and a custom agent `s1-agent` (tools `read`, `search`, `execute`). The same skills for Claude Code in `.claude/skills/` and in a plugin `s1probe`. Written by [scripts/make_s1.py](scripts/make_s1.py) and [scripts/make_s1_claude.py](scripts/make_s1_claude.py).

| # | What | Command | Outcome | Raw output |
| --- | --- | --- | --- | --- |
| 1 | Copilot lists the skills | `$COPILOT skill list --json` in the probe repository | All four listed, `enabled: true`, including `s1-user` | [s1/copilot-skill-list.json](s1/copilot-skill-list.json) |
| 2 | Copilot's agent invokes each through the `skill` tool | `$COPILOT -p "S1-PROBE-TOOL run the probe" --output-format json --allow-all-tools --no-ask-user`, script [s1/s1-script-tool.json](s1/s1-script-tool.json) | `s1-model`, `s1-hidden`, `s1-router` "loaded successfully"; `s1-user` fails `Skill not found: s1-user`, the same error as `no-such-skill`; the model is told "Available skills: s1-hidden, s1-model, s1-router, ..."; the router's context lists its references under "Related files" and `view` reads `routed.md` | [s1/copilot-skill-tool.jsonl](s1/copilot-skill-tool.jsonl), what the model saw: [s1/copilot-skill-tool-model.jsonl](s1/copilot-skill-tool-model.jsonl) |
| 3 | Slash commands in prompt mode | `$COPILOT -p "/s1-user S1-PROBE slash user" ...`, then `"/s1-model ..."` | Neither is expanded: the model receives the literal text. `s1-user` is absent from the model's `<available_skills>` | [s1/copilot-slash-prompt-mode-model.jsonl](s1/copilot-slash-prompt-mode-model.jsonl) |
| 4 | A person types the flagged skill in the interactive session | [scripts/pty_drive.py](scripts/pty_drive.py) runs `$COPILOT --allow-all-tools -i "/s1-user S1-PROBE tui user"` in a pseudo-terminal for 30 s, pressing Enter at 6 s to trust the folder | The body is injected: "The user explicitly invoked the "/s1-user" skill. Follow its instructions now." with `MARKER-USER` and `ARGUMENTS: S1-PROBE tui user` | screen: [s1/copilot-tui-slash-user-screen.txt](s1/copilot-tui-slash-user-screen.txt); model: [s1/copilot-tui-slash-model.jsonl](s1/copilot-tui-slash-model.jsonl) |
| 5 | A person types the `user-invocable: false` skill | as 4, with `/s1-hidden` | "Unknown command: /s1-hidden"; no model request | [s1/copilot-tui-slash-hidden-screen.txt](s1/copilot-tui-slash-hidden-screen.txt) |
| 6 | Custom agent profile | `$COPILOT --agent s1-agent -p "S1-PROBE agent flag" ...` | The profile body is the system prompt; tools offered: `bash`, `list_bash`, `read_bash`, `stop_bash`, `view`, `skill`, `sql` (no edit tools). In row 2's session the `task` tool lists `s1-agent` as an `agent_type` | [s1/copilot-agent-flag-model.jsonl](s1/copilot-agent-flag-model.jsonl) |
| 7 | Claude Code loads the project layout | `claude -p` (dead endpoint) in the probe repository | `s1-model`, `s1-router`, `s1-user` in `skills` and `slash_commands`; `s1-agent` in `agents`. Built-in `verify` and `design` skills are also listed | [s1/claude-init-project-layout.jsonl](s1/claude-init-project-layout.jsonl) |
| 8 | Claude Code loads the plugin layout | `claude -p --setting-sources "" --plugin-dir s1plugin` (dead endpoint) | `s1probe:s1-model`, `s1probe:s1-router`, `s1probe:s1-user`; agent `s1probe:s1-agent` | [s1/claude-init-plugin-layout.jsonl](s1/claude-init-plugin-layout.jsonl) |
| 9 | Claude Code's agent invokes each through the Skill tool | `claude -p --setting-sources "" --plugin-dir s1plugin --model haiku --max-turns 10 --allowedTools Skill Read`, prompt: invoke each skill, then read `routed.md`; live, $0.036 | `s1-model` and `s1-router` launch; `s1-user`: "cannot be used with Skill tool due to disable-model-invocation. Ask the user to run /s1probe:s1-user themselves"; `routed.md` read | [s1/claude-skill-tool-live.jsonl](s1/claude-skill-tool-live.jsonl) |
| 10 | A person types the flagged skill, prompt mode | `echo "/s1probe:s1-user probe argument" \| claude -p ... --model haiku --max-turns 2`; live, $0.016 | Expanded: the session saw `MARKER-USER` with the arguments | [s1/claude-slash-live.jsonl](s1/claude-slash-live.jsonl) |


## Generated skills load, reach the model, and validate

| # | What | Command | Outcome | Raw output |
| --- | --- | --- | --- | --- |
| 11 | Generate all three targets | `interlock skills generate --target copilot\|claude-code\|agent-skills --out <dir>` | 30, 32 and 31 files; no problems, nothing refused. Copilot's skills are `interlock-<name>` and it gets no agent file | [validate/generate.json](validate/generate.json) |
| 12 | Static validation | `interlock skills validate <dir> --target <target>` on each output (the Copilot repository root, the Claude Code plugin root with its agent file, the Agent Skills folder) | `valid: true`, no problems, exit 0, each | [validate/](validate/) |
| 13 | Every generated skill loads on Copilot | `$COPILOT skill list --json` in the generated Copilot tree | All six `interlock-*`, `source: project`, `enabled: true` | [load/copilot-skill-list-generated.json](load/copilot-skill-list-generated.json) |
| 14 | Every model-invoked skill reaches Copilot's model through the `skill` tool | test `every_model_invoked_skill_reaches_copilots_model_through_the_skill_tool`: `$COPILOT -p "SKILL-TOOL-PROBE ..." --output-format json --allow-all-tools --no-ask-user`, the scripted model calling `skill` for each skill | route, investigate, implement, review and verify "loaded successfully", and the first line of each body was in what the model received next; `interlock-design` failed with `Skill not found` | [load/copilot-skill-tool/](load/copilot-skill-tool/) |
| 15 | A person types the design skill in Copilot's interactive session | test `a_person_can_type_the_design_skill_in_copilots_interactive_session`: [crates/interlock-cli/tests/assets/pty_drive.py](../../crates/interlock-cli/tests/assets/pty_drive.py) runs `$COPILOT --allow-all-tools -i "/interlock-design fix-add TUI-DESIGN-PROBE"` in a pseudo-terminal, Enter at 6 s for the folder-trust prompt | The model received the design skill's body with the typed arguments | [load/copilot-tui-design/](load/copilot-tui-design/): `model-requests.jsonl`, `screen.txt` |
| 16 | Every generated skill and the verifier load on Claude Code | `claude -p --setting-sources "" --plugin-dir <generated plugin>` (dead endpoint) | `interlock:design` ... `interlock:verify` in `skills` and in `slash_commands` (so `/interlock:design` and `/interlock:review` can be typed), agent `interlock:verifier`, plugin `interlock`, next to the built-in `design` and `verify` | [load/claude-init-generated-plugin.jsonl](load/claude-init-generated-plugin.jsonl) |

Rows 12 to 16 are also tests: `crates/interlock-skillgen/tests/generate.rs` and `crates/interlock-cli/tests/skills_load.rs`.

## P1 gate: guided sessions end to end

### Copilot CLI, scripted model

[scripts/repeat_guided_copilot.sh](scripts/repeat_guided_copilot.sh) `<copilot> <dir> 3` runs `cargo test -p interlock-cli --test guided_copilot` three times with `INTERLOCK_REQUIRE_HOSTS=1` and `INTERLOCK_EVIDENCE_DIR` set. Each test makes a sample repository, runs `interlock setup --host copilot`, and runs one `$COPILOT -p "<request>" --output-format json --allow-all-tools --no-ask-user --plugin-dir .interlock/guided/copilot-plugin` session. The scripted model stands in for the person and the agent: it invokes the skills with the `skill` tool and runs the commands they prescribe, filling in the attempt id, token, epoch and worktree from `attempt start`'s JSON. Verification is `interlock verify <id> --host copilot`, which launches the verifier as a separate headless Copilot session; the same scripted model plays it under a second script. Copilot's bash tool does not pass `COPILOT_OFFLINE` or `COPILOT_PROVIDER_BASE_URL` to the commands it runs, so the scripted command sets them on `interlock verify`. The files below are from the third run.

| # | Run | Outcome | Raw output |
| --- | --- | --- | --- |
| 17 | Bug fix: `add(2, 3)` returns -1 | `done`: G1, G2, G3, G4, G7. The task was created from stdin. An edit to the repository's `calc.py` denied ("outside this attempt's worktree"); the edit in the worktree went through. `curl -X POST` got the hook's ask, which `copilot -p` reported as "Denied by preToolUse hook (unable to ask user for confirmation): external_reversible is not granted for this attempt". After submitting, the session's own `attempt start --role verifier` was denied: "this session did the work, so it cannot verify it". The launched verifier's attempt is `interlock_launched`; its first call, an edit, was denied ("edit is not in this attempt's tool policy"); it ran both checks and assessed. Worktrees gone at done | [p1/copilot-bug-fix/](p1/copilot-bug-fix/): `copilot-transcript.jsonl`, `model-requests.jsonl`, `launched-<attempt>.jsonl` (the verifier session), `task-log.json`, `status.json`, `attempts.json`, `notes.json` |
| 18 | Investigation: how many tries does `export()` make | `done`: G1, G2, G3, G4, G7. The worker's demo printed `tries: 3`; the answer was kept as a note. The session's own verifier attempt was denied. A `task` subagent (`agent_type: "task"`) then opened a verifier attempt, recorded `unbound`, and passed `answer`; `interlock advance` left the task `awaiting_verification`: "1 assessment(s) from an unbound verifier do not count for a criterion without a check". `interlock verify` launched the verifier, which reran the demo and passed it; `done` | [p1/copilot-investigation/](p1/copilot-investigation/) |
| 19 | Both, three runs in a row | 2 passed, 0 failed, each run | [review-fixes/guided-copilot-3-runs.txt](review-fixes/guided-copilot-3-runs.txt) |

### Claude Code, live

[scripts/init_samples.py](scripts/init_samples.py) makes two sample repositories from [scripts/make_samples.py](scripts/make_samples.py), each a git repository (the investigation one with a two-commit history). In each, `interlock setup --host claude-code`, then one session standing in for the person, [scripts/cc_guided.sh](scripts/cc_guided.sh):

```bash
printf '%s' "$REQUEST" | claude -p --model sonnet --output-format stream-json --verbose --no-session-persistence \
  --setting-sources "" --plugin-dir .interlock/guided/claude-code-plugin \
  --permission-mode acceptEdits --max-turns 90 --allowedTools Bash Read Edit Write Grep Glob Skill Agent
```

with `interlock` on PATH. The request names the route skill, as a person would type it; everything after that (implement or investigate, verify, the hand-off to the verifier subagent, every interlock command) the model chose from the skills. Records were dumped from the store afterwards with [scripts/dump_run.py](scripts/dump_run.py).

| # | Run | Outcome | Raw output |
| --- | --- | --- | --- |
| 20 | Bug fix. Request: `/interlock:route parse_duration("1h30m") in durations.py returns 1800 seconds instead of 5400. Please fix it.` | `done` in 48 s, 15 turns, $0.26, no denials. G1, G2, G3, G4, G7. Task `duration-sum`: `repro` (independent, observed, baseline fails) and `regression` (self, tested, baseline passes), scope `durations.py`, `tests/**`. Check runs: repro exited 1 on the input snapshot and 0 on the output; regression 0 and 0; the verifier ran both again on the output. The verifier attempt (`agent: interlock:verifier`) is bound `subagent` with the subagent's agent id; both assessments are `bound_via: subagent`. The model first read the route skill's task templates under `.interlock/guided/`, which the state guard allows. 16 of 16 tool calls went through the PreToolUse hook; `Stop` and `SubagentStop` ran. The final reply names `refs/interlock/tasks/duration-sum` and `git cherry-pick` and leaves applying it to the person | [p1/claude-code-bug-fix/](p1/claude-code-bug-fix/): `claude-transcript.jsonl`, `task-log.json`, `status.json`, `task.json`, `attempts.json`, `check_runs.json`, `claims.json`, `assessments.json`, `results.json`, `notes.json`, `output.diff` |
| 21 | Investigation. Request: `/interlock:route Can TTLCache.get in cache.py still return a value exactly ttl seconds after it was stored? Where is that decided, and why is it that way?` | `done` in 53 s, 14 turns, $0.26, no denials. G1, G2, G3, G4, G7. Answer: yes; `cache.py:20` compares with a strict `>`; the second commit changed `>=` to `>` for coarse clocks, quoted from its message. Scope empty; the result changed nothing. The answer is kept as a note. The verifier subagent, bound `subagent`, read `cache.py` and its history, ran its own fake-clock check (at 10 the value, at 10.001 a miss), read the commit, and passed `answer`. 16 of 16 tool calls through the PreToolUse hook | [p1/claude-code-investigation/](p1/claude-code-investigation/) |
| 22 | The guided hooks on Claude Code, live, on commit 4f7e74b. A guided worker attempt opened with `interlock attempt start hook-probe --role worker --host claude-code --worktree auto`; `claude -p ... --model haiku --permission-mode acceptEdits --allowedTools Bash Read Edit` asked to edit `durations.py` in the repository, then the same file in the worktree, then run `curl -s -X POST http://127.0.0.1:9/notify`. $0.03 | The first edit: hook exit 2, "outside this attempt's worktree". The second: allowed. The curl: `permissionDecision: ask`, which `claude -p` listed in `permission_denials` with the hook's reason | [p1/claude-code-hook-probe/](p1/claude-code-hook-probe/) |

Both live runs report the outer session's id as `host_session`: in this container a nested `claude` inherits its parent's session id. The binding still separates the main agent from the subagent by the subagent's agent id.

Live spend after the review fixes, all `--model sonnet`: $1.19 over four runs: rows 20 and 21, and the two bug-fix runs under "Review fixes" below.

### Claude Code, live, on the integrated build

The same two gates on interlock-foundation at 858879e, after integration (where the runtime branch's hook rule against paths under `.interlock/` applies to headless sessions only, and guided sessions rely on the state guard). Fresh samples from [scripts/init_samples.py](scripts/init_samples.py), `interlock setup --host claude-code` in each, then [scripts/cc_guided.sh](scripts/cc_guided.sh) with the same requests as rows 20 and 21, `--model sonnet`; records dumped with [scripts/dump_run.py](scripts/dump_run.py). Hook coverage counts the transcript's `hook_started`/`hook_response` events against its tool calls.

| # | Run | Outcome | Signals | Verifier binding | Hook coverage | Denials | Spend | Raw output |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 23 | Bug fix (request as row 20) | `done` in 45 s, 16 turns. Task `duration-sum`: `repro` failed on the input snapshot (exit 1) and passed on the output (exit 0) in interlock's runs, `regression` passed on both; the verifier reran both on the output. Output kept at `refs/interlock/tasks/duration-sum`, branch `main` unmoved, no worktrees left; the reply leaves `git cherry-pick` to the person | G1, G2, G3, G4, G7 | worker `session`; verifier `subagent` (`agent_type: interlock:verifier`, its own agent id); both assessments `bound_via: subagent` | PreToolUse on 17 of 17 tool calls (4 by the subagent); `SubagentStop` and `Stop` once each | 0 permission denials, 0 tool errors | $0.26 | [integration/claude-code-bug-fix/](integration/claude-code-bug-fix/) |
| 24 | Investigation (request as row 21) | `done` in 55 s, 13 turns. Answer: yes; `cache.py:20` uses a strict `>`; the second commit changed `>=` to `>` for coarse clocks, quoted from its message; a fake-clock run showed the value at age 10 and a miss at 10.001, which the verifier reran. Scope empty, nothing changed; the answer kept as a note | G1, G2, G3, G4, G7 | worker `session`; verifier `subagent`; the assessment `bound_via: subagent` | PreToolUse on 15 of 15 tool calls (5 by the subagent); `SubagentStop` and `Stop` once each | 0, 0 | $0.26 | [integration/claude-code-investigation/](integration/claude-code-investigation/) |

Integration spend: $0.53 of the $1.00 cap ($0.2618 and $0.2630).

## Review fixes

The skills review found one blocker, six majors and nine minors. Each confirmed item became a test; [review-fixes/regression-tests-on-4f7e74b.txt](review-fixes/regression-tests-on-4f7e74b.txt) is the final `crates/interlock-cli/tests/guided_cli.rs` and `cli.rs` run against the reviewed commit 4f7e74b: 15 of 24 fail there (14 of 15 in `guided_cli.rs`, and the foundation's bug-fix test, which now needs a real tree), and all pass on this branch. The unit tests in `interlock-supervisor` (`guided::tests`, `state_paths::tests`) and `interlock-skillgen` (`safe_write::tests`) test functions the reviewed commit does not have.

| Item | Covering tests |
| --- | --- |
| 1 G3 fails open | `result_submit_reads_the_change_from_the_tree_and_fails_closed`, `guided::tests::changes_are_read_from_a_real_tree_or_not_at_all`, `cli.rs` (real trees) |
| 2 Hooks fail open; state off limits | `guided_hooks_keep_every_caller_out_of_interlocks_state`, `state_paths::tests` (3) |
| 3 Empty scope; investigations change nothing | `an_empty_scope_allows_no_change`, `an_investigation_changes_nothing_at_all`, core `checks.rs` |
| 4 Verifier independence | `the_worker_cannot_verify_its_own_work_and_only_a_bound_verifier_counts`, `guided::tests` (binding, may-open, credentials, claiming), core `checks.rs` (unbound assessments), rows 17 and 18 on Copilot, rows 20 and 21 on Claude Code |
| 5 Setup clobbers files | `setup_installs_prefixed_skills_and_never_overwrites_what_is_not_its_own`, `setup_never_writes_through_a_symlink_or_outside_the_repository`, `safe_write::tests` (2) |
| 6 Attempt bound to its session | `a_guided_attempt_governs_only_the_session_that_opened_it_until_submitted`, `guided::tests::an_attempt_governs_only_its_own_session_until_ended`, `a_submitted_attempt_governs_nothing` |
| 7 Skills reach the model | rows 14, 15, 16 |
| 8 Designs and other notes | `notes_and_tree_auto_follow_the_attempt_not_the_directory`, row 18 |
| 9 `--tree auto` | `notes_and_tree_auto_follow_the_attempt_not_the_directory` |
| 10 Stop guard messages | `stop_guard_messages_are_complete_and_ask_each_role_for_its_own_record`, `hook::tests` |
| 11 `INTERLOCK_REQUIRE_HOSTS` | `crates/interlock-cli/tests/hosts/mod.rs`, used by every host test; the suite below |
| 12 Probe report without capabilities | `setup_refuses_to_save_a_host_report_without_capabilities` |
| 13 Terminal at done; worktrees removed | `cancel_closes_the_attempts_and_removes_their_worktrees`, the end of `the_worker_cannot_verify_...` (and the output ref) |
| 14 Checks committed only after asking; review's independence | skill text (`skills/route`, `skills/review`) |
| 15 Probe robustness | `probe::tests::a_background_process_holding_the_pipes_cannot_hold_the_probe`; root cause not reproduced (docs/skills.md, Limits) |
| 16 Nits | `skills_validate_checks_skills_and_agent_files`, `the_store_is_found_from_a_worktree_and_never_made_inside_one`, `a_task_id_cannot_climb_out_of_the_store` (passes on 4f7e74b too: the schema already refuses such ids), `guided::tests::task_ids_cannot_climb_out_of_the_worktrees_folder` |

Two live bug-fix runs came before row 20:

| Run | Outcome | Raw output |
| --- | --- | --- |
| First try, on commit 112d460 | Stopped at `awaiting_verification` after 27 turns, $0.40, 5 denials. Claude Code keeps the directory a `cd` leaves the session in, and the state guard of that commit resolved every word of a command against it: once the worker had worked in its worktree, `cd`, `interlock status` and the verifier subagent's `attempt start` were all refused. It also refused reading the route skill's own reference under `.interlock/guided/`. The model said so and did not work around it. Fixed in abd54fe: the guard judges what a command would change | [review-fixes/claude-code-first-try/](review-fixes/claude-code-first-try/) |
| Second try, on abd54fe without the output ref | `done` in 50 s, 16 turns, $0.26, no denials, verifier bound `subagent`. Its reply pointed at the attempt's worktree, which done had already removed; that led to the output ref, and row 20 reran the run with it | [review-fixes/claude-code-second-try/](review-fixes/claude-code-second-try/) |

## Test suite

`INTERLOCK_REQUIRE_HOSTS=1 INTERLOCK_COPILOT_BIN=<copilot> cargo test --workspace`, with `claude` on PATH: 162 passed, 0 failed, none skipped. Per-binary results: [tests.txt](tests.txt).

The tests added by this work:

| Test | Covers |
| --- | --- |
| `interlock-skillgen` unit tests (`frontmatter`, `validate`, `safe_write`) | Frontmatter round trip and the YAML subset; name, description, field, reference and template checks; writes that never clobber, follow symlinks or leave the root |
| `interlock-skillgen/tests/generate.rs` | The embedded catalog matches the source; the v1 set and intents; every skill drives interlock; every target validates; each target's layout, names and frontmatter; Agent Skills output validates on disk; loader refusals |
| `interlock-supervisor` `guided::tests`, `state_paths::tests` | Bindings, who may verify, whose credentials, which attempt governs, intents, worktree names, change sets from trees; what is interlock's state and which commands would change it |
| `interlock-cli/tests/cli.rs` | The foundation's end-to-end tests, now on real git trees |
| `interlock-cli/tests/guided_cli.rs` | Setup and its manifest; validation of skills and agent files; guided hooks as a host calls them, with session and agent ids; G3; scope; notes; `--tree auto`; the stop guard; settling at done and cancel; store discovery |
| `interlock-cli/tests/skills_load.rs` | Rows 13 to 16 |
| `interlock-cli/tests/guided_copilot.rs` | Rows 17 and 18 |
