//! Ontology drift detection (in-house).
pub fn drift(old: &[String], new: &[String]) -> Vec<String> {
    new.iter().filter(|x| !old.contains(x)).cloned().collect()
}
pub fn removed(old: &[String], new: &[String]) -> Vec<String> {
    old.iter().filter(|x| !new.contains(x)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_added() {
        assert_eq!(drift(&["a".into()], &["a".into(),"b".into()]), vec!["b".to_string()]);
    }
    #[test]
    fn detects_removed() {
        assert_eq!(removed(&["a".into(),"b".into()], &["a".into()]), vec!["b".to_string()]);
    }
}
