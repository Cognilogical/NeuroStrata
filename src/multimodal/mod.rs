use serde::{Deserialize, Serialize};
use std::fmt;

pub const DEFAULT_ALLOWED_MIMES: [&str; 11] = [
    "text/plain", "text/markdown", "text/csv",
    "image/png", "image/jpeg", "image/gif", "image/webp",
    "audio/wav", "audio/mpeg", "audio/ogg", "application/pdf",
];

pub const IMAGE_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub mime: String,
    pub size_bytes: usize,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultimodalConfig {
    #[serde(default = "default_false")]
    pub strict: bool,
    #[serde(default = "default_allowed_mimes")]
    pub allowed_mimes: Vec<String>,
    #[serde(default = "default_10mb")]
    pub max_attachment_bytes: usize,
    #[serde(default = "default_50mb")]
    pub max_total_bytes: usize,
    #[serde(default = "default_20")]
    pub max_attachments: usize,
    #[serde(default = "default_8192")]
    pub max_width: u32,
    #[serde(default = "default_8192")]
    pub max_height: u32,
    #[serde(default = "default_true")]
    pub verify_digests: bool,
    #[serde(default = "default_false")]
    pub require_dimensions: bool,
    #[serde(default = "default_false")]
    pub share_attachments: bool,
}

fn default_false() -> bool { false }
fn default_true() -> bool { true }
fn default_10mb() -> usize { 10 * 1024 * 1024 }
fn default_50mb() -> usize { 50 * 1024 * 1024 }
fn default_20() -> usize { 20 }
fn default_8192() -> u32 { 8192 }
fn default_allowed_mimes() -> Vec<String> {
    DEFAULT_ALLOWED_MIMES.iter().map(|s| s.to_string()).collect()
}

impl Default for MultimodalConfig {
    fn default() -> Self {
        Self {
            strict: false,
            allowed_mimes: default_allowed_mimes(),
            max_attachment_bytes: default_10mb(),
            max_total_bytes: default_50mb(),
            max_attachments: default_20(),
            max_width: default_8192(),
            max_height: default_8192(),
            verify_digests: true,
            require_dimensions: false,
            share_attachments: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MultimodalWarning {
    EmptyId,
    EmptyUri,
    UnsupportedMime { mime: String },
    SizeExceeded { size: usize, cap: usize },
    SizeZero,
    TotalSizeExceeded { total: usize, cap: usize },
    CountExceeded { count: usize, cap: usize },
    DigestMalformed { digest: String },
    DigestMismatch { expected: String, actual: String },
    DigestUnverifiable { uri: String },
    DimensionsMissing { mime: String },
    DimensionsUnparseable { mime: String },
    DimensionsExceeded { width: u32, height: u32, max_width: u32, max_height: u32 },
    DimensionsMismatch { declared: (u32, u32), parsed: (u32, u32) },
    AltTextTooLong { len: usize, cap: usize },
    DuplicateId { id: String },
    AttachmentsNotArray,
    ByteLengthMismatch { declared: usize, actual: usize },
    /// An element of the `attachments` array did not deserialize into
    /// [`Attachment`] (missing/ill-typed required fields). Detection is not
    /// optional: silently skipping would validate a smaller set than declared.
    AttachmentUnparseable { index: usize, error: String },
}

impl MultimodalWarning {
    pub fn is_strict_failure(&self, require_dimensions: bool) -> bool {
        use MultimodalWarning::*;
        match self {
            EmptyId | EmptyUri | UnsupportedMime { .. } | SizeExceeded { .. } | SizeZero
            | TotalSizeExceeded { .. } | CountExceeded { .. } | DigestMalformed { .. }
            | DigestMismatch { .. } | DimensionsExceeded { .. } | DimensionsMismatch { .. }
            | AltTextTooLong { .. } | DuplicateId { .. } | AttachmentsNotArray
            | AttachmentUnparseable { .. } | ByteLengthMismatch { .. } => true,
            DigestUnverifiable { .. } => false,
            DimensionsMissing { .. } => require_dimensions,
            DimensionsUnparseable { .. } => require_dimensions,
        }
    }
}

impl fmt::Display for MultimodalWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MultimodalWarning::EmptyId => write!(f, "EmptyId"),
            MultimodalWarning::EmptyUri => write!(f, "EmptyUri"),
            MultimodalWarning::UnsupportedMime { mime } => write!(f, "UnsupportedMime({})", mime),
            MultimodalWarning::SizeExceeded { size, cap } => write!(f, "SizeExceeded({} > {})", size, cap),
            MultimodalWarning::SizeZero => write!(f, "SizeZero"),
            MultimodalWarning::TotalSizeExceeded { total, cap } => write!(f, "TotalSizeExceeded({} > {})", total, cap),
            MultimodalWarning::CountExceeded { count, cap } => write!(f, "CountExceeded({} > {})", count, cap),
            MultimodalWarning::DigestMalformed { digest } => write!(f, "DigestMalformed({})", digest),
            MultimodalWarning::DigestMismatch { expected, actual } => write!(f, "DigestMismatch(expected={}, actual={})", expected, actual),
            MultimodalWarning::DigestUnverifiable { uri } => write!(f, "DigestUnverifiable({})", uri),
            MultimodalWarning::DimensionsMissing { mime } => write!(f, "DimensionsMissing({})", mime),
            MultimodalWarning::DimensionsUnparseable { mime } => write!(f, "DimensionsUnparseable({})", mime),
            MultimodalWarning::DimensionsExceeded { width, height, max_width, max_height } => write!(f, "DimensionsExceeded({}x{} > {}x{})", width, height, max_width, max_height),
            MultimodalWarning::DimensionsMismatch { declared, parsed } => write!(f, "DimensionsMismatch(declared=({}x{}), parsed=({}x{}))", declared.0, declared.1, parsed.0, parsed.1),
            MultimodalWarning::AltTextTooLong { len, cap } => write!(f, "AltTextTooLong({} > {})", len, cap),
            MultimodalWarning::DuplicateId { id } => write!(f, "DuplicateId({})", id),
            MultimodalWarning::AttachmentsNotArray => write!(f, "AttachmentsNotArray"),
            MultimodalWarning::ByteLengthMismatch { declared, actual } => write!(f, "ByteLengthMismatch(declared={}, actual={})", declared, actual),
            MultimodalWarning::AttachmentUnparseable { index, error } => write!(f, "AttachmentUnparseable(index={}, error={})", index, error),
        }
    }
}

pub fn is_valid_digest_format(digest: &str) -> bool {
    if let Some(rest) = digest.strip_prefix("sha256:") {
        rest.len() == 64 && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    } else {
        false
    }
}

pub fn compute_sha256_digest(bytes: &[u8]) -> String {
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    format!("sha256:{}", hex::encode(result))
}

pub fn verify_digest(bytes: &[u8], expected: &str) -> bool {
    is_valid_digest_format(expected) && compute_sha256_digest(bytes) == expected
}

pub fn parse_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png" => parse_png_dimensions(bytes),
        "image/gif" => parse_gif_dimensions(bytes),
        "image/jpeg" => parse_jpeg_dimensions(bytes),
        "image/webp" => parse_webp_dimensions(bytes),
        _ => None,
    }
}

fn parse_png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 20 {
        return None;
    }
    const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
    if bytes.get(..8)? != PNG_SIG.as_slice() {
        return None;
    }
    let (w_off, h_off) = if bytes.get(12..16) == Some(b"IHDR") && bytes.len() >= 24 {
        (16, 20)
    } else if bytes.get(8..12) == Some(b"IHDR") && bytes.len() >= 20 {
        (12, 16)
    } else {
        return None;
    };
    let width = u32::from_be_bytes([bytes[w_off], bytes[w_off + 1], bytes[w_off + 2], bytes[w_off + 3]]);
    let height = u32::from_be_bytes([bytes[h_off], bytes[h_off + 1], bytes[h_off + 2], bytes[h_off + 3]]);
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn parse_gif_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 {
        return None;
    }
    let sig = std::str::from_utf8(bytes.get(..6)?).ok()?;
    if sig != "GIF87a" && sig != "GIF89a" {
        return None;
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
    let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn parse_jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if marker == 0xD9 || marker == 0xD8 {
            i += 2;
            continue;
        }
        // Skip standalone markers
        if [0x01, 0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8].contains(&marker) {
            i += 2;
            continue;
        }
        if i + 3 >= bytes.len() {
            return None;
        }
        let len = ((bytes[i + 2] as usize) << 8 | (bytes[i + 3] as usize)) + 2;
        // SOF markers
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            if i + 8 < bytes.len() {
                let height = (bytes[i + 5] as u32) << 8 | (bytes[i + 6] as u32) ;
                let width = (bytes[i + 7] as u32) << 8 | (bytes[i + 8] as u32) ;
                if height > 0 && width > 0 {
                    return Some((width, height));
                }
            }
        }
        i += len.max(2);
    }
    None
}

fn parse_webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 12 {
        return None;
    }
    if bytes.get(..4)? != b"RIFF" {
        return None;
    }
    if bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    // Chunk at offset 12
    if bytes.len() < 20 {
        return None;
    }
    let chunk_fourcc = std::str::from_utf8(bytes.get(12..16)?).ok()?;
    match chunk_fourcc {
        "VP8 " => parse_vp8_lossy(bytes),
        "VP8L" => parse_vp8l_lossless(bytes),
        "VP8X" => parse_vp8x_extended(bytes),
        _ => None,
    }
}

fn parse_vp8_lossy(bytes: &[u8]) -> Option<(u32, u32)> {
    // VP8 chunk: fourcc at 12..16, chunk size at 16..20, payload at 20.
    // Frame tag is the 3 bytes at 20..23, the keyframe sync code
    // 0x9D 0x01 0x2A sits at 23..26, and the 14-bit little-endian width and
    // height follow at 26..30.
    if bytes.get(12..16)? != b"VP8 " {
        return None;
    }
    if bytes.len() < 30 {
        return None;
    }
    if bytes.get(23..26)? != &[0x9D, 0x01, 0x2A] {
        return None;
    }
    let width_raw = u16::from_le_bytes([*bytes.get(26)?, *bytes.get(27)?]) & 0x3FFF;
    let height_raw = u16::from_le_bytes([*bytes.get(28)?, *bytes.get(29)?]) & 0x3FFF;
    let width = width_raw as u32;
    let height = height_raw as u32;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn parse_vp8l_lossless(bytes: &[u8]) -> Option<(u32, u32)> {
    // VP8L chunk: fourcc at 12..16, chunk size at 16..20, payload at 20.
    // Signature byte 0x2F at 20, then the packed bits at 21..25: a 14-bit
    // little-endian `width-1`, then a 14-bit `height-1`.
    if bytes.get(12..16)? != b"VP8L" {
        return None;
    }
    if bytes.len() < 25 {
        return None;
    }
    if *bytes.get(20)? != 0x2F {
        return None;
    }
    let packed = u32::from_le_bytes([
        *bytes.get(21)?,
        *bytes.get(22)?,
        *bytes.get(23)?,
        *bytes.get(24)?,
    ]);
    let width = ((packed & 0x3FFF) + 1) as u32;
    let height = (((packed >> 14) & 0x3FFF) + 1) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn parse_vp8x_extended(bytes: &[u8]) -> Option<(u32, u32)> {
    // VP8X chunk: fourcc at 12..16, chunk size at 16..20, payload at 20.
    // Flags at 20, reserved 21..24, then a 24-bit little-endian `width-1`
    // (24..27) and `height-1` (27..30).
    if bytes.get(12..16)? != b"VP8X" {
        return None;
    }
    if bytes.len() < 30 {
        return None;
    }
    let width_raw = (*bytes.get(24)? as u32) | ((*bytes.get(25)? as u32) << 8) | ((*bytes.get(26)? as u32) << 16);
    let height_raw = (*bytes.get(27)? as u32) | ((*bytes.get(28)? as u32) << 8) | ((*bytes.get(29)? as u32) << 16);
    let width = width_raw + 1;
    let height = height_raw + 1;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

pub fn validate_attachment(att: &Attachment, config: &MultimodalConfig, bytes: Option<&[u8]>) -> Vec<MultimodalWarning> {
    let mut warnings = Vec::new();
    if att.id.trim().is_empty() {
        warnings.push(MultimodalWarning::EmptyId);
    }
    if att.uri.trim().is_empty() {
        warnings.push(MultimodalWarning::EmptyUri);
    }
    if !config.allowed_mimes.contains(&att.mime) {
        warnings.push(MultimodalWarning::UnsupportedMime { mime: att.mime.clone() });
    }
    if att.size_bytes == 0 {
        warnings.push(MultimodalWarning::SizeZero);
    } else if att.size_bytes > config.max_attachment_bytes {
        warnings.push(MultimodalWarning::SizeExceeded { size: att.size_bytes, cap: config.max_attachment_bytes });
    }
    if !is_valid_digest_format(&att.digest) {
        warnings.push(MultimodalWarning::DigestMalformed { digest: att.digest.clone() });
    } else if config.verify_digests {
        if let Some(b) = bytes {
            let actual_digest = compute_sha256_digest(b);
            if actual_digest != att.digest {
                warnings.push(MultimodalWarning::DigestMismatch { expected: att.digest.clone(), actual: actual_digest });
            }
        }
    }
    if let Some(b) = bytes {
        if b.len() != att.size_bytes {
            warnings.push(MultimodalWarning::ByteLengthMismatch { declared: att.size_bytes, actual: b.len() });
        }
    }
    if let Some(ref alt) = att.alt_text {
        let len = alt.chars().count();
        if len > 500 {
            warnings.push(MultimodalWarning::AltTextTooLong { len, cap: 500 });
        }
    }
    if IMAGE_MIMES.contains(&att.mime.as_str()) {
        if let Some(b) = bytes {
            match parse_dimensions(&att.mime, b) {
                Some((w, h)) => {
                    if w > config.max_width || h > config.max_height {
                        warnings.push(MultimodalWarning::DimensionsExceeded { width: w, height: h, max_width: config.max_width, max_height: config.max_height });
                    }
                    if let (Some(dw), Some(dh)) = (att.width, att.height) {
                        if w != dw || h != dh {
                            warnings.push(MultimodalWarning::DimensionsMismatch { declared: (dw, dh), parsed: (w, h) });
                        }
                    }
                }
                None => {
                    warnings.push(MultimodalWarning::DimensionsUnparseable { mime: att.mime.clone() });
                    if config.require_dimensions {
                        warnings.push(MultimodalWarning::DimensionsMissing { mime: att.mime.clone() });
                    }
                }
            }
        } else if config.require_dimensions && (att.width.is_none() || att.height.is_none()) {
            warnings.push(MultimodalWarning::DimensionsMissing { mime: att.mime.clone() });
        }
    }
    warnings
}

pub fn validate_attachments(atts: &[Attachment], config: &MultimodalConfig, bytes_resolver: Option<&dyn Fn(&str) -> Option<Vec<u8>>>) -> Vec<MultimodalWarning> {
    let mut warnings = Vec::new();

    if atts.len() > config.max_attachments {
        warnings.push(MultimodalWarning::CountExceeded {
            count: atts.len(),
            cap: config.max_attachments,
        });
    }

    let mut total = 0usize;
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for att in atts {
        total += att.size_bytes;
        if !ids.insert(att.id.clone()) {
            warnings.push(MultimodalWarning::DuplicateId { id: att.id.clone() });
        }
    }
    if total > config.max_total_bytes {
        warnings.push(MultimodalWarning::TotalSizeExceeded {
            total,
            cap: config.max_total_bytes,
        });
    }

    for att in atts {
        let bytes = bytes_resolver.as_ref().and_then(|r| r(&att.uri));
        let bytes_ref = bytes.as_deref();
        warnings.extend(validate_attachment(att, config, bytes_ref));
    }

    warnings
}

/// Normalize a declared digest to the `sha256:<hex>` form `verify_digest`
/// checks, so metadata objects that carry the bare hex (a common writer)
/// still verify instead of failing on a formatting technicality.
fn normalize_declared_sha256(sha: &str) -> String {
    if sha.starts_with("sha256:") {
        sha.to_string()
    } else if yaml_hex64(sha) {
        format!("sha256:{}", sha.to_ascii_lowercase())
    } else {
        sha.to_string()
    }
}

fn yaml_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b))
}

/// Inline digest verification (§ attachment integrity): a metadata attachment
/// that carries the bytes itself (`inline_bytes` or `content`, base64 or
/// UTF-8) alongside `sha256` has its content verified directly — the
/// strongest check available, since no resolver round-trip is needed.
fn verify_inline_attachments(metadata: &serde_json::Value) -> Vec<MultimodalWarning> {
    let mut warnings = Vec::new();
    let Some(arr) = metadata.get("attachments").and_then(|v| v.as_array()) else {
        return warnings;
    };
    for item in arr {
        let inline = item
            .get("inline_bytes")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("content").and_then(|v| v.as_str()));
        let Some(inline) = inline else { continue };
        let Some(declared) = item.get("sha256").and_then(|v| v.as_str()) else { continue };
        let expected = normalize_declared_sha256(declared);
        if !verify_digest(inline.as_bytes(), &expected) {
            warnings.push(MultimodalWarning::DigestMismatch {
                expected: expected.clone(),
                actual: compute_sha256_digest(inline.as_bytes()),
            });
        }
    }
    warnings
}

pub fn validate_metadata_attachments(metadata: &serde_json::Value, config: &MultimodalConfig, bytes_resolver: Option<&dyn Fn(&str) -> Option<Vec<u8>>>) -> Vec<MultimodalWarning> {
    match metadata.get("attachments") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(_)) => {
            let mut atts = Vec::new();
            let mut warnings = Vec::new();
            if let Some(arr) = metadata.get("attachments").and_then(|v| v.as_array()) {
                for (idx, item) in arr.iter().enumerate() {
                    match serde_json::from_value::<Attachment>(item.clone()) {
                        Ok(att) => atts.push(att),
                        // A declared element that does not parse is a
                        // validation error, never a silent skip: the plan
                        // would otherwise validate fewer attachments than the
                        // record declared.
                        Err(e) => warnings.push(MultimodalWarning::AttachmentUnparseable {
                            index: idx,
                            error: e.to_string(),
                        }),
                    }
                }
            }
            warnings.extend(validate_attachments(&atts, config, bytes_resolver));
            warnings.extend(verify_inline_attachments(metadata));
            warnings
        }
        Some(_) => vec![MultimodalWarning::AttachmentsNotArray],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> MultimodalConfig {
        MultimodalConfig::default()
    }

    #[test]
    fn mime_allowlist() {
        let c = test_config();
        assert!(c.allowed_mimes.contains(&"image/png".to_string()));
        let att = Attachment { id: "a".into(), mime: "image/png".into(), size_bytes: 1, digest: "sha256:".to_string() + &"a".repeat(64), uri: "x".into(), width: None, height: None, alt_text: None };
        assert!(validate_attachment(&att, &c, None).is_empty());
        let bad = Attachment { id: "b".into(), mime: "bad/type".into(), size_bytes: 1, digest: "sha256:".to_string() + &"a".repeat(64), uri: "x".into(), width: None, height: None, alt_text: None };
        let w = validate_attachment(&bad, &c, None);
        assert!(w.iter().any(|x| matches!(x, MultimodalWarning::UnsupportedMime { mime } if mime == "bad/type")));
    }

    #[test]
    fn size_caps() {
        let mut c = test_config();
        c.max_attachment_bytes = 100;
        let att = Attachment { id: "s".into(), mime: "text/plain".into(), size_bytes: 0, digest: "sha256:".to_string() + &"a".repeat(64), uri: "x".into(), width: None, height: None, alt_text: None };
        assert!(validate_attachment(&att, &c, None).iter().any(|x| matches!(x, MultimodalWarning::SizeZero)));
        let big = Attachment { id: "b".into(), mime: "text/plain".into(), size_bytes: 200, digest: "sha256:".to_string() + &"a".repeat(64), uri: "x".into(), width: None, height: None, alt_text: None };
        assert!(validate_attachment(&big, &c, None).iter().any(|x| matches!(x, MultimodalWarning::SizeExceeded { .. })));
    }

    #[test]
    fn digest_verification() {
        let c = test_config();
        let bytes = b"hello";
        let digest = compute_sha256_digest(bytes);
        assert!(verify_digest(bytes, &digest));
        assert!(!verify_digest(bytes, "sha256:bad"));
        assert!(!is_valid_digest_format("bad"));
        let att = Attachment { id: "d".into(), mime: "text/plain".into(), size_bytes: 5, digest: digest.clone(), uri: "x".into(), width: None, height: None, alt_text: None };
        assert!(validate_attachment(&att, &c, Some(bytes)).is_empty());
        let bad_digest = Attachment { id: "dd".into(), mime: "text/plain".into(), size_bytes: 5, digest: "sha256:".to_string() + &"b".repeat(64), uri: "x".into(), width: None, height: None, alt_text: None };
        assert!(validate_attachment(&bad_digest, &c, Some(bytes)).iter().any(|x| matches!(x, MultimodalWarning::DigestMismatch { .. })));
    }

    #[test]
    fn dimension_parsing_png() {
        // Minimal PNG: signature + IHDR chunk
        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&(100u32.to_be_bytes())); // width
        png.extend_from_slice(&(200u32.to_be_bytes())); // height
        png.extend_from_slice(&(8u32.to_be_bytes())); // bit depth
        png.extend_from_slice(&(0u32.to_be_bytes())); // color type
        png.extend_from_slice(&(0u32.to_be_bytes())); // compression
        png.extend_from_slice(&(0u32.to_be_bytes())); // filter
        png.extend_from_slice(&(0u32.to_be_bytes())); // interlace
        // Add CRC (4 bytes dummy)
        png.extend_from_slice(&[0,0,0,0]);
        // IEND chunk
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&(0u32.to_be_bytes()));
        png.extend_from_slice(&[0,0,0,0]);
        let dims = parse_dimensions("image/png", &png);
        assert_eq!(dims, Some((100, 200)));
    }

    /// A truncated JPEG whose slice ends exactly at an SOF0 marker's last
    /// sensor byte (`bytes.len() == i + 8`) must return `None`, never panic
    /// with an index-out-of-bounds on `bytes[i + 8]` (AO-24).
    #[test]
    fn jpeg_truncated_at_sof0_returns_none() {
        let bytes: [u8; 10] = [
            0xFF, 0xD8, // SOI at i=0
            0xFF, 0xC0, // SOF0 marker at i=2
            0x00, 0x12, // segment length
            0x00, 0x00, 0x80, 0x00, // ends exactly at i + 8 == bytes.len()
        ];
        assert_eq!(bytes.len(), 2 + 8);
        assert_eq!(parse_jpeg_dimensions(&bytes), None);
    }

    #[test]
    fn dimension_parsing_gif() {
        let mut gif = Vec::new();
        gif.extend_from_slice(b"GIF89a");
        gif.extend_from_slice(&(640u16.to_le_bytes()));
        gif.extend_from_slice(&(480u16.to_le_bytes()));
        let dims = parse_dimensions("image/gif", &gif);
        assert_eq!(dims, Some((640, 480)));
    }

    #[test]
    fn dimension_parsing_webp_vp8() {
        let mut webp = Vec::new();
        webp.extend_from_slice(b"RIFF");
        webp.extend_from_slice(&(30u32.to_le_bytes())); // file size - 8
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(b"VP8 ");
        webp.extend_from_slice(&(10u32.to_le_bytes())); // chunk size
        // VP8 frame: frame tag (3 bytes) at 20..23, keyframe sync code
        // 0x9D 0x01 0x2A at 23..26, then 14-bit LE width/height at 26..30.
        webp.extend_from_slice(&[0x00, 0x00, 0x00]);
        webp.extend_from_slice(&[0x9D, 0x01, 0x2A, 0x01]);
        webp.resize(30, 0);
        webp[26] = 100;
        webp[27] = 0;
        webp[28] = 200;
        webp[29] = 0;
        let dims = parse_dimensions("image/webp", &webp);
        assert_eq!(dims, Some((100, 200)));
    }

    #[test]
    fn unparseable_attachment_element_is_reported() {
        let c = test_config();
        // The second element lacks required fields (id, mime, size_bytes,
        // digest, uri) and must be surfaced, not silently skipped.
        let meta = serde_json::json!({
            "attachments": [
                {
                    "id": "ok", "mime": "text/plain", "size_bytes": 1,
                    "digest": "sha256:".to_string() + &"a".repeat(64), "uri": "x"
                },
                { "id": "half" }
            ]
        });
        let w = validate_metadata_attachments(&meta, &c, None);
        assert!(w.iter().any(|x| matches!(
            x,
            MultimodalWarning::AttachmentUnparseable { index: 1, .. }
        )));
    }

    #[test]
    fn duplicate_id() {
        let c = test_config();
        let atts = vec![
            Attachment { id: "dup".into(), mime: "text/plain".into(), size_bytes: 1, digest: "sha256:".to_string() + &"a".repeat(64), uri: "x".into(), width: None, height: None, alt_text: None },
            Attachment { id: "dup".into(), mime: "text/plain".into(), size_bytes: 1, digest: "sha256:".to_string() + &"a".repeat(64), uri: "y".into(), width: None, height: None, alt_text: None },
        ];
        let w = validate_attachments(&atts, &c, None);
        assert!(w.iter().any(|x| matches!(x, MultimodalWarning::DuplicateId { id } if id == "dup")));
    }

    #[test]
    fn inline_bytes_digest_verified_and_mismatched() {
        let c = test_config();
        let bytes = b"inline attachment body";
        let good = serde_json::json!({
            "attachments": [{
                "id": "inl", "mime": "text/plain", "size_bytes": bytes.len(),
                "digest": compute_sha256_digest(bytes), "uri": "inline://x",
                "sha256": compute_sha256_digest(bytes),
                "inline_bytes": String::from_utf8(bytes.to_vec()).unwrap(),
            }]
        });
        assert!(validate_metadata_attachments(&good, &c, None).is_empty());

        let bad = serde_json::json!({
            "attachments": [{
                "id": "inl", "mime": "text/plain", "size_bytes": bytes.len(),
                "digest": compute_sha256_digest(b"other"), "uri": "inline://x",
                "sha256": compute_sha256_digest(b"other"),
                "content": String::from_utf8(bytes.to_vec()).unwrap(),
            }]
        });
        let w = validate_metadata_attachments(&bad, &c, None);
        assert!(w.iter().any(|x| matches!(x, MultimodalWarning::DigestMismatch { .. })));
    }
}
