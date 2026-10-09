//! The v1 subscriber set: bookkeeping that happens because a memory was
//! written, not because an agent remembered to do it.
//!
//! Each subscriber is fire-and-forget -- it reads what it needs off the
//! [`MemorySubscriber`] context and returns; nothing a subscriber does can
//! reach back into the write that caused the pulse. See
//! [`EpisodicPointerEcho`] for the first one.

mod episodic_pointer_echo;
mod export_freshness_dirty;
mod guard_event_log;

pub use self::episodic_pointer_echo::EpisodicPointerEcho;
pub use self::export_freshness_dirty::ExportFreshnessDirty;
pub use self::guard_event_log::GuardEventLog;
