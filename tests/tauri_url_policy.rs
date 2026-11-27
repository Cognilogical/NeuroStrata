//! Behavioral regressions for the shared desktop external-target policy.
//!
//! The GUI crate cannot be linked from the root crate, so it compiles the very
//! same source file (`src/tauri_url_policy.rs`) through `#[path]`. These tests
//! therefore exercise the code `open_external` actually consults, rather than
//! parsing the constant it happens to declare.

#[path = "../src/tauri_url_policy.rs"]
mod url_policy;

use url_policy::{
    is_allowed_external_url, native_editor_path, resolve_external_target, ExternalTarget,
};

#[test]
fn allows_web_and_native_editor_targets() {
    for url in [
        "https://example.com/docs",
        "http://127.0.0.1:34343/health",
        "neurostrata-editor://file//home/kenton/project/src/main.rs",
        "neurostrata-editor://file//C:/project/src/main.rs?profile=alternate",
    ] {
        assert!(is_allowed_external_url(url), "must allow {url}");
    }
    assert_eq!(
        resolve_external_target("https://example.com/docs"),
        Ok(ExternalTarget::Web("https://example.com/docs"))
    );
    assert_eq!(
        resolve_external_target("neurostrata-editor://file//home/x.rs"),
        Ok(ExternalTarget::NativeEditor("/home/x.rs"))
    );
}

#[test]
fn rejects_non_web_and_non_native_schemes() {
    for url in [
        "",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "other-editor://file//home/x.rs",
        "ftp://example.com/x",
        "HTTPS://example.com",
        "HtTp://example.com",
        "NEUROSTRATA-EDITOR://file//home/x.rs",
    ] {
        assert!(!is_allowed_external_url(url), "must reject {url}");
    }
}

#[test]
fn rejects_web_url_evasions() {
    for url in [
        "https://",
        "https:///evil.example/x",
        "https:example.com",
        "https:/\\evil.example",
        "https://user@evil.example",
        "https://example.com\\@evil.example",
        " https://example.com",
        "https://example.com\nX-Evil: header",
        "https://example.com\t",
        "https://example.com\r",
    ] {
        assert!(!is_allowed_external_url(url), "must reject evasion {url:?}");
    }
}

#[test]
fn rejects_native_editor_path_evasions() {
    for url in [
        "neurostrata-editor://file/",
        "neurostrata-editor://file/relative.rs",
        "neurostrata-editor://file//home/../etc/passwd",
        "neurostrata-editor://file//home/%2e%2e/etc/passwd",
        "neurostrata-editor://file//home/%2Fetc/passwd",
        "neurostrata-editor://file//home/x.rs?profile=other",
        "neurostrata-editor://file//home/x.rs#fragment",
        "neurostrata-editor://file//home/x.rs?profile=primary&x=1",
        "neurostrata-editor://file//home\\x.rs",
        "neurostrata-editor://file//home/x.rs?profile=PRIMARY",
    ] {
        assert!(
            !is_allowed_external_url(url),
            "must reject native evasion {url:?}"
        );
    }
}

#[test]
fn native_editor_path_is_validated_before_use() {
    assert_eq!(
        native_editor_path("neurostrata-editor://file//home/x.rs?profile=primary"),
        Ok("/home/x.rs")
    );
    assert!(native_editor_path("https://example.com").is_err());
    assert!(native_editor_path("neurostrata-editor://file//home/../../etc/passwd").is_err());
}
