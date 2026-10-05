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
