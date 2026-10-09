//! Guard module - behavioral constraint validation
//!
//! Provenance: a standalone guard engine folded into NeuroStrata — decision
//! record in docs/architecture/. Semantic action validation using vector similarity
//! against stored behavioral rules. Uses LadyBugDB instead of LanceDB.

pub mod models;
pub mod sandbox;
pub mod semantic;
pub mod validator;

pub use models::{ValidateRequest, ValidateResponse, ValidateVerdict, BehavioralRule};
pub use validator::GuardValidator;
