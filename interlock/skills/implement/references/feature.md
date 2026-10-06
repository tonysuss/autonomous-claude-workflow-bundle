# Feature playbook

You own the design. The behavior criterion says what done looks like; build the least that meets it.

1. **Record the before.** Have interlock run every check on the input snapshot (`--target base`). The behavior check fails there because the behavior does not exist yet; read why it fails.
2. **Ground it.** Trace the code the feature touches (investigate, steps 3 to 5).
3. **Name the data shape first.** Write down the types or records the feature needs and the structure that organizes them: a state machine over scattered flags, a table over branching, a typed model over repeated shape assumptions. If several shapes are plausible and the change crosses a function boundary, the person can run the design skill.
4. **Build it** in small steps inside the scope, each ending in a check that passes. Subtract before you add (`references/subtract-before-you-add.md`).
5. **Test the behavior** the way a user would see it (`references/test-behavior-not-implementation.md`).
6. **Verify on the matching surface.** Have interlock run every check on your files. Inconclusive is not a pass.
7. **Claim and submit** (implement steps 4 and 5).

In the result summary: what you built, the shape you chose and why, and anything left open as an open question (`--question`).
