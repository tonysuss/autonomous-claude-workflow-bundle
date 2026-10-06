# Skills, the generator, and guided sessions

October 6, 2026. Copilot CLI 1.0.91 (offline, scripted model) and Claude Code 2.1.289 (live model, sonnet).

The interactive path: a person drives a Copilot CLI or Claude Code session, generated skills call `interlock` at each step, the hooks plugin runs in interactive mode (it asks where the headless path denies), and an independent verifier records the evidence that decides the task. interlock binds each guided attempt to the agent the host says opened it, so it can tell the verifier from the agent that did the work. The core stays host-agnostic: skills are written once, host-neutral, and a generator says them in each host's terms.

Runtime evidence for everything below is indexed in [evidence/skills/README.md](../evidence/skills/README.md).

## What is built

| Piece | Where | Design |
| --- | --- | --- |
| Six canonical v1 skills and six routed references | `skills/` | §10 v1 skill set |
| The independent verifier agent | `agents/verifier/` | §3, §9 native delegation |
| Generator and static validator (`interlock-skillgen`) | `crates/interlock-skillgen/` | §1 Skills row, §2 frontmatter gap, §4 |
| `interlock skills generate`, `validate`, `list` | `crates/interlock-cli/src/skills_cmd.rs` | §10 |
| `interlock setup --host copilot\|claude-code`, with a manifest of what it wrote | same, `interlock-skillgen/src/setup.rs`, `safe_write.rs` | §3 interactive path |
| Guided attempts bound to their caller; the state guard; `interlock verify`; notes | `crates/interlock-supervisor/src/guided.rs`, `state_paths.rs`, `run.rs`, `main.rs` | §3 |
| S1 skill load spike | [evidence/skills/s1/](../evidence/skills/s1/) | §12 P0 S1, §14 risk |
| P1 gate runs | [evidence/skills/](../evidence/skills/) | §12 P1 gate |

## The skills

| Skill | Invocation | From pstack | Job | interlock commands it drives |
| --- | --- | --- | --- | --- |
| `route` | model | poteto-mode routing | Pick the workflow, write the criteria, create the task | `init`, `task create -`, `task ready`, `status` |
| `investigate` | model, standalone | how, why | File-level evidence and history; the worker of an investigation, or an answer on its own | `brief`, `attempt start`, `check run`, `claim add`, `result submit --tree auto`, `note add --kind answer` |
| `design` | user | architect, arena | Compare whole designs, only when uncertainty and impact justify it | `brief`, `status`, `note add --kind design` |
| `implement` | model, standalone | bug-fix, feature, refactoring playbooks | The change, at its root cause wherever it occurs; in the attempt's worktree with before and after recorded, or on its own | `brief`, `status`, `attempt start --worktree auto`, `check run --target base`, `check run`, `claim add`, `result submit --tree auto` |
| `verify` | model | bug-fix steps 1 and 4, prove-it-works | Hand off to the independent verifier, then let interlock decide | `status`, `verify` (Copilot), `advance`, `task log` |
| `review` | model | interrogate | Findings with evidence, a lead judgment on each, dismissals kept | `status`, `attempt start --role reviewer`, `attempt end --note`, `note add --kind review` |

On Copilot the skills are named `interlock-<name>`: project skills share one namespace with the repository's own, and a repository may already have a `review` skill. Claude Code's plugin namespace (`interlock:<name>`) does the same job.

The verifier agent (`agents/verifier`) opens its own verifier attempt, has interlock run each check on the submitted tree in its own worktree, records one assessment per criterion, and ends its attempt: `attempt start --role verifier --worktree auto`, `brief --role verifier`, `check run`, `assess add --tree auto`, `attempt end`. Its attempt is bound to it, its token works only from it, and it never puts the token in its reply.

Routed references, emitted as files under the skills that route to them and never as standalone skills:

| Reference | Routed from | From pstack |
| --- | --- | --- |
| `interlock-basics` | every skill | (new) the loop, tokens, scope, strengths, rules |
| `prove-it-works` | implement, verify | principle-prove-it-works |
| `fix-root-causes` | implement, investigate | principle-fix-root-causes |
| `test-behavior-not-implementation` | implement, verify | principle-test-behavior-not-implementation |
| `subtract-before-you-add` | implement, design | principle-subtract-before-you-add |
| `confidence-tiers` | investigate, review | why's epistemics |

Skill-local references: `route/references/task-templates.md` (one TOML template per workflow), `implement/references/{bug-fix,feature,refactor}.md`, `investigate/references/answer-format.md`, `review/references/lead-judgment.md`, `design/references/rationale.md`. Patterns are adapted, not copied; [skills/NOTICE](../skills/NOTICE) carries pstack's MIT notice and is copied into every output.

### Without interlock

`implement` and `investigate` are standalone: they also work where there is no interlock task, which is how the evaluation's skills condition ("skills alone", design §13) uses them, and how anyone without interlock set up would. Their descriptions are written to trigger on ordinary requests ("fix this bug", "add this feature", "why does X happen") rather than on an interlock task. Whether a model takes them up is another matter: a scripted Copilot session loads them and receives their text, but in the v4 evaluation Claude Code's model (`claude-sonnet-5-5`, medium effort) invoked none in 28 runs (docs/evaluation.md, "Results v4"). Each body says when interlock applies: the agent was given a task id, `INTERLOCK_ATTEMPT` is set (a headless session, where interlock opened the attempt and runs the rest of the loop), or `interlock where` exits 0 (then, with no task yet, route comes first). Otherwise the agent follows the body's `## Without interlock` section: the same playbooks and principles, the checks run by the agent itself, nothing recorded, and a report of what changed, the root cause and the commands it ran.

`interlock where` prints `{store, exists, repo}` for the store every interlock command would open from the current directory, found the same way they find it: `--db` or `INTERLOCK_DB`, then an attempt's worktree under `.interlock/worktrees/`, then `.interlock/state.db` at the repository root. It creates nothing, exits 0 when the store exists and 3 when it does not, and is classed as a read. The skills ask it rather than test a path, because a guided setup can keep its store elsewhere (`INTERLOCK_DB`, which the generated hooks carry), a session can start in a subdirectory, and `skills generate` writes an `.interlock/` directory with no store in it. Tests: `cli.rs::where_finds_the_store_every_command_uses_and_creates_none` covers the default store from the root, a subdirectory and an attempt's worktree, an `INTERLOCK_DB` store, and a missing one; the generator and `skills_load` tests check that the generated text, as the model receives it, uses `interlock where` and names `INTERLOCK_DB`.

**What this does not fix.** The skills only say what to do. A model can skip a skill, or follow it and still not run `interlock where`, and then work in a guided repository with no task. A store kept elsewhere is found only when `INTERLOCK_DB` is set in the session's own environment: the generated hooks carry it, but a session started from a shell without it does not. In a guided session the hooks govern only an open attempt, so nothing stops edits made outside one. No live model has been seen to invoke these skills yet (above).

`standalone = true` in `skill.toml` is checked when the catalog loads: only a model-invoked skill with a requirement can be standalone, and its body must have the `## Without interlock` section. The plain Agent Skills target says so in `compatibility` ("Uses interlock-cli on PATH where the work is an interlock task; works without it") and in `metadata.interlock-standalone`. `route`, `verify`, `review` and `design` stay tied to interlock: their jobs exist only with its records.

Why: in the October 6 evaluation no session invoked a skill, in any condition. The descriptions then said "an interlock task's criteria" and "a repository that uses interlock", which a plain request never matches, and every step of every body called `interlock`.

### The four workflows

| Workflow | Skills, in order | Criteria template |
| --- | --- | --- |
| Investigation | route, investigate, verify | `answer`: independent, observed, no check unless a fact is decidable now; no scope |
| Bug fix | route, implement (bug-fix playbook), verify; review optional | `repro`: independent, observed, `baseline = "fails"`; `regression`: self, tested, `baseline = "passes"` |
| Feature | route, implement (feature playbook; design when warranted), verify; review optional | `behavior`: independent, observed, `baseline = "fails"`; `regression` |
| Refactor | route, implement (refactor playbook), verify; review optional | `pin`: independent, tested, `baseline = "passes"`; `shape`: independent, static; `regression` |

The machine-checkable parts of the principles live in the core (baselines, empty-run detection, scope on the output tree, stale evidence). The skills carry the rest as routed text.

Scope: a task's `scope.paths` lists what its result may change. An empty or missing scope allows no change at all; `["**"]` allows any. An investigation's result may change nothing, whatever its scope.

Route tells the person before it commits a check script under `checks/` (the input snapshot is a commit, on their branch), names the file, branch and message, and waits for their go-ahead when they are at the keyboard; inline checks need no commit. Review runs in the working session, so on its own it is a second look rather than an independent one; the skill offers a second reviewer on a different model family, as pstack's interrogate does, and every act-on finding goes to the verifier.

## Canonical format

```
skills/<name>/
  skill.toml      name, description, invocation = "model" | "user" | "routed",
                  requires = ["interlock-cli"], pack = "core",
                  standalone = true (optional: also works without interlock),
                  routers = [...] (routed only), argument-hint (optional)
  SKILL.md        host-neutral body, no frontmatter
  references/     *.md copied under this skill
agents/<name>/
  agent.toml      name, description, tools = ["read", "shell"], handoff-from = "verify"
  AGENT.md        the agent's instructions
skills/NOTICE     attribution, copied into every output
```

Bodies use four templates, expanded per target: `{{host}}`, `{{skill:NAME}}`, `{{agent:NAME}}`, `{{handoff:NAME}}`. Loading refuses: a name that is not lowercase-hyphenated or does not match its folder; a description that is empty, over 1024 characters, or contains `<` or `>`; a body with frontmatter; unknown fields, requirements or packs; a routed skill with no router, or a router whose body never mentions `references/<routed>.md`; a template that does not resolve for every target; any file other than `references/*.md` beside a SKILL.md.

The binary embeds `skills/` and `agents/` at build time (`build.rs`), so `interlock skills generate` needs no source tree; `--from <interlock dir>` reads a working copy instead. A test checks the embedded copy matches the source.

## The generator

```bash
interlock skills list
interlock skills generate --target copilot      --out <repo>        # .github/skills/interlock-*
interlock skills generate --target claude-code  --out <plugin dir>  # a Claude Code plugin
interlock skills generate --target agent-skills --out <dir>         # plain Agent Skills folders
interlock skills validate <dir> [--target agent-skills|copilot|claude-code]
```

`generate` validates its own output before writing and refuses to write output that fails. It writes only inside `--out`, never through a symlink, and never over a file it did not write or that changed since it did, unless `--force`; `<out>/.interlock/generated.json` records what it wrote, with SHA-256 hashes. Refusals are listed and exit 2. `validate` takes a folder of skill folders, a Claude Code plugin, or a repository with `.github/skills`, checks the agent files beside the skills too, exits 1 on any error and prints every problem as JSON.

| Intent | Copilot CLI | Claude Code | Plain Agent Skills |
| --- | --- | --- | --- |
| Model-invoked | `.github/skills/interlock-<name>/SKILL.md` with `name`, `description` | `skills/<name>/SKILL.md` in plugin `interlock`, loaded as `interlock:<name>` | `<name>/SKILL.md` with `name`, `description`, `compatibility`, `metadata.interlock-invocation: model` |
| User-invoked | the same, plus `disable-model-invocation: true` and `argument-hint` | the same, plus `disable-model-invocation: true` and `argument-hint` | `metadata.interlock-invocation: user`; no host field |
| Routed | `references/<name>.md` under each router | the same | the same |
| Verifier | none: `interlock verify` launches it | `agents/verifier.md` (`interlock:verifier`), `tools: Read, Grep, Glob, Bash` | `verify/references/verifier-agent.md`, instructions for any sub-agent |
| Hand-off | "Run `interlock verify <id> --host copilot`" | Agent tool, `subagent_type: "interlock:verifier"`, with a filled-in prompt | `interlock verify` where the host is known; otherwise a read-and-shell sub-agent, which interlock records as unbound |

Copilot gets no custom agent because its hooks cannot tell which subagent is calling (a subagent's tool calls carry their own session id and no agent type), so a verifier subagent's attempt could not be bound to it; interlock launches the verifier itself instead. That also settles the review's note about the agent's `search` tool: there is no Copilot agent file in the repository. The session interlock launches does run as a custom agent: every headless Copilot session, from `interlock verify` or `interlock run`, runs as interlock's agent for its role, written into the hooks plugin and selected with `--agent` (docs/host-spike-2026-10-05.md, "Custom agents for headless sessions"). That is a top-level session interlock starts, not a subagent the person's session hands work to.

The static validator applies the Agent Skills rules to every SKILL.md (required `name` and `description`; name 1 to 64 lowercase letters, digits and single hyphens, matching its folder; description 1 to 1024 characters; `compatibility` at most 500; `metadata` a string map; only the specification's fields), plus each host's extra fields for its target, plus: no unexpanded template, every `references/...` a body mentions exists, and a warning past 500 body lines. Agent files must name themselves after their file, describe themselves, and list tools. The frontmatter reader handles the YAML subset skill files use and reports anything else as unsupported rather than guessing.

## S1: what the flag does, and what each emission needs

Run on the pinned hosts with probe skills (raw output in [evidence/skills/s1/](../evidence/skills/s1/)).

| Question | Copilot CLI 1.0.91 | Claude Code 2.1.289 |
| --- | --- | --- |
| Is a `disable-model-invocation: true` skill listed? | Yes, by `copilot skill list`; but not in the model's `<available_skills>` | Yes, in the init event's `skills` and `slash_commands` |
| Can the agent invoke it with the skill tool? | No: `Skill not found: s1-user`, the same error as a skill that does not exist | No: "cannot be used with Skill tool due to disable-model-invocation" |
| Can a person invoke it by name? | Yes in the interactive session: `/s1-user args` injects the body with "The user explicitly invoked..." and `ARGUMENTS: args` | Yes, `/plugin:skill args` works even in `-p` |
| In `-p` (prompt mode)? | No: `copilot -p "/s1-user ..."` passes the text through unexpanded, for any skill | Yes |
| `user-invocable: false` | The agent can invoke it; `/s1-hidden` is "Unknown command" | not probed |
| Routed references under the router | Work: the skill context lists them under "Related files", and `view` reads them | Work: Read reads them |
| Custom agent profile | `--agent s1-agent` loads its body into the system prompt (after Copilot's own, as agent instructions) with only its tools; the `task` tool offers it as an `agent_type` | `agents/*.md` in a plugin load as `plugin:name` |

So the design's report ("listed but reported as not found when used") holds for the agent's skill tool on 1.0.91, and the flag does what it says: the skill is for people. The generated skills were checked the same way: on Copilot, every model-invoked `interlock-*` skill was invoked through the `skill` tool and its text reached the model, `interlock-design` was refused, and `/interlock-design` typed in the interactive TUI reached the model with its body; on Claude Code all six load and `design` and `review` are slash commands. On Copilot a user-invoked skill is usable only in the interactive session, never in `copilot -p`; that is why only `design` is user-invoked.

Claude Code ships built-in skills named `verify` and `design` in this environment (visible in the init event). The plugin namespace (`interlock:verify`) keeps them apart, which is why the Claude Code target is a plugin rather than `.claude/skills/`.

## Guided sessions

### Setup

```bash
cd <repository>
interlock setup --host copilot       # or --host claude-code
```

| | Copilot CLI | Claude Code |
| --- | --- | --- |
| Skills | `.github/skills/interlock-<name>/` (commit to share) | `.interlock/guided/claude-code-plugin/skills/` |
| Verifier | `interlock verify <id> --host copilot`, launched by the verify skill | `.interlock/guided/claude-code-plugin/agents/verifier.md` |
| Hooks | `.interlock/guided/copilot-plugin/hooks/hooks.json` | `.interlock/guided/claude-code-plugin/hooks/hooks.json` |
| Start a session | `copilot --plugin-dir .interlock/guided/copilot-plugin` | `claude --plugin-dir .interlock/guided/claude-code-plugin` |

`setup` also creates the store, inspects the host and saves its capabilities (so attempts in the session read the saved report instead of starting the host's binary from inside it), validates what it generates, and prints the files written, unchanged and refused, the session command, the host report and notes as JSON. A host that is installed but reports no capabilities (a probe that timed out reads that way) is probed once more; if it still reports none, setup saves nothing and fails. Setup writes only inside the repository that holds the store, refuses a store outside it, never writes through a symlink, and never overwrites a file it did not write, or one changed since, unless `--force`; `.interlock/setup-manifest.json` records what it wrote, with hashes. The `interlock` binary must be on the session's PATH.

The hooks run `INTERLOCK_MODE=interactive INTERLOCK_HOST=<host> INTERLOCK_DB=<store> interlock hook <event>` on `PreToolUse`, `Stop` and `SubagentStop`.

### How a guided session is governed

- **Each attempt is bound to its caller.** Every hook payload names the host session, and on Claude Code a subagent's id and type. When the hooks see `interlock attempt start`, they record who ran it; the CLI binds the new attempt to that caller in the store (`binding`: `session`, `subagent`, `interlock_launched` or `unbound`). An attempt governs only its own caller, and on Claude Code that caller's subagents; other sessions in the same checkout are not governed by it. Submitting, ending or superseding an attempt releases the session. An interactive worker attempt opened where no hook saw it goes to the first session that calls a hook; a verifier attempt never does.
- **Who may verify.** The hooks refuse `attempt start --role verifier` from the main agent of a session that holds the task's worker attempt (running or submitted). On Claude Code, a verifier attempt opened by the `interlock:verifier` subagent is bound to that subagent. `interlock verify` launches a separate headless verifier session and binds its attempt as `interlock_launched`. A verifier anyone else opens is `unbound`. A verifier's credentials (`assess add`, `check run`, `attempt end`, and the rest) work only from the caller its attempt is bound to; a command whose attempt id the hooks cannot read is refused while a foreign verifier attempt is open.
- **Unbound verifiers count only where interlock ran the check.** For a criterion without a check, an assessment is the whole evidence, so one from an unbound verifier does not count, and `status` says so. Failures from anyone still count. `status` and `brief` show each attempt's binding.
- **interlock's state is off limits.** Everything under `.interlock/` except the caller's own worktree: edit tools may not write there, and shell commands may not change it, in every hooked session, governed or not. Reading is allowed (Claude Code's skills keep their references there). The hooks judge each simple command of a shell line, following `cd`: output redirected there, a path there named by any command but a read-only one (inside a string too), and a command that would change files while its directory is there, are refused. Here-documents are data. `interlock` itself is allowed.
- **Fail closed.** A payload the hook cannot read, or one with no session id, is refused while a guided attempt is open, and so is anything when the store cannot be read.
- **Ask, not deny.** In interactive mode a call the grant does not cover gets `permissionDecision: ask`; the person decides. Hard rules still deny: edits outside the attempt's worktree or the task's scope, interlock's state, and the grant's own deny list.
- **One worktree per attempt.** `--worktree auto` makes a fresh git worktree under `.interlock/worktrees/` (workers at the input snapshot or the last accepted output; verifiers and reviewers at the output under verification) and records it on the attempt. `--tree auto`, and an attempt's `check run`, use that worktree wherever the command runs.
- **Results come from the tree, or not at all.** `result submit` reads the change set itself, from the output tree in the repository that owns the store, against the input snapshot. The caller's directory and its git variables (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` and the rest) play no part, there is no `--changed`, and when the changes cannot be read the submission is refused with exit 2.
- **The store is found from inside a worktree.** Without `INTERLOCK_DB`, a command run under `<repo>/.interlock/worktrees/` uses `<repo>/.interlock/state.db`, and refuses rather than create a fresh store there; elsewhere the store is at the nearest repository root.
- **The stop guard** holds an agent until its own attempt's evidence is recorded, once per stop. Every suggested command is complete, with `--attempt` and `--token`, and each role is asked only for its own record: a worker for claims, a verifier for assessments.
- **Done, failed and cancelled settle the task.** interlock ends the attempts the task left open and removes their worktrees. Before that, a done task's verified output is kept at `refs/interlock/tasks/<id>`, a commit on the input snapshot; `advance` prints it with `git cherry-pick <ref>`, and the verify skill tells the person. No branch moves.

### Notes

`interlock note add --task <id> --kind design|review|answer --file -` keeps a design's rationale, a review's judged findings or an investigation's answer on the task, in the store; `interlock note list --task <id>` reads them. The design, review and investigate skills use it, so their output outlives the session and needs no file the hooks would have to allow.

### Verification

| Host | How the verify skill gets an independent verifier | Binding |
| --- | --- | --- |
| Claude Code | The Agent tool with `subagent_type: "interlock:verifier"` and a filled-in prompt; the subagent opens its own attempt | `subagent` |
| Copilot CLI | `interlock verify <id> --host copilot`: interlock launches a headless verifier session with read and test tools, on exactly the submitted files, and applies what its evidence allows | `interlock_launched` |
| Other | `interlock verify` where interlock knows the host; otherwise a read-and-shell sub-agent | `unbound` for the latter |

An interactive verifier attempt needs the host's `custom_agents` capability; without it the attempt does not open and the task moves to blocked with "no independent verifier", its work kept.

## P1 gate

| Gate item | Result | Evidence |
| --- | --- | --- |
| Investigation end to end in guidance mode, Copilot | `done` via G1, G2, G3, G4, G7; scripted model; real Copilot CLI, skills and hooks; verified by `interlock verify`. The worker's session was refused a verifier attempt; a `task` subagent's verifier was recorded `unbound` and its pass did not count | `crates/interlock-cli/tests/guided_copilot.rs`, [copilot-investigation](../evidence/skills/p1/copilot-investigation/) |
| Bug fix end to end in guidance mode, Copilot | `done` via G1, G2, G3, G4, G7; an edit outside the worktree denied; an uncovered external call asked, which `copilot -p` turns into a denial; the worker's own verifier attempt refused; the launched verifier's edit denied | same test, [copilot-bug-fix](../evidence/skills/p1/copilot-bug-fix/) |
| The Copilot tests, three runs in a row | 2 of 2 passed each time, with `INTERLOCK_REQUIRE_HOSTS=1` | [review-fixes/guided-copilot-3-runs.txt](../evidence/skills/review-fixes/guided-copilot-3-runs.txt) |
| Investigation end to end in guidance mode, Claude Code, live (sonnet) | `done` via G1, G2, G3, G4, G7 in 53 s for $0.26; a correct, cited answer, rechecked by the `interlock:verifier` subagent, bound as `subagent` | [p1/claude-code-investigation](../evidence/skills/p1/claude-code-investigation/) |
| Bug fix end to end in guidance mode, Claude Code, live (sonnet) | `done` via G1, G2, G3, G4, G7 in 48 s for $0.26; the reproduction failed on the input snapshot and passed on the output, in interlock's own runs; verifier bound as `subagent`; the output kept at `refs/interlock/tasks/duration-sum` | [p1/claude-code-bug-fix](../evidence/skills/p1/claude-code-bug-fix/) |
| The guided hooks on Claude Code, live | An edit outside the worktree denied; an uncovered `curl -X POST` asked, and `claude -p` recorded it as a permission denial. Run on commit 4f7e74b | [p1/claude-code-hook-probe](../evidence/skills/p1/claude-code-hook-probe/) |
| Every generated skill loads, and reaches the model | Copilot: all six listed; each model-invoked skill's text reached the model through the `skill` tool; `/interlock-design` typed in the TUI reached it. Claude Code: all six and the verifier in the init event | `crates/interlock-cli/tests/skills_load.rs`, [load](../evidence/skills/load/) |
| Plain Agent Skills output passes static validation | `interlock skills validate` reports no problems | `crates/interlock-skillgen/tests/generate.rs`, [validate](../evidence/skills/validate/) |

## Changes from the design

- **Claude Code is a target.** The design's generator emits Copilot files and plain Agent Skills. Per the host decision, Claude Code is a first-class host, so it gets its own target: a plugin, because a plugin namespace keeps `verify` and `design` apart from Claude Code's built-in skills of those names.
- **User-invoked emission: keep the flag.** The design left this to S1 (a custom agent profile, or a skill without the flag). S1 shows the flag does the right thing on both hosts: hidden from the agent, available to a person by name.
- **The verifier opens its own attempt.** §3 has custom agents "report back with their attempt token". The verifier keeps its token to itself and reports its attempt id and verdicts; the main session never holds a verifier token.
- **On Copilot, interlock launches the verifier.** §3 has native delegation on both hosts. Copilot's hooks cannot name the calling subagent, so independence there comes from `interlock verify` launching the verifier as its own session.
- **Review informs, verify decides.** The core accepts assessments from verifier attempts only, so a reviewer attempt records its findings in its end note and a task note, and the verifier checks the act-on findings. A reproduced finding fails its criterion and R1 sends the task back.
- **Guided attempts are bound in the store.** The headless hooks read the attempt from the session's environment, which a person's session does not have. Each guided attempt records, in the store, the host session (and subagent) that opened it.

## Limits

- **Copilot was not run with a real model.** Its guided runs exercise the real CLI, skills, hooks, TUI and permissions, with a scripted model choosing every tool call. They prove the plumbing, not that a model follows the skills. The live evidence that a model follows them is from Claude Code.
- **Copilot does not pass its provider settings to commands.** Its bash tool drops `COPILOT_OFFLINE` and `COPILOT_PROVIDER_*` from the environment of the commands it runs (it keeps `COPILOT_HOME` and `COPILOT_MODEL`), so the verifier `interlock verify` launches uses the person's signed-in Copilot. With a bring-your-own-model provider, the command needs those variables set on it; the offline tests do that.
- **`interlock verify` was run on Copilot only.** It uses the same supervisor session as `interlock run`, but no guided run on Claude Code used it; there the verifier subagent is the path.
- **"Ask" was observed only where nobody can answer.** In `copilot -p` and `claude -p` an ask becomes a denial. Neither host's interactive prompt for a hook ask was exercised with a person.
- **The live runs are one each, on small repositories,** after one failed and one superseded run (see the evidence). In both, the person's request named the route skill (`/interlock:route ...`); the model chose every later skill, the hand-off, and every interlock command.
- **The state guard reads command text.** It catches commands that name interlock's state, as words or inside strings, and commands run from inside it. A path computed at run time (`touch "$(cat f)"`), or a script that writes there, is beyond it. Attempt tokens are stored only as hashes, so reading the store reveals none.
- **Binding trusts the host's payload.** It is as good as the session and agent ids the host reports. In this container a nested `claude` reports its parent session's id, so both live runs show the outer session's id; the binding still tells the main agent from the subagent by agent id. A Copilot subagent appears as its own session, so a verifier it opens is `unbound`.
- **Shell writes are not placement-checked per call.** The hooks place edit-tool writes; a `sed -i` outside the worktree, but outside `.interlock/`, is not stopped per call. Scope is enforced at G3 on the output tree, and the repository's own files are never part of an attempt's output.
- **The probe hang was not reproduced.** One earlier setup saved a host report with no capabilities. 32 concurrent probes on fresh Copilot homes all finished in about 1.5 s, and no stray processes were left. The probe now reads the host's output for at most two seconds after it exits, times out after 15 s and retries once, and setup refuses a report with no capabilities.
- **The validator reads a YAML subset.** Anchors, flow mappings and multi-line plain scalars are reported as unsupported.
- **Feature and refactor have skills and templates but no end-to-end run.** The P1 gate names investigation and bug fix; those are the workflows run.
