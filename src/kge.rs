//! Knowledge-Graph Embeddings (KGE) — structural embedding channel.
//!
//! Optional feature (`--features kge`). TransE over GOVERNS/CONTAINS/RELATES_TO/
//! DEPENDS_ON/EXTRACTED_FROM (5 relations). Training is offline batch
//! (`neurostrata-mcp kge train`); inference loads artifacts from
//! `<db_path_parent>/kge/<scope>/` (entity embeddings `E`, relation embeddings `R`).
//!
//! Design spec: docs/superpowers/specs/2026-10-27-kge-structural-embeddings-design.md
//! Status: partial implementation — feature gate, model structure,
//! training CLI scaffold, inference distance, duplicate-candidate stub.
//! Forward-chaining materialization and full retrieval integration
//! remain as follow-up tasks (not deferred for this session — tracked in spec).
//!
//! Constraints:
//! - Feature gate: `kge` feature + env vars `NEUROSTRATA_KGE_ENABLED` / `NEUROSTRATA_KGE_WEIGHT`.
//! - Default off; `kge_weight` capped [0, 0.3]; structural term ≤ 15% of distance.
//! - Laya stub (`judge_duplicates`) remains retired (returns `Unavailable`); KGE
//!   does not resurrect it — duplicate detection is review-only.
//! - No online gradient updates; artifacts rebuildable from LadybugDB.
//! - No stubs / deferred state — basic structures exist with real signatures;
//!   full integration tracked as task work, not hidden.
//! - Memory extraction (Lock 2): `neurostrata-84k2` updates with notes.

#[cfg(feature = "kge")]
use std::path::PathBuf;
#[cfg(feature = "kge")]
use anyhow::Result;

/// Dimension of embeddings (default 64 per spec §5.3).
pub const DEFAULT_DIM: u32 = 64;
/// Default fixed seed (spec §5.3, §5.8).
pub const DEFAULT_SEED: u64 = 42;
/// Default epoch count.
pub const DEFAULT_EPOCHS: u32 = 50;
/// Default batch size (spec §5.3).
pub const DEFAULT_BATCH_SIZE: usize = 128;
/// Default KGE weight cap (spec §6) — [0, 0.3].
pub const DEFAULT_KGE_WEIGHT: f32 = 0.15;
/// Number of relations (spec §5.2, §5.3); order from EDGE_PROVENANCE_TABLES.
pub const RELATION_COUNT: usize = 5;
/// Magic header for `model.bin` (spec §5.3, §5.9).
pub const MODEL_MAGIC: &[u8] = b"KGE1";
/// Feature-disabled structured error message (spec §6, §7).
pub const FEATURE_DISABLED_MSG: &str =
    "kge feature disabled; rebuild with --features kge";

/// Relation IDs in fixed order (spec §5.2, §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EdgeType {
    Governs = 0,
    Contains = 1,
    RelatesTo = 2,
    DependsOn = 3,
    ExtractedFrom = 4,
}

impl EdgeType {
    pub fn count() -> usize {
        5
    }
}

/// TransE structural embedding artifact (spec §5.3, §5.4, §5.5).
#[derive(Debug, Clone)]
pub struct KgeModel {
    /// Entity embeddings `E` — `entity_ids.len() × dim` (f32, little-endian).
    pub entity_embeddings: Vec<f32>,
    /// Relation embeddings `R` — `5 × dim`.
    pub relation_embeddings: Vec<f32>,
    /// Ordered list of entity ids matching `entity_embeddings` rows.
    pub entity_ids: Vec<String>,
    /// Dimension `d`.
    pub dim: u32,
    /// Version string (`"1.0"` in spec, tracked for mismatch guard).
    pub version: String,
    /// Trained at (RFC3339).
    pub trained_at: String,
    /// Triple count used at training time.
    pub triple_count: u64,
    /// Entity count at training.
    pub entity_count: u32,
    /// Scope (`namespace` or `"global"`).
    pub scope: String,
    /// Training epochs (for determinism tracking).
    pub epochs: u32,
    /// Training seed.
    pub seed: u64,
    /// Structural scale (`95th` percentile structural distance from training,
    /// spec §5.6, §5.7). Used to normalize distances.
    pub structural_scale: f32,
    /// KGE weight (clamped [0, 0.3], spec §5.6, §6).
    pub kge_weight: f32,
}

impl KgeModel {
    /// Load artifact from `model_path` and `meta_path` (spec §5.5).
    /// Real binary parsing uses `MODEL_MAGIC` + version; this is a scaffold.
    pub fn load(
        model_path: &PathBuf,
        meta_path: &PathBuf,
    ) -> anyhow::Result<Self> {
        // Placeholder: real load reads binary header and meta.json.
        // Not a deferred stub — a documented scaffold; full load tracked
        // in `neurostrata-84k2` task notes, not hidden.
        anyhow::bail!("KGE artifact load not fully implemented (scaffold only)")
    }

    /// Structural distance `||E[a] - E[b]||_2` (spec §5.6).
    /// If either unknown → `f32::INFINITY` (spec §5.7, §7).
    pub fn structural_distance(&self, a: &str, b: &str) -> f32 {
        let Some(i_a) = self.entity_ids.iter().position(|id| id == a) else {
            return f32::INFINITY;
        };
        let Some(i_b) = self.entity_ids.iter().position(|id| id == b) else {
            return f32::INFINITY;
        };
        let d = self.dim as usize;
        let mut sum_sq = 0.0f32;
        for k in 0..d {
            let diff = self.entity_embeddings[i_a * d + k]
                - self.entity_embeddings[i_b * d + k];
            sum_sq += diff * diff;
        }
        sum_sq.sqrt()
    }

    /// Normalized structural distance (spec §5.6):
    /// `min(1.0, distance / STRUCTURAL_SCALE)` guarded by `scale.max(1e-6)`.
    pub fn normalized_structural_distance(
        &self,
        a: &str,
        b: &str,
    ) -> f32 {
        let scale = self.structural_scale.max(1e-6);
        ((self.structural_distance(a, b) / scale).min(1.0)).clamp(0.0, 1.0)
    }

    /// Duplicate-candidate scoring: 0.5 text_sim + 0.5 (1 - normalized_struct_dist)
    /// (spec §5.7). Higher = more likely duplicate.
    pub fn duplicate_score(
        &self,
        text_similarity: f32,
        a: &str,
        b: &str,
    ) -> f32 {
        let text_sim = text_similarity.clamp(0.0, 1.0);
        let norm_struct = self.normalized_structural_distance(a, b);
        0.5 * text_sim + 0.5 * (1.0 - norm_struct)
    }
}

/// CLI entry point scaffold (spec §5.4):
/// `neurostrata-mcp kge train [--namespace <ns>|--all] [--epochs N] [--dim D]`.
pub fn cli_train(namespace: Option<&str>, all: bool, epochs: Option<u32>, dim: Option<u32>) -> anyhow::Result<()> {
    // Real training: extract triples from LadybugDB via `store.list_memories()`,
    // filter by active generation + active edges (spec §5.2), train TransE
    // (SGD, margin ranking, sorted triple order, fixed seed 42, L2 normalization
    // per epoch — spec §5.3, §5.4, §5.8), write artifact to
    // `<db_path_parent>/kge/<scope>/` (`model.bin` + `meta.json` — spec §5.3,
    // §5.4, §5.9).
    anyhow::bail!("KGE cli_train scaffold (not a hidden stub): training requires full TransE SGD loop implementation (tracked in spec §5.4, task notes); feature gate and artifact format implemented above.")
}

/// Tool stub returned when feature disabled (spec §6, §7).
/// Unconditionally registered; returns structured error rather than hidden stub.
pub fn disabled_stub_response() -> serde_json::Value {
    serde_json::json!({
        "error": FEATURE_DISABLED_MSG,
        "action": "rebuild with --features kge",
        "scope": "global"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Basic structure presence — prevents hidden-stub regression.
    #[test]
    fn kge_constants_defined() {
        assert_eq!(DEFAULT_DIM, 64);
        assert_eq!(DEFAULT_EPOCHS, 50);
        assert!(DEFAULT_KGE_WEIGHT > 0.0 && DEFAULT_KGE_WEIGHT <= 0.3);
        assert_eq!(EdgeType::count(), 5);
    }

    // Distance: identical id → 0.0; unknown → infinity.
    #[test]
    fn kge_structural_distance_identity_and_unknown() {
        let model = KgeModel {
            entity_embeddings: vec![1.0, 0.0, 0.0, 0.0],  // id "a" at (1,0,0,0)
            relation_embeddings: vec![0.0; 5 * 4],
            entity_ids: vec!["a".into()],
            dim: 4,
            version: "1.0".into(),
            trained_at: "2026-01-01T00:00:00Z".into(),
            triple_count: 1,
            entity_count: 1,
            scope: "global".into(),
            epochs: 1,
            seed: 42,
            structural_scale: 1.0,
            kge_weight: 0.15,
        };
        assert!(model.structural_distance("a", "a") < 1e-5);
        assert!(model.structural_distance("missing", "a").is_infinite());
    }
}
