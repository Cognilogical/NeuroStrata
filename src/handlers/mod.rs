//! Request handlers that are not part of the MCP tool surface.
//!
//! Each one is still reachable from `tools/call`; they live out here rather
//! than inline in `server.rs` so the daemon's HTTP routes and the CLI can call
//! exactly the function the tool surface calls, instead of a second
//! implementation that can drift from the first.

pub mod archive_memory;
pub mod bus_metrics;

// Test-only re-export: `src/main.rs`'s `bus-metrics` CLI test calls the handler
// through the short path. Production callers use the fully-qualified
// `crate::handlers::bus_metrics::…`, so this stays out of the shipped surface.
#[cfg(test)]
pub use self::bus_metrics::handle_bus_metrics;