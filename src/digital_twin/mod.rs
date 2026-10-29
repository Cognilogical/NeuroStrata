//! Digital twin domain module (in-house, optional).
#[derive(Debug, Default, Clone)]
pub struct Twin { pub id: String, pub state: std::collections::BTreeMap<String, f64> }
impl Twin {
    pub fn new(id: &str) -> Self { Self { id: id.to_string(), state: Default::default() } }
    pub fn set(&mut self, k: &str, v: f64) { self.state.insert(k.to_string(), v); }
    pub fn get(&self, k: &str) -> Option<f64> { self.state.get(k).copied() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn twin_state() {
        let mut t = Twin::new("t1");
        t.set("temp", 20.0);
        assert_eq!(t.get("temp"), Some(20.0));
    }
}
