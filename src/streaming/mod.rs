//! Streaming delta (RDF Patch-style) in-house.
#[derive(Debug, Default, Clone)]
pub struct Stream { pub events: Vec<String> }

impl Stream {
    pub fn push(&mut self, e: String) { self.events.push(e); }
    pub fn len(&self) -> usize { self.events.len() }
    pub fn to_patch(&self) -> String {
        self.events.iter().map(|e| format!("A {}", e)).collect::<Vec<_>>().join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct Delta { pub op: String, pub subject: String, pub predicate: String, pub object: String }

impl Delta {
    pub fn to_ntriples(&self) -> String {
        format!("{} {} {} .", self.subject, self.predicate, self.object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patch() {
        let mut s = Stream::default();
        s.push("x".into());
        assert!(s.to_patch().contains("A x"));
    }
    #[test]
    fn delta() {
        let d = Delta { op: "A".into(), subject: "s".into(), predicate: "p".into(), object: "o".into() };
        assert_eq!(d.to_ntriples(), "s p o .");
    }
}
