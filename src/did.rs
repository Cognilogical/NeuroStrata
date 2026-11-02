use openssl::pkey::{PKey, Id, Private};
use openssl::sign::{Signer, Verifier};
use serde_json;
use std::fs;
use std::path::Path;

pub const ED25519_MULTICODEC_PREFIX: [u8; 2] = [0xed, 0x01];

// ============================================================
// §5.1 Self-contained Base64 (RFC 4648)
// ============================================================
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode_base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b: [u8; 3] = match chunk.len() {
            1 => [chunk[0], 0, 0],
            2 => [chunk[0], chunk[1], 0],
            _ => [chunk[0], chunk[1], chunk[2]],
        };
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(B64[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for ch in input.bytes() {
        if ch == b'=' {
            continue;
        }
        let val = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(format!("invalid base64 char: {}", ch as char)),
        } as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buf >> bits) as u8);
            buf &= (1u32 << bits) - 1;
        }
    }
    Ok(bytes)
}

// ============================================================
// §5.1 Self-contained Base58btc (Bitcoin alphabet)
// ============================================================
const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

pub fn encode_base58btc(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    // Count leading zero bytes → '1' chars
    let mut zeros = 0usize;
    while zeros < bytes.len() && bytes[zeros] == 0 {
        zeros += 1;
    }
    // Work on a mutable copy (without leading zeros for arithmetic)
    let mut num: Vec<u8> = bytes[zeros..].to_vec();
    let mut out = String::with_capacity(bytes.len() * 2);
    // Prepend '1' for each leading zero byte
    for _ in 0..zeros {
        out.push('1');
    }
    // Repeat division by 58
    while !num.is_empty() && !(num.len() == 1 && num[0] == 0) {
        let mut rem = 0u16;
        let mut next = Vec::with_capacity(num.len());
        for &b in &num {
            let v = rem * 256 + (b as u16);
            let digit = (v / 58) as u8;
            rem = (v % 58) as u16;
            if !next.is_empty() || digit != 0 {
                next.push(digit);
            }
        }
        out.push(B58[rem as usize] as char);
        num = next;
    }
    // Reverse the digits produced (they come out LSB first)
    let chars: Vec<char> = out.chars().collect();
    let prefix_len = zeros;
    let reversed = chars[prefix_len..].iter().rev().cloned().collect::<String>();
    out = chars[..prefix_len].iter().cloned().collect();
    out.push_str(&reversed);
    out
}

pub fn decode_base58btc(input: &str) -> Result<Vec<u8>, String> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let mut leading_zeros = 0usize;
    while leading_zeros < input.len() && input.as_bytes()[leading_zeros] == b'1' {
        leading_zeros += 1;
    }
    // Little-endian temporary (index 0 = LSB) for arithmetic
    let mut le = Vec::new();
    for ch in input.bytes().skip(leading_zeros) {
        let digit = B58.iter().position(|&b| b == ch).ok_or_else(|| format!("invalid base58 char: {}", ch as char))? as u32;
        let mut carry = digit;
        for b in le.iter_mut() {
            let v = (*b as u32) * 58 + carry;
            *b = (v & 0xFF) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            le.push((carry & 0xFF) as u8);
            carry >>= 8;
        }
    }
    le.reverse(); // now big-endian
    let mut result = vec![0u8; leading_zeros];
    result.extend_from_slice(&le);
    Ok(result)
}

// ============================================================
// DID key methods (§5.1 / §5.2)
// ============================================================

pub fn signer_did(pubkey: &[u8]) -> String {
    let mut prefixed = Vec::with_capacity(ED25519_MULTICODEC_PREFIX.len() + pubkey.len());
    prefixed.extend_from_slice(&ED25519_MULTICODEC_PREFIX);
    prefixed.extend_from_slice(pubkey);
    format!("did:key:z{}", encode_base58btc(&prefixed))
}

pub fn public_key_from_did(did: &str) -> Result<Vec<u8>, String> {
    let prefix = "did:key:z";
    if !did.starts_with(prefix) {
        return Err("invalid did:key prefix".into());
    }
    let encoded = &did[prefix.len()..];
    let decoded = decode_base58btc(encoded)?;
    if decoded.len() < ED25519_MULTICODEC_PREFIX.len() {
        return Err("decoded did too short".into());
    }
    if &decoded[..ED25519_MULTICODEC_PREFIX.len()] != &ED25519_MULTICODEC_PREFIX[..] {
        return Err("invalid ed25519 multicodec prefix".into());
    }
    let pubkey = decoded[ED25519_MULTICODEC_PREFIX.len()..].to_vec();
    if pubkey.len() != 32 {
        return Err(format!("expected 32-byte ed25519 pubkey, got {}", pubkey.len()));
    }
    Ok(pubkey)
}

// ============================================================
// Keypair management
// ============================================================

pub fn generate_keypair() -> Result<(PKey<Private>, String, Vec<u8>), String> {
    let pkey = PKey::generate_ed25519().map_err(|e| format!("generate_ed25519: {}", e))?;
    let pubkey = pkey.raw_public_key().map_err(|e| format!("raw_public_key: {}", e))?;
    let did = signer_did(&pubkey);
    Ok((pkey, did, pubkey))
}

pub fn keypair_from_private_pem(pem: &[u8]) -> Result<(PKey<Private>, String, Vec<u8>), String> {
    let pkey = PKey::private_key_from_pem(pem).map_err(|e| format!("private_key_from_pem: {}", e))?;
    let pubkey = pkey.raw_public_key().map_err(|e| format!("raw_public_key: {}", e))?;
    let did = signer_did(&pubkey);
    Ok((pkey, did, pubkey))
}

pub fn keypair_to_private_pem(pkey: &PKey<Private>) -> Result<Vec<u8>, String> {
    pkey.private_key_to_pem_pkcs8().map_err(|e| format!("private_key_to_pem: {}", e))
}

pub fn load_or_generate_keypair(path: &Path) -> Result<(PKey<Private>, String, Vec<u8>), String> {
    if path.exists() {
        let pem = fs::read(path).map_err(|e| format!("read pem: {}", e))?;
        return keypair_from_private_pem(&pem);
    }
    // Generate new
    let (pkey, did, pubkey) = generate_keypair()?;
    // Create parent directories
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create_dir_all: {}", e))?;
    }
    let pem = keypair_to_private_pem(&pkey)?;
    fs::write(path, &pem).map_err(|e| format!("write pem: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(path).map_err(|e| format!("metadata: {}", e))?.permissions();
        perm.set_mode(0o600);
        fs::set_permissions(path, perm).map_err(|e| format!("set_permissions: {}", e))?;
    }
    Ok((pkey, did, pubkey))
}

// ============================================================
// Sign / Verify
// ============================================================

pub fn sign(payload: &[u8], privkey: &PKey<Private>) -> Result<Vec<u8>, String> {
    let mut signer = Signer::new_without_digest(privkey).map_err(|e| format!("Signer::new: {}", e))?;
    signer.sign_oneshot_to_vec(payload).map_err(|e| format!("sign_oneshot: {}", e))
}

pub fn verify(payload: &[u8], pubkey: &[u8], sig: &[u8]) -> bool {
    let pkey_result = PKey::public_key_from_raw_bytes(pubkey, Id::ED25519);
    if pkey_result.is_err() {
        return false;
    }
    let pkey = pkey_result.unwrap();
    let mut verifier = match Verifier::new_without_digest(&pkey) {
        Ok(v) => v,
        Err(_) => return false,
    };
    verifier.verify_oneshot(sig, payload).unwrap_or(false)
}

// ============================================================
// §5.2 Canonical JSON helpers
// ============================================================

pub fn canonicalize_json_value(val: &serde_json::Value) -> serde_json::Value {
    match val {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut new_map = serde_json::Map::new();
            for k in keys {
                new_map.insert(k.clone(), canonicalize_json_value(&map[k]));
            }
            serde_json::Value::Object(new_map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(|v| canonicalize_json_value(v)).collect())
        }
        other => other.clone(),
    }
}

pub fn canonical_signing_bytes(record_json: &serde_json::Value) -> Result<Vec<u8>, String> {
    let mut record = record_json.clone();
    if let serde_json::Value::Object(ref mut map) = record {
        map.remove("signature");
    }
    // Normalize through the canonical value walker so signing bytes are
    // produced from the canonicalized (not verbatim) document.
    let normalized = canonicalize_json_value(&record);
    serde_json::to_vec(&normalized).map_err(|e| format!("serde_json::to_vec: {}", e))
}

/// Canonical signing payload per `2026-10-28-did-signed-mesh-records-design.md` §5.2:
/// `{canonical_nquads}\n{update_id}\n{origin_mesh_epoch}`.
pub fn signing_payload(canonical_nquads: &str, update_id: &str, origin_mesh_epoch: u32) -> Vec<u8> {
    format!("{canonical_nquads}\n{update_id}\n{origin_mesh_epoch}").into_bytes()
}

// ============================================================
// Unit tests
// ============================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let cases: Vec<Vec<u8>> = vec![
            vec![0x00, 0x01, 0x02, 0xff],
            vec![],
            vec![0x00],
            vec![0x00, 0x00, 0x00, 0xab, 0xcd],
            (0..255).collect(),
        ];
        for data in cases {
            let enc = encode_base64(&data);
            let dec = decode_base64(&enc).unwrap();
            assert_eq!(data, dec, "base64 mismatch for {:?}", data);
        }
    }

    #[test]
    fn base58btc_roundtrip() {
        let cases: Vec<Vec<u8>> = vec![
            vec![0x00, 0x00, 0x01, 0x02],
            vec![0x00],
            vec![0x01],
            vec![0xed, 0x01, 0x00, 0x00, 0x01],
            (0..32).map(|i| (i * 7) as u8).collect(),
        ];
        for data in cases {
            let enc = encode_base58btc(&data);
            let dec = decode_base58btc(&enc).unwrap();
            assert_eq!(data, dec, "base58btc mismatch for {:?}", data);
        }
    }

    #[test]
    fn signer_did_and_public_key_from_did_roundtrip() {
        let (_pkey, did, pubkey) = generate_keypair().unwrap();
        assert!(did.starts_with("did:key:z6Mk"));
        assert_eq!(pubkey.len(), 32);
        let recovered = public_key_from_did(&did).unwrap();
        assert_eq!(pubkey, recovered);
        // Verify that decode of a wrong did fails
        assert!(public_key_from_did("did:key:zbad").is_err());
    }

    #[test]
    fn ed25519_sign_verify_roundtrip() {
        let (privkey, did, pubkey) = generate_keypair().unwrap();
        assert!(!did.is_empty());
        let payload = b"test payload for ed25519";
        let sig = sign(payload, &privkey).unwrap();
        assert!(!sig.is_empty());
        assert!(verify(payload, &pubkey, &sig));
        // Tampered payload rejected
        assert!(!verify(b"tampered", &pubkey, &sig));
        // Wrong pubkey rejected
        let (_, _, wrong_pub) = generate_keypair().unwrap();
        assert!(!verify(payload, &wrong_pub, &sig));
    }

    #[test]
    fn canonical_signing_bytes_excludes_signature() {
        let record = serde_json::json!({
            "schema_version": "synaptic-mesh-record/v1",
            "id": "r1",
            "signature": "badbase64",
            "signer_did": "did:key:z6Mktest"
        });
        let bytes = canonical_signing_bytes(&record).unwrap();
        let reconstructed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(reconstructed.get("signature").is_none());
        assert_eq!(reconstructed.get("id").unwrap().as_str().unwrap(), "r1");
    }

    #[test]
    fn load_or_generate_and_pem_roundtrip() {
        use std::env;
        let tmp = env::temp_dir().join("neurostrata_did_test.key");
        let _ = fs::remove_file(&tmp);
        let (pkey, did, pubkey) = load_or_generate_keypair(&tmp).unwrap();
        assert!(!did.is_empty());
        assert_eq!(pubkey.len(), 32);
        // Load back
        let (_pkey2, did2, pubkey2) = load_or_generate_keypair(&tmp).unwrap();
        assert_eq!(did, did2);
        assert_eq!(pubkey, pubkey2);
        // Keypair to PEM and back
        let pem = keypair_to_private_pem(&pkey).unwrap();
        let (_pkey3, did3, pubkey3) = keypair_from_private_pem(&pem).unwrap();
        assert_eq!(did, did3);
        assert_eq!(pubkey, pubkey3);
        // Clean up
        let _ = fs::remove_file(&tmp);
    }
}
