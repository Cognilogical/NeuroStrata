//! The thalamic bus: memory mutations published as pulses, so bookkeeping that
//! used to depend on an agent remembering to do it happens because the write
//! happened.
//!
//! This module is only the vocabulary -- what a pulse is, who may consume one,
//! and the guard that stops a subscriber from reacting to its own write. The
//! runtime that carries pulses to subscribers is added alongside the daemon
//! wiring; until then nothing emits, and every type here is unreachable from
//! `main`.

mod bus;
mod event;
mod subscriber;
pub(crate) mod subscribers;

pub use self::bus::{SubscriberId, ThalamicBus};
pub use self::event::ThalamicPulse;
pub use self::subscriber::{
    BackpressureEvent, BusMetrics, MemorySubscriber, RecursionToken, SubscriberContext,
    SubscriberError,
};
pub use self::subscribers::{
    check_export_freshness, EpisodicPointerEcho, ExportFreshnessDirty, GuardEventLog,
};
