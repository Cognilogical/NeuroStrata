//! Upgrade preflight: a v3 reparse stages a second copy of the store before it
//! publishes, so a low-disk host must fail before the walk, not halfway through.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// Bytes the staging generation can need while the previous generation is
/// still active. One copy is the old generation; the new one is the second.
pub fn projected_reindex_bytes(store_bytes: u64) -> u64 {
    store_bytes.saturating_mul(2)
}

/// Refuse to start a reparse when the filesystem holding the store has less
/// free space than the projected staging copy. The path is the store file; the
/// check uses its parent directory.
pub fn check_free_space(store_path: &Path, projected_bytes: u64) -> Result<()> {
    let dir = store_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let available = fs2::available_space(dir)
        .with_context(|| format!("upgrade preflight: cannot read free space for {}", dir.display()))?;
    if available < projected_bytes {
        bail!(
            "upgrade preflight: {} has {} bytes free but reindexing needs about {} bytes (store {}); free space or move the store before retrying",
            dir.display(),
            available,
            projected_bytes,
            store_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reindex_needs_two_copies() {
        assert_eq!(projected_reindex_bytes(1_000), 2_000);
        assert_eq!(projected_reindex_bytes(u64::MAX), u64::MAX);
    }

    #[test]
    fn a_huge_projection_fails_on_a_real_directory() {
        let dir = std::env::temp_dir();
        let store = dir.join("ns-preflight-test.lbug");
        let err = check_free_space(&store, u64::MAX).expect_err("no filesystem has u64::MAX free");
        let text = format!("{err}");
        assert!(text.contains("upgrade preflight"), "{text}");
        assert!(text.contains("needs about"), "{text}");
    }

    #[test]
    fn a_tiny_projection_passes() {
        let dir = std::env::temp_dir();
        let store = dir.join("ns-preflight-test.lbug");
        check_free_space(&store, 1).expect("one byte is available");
    }

    #[test]
    fn a_relative_store_path_uses_the_current_directory() {
        check_free_space(Path::new("ns-preflight-relative.lbug"), 1)
            .expect("a relative path resolves to the current directory");
    }
}
