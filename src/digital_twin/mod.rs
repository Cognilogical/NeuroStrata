//! Digital twin codebase sim (in-house): placeholder state holder.
#[derive(Debug, Default, Clone)]
pub struct Twin { pub files: usize, pub symbols: usize }
impl Twin { pub fn new() -> Self { Self::default() } }
#[cfg(test)] mod tests { use super::*; #[test] fn builds() { let t=Twin::new(); assert_eq!(t.files,0); } }
