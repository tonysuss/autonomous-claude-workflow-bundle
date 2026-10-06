//! Runs interlock tasks on a host: git worktrees per attempt, worker and
//! verifier sessions, the hooks both hosts call, and restart reconcile.

pub mod checks;
pub mod config;
mod delivery;
pub mod export;
pub mod git;
pub mod guided;
pub mod hook;
mod lock;
pub mod prompts;
mod run;
pub mod sessions;
pub mod state_paths;

pub use lock::ControllerLock;
pub use run::{RunConfig, RunError, RunReport, SessionReport, Supervisor};
