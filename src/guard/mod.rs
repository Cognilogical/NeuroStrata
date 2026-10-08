//! Guard module - behavioral constraint validation
//!
//! Ported from NeuroCortex: semantic action validation using vector similarity
//! against stored behavioral rules. Uses LadyBugDB instead of LanceDB.

pub mod models;
pub mod sandbox;
pub mod semantic;
pub mod validator;

pub use models::{ValidateRequest, ValidateResponse, ValidateVerdict, BehavioralRule};
pub use validator::GuardValidator;
