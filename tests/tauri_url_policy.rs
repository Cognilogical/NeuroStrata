//! Behavioral regressions for the shared desktop external-target policy.
//!
//! The GUI crate cannot be linked from the root crate, so it compiles the very
//! same source file (`src/tauri_url_policy.rs`) through `#[path]`. These tests
//! therefore exercise the code `open_external` actually consults, rather than
//! parsing the constant it happens to declare.

#[path = "../src/tauri_url_policy.rs"]
mod url_policy;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use url_policy::{
    is_allowed_external_url, is_allowed_navigation, native_editor_path, open_external_with,
    path_is_within_root, resolve_external_target, ExternalTarget,
};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let n = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("ns-url-policy-{}-{n}", std::process::id()));
        let root = base.join("project");
        let outside = base.join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        Self { root, outside }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(&self.outside);
    }
}

fn encode_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn editor_url(path: &str) -> String {
    let encoded = path
        .split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("neurostrata-editor://file/{encoded}?profile=primary")
}

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

#[test]
fn opener_hands_allowed_web_urls_to_the_os() {
    let handed = std::cell::RefCell::new(Vec::new());
    open_external_with("https://example.com/docs", None, |target| {
        handed.borrow_mut().push(target.to_string());
        Ok(())
    })
    .unwrap();
    assert_eq!(handed.into_inner(), vec!["https://example.com/docs"]);
}

#[test]
fn opener_never_hands_rejected_urls_to_the_os() {
    let fixture = Fixture::new();
    let root = fixture.root.to_str().unwrap();
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https:///evil.example/x",
        "HTTPS://example.com",
    ] {
        let handed = std::cell::Cell::new(false);
        let result = open_external_with(url, Some(root), |_| {
            handed.set(true);
            Ok(())
        });
        assert!(result.is_err(), "must reject {url}");
        assert!(!handed.get(), "must not open {url}");
    }
}

#[test]
fn native_editor_paths_are_confined_to_the_active_project() {
    let fixture = Fixture::new();
    let inside = fixture.root.join("main.rs");
    fs::write(&inside, "fn main() {}").unwrap();
    let outside = fixture.outside.join("secret.rs");
    fs::write(&outside, "secret").unwrap();
    let root = fixture.root.to_str().unwrap();

    let handed = std::cell::RefCell::new(Vec::new());
    open_external_with(
        &editor_url(inside.to_str().unwrap()),
        Some(root),
        |target| {
            handed.borrow_mut().push(target.to_string());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(handed.into_inner(), vec![inside.to_str().unwrap()]);

    let handed = std::cell::Cell::new(false);
    let result = open_external_with(&editor_url(outside.to_str().unwrap()), Some(root), |_| {
        handed.set(true);
        Ok(())
    });
    assert!(result.is_err(), "path outside the project must be refused");
    assert!(!handed.get(), "outside path must never reach the OS");

    assert!(path_is_within_root(&inside, &fixture.root).unwrap());
    assert!(!path_is_within_root(&outside, &fixture.root).unwrap());
    assert!(!path_is_within_root(&fixture.root, &fixture.outside).unwrap());
}

#[test]
fn reserved_characters_in_file_paths_survive_url_encoding() {
    let fixture = Fixture::new();
    let file = fixture.root.join("a#b?c@d.rs");
    fs::write(&file, "content").unwrap();
    let handed = std::cell::RefCell::new(Vec::new());
    open_external_with(
        &editor_url(file.to_str().unwrap()),
        Some(fixture.root.to_str().unwrap()),
        |target| {
            handed.borrow_mut().push(target.to_string());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(handed.into_inner(), vec![file.to_str().unwrap()]);
}

#[test]
fn navigation_policy_blocks_offsite_form_targets() {
    for url in [
        "tauri://localhost/index.html",
        "http://tauri.localhost/index.html",
        "http://127.0.0.1:34343/graph",
        "http://127.0.0.1:34343/graph?ns=x",
    ] {
        assert!(is_allowed_navigation(url, false), "must allow {url}");
    }
    assert!(is_allowed_navigation("http://localhost:5173/", true));
    assert!(!is_allowed_navigation("http://localhost:5173/", false));
    for url in [
        "https://offsite.example/collect",
        "http://127.0.0.1:34343.offsite.example",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html,<form action='https://offsite.example'>",
    ] {
        assert!(
            !is_allowed_navigation(url, false),
            "must block offsite navigation {url:?}"
        );
    }
}
