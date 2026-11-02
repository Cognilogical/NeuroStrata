//! Durable, event-keyed receipts for recall/procedure/tool-invocation
//! outcomes. A receipt is the retention record `retention::gate` verifies
//! against: keyed by `(session_id, task_id, event_id)` so a replayed event
//! cannot double-write, carrying the exposed memory ids and (optionally) the
//! worktree binding captured at completion time. `store::ReceiptStore` is the
//! in-process sink; the IPC/task surface stamps what it ran and whether the
//! worktree state it captured is still current.

pub mod store;

