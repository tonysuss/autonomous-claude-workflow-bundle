# Evidence: skills, the generator, S1, and guided sessions

October 5 and 6, 2026. Copilot CLI 1.0.91 and Claude Code 2.1.289, in the development container. What was built and what it means is in [docs/skills.md](../../docs/skills.md); this page lists what was run.

Conventions in the commands below:

- `$COPILOT` is the pinned Copilot CLI run offline with a private home: `COPILOT_HOME=<empty dir> COPILOT_OFFLINE=true COPILOT_MODEL=gpt-4.1 COPILOT_AUTO_UPDATE=false NO_PROXY=127.0.0.1 copilot`. Where a model was needed, `COPILOT_PROVIDER_BASE_URL=http://127.0.0.1:<port>/v1` pointed it at a scripted model: [scripts/spike_model.py](scripts/spike_model.py) for S1 (a script per conversation, any tool), `crates/interlock-cli/tests/guided_model` for the P1 tests.
- `claude` is Claude Code with its normal, live model unless the row says otherwise. "Dead endpoint" means `ANTHROPIC_BASE_URL=http://127.0.0.1:9 CLAUDE_CODE_MAX_RETRIES=0`: the session starts, reports what it loaded in its init event, and its first model request fails, so no model call is made.
- `interlock` is this branch's binary. `$SPIKE` is the scratch directory the S1 probes ran in.

Transcripts are trimmed by [scripts/trim.py](scripts/trim.py): strings over 2,000 to 3,000 characters are cut, streaming fragments and per-call bookkeeping events are dropped (`*_delta`, `model.call_*`, `assistant.turn_start`, `tool.execution_partial_result`, `session.background_tasks_changed`, `stream_event`), Claude Code's init event keeps only its skills, agents and plugins, and attempt tokens and e-mail addresses are redacted. A scan comparing every 64-hex string against the stores' token hashes found no raw token. Every file is under 150 KB.

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

## Generated skills load, and validate

| # | What | Command | Outcome | Raw output |
| --- | --- | --- | --- | --- |
| 11 | Generate all three targets | `interlock skills generate --target copilot\|claude-code\|agent-skills --out <dir>` | 31, 32 and 31 files; no problems | [validate/generate.json](validate/generate.json) |
| 12 | Static validation | `interlock skills validate <agent-skills dir>`; `--target copilot` on `.github/skills`; `--target claude-code` on the plugin's `skills/` | `valid: true`, no problems, exit 0, each | [validate/](validate/) |
| 13 | Every generated skill loads on Copilot | `$COPILOT skill list --json` in the generated Copilot tree | All six (`design`, `implement`, `investigate`, `review`, `route`, `verify`), `source: project`, `enabled: true` | [load/copilot-skill-list-generated.json](load/copilot-skill-list-generated.json) |
| 14 | Every generated skill and the verifier load on Claude Code | `claude -p --setting-sources "" --plugin-dir <generated plugin>` (dead endpoint) | `interlock:design` ... `interlock:verify`, agent `interlock:verifier`, plugin `interlock`, next to the built-in `design` and `verify` | [load/claude-init-generated-plugin.jsonl](load/claude-init-generated-plugin.jsonl) |

Rows 12 to 14 are also tests: `crates/interlock-skillgen/tests/generate.rs` and `crates/interlock-cli/tests/skills_load.rs`.

## P1 gate: guided sessions end to end

### Copilot CLI, scripted model

`INTERLOCK_COPILOT_BIN=<copilot> INTERLOCK_EVIDENCE_DIR=<dir> cargo test -p interlock-cli --test guided_copilot`. Each test makes a sample repository, runs `interlock setup --host copilot`, and runs one `$COPILOT -p "<request>" --output-format json --allow-all-tools --no-ask-user --plugin-dir .interlock/guided/copilot-plugin` session. The scripted model stands in for the person and the agent: it invokes the skills with the `skill` tool, runs the commands they prescribe (filling in the attempt id, token, epoch and worktree from `attempt start`'s JSON), and delegates with the `task` tool to `interlock-verifier`, which runs a second script in its own context.

| # | Run | Outcome | Raw output |
| --- | --- | --- | --- |
| 15 | Bug fix: `add(2, 3)` returns -1 | `done`: G1, G2, G3, G4, G7. route, implement and verify loaded. The guided hook denied an edit to the repository's `calc.py` ("outside this attempt's worktree") with no attempt in the environment; the edit in the worktree went through. `curl -X POST` got the hook's ask, which `copilot -p` reported as "Denied by preToolUse hook (unable to ask user for confirmation): external_reversible is not granted for this attempt". Base runs: repro failed, regression passed. The verifier attempt (`agent: interlock-verifier`) ran both checks and assessed; its conversation was offered no edit tools | [p1/copilot-bug-fix/](p1/copilot-bug-fix/): `copilot-transcript.jsonl`, `model-requests.jsonl`, `task-log.json`, `status.json`, `attempts.json` |
| 16 | Investigation: how many tries does `export()` make | `done`: G1, G2, G3, G4, G7. The worker's demo printed `tries: 3`; the verifier reran it in its own worktree | [p1/copilot-investigation/](p1/copilot-investigation/) |

### Claude Code, live

Sample repositories from [scripts/make_samples.py](scripts/make_samples.py), each a git repository; in each, `interlock setup --host claude-code`, then one session standing in for the person:

```bash
printf '%s' "$REQUEST" | claude -p --output-format stream-json --verbose --no-session-persistence \
  --setting-sources "" --plugin-dir .interlock/guided/claude-code-plugin \
  --permission-mode acceptEdits --max-turns 90 --allowedTools Bash Read Edit Write Grep Glob Skill Agent
```

with `interlock` on PATH. The request names the route skill, as a person would type it; everything after that (implement or investigate, verify, the hand-off to the verifier agent, every interlock command) the model chose from the skills. Records were dumped from the store afterwards (`task log`, `status`, `task show`, `attempt list`, and the results, claims, assessments and check runs tables).

| # | Run | Outcome | Raw output |
| --- | --- | --- | --- |
| 17 | Bug fix. Request: `/interlock:route parse_duration("1h30m") in durations.py returns 1800 seconds instead of 5400. Please fix it.` | `done` in 47 s, 16 turns, $0.28, no denials. G1, G2, G3, G4, G7. Task `parse-duration-sum` with `repro` (independent, observed, baseline fails) and `regression` (self, tested, baseline passes). Check runs: repro exited 1 on the input snapshot and 0 on the output; regression 0 and 0; the verifier ran both again on the output. Assessments from attempt `agent: interlock:verifier`. Diff: `total =` to `total +=`, plus a `test_compound` test. Every tool call, the verifier's included, went through the PreToolUse hook (17 of 17); `Stop` and `SubagentStop` ran | [p1/claude-code-bug-fix/](p1/claude-code-bug-fix/): `claude-transcript.jsonl`, `task-log.json`, `status.json`, `task.toml`, `attempts.json`, `check_runs.json`, `claims.json`, `assessments.json`, `results.json`, `output.diff` |
| 18 | Investigation. Request: `/interlock:route Can TTLCache.get in cache.py still return a value exactly ttl seconds after it was stored? Where is that decided, and why is it that way?` | `done` in 43 s, 16 turns, $0.25, no denials. G1, G2, G3, G4, G7. Answer: yes; `cache.py:20` compares with a strict `>`; commit `7c6352a` changed `>=` to `>` for coarse clocks, quoted from its message. The worker and the verifier each ran an injected-clock check (age 9.999, 10, 10.001: v, v, miss). PreToolUse hooks: 18 of 18 tool calls | [p1/claude-code-investigation/](p1/claude-code-investigation/) |
| 19 | The guided hooks on Claude Code, live. A guided worker attempt was opened with `interlock attempt start hook-probe --role worker --host claude-code --worktree auto`, then `claude -p ... --plugin-dir <plugin> --model haiku --permission-mode acceptEdits --allowedTools Bash Read Edit` was asked to edit `durations.py` in the repository, then the same file in the worktree, then run `curl -s -X POST http://127.0.0.1:9/notify`. Live, $0.03 | The first edit: hook exit 2, "outside this attempt's worktree". The second: allowed. The curl: the hook returned `permissionDecision: ask` with `hookSpecificOutput`, and Claude Code in `-p` listed it in `permission_denials` with the hook's reason | [p1/claude-code-hook-probe/](p1/claude-code-hook-probe/) |

## Test suite

`INTERLOCK_COPILOT_BIN=<copilot> cargo test --workspace`, with `claude` on PATH: 131 passed, 0 failed, nothing skipped. Per-binary results: [tests.txt](tests.txt). One earlier full run failed once: in the guided investigation test, `interlock attempt start` inside the Copilot session took longer than the bash tool's default 30-second wait. After `interlock setup` began saving the host report (so attempts in a session no longer start the host's binary to inspect it) and the scripted model began waiting up to 120 seconds, the full suite passed and the guided tests passed three more times in a row. The cause was not isolated further.

The tests added by this work:

| Test | Covers |
| --- | --- |
| `interlock-skillgen` unit tests (`frontmatter`, `validate`) | Frontmatter round trip and the YAML subset; name, description, field, reference and template checks |
| `interlock-skillgen/tests/generate.rs` | The embedded catalog matches the source; the v1 set and intents; every skill drives interlock; every target validates; each target's layout and frontmatter; Agent Skills output validates on disk; loader refusals |
| `interlock-supervisor` `guided::tests` | Attempt worktrees per role, the guided-attempt file, change sets from trees |
| `interlock-cli/tests/guided_cli.rs` | `setup` for both hosts; `skills validate` exit codes; `--worktree auto`; the store found from a worktree; guided hooks deny, allow and ask; scope from the tree; `--tree auto`; the stop guard |
| `interlock-cli/tests/skills_load.rs` | Every generated skill loads on Copilot; every skill and the verifier load on Claude Code. Each skips without its host |
| `interlock-cli/tests/guided_copilot.rs` | Rows 15 and 16. Skips without Copilot |
