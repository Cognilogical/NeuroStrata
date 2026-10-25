//! Tauri configuration-shape tests (Program Task 11, J8).
//!
//! The GUI crate (`src-tauri`) is not compiled by this crate's CI — its
//! first compile check arrives with Task 15a's GUI job — so until then the
//! shipped Tauri configuration is pinned here, at config level: the CSP
//! must be the strict policy below, and the shell open capability must
//! carry only the URL patterns a named web-ui feature emits.

use serde_json::Value;
use std::path::PathBuf;

/// The exact CSP the visualizer ships (Program Task 11): the webview's
/// only network need is the local daemon on loopback.
const EXPECTED_CSP: &str = "default-src 'self'; script-src 'self'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
connect-src 'self' http://127.0.0.1:34343";

fn read_json(rel: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn csp() -> String {
    let conf = read_json("src-tauri/tauri.conf.json");
    conf.pointer("/app/security/csp")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "app.security.csp must be a policy string, got: {}",
                conf.pointer("/app/security/csp")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "missing".to_string())
            )
        })
        .to_string()
}

/// Every `url` pattern in the capability's scoped `shell:allow-open` entry.
fn open_url_patterns() -> Vec<String> {
    let cap = read_json("src-tauri/capabilities/default.json");
    let perms = cap["permissions"].as_array().expect("permissions array");
    let mut urls = Vec::new();
    for perm in perms {
        let Some(obj) = perm.as_object() else { continue };
        if obj.get("identifier").and_then(Value::as_str) != Some("shell:allow-open") {
            continue;
        }
        if let Some(allow) = obj.get("allow").and_then(Value::as_array) {
            for entry in allow {
                if let Some(u) = entry.get("url").and_then(Value::as_str) {
                    urls.push(u.to_string());
                }
            }
        }
    }
    urls
}

#[test]
fn csp_is_set_and_strict() {
    let conf = read_json("src-tauri/tauri.conf.json");
    let raw = conf
        .pointer("/app/security/csp")
        .expect("app.security.csp exists");
    assert!(!raw.is_null(), "csp must not be null");
    let policy = csp();
    assert_eq!(policy, EXPECTED_CSP, "the CSP is the exact strict policy");
    // Directive-level assertions so a failure names the directive that moved.
    let directives: Vec<&str> = policy.split(';').map(|d| d.trim()).collect();
    for expected in [
        "default-src 'self'",
        "script-src 'self'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self' data:",
        "connect-src 'self' http://127.0.0.1:34343",
    ] {
        assert!(
            directives.contains(&expected),
            "missing directive: {expected}"
        );
    }
    assert_eq!(directives.len(), 5, "no directives beyond the named five");
}

#[test]
fn capability_has_no_file_or_root_open_patterns() {
    let urls = open_url_patterns();
    assert!(
        !urls.iter().any(|u| u == "^file://.*"),
        "file:// open pattern must be gone: {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u == "^/.*"),
        "bare-path open pattern must be gone: {urls:?}"
    );
    // The retained set is exactly the patterns a named feature emits —
    // the editor hand-off (vscode/cursor) and web links (https?) — see
    // the Task 11 PR description's retained-patterns table.
    let mut sorted = urls.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec!["^cursor://.*", "^https?://.*", "^vscode://.*"],
        "retained open patterns are exactly the named three"
    );
}

#[test]
fn daemon_connect_src_is_loopback_only() {
    let policy = csp();
    let connect = policy
        .split(';')
        .map(|d| d.trim())
        .find(|d| d.starts_with("connect-src"))
        .expect("connect-src directive present");
    let mut tokens: Vec<&str> = connect.split_whitespace().skip(1).collect();
    tokens.sort_unstable();
    assert_eq!(
        tokens,
        vec!["'self'", "http://127.0.0.1:34343"],
        "connect-src is 'self' + the loopback daemon origin only"
    );
}
