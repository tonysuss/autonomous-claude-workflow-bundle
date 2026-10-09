# Refactor playbook

The structure changes. The behavior does not. If the cleanup turns up a real bug or a missing feature, note it as an open question and leave it for its own task.

1. **Pin the behavior first.** The `pin` criterion's check captures what callers observe. Have interlock run it on the input snapshot (`--target base`): it must pass there and must test something. If the area has no coverage, the task needs a better pin before any structure moves; tell the person.
2. **Name the structure that is missing** and the target shape: module layout, types, call graph as you would build them today. The reshape must delete branches or invalid states, not add indirection.
3. **Subtract first.** Delete dead code, collapse one-caller wrappers, remove stale references (`references/subtract-before-you-add.md`).
4. **Move in small steps,** each keeping the pin green: rerun its check after each step. When you replace an internal API, move every caller and delete the old one in the same change. Check every rename against the files: renames miss uses in strings and docs.
5. **Prove behavior is unchanged** on the real artifact, not "it compiles" (`references/prove-it-works.md`). Have interlock run every check on your files.
6. **Keep it only if it lowers reader load.** If the diff does not make the code easier to follow somewhere, revert it.
7. **Claim and submit** (implement steps 4 and 5). Claim the `shape` criterion with `static` and a note naming the structure that changed.

In the result summary: the structure that changed, the pin it was held against, and what you reverted.
