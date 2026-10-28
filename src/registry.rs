//! The per-namespace domain registry: a cache of the domain slugs a namespace
//! uses, kept current on the write path so a write can be checked with one
//! read instead of a scan.
//!
//! The registry row is a **cache, not the truth**: the namespace's own rows are
//! the truth. A stale cache is a miss, repaired by the reconcile below; it is
//! never a wrong acceptance. That is why the acceptance decision is made
//! *before* any registry write, and why a failed registry update is a warning
//! and never a rejection.

use crate::traits::{Embedder, MemoryPayload, VectorStore};
use serde_json::Value;
use std::sync::Arc;

/// One registry row per namespace. The id is a deterministic UUID because the
/// mesh skips any row whose id is not a parseable UUID.
pub fn registry_row_id(namespace: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("neurostrata:domain_registry:{namespace}").as_bytes(),
    )
    .to_string()
}

pub fn registry_content(namespace: &str) -> String {
    format!("Domain registry for {namespace}")
}

/// True only for a metadata carrying `domain_registry` — the registry row's
/// mark. Nothing else in the store declares that key.
pub fn is_registry_row(metadata: &Value) -> bool {
    metadata.get("domain_registry").is_some()
}

/// The registry row's domains, sorted and deduplicated. An absent or malformed
/// shape reads as an empty list rather than an error: the cache is not truth.
pub fn registry_domains(metadata: &Value) -> Vec<String> {
    let domains: Vec<String> = metadata
        .get("domain_registry")
        .and_then(|registry| registry.get("domains"))
        .and_then(|domains| domains.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|value| {
                    if let Some(s) = value.as_str() {
                        Some(s.to_string())
                    } else {
                        value.get("slug").and_then(|s| s.as_str()).map(|s| s.to_string())
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    sorted_unique(domains)
}

/// Registry domains as v2 entries (slug/parent/aliases/concept_iri), with v1
/// backwards compat (strings map to {slug, parent: null, aliases: [], concept_iri: null}).
pub fn registry_domain_entries(metadata: &Value) -> Vec<Value> {
    let arr = metadata
        .get("domain_registry")
        .and_then(|registry| registry.get("domains"))
        .and_then(|domains| domains.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for v in arr {
        if let Some(s) = v.as_str() {
            out.push(serde_json::json!({
                "slug": s,
                "parent": serde_json::Value::Null,
                "aliases": [],
                "concept_iri": serde_json::Value::Null,
            }));
        } else if v.is_object() {
            let slug = v.get("slug").and_then(|s| s.as_str()).unwrap_or("");
            let parent = v.get("parent").cloned().unwrap_or(serde_json::Value::Null);
            let aliases = v.get("aliases").cloned().unwrap_or(serde_json::json!([]));
            let concept_iri = v.get("concept_iri").cloned().unwrap_or(serde_json::Value::Null);
            out.push(serde_json::json!({
                "slug": slug,
                "parent": parent,
                "aliases": aliases,
                "concept_iri": concept_iri,
            }));
        }
    }
    out
}

/// The registry row's metadata. `eligibility: private` keeps the namespace's
/// vocabulary out of the mesh: it is namespace-local, so it never travels.
pub fn registry_metadata(domains: &[String]) -> Value {
    serde_json::json!({
        "domain_registry": { "domains": sorted_unique(domains.to_vec()) },
        "eligibility": "private",
    })
}

/// Build a v2 registry metadata object from structured domain entries.
pub fn registry_metadata_v2(entries: &[Value]) -> Value {
    let mut normalized: Vec<Value> = entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "slug": entry.get("slug").cloned().unwrap_or(serde_json::Value::Null),
                "parent": entry.get("parent").cloned().unwrap_or(serde_json::Value::Null),
                "aliases": entry.get("aliases").cloned().unwrap_or(serde_json::json!([])),
                "concept_iri": entry.get("concept_iri").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    normalized.sort_by(|a, b| {
        let sa = a.get("slug").and_then(|v| v.as_str()).unwrap_or("");
        let sb = b.get("slug").and_then(|v| v.as_str()).unwrap_or("");
        sa.cmp(sb)
    });
    serde_json::json!({
        "domain_registry": {
            "version": 2,
            "domains": normalized,
        },
        "eligibility": "private",
    })
}

/// Validate a list of v2 domain entries. Returns Ok(()) on a valid DAG.
pub fn validate_domain_entries(entries: &[Value]) -> Result<(), String> {
    let mut slugs: Vec<String> = Vec::new();
    let mut parent_of: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();

    for (idx, entry) in entries.iter().enumerate() {
        let obj = entry
            .as_object()
            .ok_or_else(|| format!("entry {} must be an object", idx))?;
        let slug = obj
            .get("slug")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("entry {} is missing a string 'slug'", idx))?;
        if slug.trim().is_empty() {
            return Err(format!("entry {} has an empty slug", idx));
        }
        let normalized = crate::domain::normalize_domain_slug(slug)
            .ok_or_else(|| format!("entry {} slug '{}' does not normalize", idx, slug))?;
        if normalized != slug {
            return Err(format!(
                "entry {} slug '{}' is not normalized (expected '{}')",
                idx, slug, normalized
            ));
        }
        if slugs.contains(&slug.to_string()) {
            return Err(format!("duplicate slug '{}'", slug));
        }

        let parent = match obj.get("parent") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => {
                if s.trim().is_empty() {
                    return Err(format!("entry {} has an empty parent", idx));
                }
                let pnorm = crate::domain::normalize_domain_slug(s)
                    .ok_or_else(|| format!("entry {} parent '{}' does not normalize", idx, s))?;
                if pnorm != *s {
                    return Err(format!(
                        "entry {} parent '{}' is not normalized (expected '{}')",
                        idx, s, pnorm
                    ));
                }
                Some(s.clone())
            }
            Some(_) => return Err(format!("entry {} parent must be a string or null", idx)),
        };

        let aliases = obj.get("aliases").cloned().unwrap_or(serde_json::json!([]));
        let aliases = aliases
            .as_array()
            .ok_or_else(|| format!("entry {} aliases must be an array", idx))?;
        for (ai, alias) in aliases.iter().enumerate() {
            let a = alias
                .as_str()
                .ok_or_else(|| format!("entry {} alias {} must be a string", idx, ai))?;
            if a.trim().is_empty() {
                return Err(format!("entry {} has an empty alias", idx));
            }
        }

        let concept_iri = obj.get("concept_iri").cloned().unwrap_or(serde_json::Value::Null);
        match concept_iri {
            serde_json::Value::Null => {}
            serde_json::Value::String(_) => {}
            _ => return Err(format!("entry {} concept_iri must be a string or null", idx)),
        }

        slugs.push(slug.to_string());
        parent_of.insert(slug.to_string(), parent);
    }

    // Parent references must resolve to a declared slug.
    for (slug, parent) in &parent_of {
        if let Some(p) = parent {
            if !slugs.contains(p) {
                return Err(format!("parent '{}' of '{}' is not a declared slug", p, slug));
            }
        }
    }

    // Cycle detection.
    for start in &slugs {
        let mut seen = std::collections::HashSet::new();
        let mut cur = Some(start.clone());
        while let Some(node) = cur {
            if !seen.insert(node.clone()) {
                return Err(format!("cycle detected in domain DAG at '{}'", node));
            }
            cur = parent_of.get(&node).cloned().flatten();
            if let Some(next) = &cur {
                if !slugs.contains(next) {
                    break;
                }
            }
        }
    }

    // Aliases must be unique across the namespace (case-folded AND slug-normalized),
    // and must not collide with any slug.
    let mut seen_aliases = std::collections::BTreeSet::new();
    let mut seen_alias_normalized = std::collections::BTreeSet::new();
    // Collect slugs for collision check.
    let slug_set: std::collections::BTreeSet<String> = slugs.iter().cloned().collect();
    let slug_normalized_set: std::collections::BTreeSet<String> =
        slugs.iter().filter_map(|s| crate::domain::normalize_domain_slug(s)).collect();
    for entry in entries {
        if let Some(arr) = entry.get("aliases").and_then(|v| v.as_array()) {
            for alias in arr {
                if let Some(s) = alias.as_str() {
                    let lower = s.to_lowercase();
                    if !seen_aliases.insert(lower.clone()) {
                        return Err(format!("alias '{}' collides with an existing alias (case-folded)", s));
                    }
                    // slug-normalized collision
                    if let Some(norm) = crate::domain::normalize_domain_slug(s) {
                        if slug_set.contains(&norm) || slug_normalized_set.contains(&norm) {
                            return Err(format!("alias '{}' collides with slug '{}'", s, norm));
                        }
                        if !seen_alias_normalized.insert(norm.clone()) {
                            return Err(format!("alias '{}' collides with an existing alias (normalized)", s));
                        }
                    }
                    // case-folded alias vs slug
                    if slug_set.iter().any(|sl| sl.to_lowercase() == lower) {
                        return Err(format!("alias '{}' collides with slug '{}'", s, s));
                    }
                }
            }
        }
    }

    Ok(())
}

/// The outcome of the write-path check. `warning` is for the caller and is
/// never a rejection.
pub struct DomainCheck {
    /// The domain list the cache must hold after this write, or None when the
    /// cache is already current.
    pub cache: Option<Vec<String>>,
    /// A warning for the caller. Never a rejection.
    pub warning: Option<String>,
}

/// Decide whether `raw_domain` may be written in `namespace`, in the order the
/// design fixes: a non-string is rejected, a slugless string is ungoverned, an
/// absent registry bootstraps, a cache hit is free, and only then do the
/// reconcile and `register_domain` run.
pub async fn check_domain(
    store: &Arc<dyn VectorStore>,
    _emb: &Arc<dyn Embedder>,
    namespace: &str,
    raw_domain: &Value,
    register_domain: bool,
) -> Result<DomainCheck, String> {
    // F9: accept string | string[].
    let raws: Vec<String> = match raw_domain {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            let registered = read_domains(store, namespace).await.unwrap_or_default();
            return Err(non_string_rejection(namespace, raw_domain, &registered));
        }
    };
    // If array contained non-strings, reject.
    if let Value::Array(arr) = raw_domain {
        if arr.iter().any(|v| !v.is_string()) {
            let registered = read_domains(store, namespace).await.unwrap_or_default();
            return Err(non_string_rejection(namespace, raw_domain, &registered));
        }
    }

    let mut slugs: Vec<String> = Vec::new();
    for raw in &raws {
        match crate::domain::normalize_domain_slug(raw) {
            Some(slug) => slugs.push(slug),
            None => { /* ungoverned slug; skip registration but not rejection */ }
        }
    }
    slugs = sorted_unique(slugs);

    if slugs.is_empty() {
        return Ok(ungoverned());
    }

    let cached = match store.get(namespace, &registry_row_id(namespace)).await {
        Ok(Some((_, payload))) => Some(registry_domains(&payload.metadata)),
        Ok(None) => None,
        Err(e) => {
            return Err(format!(
                "ERROR [DOMAIN]: the registry row of namespace '{}' could not be read: {}. Nothing was written.",
                namespace, e
            ))
        }
    };

    let cached = match cached {
        None => {
            let mut seeded = derived_domains(store, namespace).await?;
            seeded.extend(slugs.clone());
            return Ok(DomainCheck {
                cache: Some(sorted_unique(seeded)),
                warning: None,
            });
        }
        Some(cached) => cached,
    };

    // Check if all slugs are cached.
    let missing: Vec<String> = slugs.iter().filter(|s| !cached.contains(s)).cloned().collect();
    if missing.is_empty() {
        return Ok(ungoverned());
    }

    // Reconcile: derive from rows.
    let derived = derived_domains(store, namespace).await?;
    let still_missing: Vec<String> = missing.iter().filter(|s| !derived.contains(s)).cloned().collect();
    if still_missing.is_empty() {
        let mut repaired = cached;
        repaired.extend(slugs.clone());
        return Ok(DomainCheck {
            cache: Some(sorted_unique(repaired)),
            warning: None,
        });
    }

    if register_domain {
        let mut accepted = cached;
        accepted.extend(slugs.clone());
        return Ok(DomainCheck {
            cache: Some(sorted_unique(accepted)),
            warning: None,
        });
    }

    Err(rejection(namespace, &still_missing[0], &cached))
}

/// Read-merge-write the registry under a process-wide mutex (the guard is held
/// across awaits, so this is a `tokio` mutex). `None` on success; `Some(warning)`
/// when the cache did not stick -- the write it belongs to has already been
/// accepted, so a cache failure is never a rejection.
pub async fn write_registry(
    store: &Arc<dyn VectorStore>,
    emb: &Arc<dyn Embedder>,
    namespace: &str,
    domains: &[String],
) -> Option<String> {
    static REGISTRY_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    let _guard = REGISTRY_MUTEX.lock().await;
    let registry_id = registry_row_id(namespace);
    // Read existing row (metadata + vector) for merge.
    let existing = store.get(namespace, &registry_id).await.ok().flatten();
    let existing_meta = existing.as_ref().map(|(_, p)| p.metadata.clone()).unwrap_or(serde_json::json!({}));
    let existing_domains_slugs: Vec<String> = if existing_meta.get("domain_registry").is_some() {
        // v2 entries -> slugs; v1 -> strings
        registry_domain_entries(&existing_meta).iter()
            .filter_map(|e| e.get("slug").and_then(|s| s.as_str()).map(|s| s.to_string()))
            .collect()
    } else {
        vec![]
    };
    let is_v2 = existing_meta
        .get("domain_registry")
        .and_then(|r| r.get("version"))
        .and_then(|v| v.as_u64()) == Some(2)
        || existing_meta.get("domain_registry")
            .and_then(|r| r.get("domains"))
            .and_then(|d| d.as_array())
            .map(|a| a.iter().any(|v| v.is_object()))
            .unwrap_or(false);

    let mut wanted_slugs = merge(&existing_domains_slugs, domains);
    // Also include v1 registry_domains if present (backward compat).
    let v1_slugs = registry_domains(&existing_meta);
    wanted_slugs = merge(&wanted_slugs, &v1_slugs);

    let vector = match emb.embed(&registry_content(namespace)).await {
        Ok(vector) => vector,
        Err(e) => return Some(format!("the registry content could not be embedded: {}", e)),
    };

    // At most three attempts.
    for _ in 0..3 {
        // Build metadata preserving v2 entries + unrelated keys.
        let mut meta = existing_meta.clone();
        if meta.is_null() || !meta.is_object() {
            meta = serde_json::json!({});
        }
        // Ensure object
        if !meta.is_object() {
            meta = serde_json::json!({});
        }
        let obj = meta.as_object_mut().unwrap();

        if is_v2 || obj.get("domain_registry")
            .and_then(|r| r.get("domains"))
            .and_then(|d| d.as_array())
            .map(|a| a.iter().any(|v| v.is_object()))
            .unwrap_or(false)
        {
            // v2 path: preserve entries, add new slugs.
            let mut entries = registry_domain_entries(&existing_meta);
            // add missing slugs
            for slug in &wanted_slugs {
                if !entries.iter().any(|e| e.get("slug").and_then(|s| s.as_str()) == Some(slug.as_str())) {
                    entries.push(serde_json::json!({
                        "slug": slug,
                        "parent": serde_json::Value::Null,
                        "aliases": [],
                        "concept_iri": serde_json::Value::Null,
                    }));
                }
            }
            // sort entries by slug
            entries.sort_by(|a, b| {
                let sa = a.get("slug").and_then(|v| v.as_str()).unwrap_or("");
                let sb = b.get("slug").and_then(|v| v.as_str()).unwrap_or("");
                sa.cmp(sb)
            });
            let mut v2meta = registry_metadata_v2(&entries);
            // merge v2meta.domain_registry into meta, preserving other keys
            if let Some(dr) = v2meta.get("domain_registry").cloned() {
                obj.insert("domain_registry".to_string(), dr);
            }
            obj.insert("eligibility".to_string(), serde_json::json!("private"));
        } else {
            // v1 path
            let v1meta = registry_metadata(&wanted_slugs);
            if let Some(dr) = v1meta.get("domain_registry").cloned() {
                obj.insert("domain_registry".to_string(), dr);
            }
            obj.insert("eligibility".to_string(), serde_json::json!("private"));
        }

        let payload = MemoryPayload {
            content: registry_content(namespace),
            user_id: "system".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: meta,
        };
        // vector reuse if existing
        let vec_to_use = existing.as_ref().map(|(v, _)| v.clone()).unwrap_or_else(|| vector.clone());
        if let Err(e) = store
            .upsert(namespace, &registry_id, vec_to_use, payload)
            .await
        {
            return Some(format!("the registry row could not be written: {}", e));
        }
        match store.get(namespace, &registry_id).await {
            Ok(Some((_, landed))) => {
                let landed_slugs: Vec<String> = if landed.metadata.get("domain_registry")
                    .and_then(|r| r.get("domains"))
                    .and_then(|d| d.as_array())
                    .map(|a| a.iter().any(|v| v.is_object()))
                    .unwrap_or(false)
                {
                    registry_domain_entries(&landed.metadata).iter()
                        .filter_map(|e| e.get("slug").and_then(|s| s.as_str()).map(|s| s.to_string()))
                        .collect()
                } else {
                    registry_domains(&landed.metadata)
                };
                if wanted_slugs.iter().all(|slug| landed_slugs.contains(slug)) {
                    return None;
                }
                wanted_slugs = merge(&landed_slugs, &wanted_slugs);
            }
            Ok(None) => {
                return Some("the registry row did not stick: it is absent after the write".to_string())
            }
            Err(e) => return Some(format!("the registry row could not be re-read: {}", e)),
        }
    }

    Some(
        "the registry row was rewritten under us on all three attempts; it is a cache and the next write reconciles it"
            .to_string(),
    )
}

/// The number of domains the cache holds (0 when the row is absent). Task 7's
/// `single_domain_bypasses` counter reads this; not consumed yet.
#[allow(dead_code)]
pub async fn registry_domain_count(store: &Arc<dyn VectorStore>, namespace: &str) -> usize {
    match store.get(namespace, &registry_row_id(namespace)).await {
        Ok(Some((_, payload))) => registry_domains(&payload.metadata).len(),
        Ok(None) | Err(_) => 0,
    }
}

fn ungoverned() -> DomainCheck {
    DomainCheck {
        cache: None,
        warning: None,
    }
}

async fn read_domains(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
) -> Result<Vec<String>, String> {
    match store.get(namespace, &registry_row_id(namespace)).await {
        Ok(Some((_, payload))) => Ok(registry_domains(&payload.metadata)),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(format!("the registry row could not be read: {}", e)),
    }
}

/// The slugs the namespace's own rows carry, through the same normalizer the
/// write path applies. This is the truth the cache approximates.
async fn derived_domains(
    store: &Arc<dyn VectorStore>,
    namespace: &str,
) -> Result<Vec<String>, String> {
    let rows = store.list(namespace, None, None).await.map_err(|e| {
        format!(
            "ERROR [DOMAIN]: the rows of namespace '{}' could not be listed: {}. Nothing was written.",
            namespace, e
        )
    })?;
    let domains: Vec<String> = rows
        .iter()
        .filter(|row| !is_registry_row(&row.payload.metadata))
        .filter_map(|row| crate::domain::domain_key(&row.payload.metadata))
        .collect();
    Ok(sorted_unique(domains))
}

fn rejection(namespace: &str, slug: &str, registered: &[String]) -> String {
    let registered = serde_json::to_string(registered).unwrap_or_else(|_| "[]".to_string());
    format!(
        "ERROR [DOMAIN]: domain '{}' is not registered in namespace '{}'.\nRegistered domains: {}. Use one of those, or pass\nregister_domain: true if '{}' is a genuinely new domain.",
        slug, namespace, registered, slug
    )
}

fn non_string_rejection(namespace: &str, raw: &Value, registered: &[String]) -> String {
    let registered = serde_json::to_string(registered).unwrap_or_else(|_| "[]".to_string());
    format!(
        "ERROR [DOMAIN]: domain {} is not a string in namespace '{}'.\nRegistered domains: {}. A domain must be a string; a non-string value is rejected rather than guessed at.",
        raw, namespace, registered
    )
}

fn sorted_unique(mut domains: Vec<String>) -> Vec<String> {
    domains.sort();
    domains.dedup();
    domains
}

fn merge(left: &[String], right: &[String]) -> Vec<String> {
    let mut merged = left.to_vec();
    merged.extend(right.iter().cloned());
    sorted_unique(merged)
}

/// Cache `project_root` into the namespace's registry row (best-effort).
///
/// Updates the existing row in place (reusing its vector) so no re-embedding
/// is needed. If no row exists and an embedder is provided, creates one with
/// an empty domain list. Never fails loudly: a missing cache is a miss, not
/// an error.
pub async fn cache_project_root(
    store: &Arc<dyn VectorStore>,
    emb: Option<&Arc<dyn Embedder>>,
    namespace: &str,
    project_root: &str,
) {
    let registry_id = registry_row_id(namespace);
    let (vector, mut payload) = match store.get(namespace, &registry_id).await {
        Ok(Some(found)) => found,
        _ => {
            let Some(emb) = emb else { return };
            let content = registry_content(namespace);
            let vector = match emb.embed(&content).await {
                Ok(v) => v,
                Err(_) => return,
            };
            (
                vector,
                MemoryPayload {
                    content,
                    user_id: "system".to_string(),
                    memory_type: "fact".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: registry_metadata(&[]),
                },
            )
        }
    };
    if let Some(obj) = payload.metadata.as_object_mut() {
        obj.insert("project_root".to_string(), serde_json::Value::String(project_root.to_string()));
    }
    let _ = store.upsert(namespace, &registry_id, vector, payload).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_row_id_is_stable_and_namespaced() {
        let id = registry_row_id("NeuroStrata");
        assert_eq!(id, registry_row_id("NeuroStrata"), "stable for a namespace");
        assert_ne!(id, registry_row_id("Other"), "distinct per namespace");
        let parsed = uuid::Uuid::parse_str(&id).expect("the mesh skips a non-UUID id");
        assert_eq!(parsed.get_version_num(), 5);
    }

    #[test]
    fn registry_domains_sorts_and_deduplicates_and_registry_metadata_round_trips() {
        let metadata = serde_json::json!({
            "domain_registry": { "domains": ["database-access", "auth", "auth"] }
        });

        let domains = registry_domains(&metadata);
        assert_eq!(domains, vec!["auth", "database-access"]);

        let round_tripped = registry_metadata(&domains);
        assert_eq!(registry_domains(&round_tripped), domains);
        assert_eq!(round_tripped["eligibility"], serde_json::json!("private"));
    }

    #[test]
    fn registry_domains_of_an_absent_or_malformed_shape_is_empty() {
        assert!(registry_domains(&serde_json::json!({})).is_empty());
        assert!(registry_domains(&serde_json::json!({ "domain_registry": "x" })).is_empty());
    }

    #[test]
    fn only_a_row_carrying_domain_registry_is_the_registry_row() {
        assert!(is_registry_row(&serde_json::json!({
            "domain_registry": { "domains": [] }
        })));
        assert!(!is_registry_row(&serde_json::json!({ "domain": "auth" })));
        assert!(!is_registry_row(&serde_json::json!({})));
    }
}

/// Read the enrichment state record from a namespace's registry row (F12/F14/F24).
pub fn enrichment_state(metadata: &Value) -> Value {
    metadata.get("enrichment_state").cloned().unwrap_or(serde_json::json!({
        "state": "pending",
        "watermark": 0,
        "lock_holder": serde_json::Value::Null,
        "telemetry": {}
    }))
}

/// Build an enrichment state record.
pub fn enrichment_state_record(state: &str, watermark: u64, lock_holder: Option<&str>, telemetry: Value) -> Value {
    serde_json::json!({
        "state": state,
        "watermark": watermark,
        "lock_holder": lock_holder,
        "telemetry": telemetry,
    })
}
