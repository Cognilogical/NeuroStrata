use crate::traits::MemoryPayload;
use anyhow::Result;

/// Stable canonical identity for a memory payload.
///
/// Two payloads produce the same key iff they are canonical duplicates. The
/// key is a deterministic serialization of a fixed field set; it never
/// consults vector distance or model scores.
pub fn canonical_duplicate_key(namespace: &str, payload: &MemoryPayload) -> Result<String> {
    // Sorted governs list.
    let mut governs: Vec<String> = payload
        .metadata
        .get("governs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    governs.sort();

    let mut contained_by: Vec<String> = payload
        .metadata
        .get("contained_by")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    contained_by.sort();

    // Canonicalized refs: only file, lines, symbol; ignore source_context.
    let mut refs: Vec<serde_json::Value> = payload
        .metadata
        .get("refs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    let mut obj = serde_json::Map::new();
                    if let Some(f) = r.get("file").and_then(|v| v.as_str()) {
                        obj.insert("file".to_string(), serde_json::json!(f));
                    }
                    if let Some(l) = r.get("lines").and_then(|v| v.as_str()) {
                        obj.insert("lines".to_string(), serde_json::json!(l));
                    }
                    if let Some(s) = r.get("symbol").and_then(|v| v.as_str()) {
                        obj.insert("symbol".to_string(), serde_json::json!(s));
                    }
                    serde_json::Value::Object(obj)
                })
                .collect()
        })
        .unwrap_or_default();
    // Sort refs by their serialized form for determinism.
    refs.sort_by_key(|r| r.to_string());

    // Rule enforcement/guard.
    let enforcement = payload
        .metadata
        .get("enforcement")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let guard = payload
        .metadata
        .get("guard")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    // Procedure trigger.
    let trigger = payload
        .metadata
        .get("trigger")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    // Fact attempt: presence + four keys.
    let attempt = if payload.memory_type == "fact" {
        payload.metadata.get("attempt").cloned()
    } else {
        // Still include if present? Plan: fact rows only. Keep null for non-fact.
        None
    };
    let attempt_presence = attempt.is_some();
    let attempt_obj = attempt.unwrap_or(serde_json::Value::Null);
    let attempt_approach = attempt_obj.get("approach").cloned().unwrap_or(serde_json::Value::Null);
    let attempt_outcome = attempt_obj.get("outcome").cloned().unwrap_or(serde_json::Value::Null);
    let attempt_failure_mode = attempt_obj.get("failure_mode").cloned().unwrap_or(serde_json::Value::Null);
    let attempt_context = attempt_obj.get("context").cloned().unwrap_or(serde_json::Value::Null);

    let key_obj = serde_json::json!({
        "namespace": namespace,
        "content": payload.content,
        "memory_type": payload.memory_type,
        "location": payload.location,
        "location_lines": payload.location_lines,
        "governs": governs,
        "contained_by": contained_by,
        "refs": refs,
        "enforcement": enforcement,
        "guard": guard,
        "trigger": trigger,
        "attempt_presence": attempt_presence,
        "attempt_approach": attempt_approach,
        "attempt_outcome": attempt_outcome,
        "attempt_failure_mode": attempt_failure_mode,
        "attempt_context": attempt_context,
    });

    Ok(key_obj.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MemoryPayload;

    fn payload(content: &str, memory_type: &str) -> MemoryPayload {
        MemoryPayload {
            content: content.to_string(),
            user_id: "u".to_string(),
            memory_type: memory_type.to_string(),
            agent_name: None,
            location: "src/lib.rs".to_string(),
            location_lines: "1-2".to_string(),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn identical_payloads_same_key() {
        let a = payload("hello", "fact");
        let b = payload("hello", "fact");
        assert_eq!(canonical_duplicate_key("ns", &a).unwrap(), canonical_duplicate_key("ns", &b).unwrap());
    }

    #[test]
    fn failed_attempt_vs_non_attempt_differ() {
        let mut a = payload("hello", "fact");
        a.metadata = serde_json::json!({"attempt": {"approach":"r","outcome":"failed","failure_mode":"t","context":"c"}});
        let b = payload("hello", "fact");
        assert_ne!(canonical_duplicate_key("ns", &a).unwrap(), canonical_duplicate_key("ns", &b).unwrap());
    }

    #[test]
    fn attempt_four_keys_matter() {
        let mut a = payload("hello", "fact");
        a.metadata = serde_json::json!({"attempt": {"approach":"r","outcome":"failed","failure_mode":"t","context":"c"}});
        let mut b = a.clone();
        b.metadata = serde_json::json!({"attempt": {"approach":"r2","outcome":"failed","failure_mode":"t","context":"c"}});
        assert_ne!(canonical_duplicate_key("ns", &a).unwrap(), canonical_duplicate_key("ns", &b).unwrap());
    }
}
