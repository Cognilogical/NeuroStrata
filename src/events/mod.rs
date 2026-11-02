//! The thalamic bus: memory mutations published as pulses, so bookkeeping that
//! used to depend on an agent remembering to do it happens because the write
//! happened.
//!
//! This module is the vocabulary -- what a pulse is, who may consume one, and
//! the guard that stops a subscriber from reacting to its own write -- plus
//! the shipped subscribers. The runtime is live: `daemon.rs` and
//! `server.rs` wire a `ThalamicBus` to the store and register the production
//! subscribers (see `daemon.rs`'s `build_thalamic_bus`), and every remember /
//! archive / delete surface emits `ThalamicPulse`s through it. Subscribers
//! here do the bookkeeping the pulses imply: episodic-buffer pointers
//! ([`EpisodicPointerEcho`]), the per-namespace export-freshness flag
//! ([`ExportFreshnessDirty`], read by `export-graph`), the guard's audit rows
//! ([`GuardEventLog`]), and the post-ingest relation normalization
//! (`subscribers::PostIngestNormalize`).

mod bus;
mod event;
mod subscriber;
pub(crate) mod subscribers;

pub use self::bus::ThalamicBus;
pub use self::event::ThalamicPulse;
pub use self::subscriber::{
    BackpressureEvent, BusMetrics, MemorySubscriber, RecursionToken, SubscriberContext,
    SubscriberError,
};
pub use self::subscribers::{
    check_export_freshness, clear_export_freshness, EpisodicPointerEcho, ExportFreshnessDirty,
    GuardEventLog,
};
