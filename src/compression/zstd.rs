use std::io::Read;

pub const DEFAULT_MESH_MAX_DECOMPRESSED_BYTES: usize = 1024 * 1024;
pub const DEFAULT_SNAPSHOT_MAX_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum CompressionError {
    Io(std::io::Error),
}

impl std::fmt::Display for CompressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionError::Io(e) => write!(f, "compression io error: {}", e),
        }
    }
}

impl std::error::Error for CompressionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CompressionError::Io(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for CompressionError {
    fn from(e: std::io::Error) -> Self {
        CompressionError::Io(e)
    }
}

#[derive(Debug)]
pub enum DecompressionError {
    Io(std::io::Error),
    PayloadTooLarge { max: usize, actual: usize },
}

impl std::fmt::Display for DecompressionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecompressionError::Io(e) => write!(f, "decompression io error: {}", e),
            DecompressionError::PayloadTooLarge { max, actual } => {
                write!(f, "payload too large: max={} actual={}", max, actual)
            }
        }
    }
}

impl std::error::Error for DecompressionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DecompressionError::Io(e) => Some(e),
            DecompressionError::PayloadTooLarge { .. } => None,
        }
    }
}

impl From<std::io::Error> for DecompressionError {
    fn from(e: std::io::Error) -> Self {
        DecompressionError::Io(e)
    }
}

pub fn compress(bytes: &[u8], level: i32) -> Result<Vec<u8>, CompressionError> {
    // Clamp to the same 1..=22 range `compress_mesh_bundle_with_level`
    // accepts, so out-of-range levels never reach the encoder (AO-27).
    let clamped = level.clamp(1, 22);
    let mut encoder = zstd::stream::read::Encoder::new(bytes, clamped)?;
    let mut out = Vec::new();
    encoder.read_to_end(&mut out)?;
    Ok(out)
}

pub fn compress_snapshot(bytes: &[u8]) -> Result<Vec<u8>, CompressionError> {
    compress(bytes, 3)
}

/// Compress a mesh bundle at an explicit zstd level (clamped to 1..=22, the
/// range `zstd::stream::encode_all` accepts).
pub fn compress_mesh_bundle_with_level(
    bytes: &[u8],
    level: i32,
) -> Result<Vec<u8>, CompressionError> {
    let clamped = level.clamp(1, 22);
    zstd::stream::encode_all(bytes, clamped).map_err(CompressionError::Io)
}

pub fn decompress_bounded(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError> {
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    let mut buf = Vec::new();
    decoder.take((max as u64) + 1).read_to_end(&mut buf)?;
    if buf.len() > max {
        return Err(DecompressionError::PayloadTooLarge {
            max,
            actual: buf.len(),
        });
    }
    Ok(buf)
}

pub fn decompress_snapshot(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError> {
    decompress_bounded(bytes, max)
}

pub fn decompress_mesh_bundle(bytes: &[u8], max: usize) -> Result<Vec<u8>, DecompressionError> {
    decompress_bounded(bytes, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_snapshot() {
        let data = b"hello world this is a snapshot";
        let compressed = compress_snapshot(data).unwrap();
        assert!(!compressed.is_empty());
        let decompressed = decompress_snapshot(&compressed, 1024).unwrap();
        assert_eq!(decompressed.as_slice(), data.as_slice());
    }

    #[test]
    fn roundtrip_mesh_bundle() {
        let data = b"mesh bundle payload with some content";
        let compressed = compress_mesh_bundle_with_level(data, 3).unwrap();
        let decompressed = decompress_mesh_bundle(&compressed, 1024).unwrap();
        assert_eq!(decompressed.as_slice(), data.as_slice());
    }

    #[test]
    fn payload_too_large_rejected() {
        // Zip bomb: a 1-byte compressed payload that expands to a huge buffer.
        // Instead, craft a valid compressed payload and set max too small.
        let data = b"small but valid payload for zstd compression roundtrip check";
        let compressed = compress_snapshot(data).unwrap();
        assert!(decompress_snapshot(&compressed, 1).is_err());
    }
}
