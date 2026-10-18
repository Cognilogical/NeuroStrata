//! SynapticMesh — the inter-instance synaptic layer that complements the
//! intra-instance SynapticGraph. See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`.
//!
//! Slice 1 landed the on-disk schemas (manifest, record); slice 2 landed the
//! daemon-owned storage (git checkout per mesh), the CLI dispatch surface,
//! and the MCP tool schemas. Slice 3 lands the publish path: the durable
//! outbox and the mesh-worker that drains it. Pull/replay, lifecycle
//! enforcement, and conflict resolution land in follow-up slices.

pub mod cli;
pub mod conflict;
pub mod import;
pub mod manifest;
pub mod mesh_service;
pub mod outbox;
pub mod record;
pub mod storage;
pub mod storage_api;
pub mod tools;
pub mod worker;

#[cfg(test)]
mod acceptance;

pub use cli::{handle_synaptic_mesh_command, MeshContext, SynapticMeshCommands};
pub use mesh_service::{
    conventional_base_root, handle_mesh_tool_call, resolve_node_id, MeshService,
};
pub use tools::synaptic_mesh_tool_definitions;
// manifest/record/storage/storage_api types are reached through their module
// paths by internal consumers (`storage::MeshStorage`, `super::manifest::…`);
// re-exporting names no external caller uses yet is noise, not surface.
