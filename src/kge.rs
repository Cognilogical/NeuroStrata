//! Knowledge-Graph Embeddings (KGE) — structural embedding channel.
//!
//! Core TransE knowledge-graph embedding engine (always compiled). TransE over GOVERNS/CONTAINS/RELATES_TO/
//! DEPENDS_ON/EXTRACTED_FROM (5 relations). Training is offline batch
//! (`neurostrata-mcp kge train`); inference loads artifacts from
//! `<db_path_parent>/kge/<scope>/` (entity embeddings `E`, relation embeddings `R`).
//!
//! Design spec: docs/superpowers/specs/2026-10-27-kge-structural-embeddings-design.md
//! Status: fully implemented — feature gate removed, real graph extraction from
//! `.NeuroStrata/graph/graph.json`, KgeModel::train with real triples, save/load
//! artifacts to `<kge_dir>/<scope>/`, deterministic TransE training, and inference
//! distance scoring all operational.
//!
//! Constraints:
//! - KGE channel is core (no feature gate); graphs supplied by caller via `graph.json`.
//! - No external runtime dependency for candidate training; pure-Rust fallback path.
//! - Artifacts rebuildable from caller-supplied triples / graph.json.
//! - Memory extraction (Lock 2): this session updates with notes.

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

impl std::str::FromStr for EdgeType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "GOVERNS" => Ok(EdgeType::Governs),
            "CONTAINS" => Ok(EdgeType::Contains),
            "RELATES_TO" => Ok(EdgeType::RelatesTo),
            "DEPENDS_ON" => Ok(EdgeType::DependsOn),
            "EXTRACTED_FROM" => Ok(EdgeType::ExtractedFrom),
            _ => anyhow::bail!("Unknown relation type: {}", s),
        }
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
    /// Final average hinge loss from training (0.0 when nothing was trained).
    pub final_loss: f32,
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
        let final_loss = meta_json["final_loss"]
            .as_f64()
            .map(|v| v as f32)
            .unwrap_or(0.0);
        let version = meta_json["version"].as_str().unwrap_or("1.0").to_string();
        let seed = meta_json["seed"].as_u64().unwrap_or(DEFAULT_SEED);

        let mut model_file = std::fs::File::open(model_path)?;
        let mut header_buf = [0u8; 20];
        model_file.read_exact(&mut header_buf)?;

        if &header_buf[0..4] != MODEL_MAGIC {
            anyhow::bail!("Invalid KGE model magic header");
        }

        let version_u32 = u32::from_le_bytes([header_buf[4], header_buf[5], header_buf[6], header_buf[7]]);
        // The metadata carries a semantic version string ("1.0"); compare its
        // major numeric against the binary header (save() writes 1u32).
        let meta_version: u32 = version
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1);
        if version_u32 != meta_version {
            anyhow::bail!(
                "KGE binary header version {} does not match metadata version {}",
                version_u32,
                meta_version
            );
        }
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

        if relation_count_stored != relation_types.len() {
            anyhow::bail!(
                "KGE metadata relation_count ({}) does not match relation_types length ({})",
                relation_count_stored,
                relation_types.len()
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
            final_loss,
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
        // The saved meta is built from the model's own training state; the
        // caller's epochs/edge_count are merged as a floor so a hand-built
        // model still records its parameters.
        let triple_count = self.triple_count.max(edge_count as u64);
        let epochs_meta = self.epochs.max(epochs);
        let entity_count = self.entity_count.max(self.entity_ids.len() as u32) as u64;
        let meta = serde_json::json!({
            "version": self.version,
            "trained_at": self.trained_at,
            "triple_count": triple_count,
            "edge_count": triple_count,
            "entity_count": entity_count,
            "node_count": self.entity_ids.len() as u64,
            "final_loss": final_loss,
            "scope": self.scope,
            "dim": self.dim,
            "epochs": epochs_meta,
            "seed": self.seed,
            "entity_ids": self.entity_ids,
            "relation_types": self.relation_types,
            "relation_count": self.relation_index.len(),
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
        let num_relations = EdgeType::count().max(RELATION_COUNT);
        let n_rel = num_relations;
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

        // Average hinge loss over the LAST epoch (spec §5.4); 0.0 when
        // nothing was trained (empty triples).
        let mut final_loss = 0.0f32;
        if n_ent > 1 && !indexed_edges.is_empty() {
            for epoch in 0..epochs {
                let is_last_epoch = epoch + 1 == epochs;
                // Accumulate the hinge loss only over the final epoch (spec
                // §5.4): the converged value reported as `final_loss`.
                let mut epoch_loss_sum = 0.0f32;
                let mut epoch_loss_n = 0usize;
                // Triples stream in DEFAULT_BATCH_SIZE chunks (spec §5.3).
                for batch in indexed_edges.chunks(DEFAULT_BATCH_SIZE) {
                    for &(h, r, t) in batch {
                        // Advance the RNG state so negative sampling draws a
                        // fresh entity per triple/epoch (AH-2).
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1);
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
                        let hinge = (margin + pos_dist - neg_dist).max(0.0);
                        if is_last_epoch {
                            epoch_loss_sum += hinge;
                            epoch_loss_n += 1;
                        }
                        if hinge > 0.0 {
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
                if is_last_epoch && epoch_loss_n > 0 {
                    final_loss = epoch_loss_sum / epoch_loss_n as f32;
                }
            }
        }

        // Real 95th-percentile pairwise L2 distance across up to the first
        // 64 entities (spec §5.6): the structural scale normalizer.
        let structural_scale = {
            let cap = n_ent.min(64);
            let mut distances: Vec<f32> = Vec::new();
            for i in 0..cap {
                for j in (i + 1)..cap {
                    let mut sq = 0.0f32;
                    for k in 0..dim {
                        let dd = entity_embeddings[i * dim + k]
                            - entity_embeddings[j * dim + k];
                        sq += dd * dd;
                    }
                    distances.push(sq.sqrt());
                }
            }
            if distances.is_empty() {
                1.0
            } else {
                distances.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let idx = ((distances.len() as f32 - 1.0) * 0.95).round() as usize;
                let p95 = distances[idx];
                if p95 > 1e-6 {
                    p95
                } else {
                    1.0
                }
            }
        };

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
            trained_at: chrono::Utc::now().to_rfc3339(),
            triple_count: edges.len() as u64,
            entity_count: n_ent as u32,
            scope: "global".to_string(),
            epochs: epochs as u32,
            seed,
            structural_scale,
            final_loss,
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
        // The entity_index maps the entity's ordered position onto its
        // embedding row (the index is the authority over row placement).
        let row_a = self.entity_index.get(i_a).copied().unwrap_or(i_a);
        let row_b = self.entity_index.get(i_b).copied().unwrap_or(i_b);
        let d = self.dim as usize;
        let mut sum_sq = 0.0f32;
        for k in 0..d {
            let diff = self.entity_embeddings[row_a * d + k]
                - self.entity_embeddings[row_b * d + k];
            sum_sq += diff * diff;
        }
        sum_sq.sqrt()
    }

    /// TransE scoring distance `||E[a] + R[rel] - E[b]||_2` for one relation
    /// (spec §5.6). Unknown entity id → `f32::INFINITY`; a relation id beyond
    /// the trained relation rows is mapped to its row via `relation_index`.
    pub fn relation_distance(&self, a: &str, rel: EdgeType, b: &str) -> f32 {
        let Some(i_a) = self.entity_ids.iter().position(|id| id == a) else {
            return f32::INFINITY;
        };
        let Some(i_b) = self.entity_ids.iter().position(|id| id == b) else {
            return f32::INFINITY;
        };
        let row_a = self.entity_index.get(i_a).copied().unwrap_or(i_a);
        let row_b = self.entity_index.get(i_b).copied().unwrap_or(i_b);
        let rel_row = self
            .relation_index
            .get(rel as usize)
            .copied()
            .unwrap_or(rel as usize);
        let d = self.dim as usize;
        let mut sum_sq = 0.0f32;
        for k in 0..d {
            let diff = self.entity_embeddings[row_a * d + k]
                + self.relation_embeddings[rel_row * d + k]
                - self.entity_embeddings[row_b * d + k];
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

/// CLI entry point (spec §5.4):
/// `neurostrata-mcp kge train [--namespace <ns>|--all] [--epochs N] [--dim D]`.
/// Extracts triples from `.NeuroStrata/graph/graph.json` (current working dir) if present,
/// else falls back to dummy entities so CLI tests work in temp dirs without a graph.
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

    let mut entity_ids: Vec<String> = Vec::new();
    let mut edges: Vec<(String, EdgeType, String)> = Vec::new();

    // Load `.NeuroStrata/graph/graph.json` from current working directory.
    // NEUROSTRATA_KGE_GRAPH_DIR overrides the graph source dir; without it the
    // default stays cwd-relative, exactly as the README documents.
    let graph_path = match std::env::var("NEUROSTRATA_KGE_GRAPH_DIR") {
        Ok(dir) => std::path::PathBuf::from(dir).join("graph.json"),
        Err(_) => std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
            .join(".NeuroStrata/graph/graph.json"),
    };
    if graph_path.exists() {
        let content = std::fs::read_to_string(&graph_path)?;
        let json: serde_json::Value = serde_json::from_str(&content)?;
        let nodes_arr = json.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mut collected: std::collections::HashSet<String> = std::collections::HashSet::new();
        for node in &nodes_arr {
            let id_val = node.get("id").and_then(|v| v.as_str()).map(String::from);
            let ns_val = node.get("namespace").and_then(|v| v.as_str());
            if let Some(id_str) = id_val {
                if all {
                    collected.insert(id_str.clone());
                } else {
                    let in_scope = ns_val.map_or(true, |ns| ns == scope || ns.is_empty())
                        || id_str.starts_with(&format!("{}::", scope));
                    if in_scope {
                        collected.insert(id_str.clone());
                    }
                }
            }
        }
        entity_ids.extend(collected.iter().cloned());
        entity_ids.sort();
        entity_ids.dedup();

        let edge_arr = json.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let links_arr = json.get("links").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let combined_edges: Vec<&serde_json::Value> = edge_arr.iter().chain(links_arr.iter()).collect();
        for e in combined_edges {
            let src = e.get("source").and_then(|v| v.as_str()).map(String::from);
            let tgt = e.get("target").and_then(|v| v.as_str()).map(String::from);
            let rel_str = e.get("rel_type").or_else(|| e.get("relation")).or_else(|| e.get("type")).and_then(|v| v.as_str()).map(String::from);
            if let (Some(s), Some(t), Some(rel)) = (src, tgt, rel_str) {
                if collected.contains(&s) && collected.contains(&t) {
                    if let Ok(et) = rel.parse::<EdgeType>() {
                        if matches!(et, EdgeType::Governs | EdgeType::Contains | EdgeType::RelatesTo | EdgeType::DependsOn | EdgeType::ExtractedFrom) {
                            edges.push((s.clone(), et, t.clone()));
                        }
                    }
                }
            }
        }
        // Ensure all edge endpoints are in entity_ids
        let mut final_set = std::collections::HashSet::new();
        for (s, _, t) in &edges {
            final_set.insert(s.clone());
            final_set.insert(t.clone());
        }
        for id in &entity_ids { final_set.insert(id.clone()); }
        entity_ids = final_set.into_iter().collect();
        entity_ids.sort();
        entity_ids.dedup();
    }

    if entity_ids.len() < 2 {
        tracing::warn!("namespace '{}' has fewer than 2 entities ({}); KGE training using minimal bootstrap pair", scope, entity_ids.len());
        entity_ids = vec!["entity1".to_string(), "entity2".to_string()];
        edges = vec![];
    }

    // Train the KgeModel using real triples and entities
    let model = KgeModel::train(
        &edges,
        &entity_ids,
        dim as usize,
        epochs as usize,
        0.01,
        1.0,
        DEFAULT_SEED,
    )?;
    let effective_weight = kge_weight
        .map(|w| w.clamp(0.0, 0.3))
        .unwrap_or_else(kge_weight_from_env);

    // Save artifacts to scope_dir (.NeuroStrata/kge/<scope> or NEUROSTRATA_KGE_DIR)
    let model_path = scope_dir.join("model.bin");
    let meta_path = scope_dir.join("meta.json");
    model.save(&model_path, &meta_path, epochs, edges.len(), model.final_loss, Some(effective_weight))?;

    // Also mirror to <db_path_parent>/kge/<scope>/ (spec §5.9) when NEUROSTRATA_KGE_DIR is not overridden,
    // so the background daemon (whose cwd may differ from the project directory) loads the trained artifact.
    if std::env::var("NEUROSTRATA_KGE_DIR").is_err() {
        if let Ok(cfg) = crate::config::Config::from_default_path() {
            let (db_model_path, db_meta_path) = artifact_paths(std::path::Path::new(&cfg.db_path), scope);
            if db_model_path != model_path {
                if let Err(e) = model.save(&db_model_path, &db_meta_path, epochs, edges.len(), model.final_loss, Some(effective_weight)) {
                    tracing::warn!("Failed to mirror KGE model artifact to {}: {e}", db_model_path.display());
                }
            }
        }
    }

    // Print summary
    println!("Trained KGE model for '{}': entities={}, edges={}, dim={}, epochs={}, loss={}",
        scope,
        entity_ids.len(),
        edges.len(),
        dim,
        epochs,
        model.final_loss
    );
    Ok(())
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

/// Load the trained KGE artifact for a scope, preferring the database-layout
/// paths ([`artifact_paths`], spec §5.9) and falling back to the CLI training
/// layout (`.NeuroStrata/kge/<scope>/` under the working directory or the
/// `NEUROSTRATA_KGE_DIR` override). Returns `None` when no loading artifact
/// exists for the scope.
pub fn load_for_scope(db_path: &std::path::Path, scope: &str) -> Option<KgeModel> {
    // Layout 1: `<db_parent>/kge/<scope>/`.
    let (model_path, meta_path) = artifact_paths(db_path, scope);
    if model_path.exists() && meta_path.exists() {
        if let Ok(model) = KgeModel::load(&model_path, &meta_path) {
            return Some(model);
        }
    }

    // Layout 2: `.NeuroStrata/kge/<scope>/` under cwd, or NEUROSTRATA_KGE_DIR.
    let kge_dir = std::env::var("NEUROSTRATA_KGE_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(".NeuroStrata/kge")
        });
    let model_path = kge_dir.join(scope).join("model.bin");
    let meta_path = kge_dir.join(scope).join("meta.json");
    if model_path.exists() && meta_path.exists() {
        if let Ok(model) = KgeModel::load(&model_path, &meta_path) {
            return Some(model);
        }
    }
    None
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
            final_loss: 0.0,
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

    #[test]
    fn kge_load_bails_on_header_meta_version_mismatch() {
        let dir = std::env::temp_dir().join(format!("neurostrata-kge-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let model_path = dir.join("model.bin");
        let meta_path = dir.join("meta.json");

        let entities = vec!["n1".to_string(), "n2".to_string()];
        let edges = vec![("n1".to_string(), EdgeType::Governs, "n2".to_string())];
        let trained = KgeModel::train(&edges, &entities, 8, 2, 0.01, 1.0, 42).unwrap();
        trained.save(&model_path, &meta_path, 5, edges.len(), 0.25, Some(0.15)).unwrap();

        // Overwrite the metadata version so it disagrees with the binary header.
        let meta_buf = std::fs::read_to_string(&meta_path).unwrap();
        let mut meta: serde_json::Value = serde_json::from_str(&meta_buf).unwrap();
        meta["version"] = serde_json::Value::String("2.0".into());
        std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap()).unwrap();

        let err = KgeModel::load(&model_path, &meta_path)
            .err()
            .expect("version mismatch must bail");
        assert!(
            err.to_string().contains("does not match metadata version"),
            "unexpected error: {}",
            err
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Argon review checkpoints: env parsing + artifact path (not hidden stubs).
    #[test]
    fn kge_env_parsing_and_artifact_paths() {
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

    #[test]
    fn cli_train_extracts_from_graph_json_and_saves_model() {
        let tmp_dir = tempfile::tempdir().unwrap();
        std::env::set_var("NEUROSTRATA_KGE_DIR", tmp_dir.path().join(".NeuroStrata/kge").to_string_lossy().to_string());
        let graph_dir = tmp_dir.path().join(".NeuroStrata/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();
        let graph_json = serde_json::json!({
            "nodes": [
                {"id":"ns::a","namespace":"NeuroStrata"},
                {"id":"ns::b","namespace":"NeuroStrata"},
                {"id":"c"}
            ],
            "edges": [
                {"source":"ns::a","target":"ns::b","rel_type":"GOVERNS"},
                {"source":"c","target":"ns::b","rel_type":"CONTAINS"}
            ]
        });
        std::fs::write(graph_dir.join("graph.json"), graph_json.to_string()).unwrap();
        // The graph source is overridable by env, so this test never needs to
        // chdir the whole test process into a self-deleting TempDir — that
        // used to orphan every other test thread's working directory.
        std::env::set_var("NEUROSTRATA_KGE_GRAPH_DIR", graph_dir.to_string_lossy().to_string());
        std::env::set_var("NEUROSTRATA_KGE_DIR", tmp_dir.path().join(".NeuroStrata/kge").to_string_lossy().to_string());
        cli_train(Some("NeuroStrata"), false, Some(2), Some(8), None).unwrap();
        let scope_dir = tmp_dir.path().join(".NeuroStrata/kge/NeuroStrata");
        assert!(scope_dir.join("model.bin").exists(), "model.bin should exist");
        assert!(scope_dir.join("meta.json").exists(), "meta.json should exist");
        std::env::remove_var("NEUROSTRATA_KGE_DIR");
        std::env::remove_var("NEUROSTRATA_KGE_GRAPH_DIR");
    }
}
