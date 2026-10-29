//! Incremental truth maintenance (Argon addition): retraction helpers.
pub fn retract_validity(valid_from: i64, now: i64) -> (i64, i64) {
    (valid_from, now)
}
pub fn is_active(valid_from: i64, valid_to: i64, now: i64) -> bool {
    now >= valid_from && now < valid_to
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retracts() { assert_eq!(retract_validity(10, 20), (10,20)); }
    #[test]
    fn active() { assert!(is_active(10,20,15)); assert!(!is_active(10,20,25)); }
}
