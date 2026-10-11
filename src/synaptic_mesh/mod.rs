//! SynapticMesh — the inter-instance synaptic layer that complements the
//! intra-instance SynapticGraph. See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`.
//!
//! Slice 1 landed the on-disk schemas (manifest, record); slice 2 landed the
//! daemon-owned storage (git checkout per mesh), the CLI dispatch surface,
//! and the MCP tool schemas. The outbox, mesh-worker, pull/replay, and
//! lifecycle enforcement land in follow-up slices.

pub mod cli;
pub mod manifest;
pub mod record;
pub mod storage;
pub mod storage_api;
pub mod tools;

pub use cli::{handle_synaptic_mesh_command, MeshContext, SynapticMeshCommands};
pub use manifest::{MeshManifest, MeshPolicy, Trust, MESH_MANIFEST_SCHEMA_VERSION};
pub use record::{
    Lifecycle, LifecycleState, MeshRecord, Relationship, MESH_RECORD_SCHEMA_VERSION,
};
pub use storage::MeshStorage;
pub use storage_api::StorageApi;
pub use tools::synaptic_mesh_tool_definitions;
