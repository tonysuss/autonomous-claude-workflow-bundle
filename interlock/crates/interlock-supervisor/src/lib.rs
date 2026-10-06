//! Runs interlock tasks on a host: git worktrees per attempt, worker and
//! verifier sessions, the hooks both hosts call, and restart reconcile.

pub mod checks;
pub mod git;
pub mod guided;
pub mod hook;
pub mod prompts;
mod run;
pub mod state_paths;

pub use run::{RunConfig, RunError, RunReport, SessionReport, Supervisor};
