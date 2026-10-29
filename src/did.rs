//! DID / signed record helpers (in-house, HMAC-SHA256 pseudo-signing).
use sha2::{Digest, Sha256};

pub fn signer_id(seed: &str) -> String {
    format!("did:neurostrata:{}", &hex(&sha256(seed.as_bytes()))[..16])
}

pub fn sign(record: &str, key: &str) -> String {
    hex(&sha256(format!("{}:{}", key, record).as_bytes()))
}

pub fn verify(record: &str, key: &str, sig: &str) -> bool {
    sign(record, key) == sig
}

fn sha256(data: &[u8]) -> [u8;32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sign_verify() {
        let sig = sign("rec", "key");
        assert!(verify("rec","key",&sig));
        assert!(!verify("rec","key","bad"));
    }
    #[test]
    fn did() {
        assert!(signer_id("seed").starts_with("did:neurostrata:"));
    }
}
