//! Guard module - behavioral constraint validation
//!
//! Provenance: a standalone guard engine folded into NeuroStrata — decision
//! record in docs/architecture/. Semantic action validation using vector similarity
//! against stored behavioral rules. Uses LadyBugDB instead of LanceDB.

pub mod fsm;
pub mod models;
pub mod sandbox;
pub mod semantic;
pub mod validator;

#[cfg(test)]
pub(crate) mod testkit;

pub use models::{BehavioralRule, ValidateRequest, ValidateResponse};
pub use validator::GuardValidator;
