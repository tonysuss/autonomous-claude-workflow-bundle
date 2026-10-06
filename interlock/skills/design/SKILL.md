# Design

Sketch whole alternatives (types, signatures, module boundaries) before writing code, compare them on a rubric drawn from the task's criteria, and pick one. It costs real time, so it earns its place only when both hold:

- **Uncertainty:** more than one shape is plausible, and reading the code does not settle which.
- **Impact:** the change crosses function or module boundaries, and the wrong shape would be expensive to undo.

If either is missing, say so in one line and go straight to {{skill:implement}}. Read `references/interlock-basics.md` once per session.

## 1. Ground

```bash
interlock brief <id>
interlock status <id>
```

The brief's goal, scope and criteria are the constraints every design must meet. Trace the code the change touches with {{skill:investigate}} (steps 3 to 5). Naming a file is not grounding: know how data flows through it.

## 2. Frame the rubric

Turn the criteria and the goal into three to six gradeable points, for example: meets each criterion; stays inside the scope; smallest public surface for what it hides; a change that looks right from one file is right for the whole repository; fewest new layers and states.

## 3. Sketch at least two

Write two or more structurally different designs, not variations on one. Start each from the caller's usage, then derive types and signatures, with bodies left as `not implemented` or pseudocode. Prefer the design that hides more behind a smaller interface. Subtract first (`references/subtract-before-you-add.md`): the better sketch is often the smaller one.

If your host can run sub-agents, give each sketch to a separate one with the same prompt and rubric, so the candidates are independent.

## 4. Pick and record

Score each sketch against the rubric point by point. Pick a base; graft the one or two best ideas from the others by hand so the result keeps one mental model. If the sketches diverge wildly, the framing was wrong: reframe rather than average.

Write the decision with `references/rationale.md` and keep it on the task, where it outlives the session and no hook stands in its way:

```bash
interlock note add --task <id> --kind design --file - <<'EOF'
<the rationale>
EOF
```

Show the person the comparison table.

## 5. Implement against it

Invoke {{skill:implement}} with the task id. Put the chosen design in the result summary. If implementation keeps fighting the sketch (the same workaround in unrelated places, escape hatches in the types, callers needing to know internals), throw the sketch out and come back here rather than patching around it.
