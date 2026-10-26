//! AC4 task_complete pre-check — dirty-target normalization (Task 4).
//! Before gating task to Done (Lock 2), verify 0 resolvable targets remain.

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
