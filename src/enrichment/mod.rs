//! GLiNER sidecar (in-house): SHA-256 bundle verification + candidate emission gate.
use sha2::{Digest, Sha256};
use std::path::Path;

/// Verify a model bundle's SHA-256 against an expected digest.
pub fn verify_bundle(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let got = format!("{:x}", h.finalize());
    if got.eq_ignore_ascii_case(expected_sha256) {
        Ok(())
    } else {
        Err(format!("bundle checksum mismatch: expected {} got {}", expected_sha256, got))
    }
}

/// Emit candidate spans. Requires a verified bundle; returns an error if absent.
pub fn emit_candidates(text: &str, bundle_path: Option<&Path>, expected_sha: Option<&str>) -> Result<Vec<String>, String> {
    let Some(bp) = bundle_path else {
        return Err("no model bundle provided; GLiNER sidecar inactive".to_string());
    };
    let Some(sha) = expected_sha else {
        return Err("no expected checksum provided".to_string());
    };
    verify_bundle(bp, sha)?;
    // Real ONNX inference would run here; in-house approximation returns
    // capitalized tokens as candidate spans.
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        if tok.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            out.push(tok.to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn verify_missing_fails() {
        assert!(verify_bundle(Path::new("/nonexistent"), "abc").is_err());
    }
    #[test]
    fn emit_without_bundle_errors() {
        assert!(emit_candidates("Hello world", None, None).is_err());
    }
}
