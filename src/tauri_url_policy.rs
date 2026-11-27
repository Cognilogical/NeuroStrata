//! Shared external-target policy for the desktop shell.
//!
//! The GUI crate is a separate crate from the MCP binary, so it includes this
//! file with `#[path]` rather than importing a library target. Behavioral tests
//! compile the same file directly, making validation changes observable even
//! when the GUI crate itself is not built.

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
