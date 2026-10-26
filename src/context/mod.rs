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

pub mod model;

pub use self::model::{
    ContextItem, ContextPlan, ContextRequest, Coverage, DecisionLifecycle, Degradation,
    EgressPolicy, Freshness, SemanticValidation, SourceCurrency, SourceIdentity,
    CONTEXT_PLAN_SCHEMA_VERSION,
};
