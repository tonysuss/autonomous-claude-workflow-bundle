# Host spike: headless sessions, tool filters and hooks

October 5, 2026. Claude Code 2.1.289 and Copilot CLI 1.0.91, both run in the development container.

Copilot CLI cannot sign in to GitHub there, so it ran in offline mode (`COPILOT_OFFLINE=true`) against a scripted OpenAI-compatible model on localhost (`COPILOT_PROVIDER_BASE_URL`). That exercises Copilot's real tool execution, permissions, hooks and output with no model calls. Claude Code ran against its normal model.

## One hooks plugin serves both hosts

Both hosts load a Claude-format plugin from `--plugin-dir`:

```
plugin/
  .claude-plugin/plugin.json      {"name": "...", "version": "..."}
  hooks/hooks.json                {"hooks": {"PreToolUse": [...], "Stop": [...]}}
```

Both send the same hook payloads to plugin hooks:

| Event | Fields seen |
| --- | --- |
| `PreToolUse` | `hook_event_name`, `session_id`, `tool_name` (`Bash`), `tool_input` (`command`, `description`), `cwd` |
| `Stop` | `hook_event_name`, `session_id`, `stop_hook_active`; Copilot adds `stop_reason` |

Claude Code also sends `transcript_path` and `permission_mode`; Copilot sends an ISO `timestamp`.

Claude Code honored plugin hooks with `--setting-sources ""`, which keeps the user's and the project's own settings out of the session.

## Responses that work on both

| Decision | Response | Claude Code | Copilot CLI |
| --- | --- | --- | --- |
| Deny a tool call | exit 2; `{"permissionDecision": "deny", "permissionDecisionReason": r}` on stdout; `r` on stderr | reason from stderr reaches the agent | reason from stdout reaches the agent |
| Hold the agent from stopping | exit 0; `{"decision": "block", "reason": r}` | reason reaches the agent; next stop has `stop_hook_active: true` | same; reason arrives as a user message |
| Allow | exit 0, no output | normal permission rules apply | normal permission rules apply |

Exit 2 alone denies on both, but Copilot then shows only "hook exited with code 2".

## Copilot's own hook config

Copilot also reads hooks from `config.json` under `COPILOT_HOME` (or `.github/hooks/*.json`) as `{"hooks": {"preToolUse": [{"type": "command", "bash": "...", "timeoutSec": 30}], "agentStop": [...]}}`. Those hooks get Copilot's native payloads: `toolName`, `toolArgs`, `cwd`, `sessionId`, and for `agentStop`, `stopReason`, `transcriptPath`, `stop_hook_active`. interlock uses the plugin route instead, so it neither replaces the user's `COPILOT_HOME` nor writes into the worktree.

## Native tool filters

- Claude Code: `--permission-mode dontAsk` with `--allowedTools` denies every tool not listed; `--disallowedTools` wins over allow. The final `result` event lists each denial in `permission_denials`.
- Copilot CLI: `--allow-tool shell --deny-tool 'shell(git push:*)'` denied the push before the hook's verdict mattered; the agent saw "Permission to run this tool was denied due to the following rules".

## Output streams

- Claude Code `--output-format stream-json --verbose`: `system` (init, hooks), `assistant`, `user` (tool results), and a final `result` with `is_error`, `result`, `num_turns`, `total_cost_usd`, `permission_denials`, `session_id`.
- Copilot CLI `--output-format json`: JSONL events such as `assistant.message` (`data.content`, `data.toolRequests`), `tool.execution_complete`, `assistant.turn_end`, and a final `result` with `exitCode`, `sessionId` and `usage`.

## Practical notes

- Pass the Claude Code prompt on stdin. With an argument, `claude -p` waits 3 seconds for stdin first, and variadic flags such as `--allowedTools` can swallow a trailing prompt.
- Copilot's built-in tools in this version: `bash`, `read_bash`, `stop_bash`, `list_bash`, `view`, `create`, `edit`, `grep`, `glob`, `task`, `skill`, `sql`, and others. Plugin hooks see the Claude names (`Bash`).

## Custom agents for headless sessions (October 6)

The design runs headless Copilot as `copilot -p` with `--agent`. Copilot CLI 1.0.91 was probed offline against a scripted model ([`evidence/skills/scripts/spike_model.py`](../evidence/skills/scripts/spike_model.py), logging the system prompt and the tools offered), with an agent profile in a plugin interlock writes ([raw results](../evidence/runtime/README.md#9-copilot-runs-headless-sessions-as-interlocks-agents)):

```
plugin/
  .claude-plugin/plugin.json      {"name": "interlock-hooks", ...}
  hooks/hooks.json
  agents/interlock-worker.agent.md
```

```
---
name: interlock-worker
description: ...
tools: ["view", "grep", "glob", "create", "edit", "bash"]
---

<the role's instructions>
```

What it showed:

- **`--plugin-dir` loads the plugin's agents, namespaced by the plugin.** `--agent interlock-worker` fails ("No such agent: interlock-worker, available: interlock-hooks:interlock-worker"); `--agent interlock-hooks:interlock-worker` runs. No file in the worktree or in the user's `COPILOT_HOME` is needed.
- **The body joins the system prompt.** It comes after Copilot's own system prompt, in an `<agent_instructions>` block that Copilot calls subordinate to its own instructions. It does not replace them, as S1's notes had it.
- **`tools` limits what the model is offered.** With no `tools` line every tool is offered. With a list, only the tools named, plus `skill` and `sql`, which are always offered. Tool names work: `view`, `grep`, `glob`, `bash` (with `read_bash`, `stop_bash`, `list_bash`), `create`, `edit`. Of the aliases, `execute` and `shell` give the shell tools, `agent` gives `task`, `list_agents`, `read_agent` and `write_agent`, and `read` gives `view`; `search` gives nothing in this version. An unknown name is ignored. Offline, no web tool or MCP server exists, so `web_fetch`, `web_search` and MCP names could not be checked.
- **The tool filters still hold.** Under `--agent`, `--allow-tool` still allows and `--deny-tool 'shell(git push:*)'` still denies ("Permission to run this tool was denied due to the following rules"), and the plugin's `PreToolUse` hook still runs on every call. The hook runs first: when it denies, its reason is what the agent sees. Its payload does not name the agent.
- **Copilot records the agent the session ran.** `COPILOT_HOME/session-state/<session id>/events.jsonl` has a `subagent.selected` event with `agentName` (`interlock-hooks:interlock-worker`) and its `tools`. The `-p --output-format json` stream does not carry it.

So the adapter now does it. For each headless session (`interlock run`, `interlock verify`) on a host that reports custom agents, it writes `agents/interlock-<role>.agent.md` into the run's hooks plugin, with the role's instructions as the body and, as `tools`, every tool the attempt's grant allows and does not deny outright, by Copilot's names; it passes `--agent interlock-hooks:interlock-<role>` and keeps every `--allow-tool` and `--deny-tool` flag. The role's instructions now reach the model as system instructions instead of leading the prompt, the verifier is never offered an edit tool, and neither role is offered `task` (sub-agents), which no workflow grants and which Copilot's permission flags do not gate. Tests: `run_copilot::each_session_runs_as_interlocks_agent_for_its_role_and_the_tool_filters_hold` (a whole run: the agent each session ran, from Copilot's session log; the instructions in the system prompt; the tools offered; denials), `run_copilot::copilot_keeps_the_deny_rules_for_a_session_run_as_an_agent` (the adapter's plan under a plugin with no hooks, so Copilot's own rules refuse `git push` and `git commit`), and `copilot::tests` for the profile.

This is not the design's native delegation: these are top-level agents interlock starts, one session each, not sub-agents the main agent hands work to. Guided sessions still verify through `interlock verify` (docs/skills.md).
