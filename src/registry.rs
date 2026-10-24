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
                .filter_map(|value| value.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    sorted_unique(domains)
}

/// The registry row's metadata. `eligibility: private` keeps the namespace's
/// vocabulary out of the mesh: it is namespace-local, so it never travels.
pub fn registry_metadata(domains: &[String]) -> Value {
    serde_json::json!({
        "domain_registry": { "domains": sorted_unique(domains.to_vec()) },
        "eligibility": "private",
    })
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
    let raw = match raw_domain.as_str() {
        Some(raw) => raw,
        None => {
            let registered = read_domains(store, namespace).await.unwrap_or_default();
            return Err(non_string_rejection(namespace, raw_domain, &registered));
        }
    };
    // A string with no usable slug has no slug to register, and refusing it
    // would leave the caller with no remedy. The write stores it as given.
    let slug = match crate::domain::normalize_domain_slug(raw) {
        Some(slug) => slug,
        None => return Ok(ungoverned()),
    };

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
        // The bootstrap: the namespace's first domain write grandfathered every
        // legacy domain at once. The write always succeeds; err toward
        // registering more.
        None => {
            let mut seeded = derived_domains(store, namespace).await?;
            seeded.push(slug);
            return Ok(DomainCheck {
                cache: Some(sorted_unique(seeded)),
                warning: None,
            });
        }
        Some(cached) => cached,
    };

    if cached.contains(&slug) {
        // The hot path: one read, no write.
        return Ok(ungoverned());
    }

    // The reconcile: a cache miss is not a rejection when the namespace's own
    // rows carry the slug. The same normalizer on both paths, or a never-used
    // slug could slip through as a match.
    let derived = derived_domains(store, namespace).await?;
    if derived.contains(&slug) {
        let mut repaired = cached;
        repaired.extend(derived);
        return Ok(DomainCheck {
            cache: Some(sorted_unique(repaired)),
            warning: None,
        });
    }

    if register_domain {
        let mut accepted = cached;
        accepted.push(slug);
        return Ok(DomainCheck {
            cache: Some(sorted_unique(accepted)),
            warning: None,
        });
    }

    Err(rejection(namespace, &slug, &cached))
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
    let mut wanted = match store.get(namespace, &registry_id).await {
        Ok(Some((_, payload))) => merge(&registry_domains(&payload.metadata), domains),
        Ok(None) => merge(&[], domains),
        Err(e) => return Some(format!("the registry row could not be read: {}", e)),
    };
    let vector = match emb.embed(&registry_content(namespace)).await {
        Ok(vector) => vector,
        Err(e) => return Some(format!("the registry content could not be embedded: {}", e)),
    };

    // At most three attempts: enough for a rare registration race, no more.
    for _ in 0..3 {
        let payload = MemoryPayload {
            content: registry_content(namespace),
            user_id: "system".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: registry_metadata(&wanted),
        };
        if let Err(e) = store
            .upsert(namespace, &registry_id, vector.clone(), payload)
            .await
        {
            return Some(format!("the registry row could not be written: {}", e));
        }
        match store.get(namespace, &registry_id).await {
            Ok(Some((_, landed))) => {
                let landed = registry_domains(&landed.metadata);
                // Every slug in the pre-write set must still be present: the
                // check is against the whole set, not only the new slug.
                if wanted.iter().all(|slug| landed.contains(slug)) {
                    return None;
                }
                wanted = merge(&landed, &wanted);
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
