//! AC4 task_complete pre-check — dirty-target normalization (Task 4).
//! Before gating task to Done (Lock 2), verify 0 resolvable targets remain.

/// Full AC4 pass over one namespace before completion: normalize declared
/// targets in place, then re-classify and enforce the gate (orphaned targets
/// warn and pass; resolvable declared targets block).
pub async fn normalize_and_precheck_namespace(
    store: &dyn crate::traits::VectorStore,
    namespace: &str,
) -> Result<(), String> {
    crate::events::subscribers::post_ingest_normalize::normalize_namespace_targets(
        store, namespace,
    )
    .await
    .inspect_err(|e| {
        tracing::warn!("precheck normalize_namespace_targets failed for '{namespace}': {e}");
    })?;
    let memories = store
        .list(namespace, None, None)
        .await
        .map_err(|e| e.to_string())?;
    let summary = crate::diagnostic::classify::classify_namespace_targets(&memories);
    match crate::diagnostic::classify::classify_unmatched(
        summary.materialized,
        summary.declared,
        summary.orphaned,
    ) {
        Ok(Some(warning)) => tracing::warn!("{warning}"),
        Ok(None) => {}
        Err(e) => tracing::warn!("task_complete_precheck classify_unmatched blocked: {e}"),
    }
    task_complete_precheck(&summary.declared_targets)
}

pub fn task_complete_precheck(dirty_target_ids: &[String]) -> Result<(), String> {
    if dirty_target_ids.is_empty() {
        Ok(()) // AC4 passes: doctor reports 0 resolvable targets
    } else {
        Err(format!(
            "AC4 blocked: {} resolvable targets dirty before task_complete (must normalize via ThalamicBus post-ingest subscriber)",
            dirty_target_ids.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precheck_empty_ok() {
        assert!(task_complete_precheck(&[]).is_ok());
    }
}
