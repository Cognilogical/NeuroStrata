//! Multimodal validation helpers (in-house).
pub fn embed_text(s: &str) -> Vec<f32> { s.bytes().map(|b| b as f32 / 255.0).collect() }
pub fn validate_mime(mime: &str) -> bool {
    matches!(mime, "text/plain" | "image/png" | "image/jpeg" | "audio/wav")
}
pub fn describe(bytes: &[u8]) -> String {
    format!("bytes={}", bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mime() { assert!(validate_mime("image/png")); assert!(!validate_mime("application/exe")); }
    #[test]
    fn describe_len() { assert_eq!(describe(&[1,2,3]), "bytes=3"); }
}
