# Route

Pick the workflow, write the criteria that will decide when the work is done, and create the interlock task. Nothing advances on your word: interlock moves the task only when the evidence the criteria name exists. Read `references/interlock-basics.md` once per session.

## 1. Pick the workflow

| The request | Workflow | Next skill |
| --- | --- | --- |
| How does X work, why is Y built this way, are we sure about Z, should we do X or Y. Read-only; the deliverable is a cited answer | `investigation` | {{skill:investigate}} |
| A reported defect: something that should work does not | `bug-fix` | {{skill:implement}} |
| New or changed behavior | `feature` | {{skill:implement}} |
| The structure changes and the behavior must not (rename, extract, inline, dedupe, move) | `refactor` | {{skill:implement}} |

If a refactor turns up a real bug or a missing feature, that is a second task. If nothing fits, or the request is a one-line answer from what you already know, answer it without a task.

## 2. Write the criteria

Copy the template for the workflow from `references/task-templates.md`. Each criterion is one statement that must hold, and, wherever a command can show it, a `check` that interlock will run itself.

- **Make each check fail for the right reason.** A reproduction must exit non-zero while the problem exists (`baseline = "fails"`). A regression guard must pass today and run real tests (`baseline = "passes"`). interlock runs both on the input snapshot and refuses a task whose checks prove nothing.
- **Short checks go inline** (`python3 -c '...'`, `cargo test name`), so nothing needs committing. Files a check runs are protected: no result may change them.
- **A longer check needs a script under `checks/`, committed before step 4,** because the input snapshot is a commit. That commit lands on the person's current branch, so tell them first: name the file, the branch, and the commit message, and wait for their go-ahead when they are at the keyboard. If they decline, keep the check inline.
- **Independent where it matters.** The criterion that captures the request gets `producer = "independent"`: only an independent verifier can pass it. Routine guards can be `producer = "self"`.
- **Scope** lists the files and globs the work may change. An empty scope allows no change at all, which is right for an investigation; `["**"]` allows any. A result that changes anything else is rejected.

Show the person the criteria in a few lines and go on. Ask only if the request is genuinely ambiguous about what done means.

## 3. Create the task

From the repository root, with the task's TOML on stdin, so no file is written:

```bash
interlock init                                  # once per repository; safe to repeat
interlock task create - <<'EOF'
id = "<id>"
...the template, filled in...
EOF
```

Task ids are short, lowercase and hyphenated, like `export-retry`. A refusal names the field to fix.

## 4. Record the input snapshot

```bash
interlock task ready <id> --base "$(git rev-parse HEAD)"
interlock status <id>
```

The status should read `ready`. Uncommitted files are not in the snapshot.

## 5. Hand off

Invoke the next skill from the table in step 1 with the task id. Tell the person the task id, the workflow, and each criterion in one line.
