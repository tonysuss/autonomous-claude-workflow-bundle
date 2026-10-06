# Skills, the generator, and guided sessions

October 6, 2026. Copilot CLI 1.0.91 (offline, scripted model) and Claude Code 2.1.289 (live model).

The interactive path: a person drives a Copilot CLI or Claude Code session, generated skills call `interlock` at each step, the hooks plugin runs in interactive mode (it asks where the headless path denies), and an independent verifier custom agent records the evidence that decides the task. The core stays host-agnostic: skills are written once, host-neutral, and a generator says them in each host's terms.

Runtime evidence for everything below is indexed in [evidence/skills/README.md](../evidence/skills/README.md).

## What is built

| Piece | Where | Design |
| --- | --- | --- |
| Six canonical v1 skills and five routed references | `skills/` | §10 v1 skill set |
| The independent verifier agent | `agents/verifier/` | §3, §9 native delegation |
| Generator and static validator (`interlock-skillgen`) | `crates/interlock-skillgen/` | §1 Skills row, §2 frontmatter gap, §4 |
| `interlock skills generate`, `validate`, `list` | `crates/interlock-cli/src/skills_cmd.rs` | §10 |
| `interlock setup --host copilot\|claude-code` | same, and `interlock-skillgen/src/setup.rs` | §3 interactive path |
| Guided attempts: `attempt start --worktree auto`, hooks that find the guided attempt, `result submit --tree auto` | `crates/interlock-supervisor/src/guided.rs`, `main.rs` | §3 |
| S1 skill load spike | [evidence/skills/s1/](../evidence/skills/s1/) | §12 P0 S1, §14 risk |
| P1 gate runs | [evidence/skills/](../evidence/skills/) | §12 P1 gate |

## The skills

| Skill | Invocation | From pstack | Job | interlock commands it drives |
| --- | --- | --- | --- | --- |
| `route` | model | poteto-mode routing | Pick the workflow, write the criteria, create the task | `init`, `task create`, `task ready`, `status` |
| `investigate` | model | how, why | File-level evidence and history; the worker of an investigation | `brief`, `attempt start`, `check run`, `claim add`, `result submit --tree auto` |
| `design` | user | architect, arena | Compare whole designs, only when uncertainty and impact justify it | `brief`, `status` |
| `implement` | model | bug-fix, feature, refactoring playbooks | Smallest change, in the attempt's worktree; before and after recorded | `brief`, `status`, `attempt start --worktree auto`, `check run --target base`, `check run`, `claim add`, `result submit --tree auto` |
| `verify` | model | bug-fix steps 1 and 4, prove-it-works | Hand off to the verifier agent, then let interlock decide | `status`, `advance`, `task log` |
| `review` | model | interrogate | Findings with evidence, a lead judgment on each, dismissals kept | `status`, `attempt start --role reviewer`, `attempt end --note` |

The verifier agent (`agents/verifier`) opens its own verifier attempt, has interlock run each check on the submitted tree in its own worktree, records one assessment per criterion, and ends its attempt: `attempt start --role verifier --worktree auto`, `brief --role verifier`, `check run`, `assess add --tree auto`, `attempt end`. Its token never passes through the main session.

Routed references, emitted as files under the skills that route to them and never as standalone skills:

| Reference | Routed from | From pstack |
| --- | --- | --- |
| `interlock-basics` | every skill | (new) the loop, tokens, strengths, rules |
| `prove-it-works` | implement, verify | principle-prove-it-works |
| `fix-root-causes` | implement, investigate | principle-fix-root-causes |
| `test-behavior-not-implementation` | implement, verify | principle-test-behavior-not-implementation |
| `subtract-before-you-add` | implement, design | principle-subtract-before-you-add |
| `confidence-tiers` | investigate, review | why's epistemics |

Skill-local references: `route/references/task-templates.md` (one TOML template per workflow), `implement/references/{bug-fix,feature,refactor}.md`, `investigate/references/answer-format.md`, `review/references/lead-judgment.md`, `design/references/rationale.md`. Patterns are adapted, not copied; [skills/NOTICE](../skills/NOTICE) carries pstack's MIT notice and is copied into every output.

### The four workflows

| Workflow | Skills, in order | Criteria template |
| --- | --- | --- |
| Investigation | route, investigate, verify | `answer`: independent, observed, no check unless a fact is decidable now |
| Bug fix | route, implement (bug-fix playbook), verify; review optional | `repro`: independent, observed, `baseline = "fails"`; `regression`: self, tested, `baseline = "passes"` |
| Feature | route, implement (feature playbook; design when warranted), verify; review optional | `behavior`: independent, observed, `baseline = "fails"`; `regression` |
| Refactor | route, implement (refactor playbook), verify; review optional | `pin`: independent, tested, `baseline = "passes"`; `shape`: independent, static; `regression` |

The machine-checkable parts of the principles live in the core already (baselines, empty-run detection, scope on the output tree, stale evidence). The skills carry the rest as routed text.

## Canonical format

```
skills/<name>/
  skill.toml      name, description, invocation = "model" | "user" | "routed",
                  requires = ["interlock-cli"], pack = "core",
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
interlock skills generate --target copilot      --out <repo>        # .github/skills, .github/agents
interlock skills generate --target claude-code  --out <plugin dir>  # a Claude Code plugin
interlock skills generate --target agent-skills --out <dir>         # plain Agent Skills folders
interlock skills validate <dir> [--target agent-skills|copilot|claude-code]
```

`generate` validates its own output before writing and refuses to write output that fails. `validate` exits 1 on any error and prints every problem as JSON.

| Intent | Copilot CLI | Claude Code | Plain Agent Skills |
| --- | --- | --- | --- |
| Model-invoked | `.github/skills/<name>/SKILL.md` with `name`, `description` | `skills/<name>/SKILL.md` in plugin `interlock`, loaded as `interlock:<name>` | `<name>/SKILL.md` with `name`, `description`, `compatibility`, `metadata.interlock-invocation: model` |
| User-invoked | the same, plus `disable-model-invocation: true` and `argument-hint` | the same, plus `disable-model-invocation: true` and `argument-hint` | `metadata.interlock-invocation: user`; no host field |
| Routed | `references/<name>.md` under each router | the same | the same |
| Verifier agent | `.github/agents/interlock-verifier.agent.md`, `tools: ["read", "search", "execute"]` | `agents/verifier.md` (`interlock:verifier`), `tools: Read, Grep, Glob, Bash` | `verify/references/verifier-agent.md`, instructions for any sub-agent |
| Hand-off sentence | `task` tool, `agent_type: "interlock-verifier"`, `mode: "sync"` | Agent tool, `subagent_type: "interlock:verifier"` | a sub-agent with read and shell tools only |

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
| Custom agent profile | `--agent s1-agent` loads its body as the system prompt with only its tools; the `task` tool offers it as an `agent_type` | `agents/*.md` in a plugin load as `plugin:name` |

So the design's report ("listed but reported as not found when used") holds for the agent's skill tool on 1.0.91, and the flag now does what it says: the skill is for people. The settled emission is in the generator table above. A custom agent profile is not needed for user-invoked skills. On Copilot a user-invoked skill is usable only in the interactive session, never in `copilot -p`; that is why only `design` is user-invoked, and why the skills a workflow chains through (route, investigate, implement, verify, review) are model-invoked.

Claude Code ships built-in skills named `verify` and `design` in this environment (visible in the init event). The plugin namespace (`interlock:verify`) keeps them apart, which is why the Claude Code target is a plugin rather than `.claude/skills/` (that layout also loads; see the S1 evidence).

## Guided sessions

### Setup

```bash
cd <repository>
interlock setup --host copilot       # or --host claude-code
```

| | Copilot CLI | Claude Code |
| --- | --- | --- |
| Skills | `.github/skills/<name>/` (commit to share) | `.interlock/guided/claude-code-plugin/skills/` |
| Verifier | `.github/agents/interlock-verifier.agent.md` | `.interlock/guided/claude-code-plugin/agents/verifier.md` |
| Hooks | `.interlock/guided/copilot-plugin/hooks/hooks.json` | `.interlock/guided/claude-code-plugin/hooks/hooks.json` |
| Start a session | `copilot --plugin-dir .interlock/guided/copilot-plugin` | `claude --plugin-dir .interlock/guided/claude-code-plugin` |

`setup` also creates the store, inspects the host once and saves its capabilities (so attempts in the session read the saved report instead of starting the host's binary from inside it), validates what it generates before writing, and prints the files, the session command, the host report and notes as JSON. The `interlock` binary must be on the session's PATH.

The hooks run `INTERLOCK_MODE=interactive INTERLOCK_HOST=<host> INTERLOCK_DB=<store> interlock hook <event>` on `PreToolUse`, `Stop` and `SubagentStop`.

### How a guided session is governed

- **The hooks find the attempt.** A person's session has no `INTERLOCK_ATTEMPT` in its environment. `interlock attempt start` in interactive mode records the attempt in `.interlock/guided-attempt.json`, and interactive hooks govern that attempt while it is open and its task is active. Outside an open attempt they allow everything, as before.
- **Ask, not deny.** In interactive mode a call the grant does not cover gets `permissionDecision: ask`; the person decides. Hard rules still deny: edits outside the attempt's worktree or the task's scope, commands that touch the store, and the grant's own deny list.
- **One worktree per attempt.** `--worktree auto` makes a fresh git worktree under `.interlock/worktrees/` (workers at the input snapshot or the last accepted output; verifiers and reviewers at the output under verification) and records it on the attempt.
- **Results come from the tree.** `result submit --tree auto` records the worktree's tree, and the change set is read from it, so scope holds whatever the caller lists.
- **The store is found from inside a worktree.** Without `INTERLOCK_DB`, interlock uses the nearest `.interlock/state.db` above the current directory, so commands run inside `.interlock/worktrees/...` reach the repository's store.
- **The stop guard holds the session** until the open attempt's evidence is recorded, once per stop, as in the headless path.

### Verification is delegated

The verify skill hands off with the host's native delegation (Copilot's `task` tool, Claude Code's Agent tool) to the generated verifier agent, which has read and shell tools only. The verifier opens its own attempt and records its own assessments. An interactive verifier attempt needs the host's `custom_agents` capability; without it the attempt does not open and the task moves to blocked with "no independent verifier", its work kept.

## P1 gate

| Gate item | Result | Evidence |
| --- | --- | --- |
| Investigation end to end in guidance mode, Copilot | `done` via G1, G2, G3, G4, G7; scripted model; real Copilot CLI, skills, hooks and custom agent | `crates/interlock-cli/tests/guided_copilot.rs`, [copilot-investigation](../evidence/skills/p1/copilot-investigation/) |
| Bug fix end to end in guidance mode, Copilot | `done` via G1, G2, G3, G4, G7; an edit outside the worktree denied by the guided hook; an uncovered external call asked, which `copilot -p` turns into a denial | same test, [copilot-bug-fix](../evidence/skills/p1/copilot-bug-fix/) |
| Investigation end to end in guidance mode, Claude Code, live | `done` via G1, G2, G3, G4, G7 in 43 s for $0.25; a correct, cited answer, rechecked by `interlock:verifier` with its own injected-clock run | [p1/claude-code-investigation](../evidence/skills/p1/claude-code-investigation/) |
| Bug fix end to end in guidance mode, Claude Code, live | `done` via G1, G2, G3, G4, G7 in 47 s for $0.28; the reproduction failed on the input snapshot and passed on the output, in interlock's own runs; the verifier ran as `interlock:verifier` | [p1/claude-code-bug-fix](../evidence/skills/p1/claude-code-bug-fix/) |
| The guided hooks on Claude Code, live | An edit outside the worktree denied; an uncovered `curl -X POST` asked, and `claude -p` (no one to ask) recorded it as a permission denial | [p1/claude-code-hook-probe](../evidence/skills/p1/claude-code-hook-probe/) |
| Every generated skill loads | Copilot: all six by `copilot skill list`; Claude Code: all six and the verifier in the init event | `crates/interlock-cli/tests/skills_load.rs` |
| Plain Agent Skills output passes static validation | `interlock skills validate` reports no problems | `crates/interlock-skillgen/tests/generate.rs`, [validate](../evidence/skills/validate/) |

## Changes from the design

- **Claude Code is a target.** The design's generator emits Copilot files and plain Agent Skills. Per the host decision, Claude Code is a first-class host, so it gets its own target: a plugin, because a plugin namespace keeps `verify` and `design` apart from Claude Code's built-in skills of those names.
- **User-invoked emission: keep the flag.** The design left this to S1 (a custom agent profile, or a skill without the flag). S1 shows the flag now does the right thing on both hosts: hidden from the agent, available to a person by name. A profile is not needed.
- **The verifier opens its own attempt.** §3 has custom agents "report back with their attempt token". The verifier keeps its token to itself and reports its attempt id and verdicts; the main session never holds a verifier token.
- **Review informs, verify decides.** The core accepts assessments from verifier attempts only, so a reviewer attempt records its findings in its end note and a file, and the verifier checks the act-on findings. A reproduced finding fails its criterion and R1 sends the task back.
- **Guided attempts are found through a file.** The headless hooks read the attempt from the session's environment, which a person's session does not have. `.interlock/guided-attempt.json` names the attempt the guided session opened last; one guided attempt per checkout.

## Limits

- **Copilot was not run with a real model.** Its guided runs exercise the real CLI, skills, hooks, custom agent and permissions, with a scripted model choosing every tool call. They prove the plumbing, not that a model follows the skills. The live evidence that a model follows them is from Claude Code.
- **The interactive Copilot TUI was driven only for S1.** The slash-command finding comes from a pseudo-terminal run of the real TUI against the scripted model. The P1 runs use `copilot -p`, where user-invoked skills cannot be used at all; no P1 workflow needs one.
- **"Ask" was observed only where nobody can answer.** In `copilot -p` and `claude -p` an ask becomes a denial ("unable to ask user for confirmation" on Copilot; a permission denial on Claude Code). Neither host's interactive prompt for a hook ask was exercised with a person.
- **The live runs are one each, on small repositories.** A model following the skills once is evidence they work, not a measure of how often. In both, the person's request named the route skill (`/interlock:route ...`); the model chose every later skill, the hand-off, and every interlock command.
- **Attempt tokens and the guided-attempt file are bookkeeping, not security.** A session with shell access can read the file, and the main session could open a verifier attempt itself. Independence rests on the verify skill handing off and on the custom agent's restricted tools, as in the design.
- **Shell writes are not placement-checked per call.** The hooks place edit-tool writes; a `sed -i` outside the worktree is not stopped per call. Scope is enforced at G3 on the output tree, and the repository's own files are never part of an attempt's output.
- **One guided attempt per checkout at a time.** Two people driving two tasks in one checkout would share the guided-attempt file.
- **The validator reads a YAML subset.** Anchors, flow mappings and multi-line plain scalars are reported as unsupported.
- **Feature and refactor have skills and templates but no end-to-end run.** The P1 gate names investigation and bug fix; those are the workflows run.
