//! The context read-planning boundary.
//!
//! `context/` owns read planning and rendering only. It does not own
//! publication, receipts, or host egress: storage owns generations, stubs,
//! and receipts; adapters own host-side injection policy; the server owns
//! ranking policy.
//!
//! Nothing in this module may compute freshness or rank independently of the
//! shared service -- one plan, one renderer, one answer, so the adapter and
//! the server can never disagree about what was planned.

pub mod freshness;
pub mod model;
pub mod plan;
pub mod render;
pub mod service;
pub mod source;

pub use self::model::{ContextItem, CONTEXT_PLAN_SCHEMA_VERSION};
#[cfg(test)]
pub use self::model::{ContextRequest, EgressPolicy, SourceIdentity};
pub use self::plan::Budget;
pub use self::render::render as render_plan_text;
pub use self::service::ContextService;
