//! Optional RAG chat layer (in-house, deterministic prompt builder).
pub fn build_prompt(context: &str, question: &str) -> String {
    format!("Context:\n{}\n\nQuestion: {}\nAnswer:", context, question)
}
pub fn answer_offline(context: &str, question: &str) -> String {
    format!("Based on context ({} chars), question: {}", context.len(), question)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt() { assert!(build_prompt("ctx","q?").contains("ctx")); }
    #[test]
    fn offline() { assert!(answer_offline("ctx","q?").contains("q?")); }
}
