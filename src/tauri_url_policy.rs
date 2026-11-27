//! Shared external-target policy for the desktop shell.
//!
//! The GUI crate is a separate crate from the MCP binary, so it includes this
//! file with `#[path]` rather than importing a library target. Behavioral tests
//! compile the same file directly, making validation changes observable even
//! when the GUI crate itself is not built.

use std::path::Path;

/// The reserved, non-branded scheme used for file-backed editor deep links.
pub const NATIVE_EDITOR_SCHEME: &str = "neurostrata-editor://";

const NATIVE_FILE_PREFIX: &str = "neurostrata-editor://file/";
const WEB_SCHEMES: [&str; 2] = ["https://", "http://"];
const PROFILES: [&str; 2] = ["profile=primary", "profile=alternate"];

/// What the shell is allowed to hand to the OS, after validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTarget<'a> {
    Web(&'a str),
    NativeEditor(&'a str),
}

/// Validate a target before any OS handoff. Case is significant: an uppercase
/// scheme is not the scheme the allowlist names, and silently normalizing it
/// would make the check diverge from what different openers accept.
pub fn resolve_external_target(url: &str) -> Result<ExternalTarget<'_>, &'static str> {
    if url.is_empty()
        || url.trim() != url
        || url.chars().any(char::is_control)
        || url.contains('\\')
    {
        return Err("malformed target");
    }

    if url.starts_with(NATIVE_EDITOR_SCHEME) {
        let path = native_editor_path(url)?;
        return Ok(ExternalTarget::NativeEditor(path));
    }

    for scheme in WEB_SCHEMES {
        if let Some(authority_and_rest) = url.strip_prefix(scheme) {
            let authority = authority_and_rest
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("");
            if authority.is_empty() || authority.contains('@') {
                return Err("web target needs an explicit host and no embedded credentials");
            }
            return Ok(ExternalTarget::Web(url));
        }
    }
    Err("only web URLs and native editor links may be opened")
}

/// Cheap predicate for callers that only need the decision.
pub fn is_allowed_external_url(url: &str) -> bool {
    resolve_external_target(url).is_ok()
}

/// Validate a native editor link and return its filesystem path. The path is
/// absolute, has no dot segments (including percent-encoded ones), and carries
/// at most one of the two generic editor profiles declared by the UI.
pub fn native_editor_path(url: &str) -> Result<&str, &'static str> {
    let rest = url
        .strip_prefix(NATIVE_FILE_PREFIX)
        .ok_or("not a native editor file link")?;
    let (path, profile) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    if let Some(profile) = profile {
        if !PROFILES.contains(&profile) {
            return Err("unknown editor profile");
        }
    }
    if path.is_empty()
        || !path.starts_with('/')
        || path.contains('#')
        || path.contains('@')
        || path.contains('\\')
    {
        return Err("editor path must be absolute and unadorned");
    }
    let lowered = path.to_ascii_lowercase();
    for needle in ["%2e", "%2f", "%5c", "%00"] {
        if lowered.contains(needle) {
            return Err("encoded separator or dot segment in editor path");
        }
    }
    if path.split('/').any(|part| part == "." || part == "..") {
        return Err("dot segment in editor path");
    }
    Ok(path)
}

/// Profile carried by an editor link. Call only on a link that has passed
/// `native_editor_path`, which guarantees the query is one of `PROFILES`.
pub fn native_editor_profile(url: &str) -> &'static str {
    match url.split_once('?').map(|(_, query)| query) {
        Some(profile) if profile == PROFILES[1] => "alternate",
        _ => "primary",
    }
}

/// Resolve a target end-to-end: validate it, confine native editor links to the
/// active project, and only then call the OS handoff. Keeping the handoff behind
/// this function makes "validation happened" testable behavior rather than a
/// source-text claim about where a call appears.
pub fn open_external_with<F>(
    url: &str,
    project_root: Option<&str>,
    handoff: F,
) -> Result<(), String>
where
    F: FnOnce(&str) -> Result<(), String>,
{
    if !url.starts_with(NATIVE_EDITOR_SCHEME) {
        if !is_allowed_external_url(url) {
            return Err(format!(
                "Refusing to open '{url}': target failed URL policy"
            ));
        }
        return handoff(url);
    }

    let encoded_path = native_editor_path(url)?;
    let root = project_root.ok_or("no active project: editor targets are confined to a project")?;
    let decoded = percent_decode(encoded_path)?;
    if decoded.contains('\0') || decoded.chars().any(char::is_control) {
        return Err("control character in editor path".to_string());
    }
    if !path_is_within_root(&decoded, root)? {
        return Err(format!(
            "Refusing to open '{decoded}': editor targets must be inside the active project"
        ));
    }
    handoff(&decoded)
}

/// Canonicalize both paths so symlinks and `..` cannot escape the project.
pub fn path_is_within_root(path: impl AsRef<Path>, root: impl AsRef<Path>) -> Result<bool, String> {
    let target = std::fs::canonicalize(path.as_ref())
        .map_err(|e| format!("cannot resolve editor target: {e}"))?;
    let project = std::fs::canonicalize(root.as_ref())
        .map_err(|e| format!("cannot resolve project root: {e}"))?;
    Ok(target.starts_with(project))
}

fn percent_decode(value: &str) -> Result<String, String> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("truncated percent escape in editor path".to_string());
            }
            let Some(hi) = nibble(bytes[i + 1]) else {
                return Err("invalid percent escape in editor path".to_string());
            };
            let Some(lo) = nibble(bytes[i + 2]) else {
                return Err("invalid percent escape in editor path".to_string());
            };
            decoded.push(hi * 16 + lo);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| "editor path is not UTF-8".to_string())
}

/// Webview navigations (including form submissions) are confined to the app's
/// own origin, the loopback daemon, and the dev server in debug builds. This is
/// a second, behavior-level barrier: issue #413's CSP stays byte-exact, with no
/// `form-action` directive added to it.
pub fn is_allowed_navigation(url: &str, dev: bool) -> bool {
    fn on_origin(url: &str, origin: &str) -> bool {
        url.strip_prefix(origin).map_or(false, |rest| {
            rest.is_empty()
                || rest.starts_with('/')
                || rest.starts_with('?')
                || rest.starts_with('#')
        })
    }
    on_origin(url, "tauri://localhost")
        || on_origin(url, "http://tauri.localhost")
        || on_origin(url, "https://tauri.localhost")
        || on_origin(url, "http://127.0.0.1:34343")
        || (dev && on_origin(url, "http://localhost:5173"))
}
