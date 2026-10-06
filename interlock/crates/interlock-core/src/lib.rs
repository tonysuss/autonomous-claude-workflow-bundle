//! The policy core. Pure functions only: no file, network, or clock access.
//! Callers pass the current time and every record a decision needs, and get
//! back the updated records plus the move that was made, or a refusal.

pub mod brief;
pub mod budget;
pub mod capability;
pub mod classify;
pub mod delivery;
pub mod digest;
pub mod evidence;
pub mod grants;
pub mod lifecycle;
pub mod scope;
pub mod tools;
pub mod vacuity;
pub mod workflow;

pub use capability::{Capability, CapabilityCheck, CapabilitySet, Fallback, Requirement};
pub use evidence::{CriterionVerdict, EvidenceReport, evaluate};
pub use lifecycle::{Move, Refusal, RefusalCode, Signal};
pub use workflow::{Mode, Workflow};
