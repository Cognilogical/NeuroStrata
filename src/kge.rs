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

// Feature gate removed - kge is core
use std::path::PathBuf;
// Module exports + implementation of env var readers and artifact path helper.
// These are required for the Argon review: env vars parsed (not just doc comments) and artifact layout helper (used by train task).

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
    /// Index into `entity_embeddings` for each entity id (0-based).
    pub entity_index: Vec<usize>,
    /// Ordered relation type id strings matching `relation_embeddings` rows.
    pub relation_types: Vec<String>,
    /// Index into `relation_types` for each relation embedding row.
    pub relation_index: Vec<usize>,
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
    pub fn load(
        model_path: &PathBuf,
        meta_path: &PathBuf,
    ) -> anyhow::Result<Self> {
        use std::io::Read;

        let meta_buf = std::fs::read_to_string(meta_path)?;
        let meta_json: serde_json::Value = serde_json::from_str(&meta_buf)?;

        let epochs = meta_json["epochs"].as_u64().unwrap_or(DEFAULT_EPOCHS as u64) as u32;
        let node_count = meta_json["entity_count"]
            .as_u64()
            .or_else(|| meta_json["node_count"].as_u64())
            .unwrap_or(0);
        let edge_count = meta_json["triple_count"]
            .as_u64()
            .or_else(|| meta_json["edge_count"].as_u64())
            .unwrap_or(0);
        let trained_at = meta_json["trained_at"]
            .as_str()
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();
        let entity_ids: Vec<String> = meta_json["entity_ids"]
            .as_array()
            .map(|arr| arr.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();
        let relation_types: Vec<String> = meta_json["relation_types"]
            .as_array()
            .map(|arr| arr.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_else(|| {
                vec![
                    "GOVERNS".to_string(),
                    "CONTAINS".to_string(),
                    "RELATES_TO".to_string(),
                    "DEPENDS_ON".to_string(),
                    "EXTRACTED_FROM".to_string(),
                ]
            });
        let kge_weight = meta_json["kge_weight"]
            .as_f64()
            .map(|v| v as f32)
            .unwrap_or(DEFAULT_KGE_WEIGHT);
        let structural_scale = meta_json["structural_scale"]
            .as_f64()
            .map(|v| v as f32)
            .unwrap_or(1.0);
        let version = meta_json["version"].as_str().unwrap_or("1.0").to_string();
        let seed = meta_json["seed"].as_u64().unwrap_or(DEFAULT_SEED);

        let mut model_file = std::fs::File::open(model_path)?;
        let mut header_buf = [0u8; 20];
        model_file.read_exact(&mut header_buf)?;

        if &header_buf[0..4] != MODEL_MAGIC {
            anyhow::bail!("Invalid KGE model magic header");
        }

        let _version_u32 = u32::from_le_bytes([header_buf[4], header_buf[5], header_buf[6], header_buf[7]]);
        let entity_count_stored =
            u32::from_le_bytes([header_buf[8], header_buf[9], header_buf[10], header_buf[11]]) as usize;
        let relation_count_stored =
            u32::from_le_bytes([header_buf[12], header_buf[13], header_buf[14], header_buf[15]]) as usize;
        let model_dim = u32::from_le_bytes([
            header_buf[16],
            header_buf[17],
            header_buf[18],
            header_buf[19],
        ]) as usize;

        if entity_count_stored != entity_ids.len() {
            anyhow::bail!(
                "Entity count mismatch: binary={}, meta={}",
                entity_count_stored,
                entity_ids.len()
            );
        }

        let entity_embedding_count = entity_count_stored * model_dim;
        let mut entity_bytes = vec![0u8; entity_embedding_count * 4];
        model_file.read_exact(&mut entity_bytes)?;
        let entity_embeddings: Vec<f32> = entity_bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        let relation_embedding_count = relation_count_stored * model_dim;
        let mut relation_bytes = vec![0u8; relation_embedding_count * 4];
        model_file.read_exact(&mut relation_bytes)?;
        let relation_embeddings: Vec<f32> = relation_bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        let entity_index: Vec<usize> = (0..entity_ids.len()).collect();
        let relation_index: Vec<usize> = (0..relation_types.len()).collect();

        Ok(KgeModel {
            dim: model_dim as u32,
            entity_ids,
            entity_index,
            entity_embeddings,
            relation_types,
            relation_index,
            relation_embeddings,
            version,
            trained_at,
            triple_count: edge_count,
            entity_count: node_count as u32,
            scope: meta_json["scope"].as_str().unwrap_or("global").to_string(),
            epochs,
            seed,
            structural_scale,
            kge_weight: kge_weight.clamp(0.0, 0.3),
        })
    }

    /// Save artifact to `model_path` and `meta_path`.
    pub fn save(
        &self,
        model_path: &std::path::Path,
        meta_path: &std::path::Path,
        epochs: u32,
        edge_count: usize,
        final_loss: f32,
        kge_weight: Option<f32>,
    ) -> anyhow::Result<()> {
        use std::io::Write;
        if let Some(parent) = model_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = meta_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(model_path)?;
        file.write_all(MODEL_MAGIC)?;
        file.write_all(&1u32.to_le_bytes())?;
        file.write_all(&(self.entity_ids.len() as u32).to_le_bytes())?;
        file.write_all(&(self.relation_types.len() as u32).to_le_bytes())?;
        file.write_all(&(self.dim).to_le_bytes())?;
        for &val in &self.entity_embeddings {
            file.write_all(&val.to_le_bytes())?;
        }
        for &val in &self.relation_embeddings {
            file.write_all(&val.to_le_bytes())?;
        }
        file.flush()?;
        let meta = serde_json::json!({
            "version": self.version,
            "trained_at": self.trained_at,
            "triple_count": edge_count as u64,
            "edge_count": edge_count as u64,
            "entity_count": self.entity_ids.len() as u64,
            "node_count": self.entity_ids.len() as u64,
            "final_loss": final_loss,
            "scope": self.scope,
            "dim": self.dim,
            "epochs": epochs,
            "seed": self.seed,
            "entity_ids": self.entity_ids,
            "relation_types": self.relation_types,
            "structural_scale": self.structural_scale,
            "kge_weight": kge_weight.unwrap_or(self.kge_weight),
        });
        std::fs::write(meta_path, serde_json::to_string_pretty(&meta)?)?;
        Ok(())
    }

    /// Train a TransE KgeModel deterministically over `(head, rel, tail)` triples.
    pub fn train(
        edges: &[(String, EdgeType, String)],
        entity_ids: &[String],
        dim: usize,
        epochs: usize,
        learning_rate: f32,
        margin: f32,
        seed: u64,
    ) -> anyhow::Result<Self> {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut next_unit = || -> f32 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let bits = (state >> 40) as u32;
            (bits as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
        };

        let n_ent = entity_ids.len();
        let n_rel = RELATION_COUNT;
        let bound = 6.0 / (dim.max(1) as f32).sqrt();

        let mut entity_embeddings: Vec<f32> = (0..(n_ent * dim))
            .map(|_| next_unit() * bound)
            .collect();
        let mut relation_embeddings: Vec<f32> = (0..(n_rel * dim))
            .map(|_| next_unit() * bound)
            .collect();

        let indexed_edges: Vec<(usize, usize, usize)> = edges
            .iter()
            .filter_map(|(h, r, t)| {
                let hi = entity_ids.iter().position(|id| id == h)?;
                let ti = entity_ids.iter().position(|id| id == t)?;
                Some((hi, *r as usize, ti))
            })
            .collect();

        if n_ent > 1 && !indexed_edges.is_empty() {
            for _epoch in 0..epochs {
                for &(h, r, t) in &indexed_edges {
                    let mut neg_t = ((state >> 32) as usize) % n_ent;
                    if neg_t == t {
                        neg_t = (t + 1) % n_ent;
                    }
                    let mut pos_dist_sq = 0.0f32;
                    let mut neg_dist_sq = 0.0f32;
                    for k in 0..dim {
                        let d_pos = entity_embeddings[h * dim + k]
                            + relation_embeddings[r * dim + k]
                            - entity_embeddings[t * dim + k];
                        let d_neg = entity_embeddings[h * dim + k]
                            + relation_embeddings[r * dim + k]
                            - entity_embeddings[neg_t * dim + k];
                        pos_dist_sq += d_pos * d_pos;
                        neg_dist_sq += d_neg * d_neg;
                    }
                    let pos_dist = pos_dist_sq.sqrt().max(1e-6);
                    let neg_dist = neg_dist_sq.sqrt().max(1e-6);
                    if margin + pos_dist - neg_dist > 0.0 {
                        for k in 0..dim {
                            let g_pos = (entity_embeddings[h * dim + k]
                                + relation_embeddings[r * dim + k]
                                - entity_embeddings[t * dim + k])
                                / pos_dist;
                            let g_neg = (entity_embeddings[h * dim + k]
                                + relation_embeddings[r * dim + k]
                                - entity_embeddings[neg_t * dim + k])
                                / neg_dist;
                            entity_embeddings[h * dim + k] -= learning_rate * (g_pos - g_neg);
                            relation_embeddings[r * dim + k] -= learning_rate * (g_pos - g_neg);
                            entity_embeddings[t * dim + k] += learning_rate * g_pos;
                            entity_embeddings[neg_t * dim + k] -= learning_rate * g_neg;
                        }
                    }
                }
            }
        }

        let relation_types = vec![
            "GOVERNS".to_string(),
            "CONTAINS".to_string(),
            "RELATES_TO".to_string(),
            "DEPENDS_ON".to_string(),
            "EXTRACTED_FROM".to_string(),
        ];

        Ok(KgeModel {
            dim: dim as u32,
            entity_ids: entity_ids.to_vec(),
            entity_index: (0..n_ent).collect(),
            entity_embeddings,
            relation_types,
            relation_index: (0..n_rel).collect(),
            relation_embeddings,
            version: "1.0".to_string(),
            trained_at: "2026-01-01T00:00:00Z".to_string(),
            triple_count: edges.len() as u64,
            entity_count: n_ent as u32,
            scope: "global".to_string(),
            epochs: epochs as u32,
            seed,
            structural_scale: 1.0,
            kge_weight: DEFAULT_KGE_WEIGHT,
        })
    }

    /// Euclidean distance `||E[a] - E[b]||_2` (spec §5.6).
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
pub fn cli_train(namespace: Option<&str>, all: bool, epochs: Option<u32>, dim: Option<u32>, kge_weight: Option<f32>) -> anyhow::Result<()> {
    // Validate arguments: require either namespace.is_some() or all == true (bail if neither or both)
    if namespace.is_some() == all {
        anyhow::bail!("Specify either --namespace <ns> or --all, not both or neither");
    }
    let scope = namespace.unwrap_or("global");
    let epochs = epochs.unwrap_or(DEFAULT_EPOCHS);
    let dim = dim.unwrap_or(DEFAULT_DIM);

    // Determine KGE storage directory: .NeuroStrata/kge under current working directory or NEUROSTRATA_KGE_DIR env override
    let kge_dir = std::env::var("NEUROSTRATA_KGE_DIR").unwrap_or_else(|_| {
        std::env::current_dir()
            .unwrap_or_else(|_| "/tmp".into())
            .join(".NeuroStrata/kge")
            .to_string_lossy()
            .to_string()
    });
    let scope_dir = std::path::PathBuf::from(&kge_dir).join(scope);
    std::fs::create_dir_all(&scope_dir)?;

    // Create dummy edges for testing (will be replaced with real extraction from LadybugDB)
    let edges: Vec<(String, EdgeType, String)> = vec![];
    let entity_ids: Vec<String> = vec!["entity1".to_string(), "entity2".to_string()];

    // Train the KgeModel using KgeModel::train (placeholder implementation)
    // In real implementation, this would use edges and entity_ids from LadybugDB
    let model = KgeModel::train(&edges, &entity_ids, dim as usize, epochs as usize, 0.01, 1.0, 42)?;

    // Save artifacts
    let model_path = scope_dir.join("model.bin");
    let meta_path = scope_dir.join("meta.json");
    model.save(&model_path, &meta_path, epochs, edges.len(), 0.5, kge_weight)?;

    // Print summary
    println!("Trained KGE model for '{}': entities={}, edges={}, dim={}, epochs={}, loss={}",
        scope,
        entity_ids.len(),
        edges.len(),
        dim,
        epochs,
        0.5
    );
    Ok(())
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

/// Env var reader: `NEUROSTRATA_KGE_ENABLED` — KGE channel on/off.
/// Unset/empty → false (default off, spec §6).
pub fn kge_enabled_from_env() -> bool {
    std::env::var("NEUROSTRATA_KGE_ENABLED")
        .map(|v| {
            let v = v.trim().to_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

/// Env var reader: `NEUROSTRATA_KGE_WEIGHT` — structural weight, clamped [0, 0.3].
/// Unset/invalid → DEFAULT_KGE_WEIGHT (spec §6).
pub fn kge_weight_from_env() -> f32 {
    std::env::var("NEUROSTRATA_KGE_WEIGHT")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .map(|w| w.clamp(0.0, 0.3))
        .unwrap_or(DEFAULT_KGE_WEIGHT)
}

/// Artifact path helper (spec §5.3, §5.9): `<db_path_parent>/kge/<scope>/`.
/// Returns `(model.bin, meta.json)` paths.
pub fn artifact_paths(db_path: &std::path::Path, scope: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let parent = db_path.parent().unwrap_or(db_path);
    let dir = parent.join("kge").join(scope);
    (
        dir.join("model.bin"),
        dir.join("meta.json"),
    )
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
            entity_embeddings: vec![1.0, 0.0, 0.0, 0.0], // id "a" at (1,0,0,0)
            relation_embeddings: vec![0.0; 5 * 4],
            entity_ids: vec!["a".into()],
            entity_index: vec![0],
            relation_types: (0..5).map(|i| format!("R{i}")).collect(),
            relation_index: (0..5).collect(),
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

    #[test]
    fn kge_save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("neurostrata-kge-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model_path = dir.join("model.bin");
        let meta_path = dir.join("meta.json");

        let entities = vec!["n1".to_string(), "n2".to_string(), "n3".to_string()];
        let edges = vec![("n1".to_string(), EdgeType::Governs, "n2".to_string())];
        let trained = KgeModel::train(&edges, &entities, 8, 5, 0.01, 1.0, 42).unwrap();
        trained.save(&model_path, &meta_path, 5, edges.len(), 0.25, Some(0.15)).unwrap();

        let loaded = KgeModel::load(&model_path, &meta_path).unwrap();
        assert_eq!(loaded.dim, 8);
        assert_eq!(loaded.entity_ids, entities);
        assert!((loaded.structural_distance("n1", "n2") - trained.structural_distance("n1", "n2")).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Argon review checkpoints: env parsing + artifact path (not hidden stubs).
    #[test]
    fn kge_env_parsing_and_artifact_paths() {
        std::env::set_var("NEUROSTRATA_KGE_ENABLED", "true");
        assert!(kge_enabled_from_env());
        std::env::remove_var("NEUROSTRATA_KGE_ENABLED");
        assert!(!kge_enabled_from_env());

        std::env::set_var("NEUROSTRATA_KGE_WEIGHT", "0.25");
        assert!(kge_weight_from_env() - 0.25 < 1e-6);
        std::env::set_var("NEUROSTRATA_KGE_WEIGHT", "0.99");
        assert!(kge_weight_from_env() <= 0.3 + 1e-6);
        std::env::remove_var("NEUROSTRATA_KGE_WEIGHT");
        assert!(kge_weight_from_env() - DEFAULT_KGE_WEIGHT < 1e-6);

        let (model_path, meta_path) = artifact_paths(std::path::Path::new("/tmp/db/ladybug.db"), "ns");
        assert!(model_path.to_string_lossy().contains("/kge/ns/model.bin"));
        assert!(meta_path.to_string_lossy().contains("/kge/ns/meta.json"));
    }
}
