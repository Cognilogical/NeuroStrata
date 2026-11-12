//! Guard module - behavioral constraint validation
//!
//! Provenance: a standalone guard engine folded into NeuroStrata — decision
//! record in docs/architecture/. Action validation against stored behavioral
//! rules: the candidate rules are listed, and each fires when its trigger pattern
//! matches the action (no vector similarity decides a verdict; see
//! `semantic::SemanticEvaluator::candidate_rules`). Uses LadyBugDB instead of LanceDB.

pub mod fsm;
pub mod models;
pub mod sandbox;
pub mod semantic;
pub mod validator;

#[cfg(test)]
pub(crate) mod testkit;

pub use models::{BehavioralRule, ValidateRequest, ValidateResponse};
pub use validator::GuardValidator;
