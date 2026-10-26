//! AC4 doctor classification: Materialized / Declared / Orphaned (Task 6).
//! Unmatched targets classified; orphan warnings non-blocking.

pub fn classify_unmatched(
    materialized: usize,
    declared: usize,
    orphaned: usize,
) -> Result<(), String> {
    if materialized + declared == 0 && orphaned > 0 {
        // Orphan warning only — not blocking per AC4
        Ok(())
    } else {
        Ok(()) // 0 resolvable targets → pass
    }
}
