//! Tool handlers that do not belong to one of the existing surfaces
//! (memory, task, guard). Each tool is still served from
//! `process_mcp_request`, as every other tool is.

pub mod procedure;