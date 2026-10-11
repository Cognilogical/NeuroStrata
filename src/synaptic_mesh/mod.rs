//! SynapticMesh — the inter-instance synaptic layer that complements the
//! intra-instance SynapticGraph. See `docs/superpowers/specs/2026-10-10-synaptic-mesh-design.md`.
//!
//! This module is the foundation: the on-disk schemas (manifest, record) and
//! the storage layout primitives. Outbox integration, mesh-worker, pull/replay,
//! hooks, recovery, and erasure land in follow-up slices.

pub mod manifest;
pub mod record;
pub mod storage;

pub use manifest::{MeshManifest, MeshPolicy, Trust, MESH_MANIFEST_SCHEMA_VERSION};
pub use record::{Lifecycle, LifecycleState, MeshRecord, Relationship, MESH_RECORD_SCHEMA_VERSION};
pub use storage::MeshStorage;
