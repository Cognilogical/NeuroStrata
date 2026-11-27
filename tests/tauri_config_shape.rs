//! Tauri configuration-shape tests (Issue #413).
//!
//! The GUI crate (`src-tauri`) is not part of the root crate's build, so the
//! shipped Tauri configuration is pinned here at config level. The webview must
//! run under a strict, closed CSP and the `shell:allow-open` capability must
//! carry only web links and the reserved native editor scheme. Behavioral URL
//! checks live in `tests/tauri_url_policy.rs`.

use serde_json::Value;
use std::path::PathBuf;

/// The exact five-directive CSP required by issue #413. `connect-src` allows
/// only same-origin and the loopback daemon: no extra directive or token.
const EXPECTED_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' http://127.0.0.1:34343";

/// URL patterns the scoped `shell:allow-open` capability may allow: web links
/// plus the reserved NeuroStrata-native editor deep-link scheme. No branded
/// scheme is ever pinned here.
const EXPECTED_OPEN_PATTERNS: [&str; 2] = ["^neurostrata-editor://.*", "^https?://.*"];

/// Patterns that must never reappear: they turn the opener into an arbitrary
/// local-file/executable launcher. Branded editor schemes are equally excluded;
/// file-backed nodes use the reserved native editor scheme instead.
const FORBIDDEN_OPEN_PATTERNS: [&str; 2] = ["^file://.*", "^/.*"];

fn config_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read_text(rel: &str) -> String {
    let path = config_path(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_json(rel: &str) -> Value {
    serde_json::from_str(&read_text(rel)).unwrap_or_else(|e| panic!("parse {rel}: {e}"))
}

fn csp() -> String {
    let conf = read_json("src-tauri/tauri.conf.json");
    match conf.pointer("/app/security/csp").and_then(Value::as_str) {
        Some(policy) => policy.to_string(),
        None => panic!(
            "app.security.csp must be a policy string, got: {}",
            conf.pointer("/app/security/csp")
                .map(Value::to_string)
                .unwrap_or_else(|| "missing".to_string())
        ),
    }
}

fn dev_csp() -> String {
    let conf = read_json("src-tauri/tauri.conf.json");
    match conf.pointer("/app/security/devCsp").and_then(Value::as_str) {
        Some(policy) => policy.to_string(),
        None => panic!(
            "app.security.devCsp must be a policy string, got: {}",
            conf.pointer("/app/security/devCsp")
                .map(Value::to_string)
                .unwrap_or_else(|| "missing".to_string())
        ),
    }
}

/// Tokens of a single CSP directive, in source order.
fn directive_tokens(policy: &str, name: &str) -> Vec<String> {
    policy
        .split(';')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .find_map(|directive| {
            let mut parts = directive.split_whitespace();
            if parts.next() == Some(name) {
                Some(parts.map(str::to_string).collect())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("CSP has no {name} directive: {policy}"))
}

/// Names of every directive present in a policy, in source order.
fn directive_names(policy: &str) -> Vec<String> {
    policy
        .split(';')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .map(|directive| {
            directive
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// Every `url` pattern across scoped `shell:allow-open` entries.
fn scoped_open_patterns(permissions: &[Value]) -> Vec<String> {
    let mut patterns = Vec::new();
    for permission in permissions {
        let Some(entry) = permission.as_object() else {
            continue;
        };
        if entry.get("identifier").and_then(Value::as_str) != Some("shell:allow-open") {
            continue;
        }
        let allow = entry
            .get("allow")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("scoped shell:allow-open must carry an allow list"));
        for allowed in allow {
            let url = allowed
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("shell:allow-open allow entry without a url: {allowed}"));
            patterns.push(url.to_string());
        }
    }
    patterns
}

#[test]
fn csp_is_set_and_strict() {
    let actual = csp();
    assert_eq!(
        actual, EXPECTED_CSP,
        "app.security.csp must be the exact strict policy"
    );

    let directives: Vec<&str> = actual
        .split(';')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .collect();
    assert_eq!(
        directives.len(),
        5,
        "the CSP must contain exactly issue #413's five directives: {actual}"
    );
}

#[test]
fn capability_has_no_file_or_root_open_patterns() {
    let rel = "src-tauri/capabilities/default.json";
    let capability = read_json(rel);
    let permissions = capability["permissions"]
        .as_array()
        .expect("capabilities/default.json permissions must be an array");

    for permission in permissions {
        if let Some(name) = permission.as_str() {
            assert_ne!(
                name, "shell:allow-open",
                "a bare shell:allow-open permission reopens every URL/file target; keep only the scoped entry"
            );
        }
    }

    let raw = std::fs::read_to_string(config_path(rel)).expect("read capabilities/default.json");
    for forbidden in FORBIDDEN_OPEN_PATTERNS {
        assert!(
            !raw.contains(forbidden),
            "{rel} must not allow open pattern {forbidden}"
        );
    }

    let scoped_entries = permissions
        .iter()
        .filter(|permission| {
            permission
                .get("identifier")
                .and_then(Value::as_str)
                .map(|id| id == "shell:allow-open")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        scoped_entries, 1,
        "exactly one scoped shell:allow-open entry is allowed"
    );

    let mut patterns = scoped_open_patterns(permissions);
    patterns.sort();
    let mut expected: Vec<String> = EXPECTED_OPEN_PATTERNS
        .iter()
        .map(|pattern| pattern.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        patterns, expected,
        "scoped shell:allow-open patterns must be exactly web links and the native editor scheme"
    );
    for pattern in &patterns {
        assert!(
            pattern.starts_with("^https?://") || pattern.starts_with("^neurostrata-editor://"),
            "unexpected scoped open pattern {pattern}: only web links and the native editor scheme are allowed"
        );
    }

    // The capability above is the ACL; `plugins.shell.open` is the separate
    // plugin switch that decides whether plugin-shell's own `open` command
    // exists at all. `open_external` (src-tauri/src/main.rs) is the enforced
    // allowlist the web-ui actually calls, so leaving the plugin switch on only
    // widens the surface. `false` must be explicit: an absent key deserializes
    // to `ShellAllowlistOpen::Unset`, which tauri-plugin-shell treats like
    // `true` (it installs a default URL regex), so removing the key would
    // silently re-enable the opener.
    let tauri_conf = read_json("src-tauri/tauri.conf.json");
    match tauri_conf.pointer("/plugins/shell/open") {
        Some(Value::Bool(false)) => {}
        other => panic!(
            "src-tauri/tauri.conf.json must set plugins.shell.open to false \
             (external URLs go through open_external; absence re-enables the opener), got: {}",
            other
                .map(Value::to_string)
                .unwrap_or_else(|| "missing".to_string())
        ),
    }
}

#[test]
fn dev_csp_permits_vite_hmr_without_loosening_production_csp() {
    // Without `devCsp`, Tauri injects the production `csp` in dev too
    // (tauri-utils SecurityConfig: "If dev_csp is not specified, this
    // value is also injected on dev"), which blocks Vite's HMR websocket
    // to the configured `devUrl` origin and kills hot-reload.
    let prod = csp();
    assert!(
        !prod.contains("localhost:5173"),
        "production CSP must not carry the dev-server origin: {prod}"
    );
    let dev = dev_csp();
    // Compare the whole directive set, not just values: an extra directive
    // such as `worker-src https:` must fail the pin instead of going unnoticed.
    let mut dev_names = directive_names(&dev);
    dev_names.sort();
    let mut prod_names = directive_names(&prod);
    prod_names.sort();
    assert_eq!(
        dev_names, prod_names,
        "devCsp must contain exactly the production directives, with no additions"
    );
    let mut tokens = directive_tokens(&dev, "connect-src");
    tokens.sort();
    let mut expected = directive_tokens(&prod, "connect-src");
    expected.push("http://localhost:5173".to_string());
    expected.push("ws://localhost:5173".to_string());
    expected.sort();
    assert_eq!(
        tokens, expected,
        "devCsp connect-src must be the production set plus only the Vite HMR origins"
    );
    // Vite's React plugin injects an inline Refresh preamble in dev only.
    let mut dev_script = directive_tokens(&dev, "script-src");
    dev_script.sort();
    let mut expected_script = vec!["'self'".to_string(), "'unsafe-inline'".to_string()];
    expected_script.sort();
    assert_eq!(
        dev_script, expected_script,
        "devCsp script-src must permit the inline React Refresh preamble"
    );
    assert_eq!(
        directive_tokens(&prod, "script-src"),
        vec!["'self'".to_string()],
        "production script-src must stay same-origin-only"
    );
    // Other non-connect directives stay identical to production.
    for name in ["default-src", "style-src", "img-src"] {
        assert_eq!(
            directive_tokens(&dev, name),
            directive_tokens(&prod, name),
            "devCsp {name} must match the production policy"
        );
    }
}

#[test]
fn vite_enables_react_refresh() {
    let vite = read_text("web-ui/vite.config.ts");
    assert!(
        vite.contains("@vitejs/plugin-react") && vite.contains("react()"),
        "the dev server must enable the React plugin that injects the Refresh preamble"
    );
}

#[test]
fn daemon_connect_src_is_loopback_only() {
    let policy = csp();
    let mut tokens = directive_tokens(&policy, "connect-src");
    tokens.sort();
    // Issue #413 requires exactly same-origin and the loopback daemon.
    let mut expected = vec!["'self'".to_string(), "http://127.0.0.1:34343".to_string()];
    expected.sort();
    assert_eq!(
        tokens, expected,
        "connect-src must allow only same-origin and the loopback daemon"
    );
}

#[test]
fn open_external_delegates_to_shared_policy() {
    let src = read_text("src-tauri/src/main.rs");
    let validation = src
        .find("url_policy::is_allowed_external_url")
        .expect("open_external must call the shared URL validator");
    let opener = src
        .find("open::that(")
        .expect("open_external must still hand allowed targets to the OS");
    assert!(
        validation < opener,
        "URL validation must happen before any target is opened"
    );
    assert!(
        src.contains("url_policy::native_editor_path"),
        "native editor links must be resolved through the validated path helper"
    );
}

#[test]
fn ui_emits_native_editor_links() {
    let ui = read_text("web-ui/src/components/UIPanel.tsx");
    assert!(
        ui.contains("neurostrata-editor://file/"),
        "the editor action must emit the native, non-branded editor deep link"
    );
}

#[test]
fn port_override_doc_distinguishes_backend_requests() {
    let doc = read_text("docs/features-api/ops/security.md");
    assert!(
        doc.contains("NEUROSTRATA_DAEMON_PORT"),
        "docs must describe the supported daemon port override"
    );
    assert!(
        doc.contains("reqwest"),
        "docs must state that Rust backend daemon calls bypass the webview CSP"
    );
    assert!(
        !doc.contains("will block the overridden origin"),
        "the false claim that a non-default port is CSP-blocked must be removed"
    );
}
