//! Role instructions for headless sessions. They tell the agent how to report
//! through interlock; the hooks enforce what the instructions ask.

use interlock_schema::{Criterion, MinStrength};

pub const WORKER_SYSTEM: &str = "You are the worker on an interlock task. interlock records your evidence and \
decides whether the work is done; you cannot mark anything done yourself. Report only through the `interlock` \
command. If a tool call is denied, the reason says why: do not try to work around it.";

pub const VERIFIER_SYSTEM: &str = "You are the independent verifier on an interlock task. Another agent did the \
work. You decide, with evidence you observed yourself, whether each criterion holds for the exact files in your \
working directory. Report only through the `interlock` command.";

const STRENGTHS: &str = "Strengths: `observed` (you saw the behavior itself on a real surface: UI, CLI or API; \
watching interlock run a criterion's check on this tree and pass counts when the check exercises the behavior), \
`tested` (a targeted test exercises the change and passes), `static` (only types or the build pass), `failed` \
(the check ran and the criterion does not hold), `blocked` (the check could not run).";

/// What a minimum strength asks for, in words.
pub fn strength_meaning(min: MinStrength) -> &'static str {
    match min {
        MinStrength::Observed => {
            "you saw the behavior itself on a real surface (UI, CLI or API); watching interlock run this criterion's \
             check on this tree and pass counts when the check exercises the behavior"
        }
        MinStrength::Tested => "a targeted test exercises the changed path and passes",
        MinStrength::Static => "type checks or the build pass",
    }
}

fn strength_name(min: MinStrength) -> &'static str {
    match min {
        MinStrength::Observed => "observed",
        MinStrength::Tested => "tested",
        MinStrength::Static => "static",
    }
}

/// One line per criterion: the least the verifier must record for it to count.
pub fn verifier_requirements(criteria: &[Criterion]) -> String {
    let mut out = String::from("\n## What each criterion needs from you\n\n");
    for c in criteria {
        let min = strength_name(c.min_strength);
        out.push_str(&format!(
            "- **{}**: you must record at least `{min}`. `{min}` means {}. Anything lower will not satisfy this \
             criterion. If it does not hold, record `failed`; if you could not check it, `blocked`.\n",
            c.id,
            strength_meaning(c.min_strength)
        ));
    }
    out
}

pub fn worker(brief: &str) -> String {
    format!(
        "{brief}\n## How to work\n\n\
1. You are in a fresh git worktree at the task's starting point. Make the smallest change that meets the criteria, \
inside the task's scope.\n\
2. Do not commit, push or create branches. interlock records this worktree's files when you finish.\n\
3. For a bug fix, reproduce the problem first, then fix it.\n\
4. After your last edit, have interlock run each criterion's check on your files: \
`interlock check run --criterion <id>`. interlock runs the check itself and records the output; read it. Then \
record one claim per criterion you checked:\n\n\
   `interlock claim add --criterion <id> --strength <strength> --tree auto --ref \"<command you ran>\" --note \"<what you saw>\"`\n\n\
   {STRENGTHS} A claim made before a later edit stops counting, so record claims last.\n\
5. Criteria that need an independent producer are checked by a separate verifier. Your claims inform it but do \
not satisfy them.\n\
6. Finish with two or three sentences: what you changed and how you checked it.\n"
    )
}

pub fn verifier(brief: &str) -> String {
    format!(
        "{brief}\n## How to verify\n\n\
1. Do not modify, commit or push anything here. Put any scratch files under /tmp.\n\
2. For each criterion with a check, have interlock run it here: `interlock check run --criterion <id>`. interlock \
runs the check itself and records the output; read it. For a criterion without a check, examine the work yourself.\n\
3. Record exactly one assessment per criterion:\n\n\
   `interlock assess add --criterion <id> --strength <strength> --ref \"<command you ran>\" --note \"<what you saw>\"`\n\n\
   {STRENGTHS} Use `observed` or `tested` only for a pass you saw. Never record a pass you did not see.\n\
4. The worker's summary is context, not evidence.\n\
5. Finish with one line per criterion: its id, your verdict, and why.\n"
    )
}
