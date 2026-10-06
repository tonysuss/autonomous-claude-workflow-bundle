//! The supervisor's delivery step: a verified task that needs integration
//! goes to the forge through `interlock-forge`, configured from the
//! environment (INTERLOCK_GH_BIN, INTERLOCK_FORGE_*).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use chrono::Utc;
use interlock_forge::{DeliverConfig, DeliverError, GhForge};
use interlock_schema::State;
use interlock_store::Store;

use crate::run::{Result, RunError};

impl From<DeliverError> for RunError {
    fn from(e: DeliverError) -> RunError {
        match e {
            DeliverError::Store(s) => RunError::Store(s),
            other => RunError::Other(other.to_string()),
        }
    }
}

/// One delivery step for a verified or integrating task. Returns why the run
/// should stop, or `None` when the task moved and the loop should look again.
/// Without landing authority, G5 blocks the task at verified with the reason.
pub(crate) fn step(store: &mut Store, repo: &Path, task_id: &str, cancel: &AtomicBool) -> Result<Option<String>> {
    let task = store.task(task_id)?;
    // R2 when the evidence went stale (for an integrating task too), G7 when no delivery is needed.
    if !store.advance(task_id, Utc::now())?.is_empty() {
        return Ok(None);
    }
    let cfg = match DeliverConfig::from_env() {
        Ok(cfg) => cfg,
        Err(e) => return Ok(Some(format!("cannot integrate: {e}"))),
    };
    let forge = GhForge::from_env(repo);
    let delivery = interlock_forge::integrate(store, repo, &forge, &cfg, task_id, cancel)?;
    let after = store.task(task_id)?.state;
    if after != task.state && after != State::Integrating {
        return Ok(None);
    }
    Ok(Some(delivery.stopped_because))
}
