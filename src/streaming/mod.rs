//! Streaming continuous queries (in-house): simple event buffer.
#[derive(Debug, Default)]
pub struct Stream { pub events: Vec<String> }
impl Stream { pub fn push(&mut self, e: String) { self.events.push(e); } pub fn len(&self) -> usize { self.events.len() } }
#[cfg(test)] mod tests { use super::*; #[test] fn push() { let mut s=Stream::default(); s.push("e".into()); assert_eq!(s.len(),1); } }
