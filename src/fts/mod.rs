//! Full-text search (in-house BM25 approximation, since tantivy conflicts with lbug's zstd).
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, Clone)]
pub struct InvertedIndex {
    pub docs: HashMap<String, String>, // id -> content
    pub term_df: HashMap<String, usize>,
    pub doc_len: HashMap<String, usize>,
}

impl InvertedIndex {
    pub fn new() -> Self { Self::default() }
    pub fn add(&mut self, id: String, content: String) {
        let len = content.split_whitespace().count();
        self.doc_len.insert(id.clone(), len);
        let mut seen: HashSet<String> = HashSet::new();
        for tok in content.split_whitespace() {
            let t = tok.to_lowercase();
            if seen.insert(t.clone()) {
                *self.term_df.entry(t).or_insert(0) += 1;
            }
        }
        self.docs.insert(id, content);
    }
    /// BM25-ish score for query against doc.
    pub fn score(&self, query: &str, doc_id: &str) -> f32 {
        let content = match self.docs.get(doc_id) { Some(c) => c.to_lowercase(), None => return 0.0 };
        let n = self.docs.len() as f32;
        let avgdl = if self.doc_len.is_empty() { 1.0 } else {
            self.doc_len.values().sum::<usize>() as f32 / self.doc_len.len() as f32
        };
        let dl = *self.doc_len.get(doc_id).unwrap_or(&1) as f32;
        let mut score = 0.0f32;
        for q in query.split_whitespace() {
            let q = q.to_lowercase();
            let tf = content.matches(&q).count() as f32;
            let df = *self.term_df.get(&q).unwrap_or(&0) as f32;
            if df == 0.0 { continue; }
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            let denom = tf + 1.2 * (1.0 - 0.75 + 0.75 * dl / avgdl);
            score += idf * (tf * (1.0 + 1.2)) / denom;
        }
        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic_search() {
        let mut idx = InvertedIndex::new();
        idx.add("a".into(), "hello world".into());
        idx.add("b".into(), "foo bar".into());
        assert!(idx.score("hello", "a") > idx.score("hello", "b"));
    }
}
