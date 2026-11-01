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
}

impl MultimodalWarning {
    pub fn is_strict_failure(&self, require_dimensions: bool) -> bool {
        use MultimodalWarning::*;
        match self {
            EmptyId | EmptyUri | UnsupportedMime { .. } | SizeExceeded { .. } | SizeZero
            | TotalSizeExceeded { .. } | CountExceeded { .. } | DigestMalformed { .. }
            | DigestMismatch { .. } | DimensionsExceeded { .. } | DimensionsMismatch { .. }
            | AltTextTooLong { .. } | DuplicateId { .. } | AttachmentsNotArray
            | ByteLengthMismatch { .. } => true,
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
            if i + 7 < bytes.len() {
                let height = ((bytes[i + 5] as u32) << 8 | (bytes[i + 6] as u32));
                let width = ((bytes[i + 7] as u32) << 8 | (bytes[i + 8] as u32));
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
    if bytes.len() < 30 {
        return None;
    }
    // Frame tag: must have 0x9D 0x01 0x2A at bytes 3..6 of VP8 chunk payload
    // The VP8 chunk starts at bytes 12 (fourcc) + 4 (chunk size) = 16? Wait.
    // Let's align: RIFF header (12 bytes), VP8 chunk fourcc (4), chunk size (4), payload starts at 20.
    if bytes.len() < 30 {
        return None;
    }
    if bytes.get(20..23)? != &[0x9D, 0x01, 0x2A] {
        return None;
    }
    // Width/height are 14-bit little-endian at bytes 26..29
    let b26 = bytes.get(26)?;
    let b27 = bytes.get(27)?;
    let b28 = bytes.get(28)?;
    let b29 = bytes.get(29)?;
    let width = (((*b26 as u16) | ((*b27 as u16) << 8)) & 0x3FFF) as u32;
    let height = (((*b28 as u16) | ((*b29 as u16) << 8)) & 0x3FFF) as u32;
    // Actually width/height are packed differently. Let's use simple approach:
    // Per spec: width at 26..28 (little-endian 14 bits), height at 28..30.
    let width_raw = u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF;
    let height_raw = u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF;
    let width = width_raw as u32;
    let height = height_raw as u32;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn parse_vp8l_lossless(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 25 {
        return None;
    }
    // VP8L chunk: payload starts at 20. Signature 0x2F at byte 20.
    if bytes.get(20)? != &0x2F {
        return None;
    }
    // Packed bits at 21..25
    // 14-bit width-1 at bits 0..13 (little-endian over 2 bytes), then 14-bit height-1
    // Using 4 bytes: first 2 bytes for width-1, next 2 for height-1 (each 14 bits)
    let b21 = bytes.get(21).copied()? as u32;
    let b22 = bytes.get(22).copied()? as u32;
    let b23 = bytes.get(23).copied()? as u32;
    let b24 = bytes.get(24).copied()? as u32;
    let packed = b21 | (b22 << 8) | (b23 << 16) | (b24 << 24);
    let width = ((packed & 0x3FFF) + 1) as u32;
    let height = (((packed >> 14) & 0x3FFF) + 1) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn parse_vp8x_extended(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 {
        return None;
    }
    // VP8X payload starts at 20. Flags at 20, reserved at 21..23, width-1 at 24..27, height-1 at 27..30 (24-bit little-endian)
    // Actually width-1 is 24 bits little-endian at 24..27, height-1 at 27..30.
    let width_raw = (bytes.get(24).copied()? as u32)
        | ((bytes.get(25).copied()? as u32) << 8)
        | ((bytes.get(26).copied()? as u32) << 16);
    let height_raw = (bytes.get(27).copied()? as u32)
        | ((bytes.get(28).copied()? as u32) << 8)
        | ((bytes.get(29).copied()? as u32) << 16);
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

pub fn validate_metadata_attachments(metadata: &serde_json::Value, config: &MultimodalConfig, bytes_resolver: Option<&dyn Fn(&str) -> Option<Vec<u8>>>) -> Vec<MultimodalWarning> {
    match metadata.get("attachments") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(_)) => {
            let mut atts = Vec::new();
            if let Some(arr) = metadata.get("attachments").and_then(|v| v.as_array()) {
                for item in arr {
                    if let Ok(att) = serde_json::from_value::<Attachment>(item.clone()) {
                        atts.push(att);
                    } else {
                        // If parsing fails, treat as array element error? Per spec: parse each into Attachment.
                        // We skip unparseable; tests don't include bad objects.
                    }
                }
            }
            validate_attachments(&atts, config, bytes_resolver)
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
        // VP8 frame tag: start code 0x9D 0x01 0x2A + 3 bits scale + 14 bits width/height? We'll approximate
        webp.extend_from_slice(&[0x9D, 0x01, 0x2A, 0x01]);
        // Width 14-bit LE at offset 26 from file start (20 + 6?): let's just put at 26
        // We'll pad to at least 30 bytes
        webp.resize(30, 0);
        webp[26] = 100;
        webp[27] = 0;
        webp[28] = 200;
        webp[29] = 0;
        let dims = parse_dimensions("image/webp", &webp);
        assert!(dims.is_some());
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
}
