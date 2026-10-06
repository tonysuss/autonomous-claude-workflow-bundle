# Task templates

Copy one, then replace every `<...>`. Fields: `id`, `repository` (always `"."`), `workflow`, `intent`, optional `environment` (where checks run, like `linux-python3`), `[budget] max_attempts`, `[scope] paths`, and one `[[criterion]]` block per criterion with `id`, `statement`, optional `check`, `min_strength` (`observed`, `tested` or `static`), `producer` (`independent` or `self`), and optional `baseline` (`fails`, `passes` or `any`).

Scope: an empty or missing `[scope]` allows no change at all; `paths = ["**"]` allows any. An investigation may never change a file, whatever its scope.

## Investigation

```toml
id = "<short-id>"
repository = "."
workflow = "investigation"
intent = "<the question, in the person's words>"

[budget]
max_attempts = 2

[[criterion]]
id = "answer"
statement = "<the question> is answered with file and line citations, and a command shows the behavior the answer rests on"
min_strength = "observed"
producer = "independent"
```

Add a criterion with a `check` when part of the answer is a fact a command can decide now, such as "the export retries on a timeout".

## Bug fix

```toml
id = "<short-id>"
repository = "."
workflow = "bug-fix"
intent = "<what is broken, as reported, and what correct behavior looks like>"

[budget]
max_attempts = 3

[scope]
paths = ["<files or globs the fix may change>", "<tests/**>"]

[[criterion]]
id = "repro"
statement = "<the reported behavior no longer happens>"
check = "<command that exits non-zero while the bug is present>"
min_strength = "observed"
producer = "independent"
baseline = "fails"

[[criterion]]
id = "regression"
statement = "The existing tests pass"
check = "<the project's test command, one that finds and runs its tests>"
min_strength = "tested"
producer = "self"
baseline = "passes"
```

## Feature

```toml
id = "<short-id>"
repository = "."
workflow = "feature"
intent = "<the new behavior, who it is for, and what done looks like>"

[budget]
max_attempts = 3

[scope]
paths = ["<files or globs the feature may change>", "<tests/**>"]

[[criterion]]
id = "behavior"
statement = "<the new behavior, observable from outside>"
check = "<command that exercises the behavior and fails until it exists>"
min_strength = "observed"
producer = "independent"
baseline = "fails"

[[criterion]]
id = "regression"
statement = "The existing tests pass"
check = "<the project's test command>"
min_strength = "tested"
producer = "self"
baseline = "passes"
```

## Refactor

```toml
id = "<short-id>"
repository = "."
workflow = "refactor"
intent = "<the structure that changes, and the behavior that must not>"

[budget]
max_attempts = 3

[scope]
paths = ["<files or globs the refactor may change>"]

[[criterion]]
id = "pin"
statement = "<the behavior contract: what callers observe stays the same>"
check = "<characterization or equivalence check that passes today>"
min_strength = "tested"
producer = "independent"
baseline = "passes"

[[criterion]]
id = "shape"
statement = "<the target structure, for example: retry logic lives in one function that both exporters call>"
min_strength = "static"
producer = "independent"

[[criterion]]
id = "regression"
statement = "The existing tests pass"
check = "<the project's test command>"
min_strength = "tested"
producer = "self"
baseline = "passes"
```
