# Fix root causes

Do not fix symptoms. Trace each problem to its cause and fix it there. Symptom fixes pile up, each makes the system harder to reason about, and the real bug stays.

- **Reproduce first.** Have interlock run the reproduction on the input snapshot and watch it fail. A bug you have not seen fail is a guess.
- **Narrow it down.** List the candidate causes, then rule them out with runtime evidence, taking the test that cuts the most possibilities each time. When the state is unclear, add a print or a log line, run it, and read it. Do not guess.
- **Ask why until it stops.** The line that throws is rarely the cause. Ask why that value was wrong, then why again.
- **No silencing guards.** A null check that hides a crash is a symptom fix. So is a retry around a race.
- **Fix the pattern, not the instance.** Search for the same mistake elsewhere in scope and fix every copy, or say why not.
- **Only what the evidence justifies ships.** A change added because it "might help" is a hypothesis. If the evidence does not need it, take it out.
- **Restart bugs: suspect state first.** When something fails only after a restart, look at what persisted (caches, lock files, saved state) before the code.
