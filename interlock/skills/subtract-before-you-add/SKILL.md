# Subtract before you add

Remove complexity first, then build. Adding to a complex system compounds it; removing first leaves less code, shows the essential structure, and often makes the next step obvious.

- Delete dead code, one-caller wrappers, redundant validators and stale references before introducing the new shape.
- Build for the usage you can see, the request and what the existing code already accepts, not for edge cases you imagine. No speculative parsers, flags or guards.
- The smallest change that achieves the whole goal ships. A cleanup that "might help" and that the goal does not need gets reverted.
- When an internal API is replaced, move every caller and delete the old API in the same change. No compatibility shims for callers you control.
- Measure the result by reader load: fewer layers to trace and less state to hold. If the diff does not lower it somewhere, reconsider it.
