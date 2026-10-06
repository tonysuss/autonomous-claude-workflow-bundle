//! Role instructions for headless sessions. They tell the agent how to report
//! through interlock; the hooks enforce what the instructions ask.

use interlock_schema::{Criterion, MinStrength};

pub const WORKER_SYSTEM: &str = "You are the worker on an interlock task. interlock records your evidence and \
decides whether the work is done; you cannot mark anything done yourself. Report only through the `interlock` \
command. If a tool call is denied, the reason says why: do not try to work around it.";

pub const VERIFIER_SYSTEM: &str = "You are the independent verifier on an interlock task. Another agent did the \
work. You decide, with evidence you observed yourself, whether the exact files in your working directory achieve \
the task's goal and whether each criterion holds. Report only through the `interlock` command.";

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

/// What the worker is asked to do about the goal. The criteria restate the
/// goal; they are evidence that it is met, not a narrower target.
const WORKER_GOAL: &str = "The goal is the target; the criteria are how its evidence is recorded, not a narrower \
target. Fix the cause, not the symptom, and fix it wherever it occurs inside the task's scope: search for other call \
sites and code paths with the same defect. Handle the inputs the goal implies, such as the forms the existing code \
already accepts. Change nothing the goal does not need. If you leave part of the goal undone, say which part and why.";

/// What the verifier is asked to judge beyond the criteria's wording.
pub const VERIFIER_GOAL: &str = "Judge the work against the task's goal, not only the criteria's wording. A \
criterion that restates part of the goal holds only if the change achieves that part wherever it applies. Read the \
change, search the code around it for other call sites and code paths with the same defect, and try inputs the goal \
implies, such as the forms the existing code already accepts. A gap the worker's summary names is still a gap. Do not \
fail work for what the goal does not ask for, such as style or extra features.";

/// How the verifier records a gap: `failed` sends the work back to a worker who can close it;
/// `blocked` keeps the task from done and puts it in front of the operator, for a gap no worker
/// can close inside the task.
pub const VERIFIER_GAPS: &str = "Record every gap; none is only a remark in a note. When a criterion's statement \
covers the missing part and the worker can close it inside the task's scope, record `failed` on that criterion. When \
no criterion covers it, or the task's scope does not let the worker close it, record `blocked` on the criterion \
closest to it: only a person can settle that. Either way, the note says what is missing, where, and the input or \
command that shows it; for `blocked`, it also says why the worker cannot close it.";

/// What `interlock check run` does with the tree, so a verifier knows its own directory cannot
/// sway a check.
pub const VERIFIER_CHECKS: &str = "interlock runs the check itself, always in a clean checkout of the tree in your \
working directory, so files the tree leaves out (ignored or generated ones) cannot affect it, and records the output; \
read it.";

/// Where the verifier may write: the files it judges are the evidence.
pub const VERIFIER_SCRATCH: &str = "Never create, change or delete a file in your working directory, not even for \
a moment: its files are what you are judging. To try inputs or run a probe of your own, copy the tree first \
(`cp -r . /tmp/<name>`) and write only in the copy, or put scratch files under /tmp.";

pub fn worker(brief: &str) -> String {
    format!(
        "{brief}\n## How to work\n\n\
1. You are in a fresh git worktree at the task's starting point. Work inside the task's scope. {WORKER_GOAL}\n\
2. Do not commit, push or create branches. interlock records this worktree's files when you finish.\n\
3. For a bug fix, reproduce the problem first, then fix it.\n\
4. After your last edit, have interlock run each criterion's check on your files: \
`interlock check run --criterion <id>`. interlock runs the check itself and records the output; read it. Then \
record one claim per criterion you checked:\n\n\
   `interlock claim add --criterion <id> --strength <strength> --tree auto --ref \"<command you ran>\" --note \"<what you saw>\"`\n\n\
   {STRENGTHS} A claim made before a later edit stops counting, so record claims last.\n\
5. Criteria that need an independent producer are checked by a separate verifier, which judges the work against \
the goal as well as the criteria. Your claims inform it but do not satisfy them.\n\
6. Finish with two or three sentences: what you changed, where the cause was, and how you checked it.\n"
    )
}

pub fn verifier(brief: &str) -> String {
    format!(
        "{brief}\n## How to verify\n\n\
1. Do not commit or push anything. {VERIFIER_SCRATCH}\n\
2. For each criterion with a check, have interlock run it here: `interlock check run --criterion <id>`. \
{VERIFIER_CHECKS} For a criterion without a check, examine the work yourself.\n\
3. {VERIFIER_GOAL}\n\
4. {VERIFIER_GAPS}\n\
5. Record exactly one assessment per criterion:\n\n\
   `interlock assess add --criterion <id> --strength <strength> --ref \"<command you ran>\" --note \"<what you saw>\"`\n\n\
   {STRENGTHS} Use `observed` or `tested` only for a pass you saw. Never record a pass you did not see.\n\
6. The worker's summary is context, not evidence.\n\
7. Finish with one line per criterion: its id, your verdict, and why.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIEF: &str = "# Task t1 (Worker brief)\n\nGoal: make the thing work.\n";

    /// The numbered steps, in order, each by its first words.
    fn steps(prompt: &str) -> Vec<String> {
        prompt
            .lines()
            .filter(|l| l.len() > 3 && l.as_bytes()[0].is_ascii_digit() && &l[1..3] == ". ")
            .map(|l| l[3..].split_whitespace().take(4).collect::<Vec<_>>().join(" "))
            .collect()
    }

    #[test]
    fn the_worker_is_sent_to_the_cause_wherever_it_occurs_not_to_the_smallest_change() {
        let p = worker(BRIEF);
        assert!(p.starts_with(BRIEF), "the brief, with its goal, comes first");
        assert!(!p.contains("smallest change"), "{p}");
        assert_eq!(
            steps(&p),
            [
                "You are in a",
                "Do not commit, push",
                "For a bug fix,",
                "After your last edit,",
                "Criteria that need an",
                "Finish with two or",
            ]
        );
        for want in [
            "not a narrower target",
            "Fix the cause, not the symptom",
            "wherever it occurs inside the task's scope",
            "other call sites and code paths with the same defect",
            "inputs the goal implies",
            "Change nothing the goal does not need",
            "say which part and why",
        ] {
            assert!(p.contains(want), "worker prompt lacks {want:?}: {p}");
        }
    }

    #[test]
    fn the_verifier_judges_the_goal_and_records_every_gap_as_failed_or_blocked() {
        let p = verifier(BRIEF);
        assert!(p.starts_with(BRIEF), "the brief, with its goal, comes first");
        assert!(p.contains("## How to verify"), "the heading other code looks for");
        // Scratch rule first, then checks, then the goal, then gaps, then the record.
        assert_eq!(
            steps(&p),
            [
                "Do not commit or",
                "For each criterion with",
                "Judge the work against",
                "Record every gap; none",
                "Record exactly one assessment",
                "The worker's summary is",
                "Finish with one line",
            ]
        );
        for want in [
            "Judge the work against the task's goal, not only the criteria's wording",
            "other call sites and code paths with the same defect",
            "inputs the goal implies",
            "A gap the worker's summary names is still a gap",
            "Do not fail work for what the goal does not ask for",
            "the worker can close it inside the task's scope, record `failed` on that criterion",
            "or the task's scope does not let the worker close it, record `blocked` on the criterion closest to it",
            "what is missing, where, and the input or command that shows it",
            "Never create, change or delete a file in your working directory, not even for a moment",
            "copy the tree first (`cp -r . /tmp/<name>`) and write only in the copy",
            "always in a clean checkout of the tree in your working directory",
        ] {
            assert!(p.contains(want), "verifier prompt lacks {want:?}: {p}");
        }
        // A known gap is never only a remark: no wording lets a note stand in for a verdict.
        assert!(!p.contains("is not a failure") && !p.contains("mention it in your note"), "{p}");
        assert!(!p.contains("Do not modify, commit or push anything here"), "the old scratch rule allowed nothing");
        assert!(VERIFIER_SYSTEM.contains("achieve the task's goal"));
    }

    /// The guided verifier agent judges and records gaps with the same words as the headless
    /// verifier, so the two cannot drift apart.
    #[test]
    fn the_guided_verifier_agent_uses_the_same_judging_text() {
        let agent = include_str!("../../../agents/verifier/AGENT.md");
        for (name, text) in [
            ("VERIFIER_GOAL", VERIFIER_GOAL),
            ("VERIFIER_GAPS", VERIFIER_GAPS),
            ("VERIFIER_SCRATCH", VERIFIER_SCRATCH),
            ("VERIFIER_CHECKS", VERIFIER_CHECKS),
        ] {
            assert!(agent.contains(text), "agents/verifier/AGENT.md lacks {name}'s text verbatim:\n{text}");
        }
    }

    #[test]
    fn the_prompts_name_no_task_and_no_language() {
        // The guidance is general: it must not carry any task's details.
        for p in [worker(BRIEF), verifier(BRIEF)] {
            let body = p.strip_prefix(BRIEF).unwrap();
            for word in ["section", "duration", "dotted", "Lookup", "day", "Go ", "Python", "CLI's"] {
                assert!(!body.contains(word), "{word:?} in {body}");
            }
        }
    }
}
