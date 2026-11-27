// Legacy ingest module: these clippy categories predate the watcher work and
// are out of scope for it. Suppress them here so the staged-quality gate stays
// scoped to the code this branch actually changes.
#![allow(
    clippy::comparison_to_empty,
    clippy::drop_non_drop,
    clippy::for_kv_map,
    clippy::items_after_test_module,
    clippy::option_map_unit_fn,
    clippy::question_mark,
    clippy::redundant_pattern_matching,
    clippy::regex_creation_in_loops,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_map_or,
    clippy::useless_vec
)]

use crate::parser::get_language;
use crate::parser::schema::ParserSchema;
use crate::traits::{Embedder, MemoryPayload, VectorStore};
use ignore::WalkBuilder;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tree_sitter::{Parser, Query, StreamingIterator};

// The id/hash scheme lives in `generation.rs`, a module deliberately free of
// `crate::` dependencies so an integration test can pin it without a library
// target. These re-exports keep `ingest.rs` and the store naming the same
// functions through the path they already use.
pub use crate::parser::generation::{
    exact_target_match, exact_target_match_fallible, exact_target_spellings, generation_key,
    generation_scoped_id, ingest_version, logical_path_of, sha256_hex, split_scoped_id, symbol_id,
    NAMESPACE_SEPARATOR,
};

/// Directory names the ingest walk never descends into. Shared with the
/// freshness watcher's event filter so the walker and the watcher agree on
/// what is eligible before anything becomes a trigger.
pub const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    ".venv",
    "venv",
    "env",
    ".env",
    "dist",
    "build",
    "out",
    ".dolt",
    ".git",
    ".next",
    ".nuxt",
    "__pycache__",
    ".fastembed_cache",
    ".idea",
    ".vscode",
    "coverage",
    // Daemon-owned session/plugin state (session_queries.json,
    // active_session.json, harness plugins). The daemon writes these after
    // every MCP call; ingesting them makes the freshness watcher mark its own
    // root dirty and re-ingest on each debounce (issue #355).
    ".NeuroStrata",
];

/// Shared eligibility predicate: would a normal ingest walk consider this
/// path eligible? Skipped directories and database/transient store artifacts
/// are rejected; only paths inside the registered root survive.
pub fn is_ingest_eligible_path(path: &Path, root: &Path) -> bool {
    if !is_within(path, root) {
        return false;
    }
    if path_is_in_skipped_dir(path) {
        return false;
    }
    !is_store_artifact(path)
}

/// Lexical root containment that copes with relative roots (CLI-style) as well
/// as absolute ones, without touching the filesystem. Component-aware: interior
/// `.`/`..` are resolved, and a path whose `..` escapes its root is rejected.
fn is_within(path: &Path, root: &Path) -> bool {
    let Some(path_norm) = normalize_lexical(path) else {
        return false;
    };
    let Some(root_norm) = normalize_lexical(root) else {
        return false;
    };

    if root_norm.is_empty() || root_norm == "." {
        // A relative, empty, or "." root never authorizes absolute paths.
        // `has_root`, not `is_absolute`: on Windows `\tmp\x` has no drive, so
        // it is not absolute, but it is still not relative to the root.
        return !path.has_root() && !path_norm.is_empty();
    }
    if path.is_absolute() != root.is_absolute() {
        // A relative root does not authorize absolute paths.
        return false;
    }
    path_norm == root_norm || path_norm.starts_with(&format!("{}/", root_norm))
}

/// Component-aware lexical normalization: resolves interior `.` and `..`,
/// returns `None` when a `..` escapes the start of the path.
fn normalize_lexical(path: &Path) -> Option<String> {
    let s = path.to_string_lossy().replace('\\', "/");
    let absolute = s.starts_with('/');
    let mut parts: Vec<String> = Vec::new();
    for comp in s.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            if absolute {
                if parts.pop().is_none() {
                    // `..` past the root is an escape.
                    return None;
                }
            } else {
                match parts.last() {
                    Some(top) if top != ".." => {
                        parts.pop();
                    }
                    _ => parts.push("..".to_string()),
                }
            }
        } else {
            parts.push(comp.to_string());
        }
    }
    let joined = if absolute {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    };
    Some(joined)
}

fn path_is_in_skipped_dir(path: &Path) -> bool {
    // Compare whole components with the host's own separators. A Unix filename
    // may legitimately contain backslashes (`src\vendor\module.rs` is one file,
    // not a path inside `vendor`), while `Path` on Windows already exposes both
    // `\` and `/` as components. Components also cover the case where the path
    // IS the skipped directory (a watched/ingested `.../target` node), which a
    // `/name/` substring test misses. Windows filesystems are case-insensitive.
    path.components().any(|component| {
        let std::path::Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy();
        SKIPPED_DIRS.iter().any(|skip_dir| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(skip_dir)
            } else {
                &*name == *skip_dir
            }
        })
    })
}

fn is_store_artifact(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".db")
        || lower.ends_with(".wal")
        || lower.ends_with(".lock")
        || lower.ends_with(".checkpoint")
        || lower.ends_with(".shm")
}

/// Schemas declare extensions bare ("rs"), but one passed via --schema-path may
/// use the dotted form. Both sides go through here so either convention matches.
fn normalize_ext(ext: &str) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

fn build_ext_map(schema: &ParserSchema) -> HashMap<String, String> {
    let mut ext_to_lang = HashMap::new();
    for (lang_name, lang_schema) in &schema.languages {
        for ext in &lang_schema.extensions {
            ext_to_lang.insert(normalize_ext(ext), lang_name.clone());
        }
    }
    ext_to_lang
}

/// Upper bound on the source text stored and embedded for one symbol. The
/// embedder truncates its input anyway (see MAX_EMBED_TOKENS in src/embed.rs);
/// cutting here keeps the stored content and the vector describing the same
/// text instead of letting them drift apart on large definitions.
const MAX_SYMBOL_CHARS: usize = 4000;

fn truncate_for_embedding(body: &str) -> String {
    if body.chars().count() <= MAX_SYMBOL_CHARS {
        return body.to_string();
    }
    let cut: String = body.chars().take(MAX_SYMBOL_CHARS).collect();
    format!(
        "{}\n... (truncated at {} characters)",
        cut, MAX_SYMBOL_CHARS
    )
}

/// Node ids are paths, and a memory written by an agent names a file the way a
/// human does: `src/parser/ingest.rs`. The walker yields a platform path with
/// backslashes and a leading `./` on Windows, and an edge only forms when the
/// two strings match exactly, so every id goes through here first.
pub fn normalize_node_path(path: &str) -> String {
    let forward = path.replace('\\', "/");
    let trimmed = forward.strip_prefix("./").unwrap_or(&forward);
    trimmed.trim_end_matches('/').to_string()
}

/// The stable key identifying the source (worktree) being ingested.
///
/// This is what keeps two concurrent worktrees of one repository apart in the
/// generation pointer table: each source's active generation is recorded under
/// its own key, so publishing one worktree cannot overwrite the other's active
/// pointer. It doubles as `SourceIdentity.worktree_id` until Task 2's
/// `source::resolve` fills repo/revision from a real checkout.
pub fn source_key_for(dir_path: &Path) -> String {
    std::fs::canonicalize(dir_path)
        .unwrap_or_else(|_| dir_path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

// The separator between a namespace and the path it qualifies (`NAMESPACE_SEPARATOR`)
// is defined once in `crate::parser::generation` and re-exported at the top of
// this module, so `ingest.rs` and the store keep naming the same constant
// through the path they already use. `qualified_id` below builds the two-segment
// legacy form from it; staged ingestion builds the three-segment form with
// `generation_scoped_id`.

/// A node id, qualified by the namespace that owns it.
///
/// Node ids are paths, and the Memory table is PRIMARY KEY (id) -- one column,
/// because lbug 0.15.3 has no composite key (see examples/composite_pk_repro.rs
/// for the parser refusing one). So an id is unique across the WHOLE database,
/// while paths are only unique within a project. Every repository has a `src`
/// and a `README.md`, and the database is shared by all of them, which meant the
/// second project ingested silently took the first project's nodes: upsert
/// MERGEs on the id, finds the existing row, and updates it.
///
/// Qualifying the id with its namespace is what makes "projects are separated by
/// namespace" true in the schema rather than only in the documentation. It is
/// also the only one of the three candidate fixes the engine allows.
///
/// The bare path is still what a memory's `location` carries and what a rule
/// names, so declarations keep reading the way a human writes them --
/// `resolve_declared_target` bridges the two.
pub fn qualified_id(namespace: &str, node_path: &str) -> String {
    format!("{}{}{}", namespace, NAMESPACE_SEPARATOR, node_path)
}

/// Whether an ingested id names the namespace holding it.
///
/// False for anything written before ids carried a namespace. Such a node still
/// resolves -- KnownIds matches a bare path either way -- but its id is unique
/// only within its project rather than within the database, so another project
/// ingesting the same path can still take it. `doctor` reports these so the
/// state is visible instead of silent; a re-ingest is what migrates them.
pub fn is_qualified(namespace: &str, id: &str) -> bool {
    id.starts_with(&qualified_id(namespace, ""))
}

/// The id a node gets, relative to the directory being ingested.
///
/// Ids used to inherit whatever the caller passed: the CLI walked a relative
/// path and produced `src/store/ladybug.rs`, while the GUI passes an absolute
/// project path and produced `C:/dev/projects/neurostrata/src/store/ladybug.rs`.
/// GOVERNS edges match ids by exact string, so a rule an agent wrote against
/// `src/store/ladybug.rs` could never link to the same file ingested from the
/// GUI -- and an absolute id does not survive the repository being checked out
/// anywhere else (bead neurostrata-fld).
pub fn node_id_for(root: &Path, path: &Path) -> String {
    // A relative walk already produces the documented form. `ingest ./src <ns>`
    // is the example in CLI-readme.md, and it must keep yielding src/lib.rs --
    // the same shape a rule names when it says "src/lib.rs" -- rather than the
    // lib.rs that stripping the ingest root would leave.
    let relative = if path.is_relative() {
        if root.is_absolute() {
            // A relative walk under a marker-pinned root: the root is the
            // marker's absolute folder, the path the walk's relative spelling.
            // Resolve the path through the working directory so the key is
            // relative to the marker root, which is where lookups start.
            let absolute = std::env::current_dir().unwrap_or_default().join(path);
            absolute
                .strip_prefix(root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| path.to_path_buf())
        } else {
            path.to_path_buf()
        }
    } else {
        // Absolute: relative to the root being ingested. This used to try the
        // process's working directory first, which made an id depend on where
        // the daemon happened to be launched -- ingesting /workspace/Project
        // from /workspace produced Project/src/main.rs, which no rule names.
        // The CLI's subdirectory case is handled where the CLI reads its path.
        path.strip_prefix(root).unwrap_or(path).to_path_buf()
    };

    let normalized = normalize_node_path(&relative.to_string_lossy());
    if normalized.is_empty() {
        // The root itself. Name it, rather than leaving an empty id.
        normalize_node_path(
            &root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| root.to_string_lossy().to_string()),
        )
    } else {
        normalized
    }
}

/// One extracted symbol, owned so that no tree-sitter type is alive when the
/// embedding and upsert futures are awaited.
struct SymbolRow {
    name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
    /// The definition text cut for embedding and storage. Identity does NOT use
    /// this: `span_sha256` hashes the full, untruncated text, so re-ingesting
    /// unchanged code is the same symbol however its start line moves.
    body: String,
    /// SHA-256 of the full (untruncated) definition text. This is the symbol's
    /// stable identity tail (`symbol_id`), stable across line moves.
    span_sha256: String,
    calls: Vec<String>,
}

/// Staged symbol metadata captured during generation ingestion for pre-publish
/// cross-file call resolution (`neurostrata-rbde`).
#[derive(Clone, Debug)]
pub struct StagedSymbolInfo {
    pub id: String,
    pub file_path: String,
    pub name: String,
    pub kind: String,
    pub calls: Vec<String>,
}

/// Mask comments (`//`, `/* */`, `#`) and string/char literals (`"..."`, `'...'`)
/// with spaces so call extraction never produces false-positive identifiers from
/// non-code spans (`neurostrata-rbde`).
fn mask_comments_and_strings(body: &str) -> Vec<u8> {
    let bytes = body.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut line_start = true;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        // Line comment: //
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        // Block comment: /* ... */
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
            while i < bytes.len() {
                if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    break;
                }
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            continue;
        }
        // Python/shell hash comment at start of line or after whitespace (not Rust #[attr])
        if b == b'#' && line_start && (i + 1 >= bytes.len() || bytes[i + 1] != b'[') {
            while i < bytes.len() && bytes[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        // Double-quoted string literal
        if b == b'"' {
            out[i] = b' ';
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    continue;
                }
                let ch = bytes[i];
                if ch != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
                if ch == b'"' {
                    break;
                }
            }
            line_start = false;
            continue;
        }
        // Single-quoted string/char literal (skip Rust lifetime `'a` where closing `'` is absent)
        if b == b'\'' {
            // Check if there is a closing `'` on the same line within a reasonable span
            let mut j = i + 1;
            let mut has_close = false;
            while j < bytes.len() && bytes[j] != b'\n' {
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    j += 2;
                    continue;
                }
                if bytes[j] == b'\'' {
                    has_close = true;
                    break;
                }
                j += 1;
            }
            if has_close {
                while i <= j {
                    out[i] = b' ';
                    i += 1;
                }
                line_start = false;
                continue;
            }
        }
        if !b.is_ascii_whitespace() {
            line_start = false;
        }
        i += 1;
    }
    out
}

/// Extract identifiers followed by `(` (or `::<...>( `) from `body`, excluding
/// `self_name`, comments/strings, and language keywords/builtins. Sorted and deduplicated.
pub fn extract_called_identifiers(body: &str, self_name: &str) -> Vec<String> {
    const KEYWORDS: &[&str] = &[
        "if",
        "else",
        "for",
        "while",
        "loop",
        "match",
        "switch",
        "return",
        "sizeof",
        "typeof",
        "catch",
        "fn",
        "def",
        "function",
        "pub",
        "Some",
        "None",
        "Ok",
        "Err",
        "vec",
        "format",
        "println",
        "eprintln",
        "assert",
        "assert_eq",
        "assert_ne",
    ];
    let masked = mask_comments_and_strings(body);
    let bytes = masked.as_slice();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let ident = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            // Skip horizontal whitespace
            let mut k = i;
            while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                k += 1;
            }
            // Support Rust turbofish `ident::<T>(`
            if k + 2 < bytes.len()
                && bytes[k] == b':'
                && bytes[k + 1] == b':'
                && bytes[k + 2] == b'<'
            {
                let mut depth = 1usize;
                k += 3;
                while k < bytes.len() && depth > 0 {
                    if bytes[k] == b'<' {
                        depth += 1;
                    } else if bytes[k] == b'>' {
                        depth -= 1;
                    }
                    k += 1;
                }
            }
            if k < bytes.len()
                && bytes[k] == b'('
                && !ident.is_empty()
                && ident != self_name
                && !KEYWORDS.contains(&ident)
            {
                out.push(ident.to_string());
            }
        } else {
            i += 1;
        }
    }
    out.sort();
    out.dedup();
    out
}
/// One embedded markdown section.
#[derive(Clone)]
struct DocSection {
    slug: String,
    heading: String,
    /// Markdown heading depth (# = 1, ## = 2, ...); 0 for the implicit root.
    /// Persisted as `heading_level` in the doc-section node metadata.
    level: usize,
    text: String,
    sha256: String,
    backticked: Vec<String>,
    links: Vec<String>,
}

/// Parse markdown into sections by heading.
fn parse_markdown_sections(content: &str) -> Vec<DocSection> {
    let mut sections: Vec<DocSection> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_level: usize = 0;
    let mut body: Vec<String> = Vec::new();

    let flush =
        |heading: Option<String>, level: usize, body: &Vec<String>, out: &mut Vec<DocSection>| {
            let heading = heading.unwrap_or_else(|| "root".to_string());
            let text = body.join("\n").trim().to_string();
            if text.is_empty() {
                return;
            }
            let slug = slugify(&heading);
            let mut backticked = Vec::new();
            let mut links = Vec::new();
            for line in body {
                let mut rest = line.as_str();
                while let Some(start) = rest.find('`') {
                    let after = &rest[start + 1..];
                    if let Some(end) = after.find('`') {
                        let code = after[..end].trim();
                        if !code.is_empty() && !code.contains('\n') {
                            backticked.push(code.to_string());
                        }
                        rest = &after[end + 1..];
                    } else {
                        break;
                    }
                }
                let mut r = line.as_str();
                while let Some(start) = r.find('[') {
                    let after = &r[start..];
                    if let Some(bracket_end) = after.find(']') {
                        let rest2 = &after[bracket_end + 1..];
                        if rest2.starts_with('(') {
                            if let Some(paren_end) = rest2.find(')') {
                                let url = &rest2[1..paren_end];
                                links.push(url.to_string());
                            }
                        }
                        r = &after[bracket_end + 1..];
                    } else {
                        break;
                    }
                }
            }
            backticked.sort();
            backticked.dedup();
            links.sort();
            links.dedup();
            let sha = sha256_hex(text.as_bytes());
            out.push(DocSection {
                slug,
                heading,
                level,
                text,
                sha256: sha,
                backticked,
                links,
            });
        };

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            if level == 0 {
                continue;
            }
            if current_heading.is_some() {
                flush(current_heading.take(), current_level, &body, &mut sections);
                body.clear();
            }
            let heading_text = trimmed[level..].trim().to_string();
            current_heading = Some(heading_text);
            current_level = level;
        } else {
            body.push(line.to_string());
        }
    }
    if current_heading.is_some() || !body.is_empty() {
        flush(current_heading.take(), current_level, &body, &mut sections);
    }
    if sections.is_empty() {
        let text = content.trim().to_string();
        if !text.is_empty() {
            sections.push(DocSection {
                slug: "root".to_string(),
                heading: "root".to_string(),
                level: 0,
                text: text.clone(),
                sha256: sha256_hex(text.as_bytes()),
                backticked: vec![],
                links: vec![],
            });
        }
    }
    sections
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == ' ' || c == '-' || c == '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

// `symbol_id` and `NAMESPACE_SEPARATOR` are re-exported from
// `crate::parser::generation` at the top of this module.

/// Told what the walk has done so far, so a caller does not have to wait for
/// the whole thing to find out. Implementations are called from the walk, so
/// they must be cheap and must not block.
pub trait IngestObserver: Send + Sync {
    fn file_ingested(&self, path: &str, symbols: usize);
    fn file_skipped(&self, path: &str, reason: &str) {
        // Security skips are operator-visible at info; the per-file
        // "unchanged" skip stays at debug so a large tree is not spammed.
        if reason.contains("ERROR [SECURITY]") {
            tracing::info!("ingest skipped file: {path} ({reason})");
        } else {
            tracing::debug!("ingest skipped file: {path} ({reason})");
        }
    }
    fn relinked(&self, edges: usize);
    /// Called once per run when the previous generation's `ingest_version`
    /// differs from the current one (or is absent). Implementors surface this
    /// as a warning; the ingest continues with a full re-parse.
    fn version_gate_tripped(&self, prev: &str, current: &str) {
        tracing::warn!("ingest version gate tripped: prev={prev} current={current}");
    }
}

/// An owned item streamed from the blocking producer to the async consumer.
/// No borrowed tree-sitter or filesystem types cross the channel boundary.
enum IngestItem {
    /// A structural (directory/file/markdown) node to upsert with zero vector.
    NodeUpsert {
        node_id: String,
        node_path: String,
        contained_by: Vec<String>,
        mem_type: String,
        abs_path: String,
        /// SHA-256 of the file bytes, captured for parseable code files only.
        /// Directories and non-code files carry none.
        file_sha256: Option<String>,
    },
    /// Parsed symbols from a file, ready for embed + store.
    ParsedSymbols {
        node_path: String,
        lang_name: String,
        symbols: Vec<SymbolRow>,
        path_display: String,
        /// SHA-256 of the file bytes, shared by every symbol in the file.
        file_sha256: String,
    },
    /// A document the secret scanner refused: skipped whole, reported
    /// through the observer. Carries the file's display path and the
    /// scanner's reason — never the secret itself.
    SkippedFile {
        path_display: String,
        reason: String,
        /// The previous generation had rows for this path. Such a skip is a
        /// change (the old rows must not stay active); a never-indexed skip is
        /// not.
        had_previous: bool,
    },
    /// Embedded markdown sections.
    DocSections {
        node_path: String,
        sections: Vec<DocSection>,
        file_sha256: String,
    },
}

/// How many refusal reasons the generation summary carries. The count of
/// refusals is exact; only the samples are capped.
const MAX_DEGRADED_EMBED_SAMPLES: usize = 8;

/// The embeddings a provider refused during one generation.
///
/// A refusal is a warning, not an abort: the refused item is skipped and the
/// generation continues. The tally is what the generation summary reports --
/// the exact count, plus the first few reasons so an operator can see why
/// the provider said no.
#[derive(Debug, Default)]
struct DegradedEmbeddings {
    count: usize,
    samples: Vec<String>,
}

impl DegradedEmbeddings {
    /// Folds one batch outcome into the tally.
    fn note(&mut self, outcome: &crate::traits::BatchEmbedOutcome) {
        self.count += outcome.embeddings.iter().filter(|e| e.is_none()).count();
        for reason in &outcome.warnings {
            if self.samples.len() < MAX_DEGRADED_EMBED_SAMPLES {
                self.samples.push(reason.clone());
            }
        }
    }

    /// The summary block for the generation, or `None` when nothing was
    /// refused.
    fn summary(&self) -> Option<String> {
        if self.count == 0 {
            return None;
        }
        let mut out = format!(
            "WARNING: {} embedding(s) were refused by the embedder and skipped; the generation continued without them.\n",
            self.count
        );
        for reason in &self.samples {
            out.push_str(&format!("  - {}\n", reason));
        }
        Some(out)
    }
}

/// What the consumer reports back to the producer.
struct ConsumerOutcome {
    /// Items that were not a bare directory node.
    content_items: usize,
    staged_symbols: Vec<StagedSymbolInfo>,
    /// The embeddings the provider refused.
    degraded: DegradedEmbeddings,
}

/// Secret scan over a document's text, run in the producer as the file is
/// read — before any part of the document reaches the store. The verdict
/// text is the reason the skip reports.
fn scan_for_ingest(text: &str) -> Option<String> {
    crate::secrets::scan_entry_point(text, &serde_json::json!({}), "ingest").map(|r| r.to_string())
}

/// Read one file's bytes during an ingest walk.
///
/// Extracted so the abort path has a fixture that does not depend on file
/// mode: reading a directory fails with EISDIR for every uid, including a
/// root test process whose DAC override can read a mode-000 file.
fn read_ingest_file(path: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("failed to read {} during ingest: {e}", path.display()))
}

/// Compile every tree-sitter query a schema declares into an `Arc` cache keyed
/// by `language:query_name`. The producer then looks patterns up per file
/// instead of recompiling them per file; invalid patterns are skipped and
/// logged here, so the per-file pass never sees them.
fn compile_language_queries(schema: &ParserSchema) -> HashMap<String, std::sync::Arc<Query>> {
    let mut compiled = HashMap::new();
    for (lang_name, lang_schema) in &schema.languages {
        let ts_lang = match get_language(lang_name) {
            Some(l) => l,
            None => continue,
        };
        for (query_name, query_str) in &lang_schema.queries {
            match Query::new(&ts_lang, query_str) {
                Ok(q) => {
                    compiled.insert(format!("{lang_name}:{query_name}"), std::sync::Arc::new(q));
                }
                Err(e) => {
                    tracing::warn!("Invalid tree-sitter query for {lang_name}: {e}");
                }
            }
        }
    }
    compiled
}

/// Number of worker threads the parallel producer spawns for a given entry
/// count. Bounded to the available parallelism (clamped 1..=8) and never
/// more than there are entries to process.
pub(crate) fn ingest_worker_count(total_entries: usize) -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
        .min(total_entries.max(1))
}

/// One entry collected during the walk, before any file I/O or parsing.
#[derive(Clone)]
struct WalkedEntry {
    pub path: std::path::PathBuf,
    pub path_str: String,
    pub abs_path: String,
    pub node_path: String,
    pub node_id: String,
    pub contained_by: Vec<String>,
    pub is_file: bool,
    pub mem_type: String,
}

/// The result of processing one `WalkedEntry` on a worker thread. Carries
/// everything the producer thread needs to replay the original side effects in
/// deterministic walk order.
enum ProcessedEntry {
    Directory,
    UnchangedFile {
        file_sha256: String,
    },
    SecretSkipped {
        reason: String,
        had_previous: bool,
        file_sha256: String,
    },
    ParsedFile {
        file_sha256: String,
        doc_sections: Vec<DocSection>,
        symbols: Vec<SymbolRow>,
        imports: Vec<String>,
        lang_name: Option<String>,
    },
}

/// Process a single walked entry on a worker thread: read bytes, hash, secret
/// scan, and (when applicable) parse the AST. Returns an owned `ProcessedEntry`
/// so no tree-sitter or filesystem types cross back to the producer thread.
fn process_walked_entry(
    entry: &WalkedEntry,
    ext_to_lang: &HashMap<String, String>,
    schema: &crate::parser::schema::ParserSchema,
    compiled_queries: &HashMap<String, std::sync::Arc<tree_sitter::Query>>,
    prev_file_map: &HashMap<String, String>,
    force_reparse: bool,
    parsers_by_lang: &mut HashMap<String, Parser>,
) -> Result<ProcessedEntry, anyhow::Error> {
    if !entry.is_file {
        return Ok(ProcessedEntry::Directory);
    }

    let bytes = read_ingest_file(&entry.path)?;
    let file_sha256 = sha256_hex(&bytes);

    // Incremental skip.
    if !force_reparse {
        if let Some(prev_hash) = prev_file_map.get(&entry.node_path) {
            if prev_hash == &file_sha256 {
                return Ok(ProcessedEntry::UnchangedFile { file_sha256 });
            }
        }
    }

    let lang_name = entry
        .path
        .extension()
        .map(|ext_os| normalize_ext(&ext_os.to_string_lossy()))
        .and_then(|ext| ext_to_lang.get(&ext).cloned())
        .filter(|ln| get_language(ln).is_some());

    let content_for_parse = if lang_name.is_some() {
        Some(String::from_utf8_lossy(&bytes).to_string())
    } else {
        None
    };
    let scan_text = if let Some(t) = &content_for_parse {
        Some(t.clone())
    } else if entry.mem_type == "markdown" {
        Some(String::from_utf8_lossy(&bytes).to_string())
    } else {
        None
    };
    if let Some(text) = &scan_text {
        if let Some(reason) = scan_for_ingest(text) {
            return Ok(ProcessedEntry::SecretSkipped {
                reason,
                had_previous: prev_file_map.contains_key(&entry.node_path),
                file_sha256,
            });
        }
    }

    // ParsedFile.
    let mut doc_sections = Vec::new();
    if entry.mem_type == "markdown" {
        let content = String::from_utf8_lossy(&bytes).to_string();
        doc_sections = parse_markdown_sections(&content);
    }

    let mut symbols = Vec::new();
    let mut imports = Vec::new();
    let mut parsed_lang = None;

    if let Some(ln) = &lang_name {
        let content = String::from_utf8_lossy(&bytes).to_string();
        let ts_lang = get_language(ln).expect("language present");
        let parser = if let Some(p) = parsers_by_lang.get_mut(ln) {
            p
        } else {
            let mut p = Parser::new();
            p.set_language(&ts_lang)?;
            parsers_by_lang.insert(ln.clone(), p);
            parsers_by_lang.get_mut(ln).unwrap()
        };

        if let Some(tree) = parser.parse(&content, None) {
            let lang_schema = &schema.languages[ln];
            let mut pending: Vec<SymbolRow> = Vec::new();

            for (query_name, _query_str) in &lang_schema.queries {
                let query_key = format!("{ln}:{query_name}");
                let query: &tree_sitter::Query = match compiled_queries.get(&query_key) {
                    Some(q) => q.as_ref(),
                    None => continue,
                };

                let mut cursor = tree_sitter::QueryCursor::new();
                let mut iter = cursor.matches(query, tree.root_node(), content.as_bytes());

                while let Some(m) = iter.next() {
                    let mut name: Option<&str> = None;
                    let mut definition: Option<tree_sitter::Node> = None;

                    for capture in m.captures() {
                        let capture_name = query.capture_names()[capture.index as usize];
                        if capture_name == "name" {
                            name = capture.node.utf8_text(content.as_bytes()).ok();
                        } else if definition.map_or(true, |d: tree_sitter::Node| {
                            capture.node.byte_range().len() > d.byte_range().len()
                        }) {
                            definition = Some(capture.node);
                        }
                    }

                    let (name, definition) = match (name, definition) {
                        (Some(n), Some(d)) => (n, d),
                        _ => continue,
                    };

                    let full = definition.utf8_text(content.as_bytes()).unwrap_or("");
                    pending.push(SymbolRow {
                        name: name.to_string(),
                        kind: query_name.clone(),
                        start_line: definition.start_position().row + 1,
                        end_line: definition.end_position().row + 1,
                        body: truncate_for_embedding(full),
                        span_sha256: sha256_hex(full.as_bytes()),
                        calls: extract_called_identifiers(full, name),
                    });
                }
            }

            symbols = pending;
        }

        imports = extract_import_specifiers(ln, &content);
        parsed_lang = Some(ln.clone());
    }

    Ok(ProcessedEntry::ParsedFile {
        file_sha256,
        doc_sections,
        symbols,
        imports,
        lang_name: parsed_lang,
    })
}

/// The most results the ingest may hold between the workers and the replay.
///
/// A worker may not start an index that is more than this many indices ahead
/// of the replay, so the reorder buffer (and the channel behind it) holds a
/// bounded number of results however large the tree is. The window is wide
/// enough to keep every worker busy and narrow enough to bound the memory.
const INGEST_REPLAY_WINDOW: usize = 64;

/// What one streaming run cost.
#[derive(Debug, Default, Clone, Copy)]
struct StreamStats {
    /// The most results that were held between a worker and the replay at one
    /// time. It never exceeds the window: that is the bound the run promises.
    peak_buffered: usize,
}

/// The state the workers and the replay share: how far the replay has got,
/// and whether it has given up.
///
/// One mutex guards both fields, and that is what makes the cancellation
/// safe. A worker reads the abort flag and waits on the condvar while it
/// holds this lock, and the canceller takes the same lock before it sets the
/// flag and notifies. A worker that read "not aborted" therefore cannot sleep
/// through the notification: the canceller cannot set the flag until the
/// worker has released the lock inside `Condvar::wait`, and the flag is set
/// before the notify.
#[derive(Debug, Default)]
struct ReplayGateState {
    /// The index the replay takes next.
    position: usize,
    /// Set when the replay gives up: the workers must stop.
    aborted: bool,
}

/// The gate the workers wait on: the replay's position and the abort flag
/// under ONE lock, with the condvar that wakes them.
///
/// The lock is what makes the cancellation safe. A worker reads the abort
/// flag and waits on the condvar while it holds the lock; the replay sets
/// the flag and notifies. If the flag were set outside that lock, the two
/// could interleave -- the worker reads "not aborted", the replay sets the
/// flag and notifies (the worker is not waiting yet, so the notification is
/// lost), and the worker sleeps through it. Under one lock the replay
/// cannot set the flag until the worker has released the lock inside
/// `Condvar::wait`, and the flag is set before the notify.
struct ReplayGate {
    state: std::sync::Mutex<ReplayGateState>,
    advanced: std::sync::Condvar,
}

impl ReplayGate {
    fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(ReplayGateState::default()),
            advanced: std::sync::Condvar::new(),
        }
    }

    /// Waits until `idx` is within `window` of the replay position.
    ///
    /// Returns `false` when the run was aborted: the worker must stop
    /// instead of running an index nobody will replay.
    fn wait_for_slot(&self, idx: usize, window: usize) -> bool {
        let mut state = self.state.lock().expect("replay gate mutex");
        while idx >= state.position.saturating_add(window) {
            if state.aborted {
                return false;
            }
            state = self.advanced.wait(state).expect("replay gate mutex");
        }
        !state.aborted
    }

    /// The replay took `next` as the next index: the workers may run on.
    fn advance_to(&self, next: usize) {
        let mut state = self.state.lock().expect("replay gate mutex");
        state.position = next;
        self.advanced.notify_all();
    }

    /// The replay gave up: every worker waiting for it must wake and stop.
    fn abort(&self) {
        let mut state = self.state.lock().expect("replay gate mutex");
        state.aborted = true;
        self.advanced.notify_all();
    }
}

/// Runs `process` over `0..total` on a bounded worker pool and calls `replay`
/// once per index, in index order.
///
/// The run has three moving parts:
///
/// * the workers hand their results back over a bounded channel, so a slow
///   replay slows the workers down instead of growing an unbounded queue;
/// * a worker may not start an index that is more than `window` indices ahead
///   of the replay, so the reorder buffer holds at most `window` results;
/// * `replay` is called in index order, so the side effects land in the same
///   order a sequential run would put them in -- the determinism contract
///   does not depend on the parallelism.
///
/// `make_process` builds one worker-local processor (the tree-sitter parser
/// cache is per worker), and a worker that panics does not take the run with
/// it: the panic becomes that entry's error, so the replay sees it in order
/// like any other failure.
fn stream_in_walk_order<T: Send, P>(
    total: usize,
    worker_count: usize,
    window: usize,
    make_process: impl Fn() -> P + Sync,
    replay: impl FnMut(usize, anyhow::Result<T>) -> anyhow::Result<()>,
) -> anyhow::Result<StreamStats>
where
    P: FnMut(usize) -> anyhow::Result<T> + Send,
{
    stream_in_walk_order_bounded(total, worker_count, window, window, make_process, replay)
}

/// The same run, with the result channel's capacity spelled out separately
/// from the replay window.
///
/// Production ties the two together (the channel holds the window's worth of
/// results). The seam exists so a test can make the channel the tighter of
/// the two: then a worker really does block in `send` while the replay is
/// over, which is the case the abort path has to release.
fn stream_in_walk_order_bounded<T: Send, P>(
    total: usize,
    worker_count: usize,
    window: usize,
    channel_capacity: usize,
    make_process: impl Fn() -> P + Sync,
    mut replay: impl FnMut(usize, anyhow::Result<T>) -> anyhow::Result<()>,
) -> anyhow::Result<StreamStats>
where
    P: FnMut(usize) -> anyhow::Result<T> + Send,
{
    if total == 0 {
        return Ok(StreamStats::default());
    }
    let window = window.max(1);
    let worker_count = worker_count.max(1);
    let next_idx = AtomicUsize::new(0);
    // The gate the workers wait on: the replay's position and the abort flag
    // under ONE lock, so a cancellation cannot slip between a worker's check
    // and its wait.
    let gate = ReplayGate::new();
    // The results that are produced but not yet replayed.
    let buffered = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);

    std::thread::scope(|scope| -> anyhow::Result<()> {
        // The channel lives INSIDE the scope: on the abort path the receiver
        // is dropped before the scope joins the workers, so a worker blocked
        // in `send` fails and wakes instead of waiting for a receiver that
        // will never read again. A receiver created outside the scope would
        // outlive the abort and leave that worker blocked forever.
        let (result_tx, result_rx) =
            std::sync::mpsc::sync_channel::<(usize, anyhow::Result<T>)>(channel_capacity.max(1));
        for _ in 0..worker_count {
            let result_tx = result_tx.clone();
            let next_idx = &next_idx;
            let gate = &gate;
            let buffered = &buffered;
            let peak = &peak;
            let make_process = &make_process;
            scope.spawn(move || {
                let mut process = make_process();
                loop {
                    let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                    if idx >= total {
                        break;
                    }
                    // Backpressure: do not run more than `window` indices
                    // ahead of the replay.
                    if !gate.wait_for_slot(idx, window) {
                        // The replay gave up: this index will never be
                        // replayed, so the worker stops.
                        break;
                    }
                    // A panicking worker must not take the whole run with it:
                    // the panic becomes this entry's error, and the replay
                    // reports it in order like any other failure.
                    let res =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(idx)))
                            .unwrap_or_else(|_| {
                                Err(anyhow::anyhow!(
                                    "ingest worker panicked while processing index {}",
                                    idx
                                ))
                            });
                    let live = buffered.fetch_add(1, Ordering::Relaxed) + 1;
                    peak.fetch_max(live, Ordering::Relaxed);
                    if result_tx.send((idx, res)).is_err() {
                        break;
                    }
                }
            });
        }
        // The scope's own sender goes away: the channel closes when the
        // workers are done.
        drop(result_tx);

        let mut buffer: std::collections::BTreeMap<usize, anyhow::Result<T>> =
            std::collections::BTreeMap::new();
        let mut next = 0usize;
        let mut outcome: anyhow::Result<()> = Ok(());
        while let Ok((idx, res)) = result_rx.recv() {
            buffer.insert(idx, res);
            while let Some(res) = buffer.remove(&next) {
                if let Err(e) = replay(next, res) {
                    // The replay is done: the workers that wait for it are
                    // woken below, so the run can end instead of waiting for
                    // a replay that will never come.
                    outcome = Err(e);
                    gate.abort();
                    break;
                }
                next += 1;
                buffered.fetch_sub(1, Ordering::Relaxed);
                gate.advance_to(next);
            }
            if outcome.is_err() {
                break;
            }
        }
        if let Err(e) = outcome {
            // The run is over. Cancel the gate and close the channel BEFORE
            // the scope joins the workers: the gate wakes the workers that
            // wait for the replay, and the closed channel fails the `send`
            // of a worker blocked on a full one, so it wakes instead of
            // waiting for a receiver that will never read again.
            gate.abort();
            drop(result_rx);
            return Err(e);
        }
        if !buffer.is_empty() {
            anyhow::bail!(
                "ingest replay ended with {} result(s) that never arrived in order",
                buffer.len()
            );
        }
        Ok(())
    })?;

    Ok(StreamStats {
        peak_buffered: peak.load(Ordering::Relaxed),
    })
}

/// The state the deterministic replay writes into.
///
/// The replay applies the side effects of the processed entries in walk
/// order. The state is threaded through explicitly rather than captured by a
/// closure, so the replay is a plain function that a test can drive
/// directly.
struct ReplayState<'a> {
    /// The walk, in the order the entries were found.
    entries: &'a [WalkedEntry],
    /// Every path this walk visited.
    current_paths: &'a mut std::collections::HashSet<String>,
    /// What kind of node each visited path is.
    current_kinds: &'a mut std::collections::HashMap<String, String>,
    /// The content hash of each visited file.
    current_hashes: &'a mut std::collections::HashMap<String, String>,
    /// The imports each file declared, before they are resolved.
    raw_imports: &'a mut std::collections::HashMap<String, Vec<String>>,
    /// The consumer the replay feeds.
    tx: &'a mpsc::Sender<IngestItem>,
    /// The optional observer.
    observer: &'a Option<std::sync::Arc<dyn IngestObserver>>,
}

/// Applies one processed entry's side effects, in walk order.
///
/// The path and the kind are recorded before the result is unwrapped, so a
/// failed entry still leaves the walk's shape behind; the failure itself
/// aborts the replay, exactly as it aborted the sequential replay before.
fn replay_processed_entry(
    state: &mut ReplayState<'_>,
    idx: usize,
    result: anyhow::Result<ProcessedEntry>,
) -> anyhow::Result<()> {
    let entry = &state.entries[idx];
    state.current_paths.insert(entry.node_path.clone());
    state
        .current_kinds
        .insert(entry.node_path.clone(), entry.mem_type.clone());

    let processed = result?;

    match processed {
        ProcessedEntry::Directory => {
            if let Err(e) = state.tx.blocking_send(IngestItem::NodeUpsert {
                node_id: entry.node_id.clone(),
                node_path: entry.node_path.clone(),
                contained_by: entry.contained_by.clone(),
                mem_type: entry.mem_type.clone(),
                abs_path: entry.abs_path.clone(),
                file_sha256: None,
            }) {
                return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
            }
        }
        ProcessedEntry::UnchangedFile { file_sha256 } => {
            state
                .current_hashes
                .insert(entry.node_path.clone(), file_sha256);
            if let Some(obs) = &state.observer {
                obs.file_skipped(&entry.path_str, "unchanged");
            }
        }
        ProcessedEntry::SecretSkipped {
            reason,
            had_previous,
            file_sha256,
        } => {
            state
                .current_hashes
                .insert(entry.node_path.clone(), file_sha256);
            if !had_previous {
                state.current_paths.remove(&entry.node_path);
                state.current_kinds.remove(&entry.node_path);
            }
            if let Err(e) = state.tx.blocking_send(IngestItem::SkippedFile {
                path_display: entry.path_str.clone(),
                reason,
                had_previous,
            }) {
                return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
            }
        }
        ProcessedEntry::ParsedFile {
            file_sha256,
            doc_sections,
            symbols,
            imports,
            lang_name,
        } => {
            state
                .current_hashes
                .insert(entry.node_path.clone(), file_sha256.clone());

            if let Err(e) = state.tx.blocking_send(IngestItem::NodeUpsert {
                node_id: entry.node_id.clone(),
                node_path: entry.node_path.clone(),
                contained_by: entry.contained_by.clone(),
                mem_type: entry.mem_type.clone(),
                abs_path: entry.abs_path.clone(),
                file_sha256: Some(file_sha256.clone()),
            }) {
                return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
            }

            if !doc_sections.is_empty() {
                if let Err(e) = state.tx.blocking_send(IngestItem::DocSections {
                    node_path: entry.node_path.clone(),
                    sections: doc_sections,
                    file_sha256: file_sha256.clone(),
                }) {
                    return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
                }
            }

            if !imports.is_empty() {
                state.raw_imports.insert(entry.node_path.clone(), imports);
            }

            if !symbols.is_empty() {
                if let Some(ln) = lang_name {
                    let path_display = entry.path_str.clone();
                    if let Err(e) = state.tx.blocking_send(IngestItem::ParsedSymbols {
                        node_path: entry.node_path.clone(),
                        lang_name: ln,
                        symbols,
                        path_display,
                        file_sha256: file_sha256.clone(),
                    }) {
                        return Err(anyhow::anyhow!("ingest consumer dropped: {}", e));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Folder the stored node keys are relative to. A marker pinning an ancestor
/// of the walk root can supply that root, but only when it is the namespace's
/// cached root: `server::marker_use` applies marker spelling to lookups only
/// in that case, so a marker root that differs from the cached one would make
/// every key unreachable. Marker discovery needs an absolute path (relative
/// paths find no marker), while the returned root stays in the walk's own
/// spelling so a relative walk keeps producing relative keys.
fn key_root_for_ingest(
    dir_path: &Path,
    namespace: &str,
    cached_root: Option<&str>,
) -> std::path::PathBuf {
    let absolute = if dir_path.is_absolute() {
        dir_path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(dir_path)
    };
    match crate::root_marker::find_root_marker(&absolute) {
        Ok(Some(found))
            if found.config.namespace.eq_ignore_ascii_case(namespace)
                && crate::paths::marker_root_is_cached_root(&found, cached_root) =>
        {
            // `marker_dir()` maps a linked worktree back to the main checkout,
            // but the walker yields paths in the worktree's own spelling. Keep
            // that spelling: a relative walk must strip a relative root, and
            // an absolute one an absolute root, or the key becomes absolute.
            let root = found.root;
            if !absolute.starts_with(&root) {
                return dir_path.to_path_buf();
            }
            if dir_path.is_absolute() {
                root
            } else {
                match std::env::current_dir() {
                    Ok(cwd) => match root.strip_prefix(&cwd) {
                        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
                        _ => root,
                    },
                    Err(_) => root,
                }
            }
        }
        _ => dir_path.to_path_buf(),
    }
}

/// Blocking producer: walks the directory tree, reads files, parses ASTs,
/// and streams owned results through `tx`. Runs on the blocking thread pool
/// so filesystem I/O never stalls the async runtime.
fn run_producer(
    dir_path: std::path::PathBuf,
    ext_to_lang: HashMap<String, String>,
    schema: crate::parser::schema::ParserSchema,
    tx: mpsc::Sender<IngestItem>,
    namespace: String,
    generation: u64,
    prev_file_map: std::collections::HashMap<String, String>,
    force_reparse: bool,
    observer: Option<std::sync::Arc<dyn IngestObserver>>,
    cached_root: Option<String>,
) -> anyhow::Result<(
    std::collections::HashSet<String>,
    std::collections::HashMap<String, String>,
    std::collections::HashMap<String, Vec<String>>,
    std::collections::HashMap<String, String>,
)> {
    let mut current_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    // What kind of node each visited path is (`directory`, `file` or
    // `markdown`). A path alone cannot say whether the previous generation's
    // node at that path is still the right one: a file replaced by a directory
    // of the same name, or the reverse, keeps the path and changes the node.
    let mut current_kinds: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut current_hashes: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut raw_imports: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let key_root = key_root_for_ingest(&dir_path, &namespace, cached_root.as_deref());

    let walker = WalkBuilder::new(&dir_path).build();

    let mut unreadable = 0usize;

    // One compiled tree-sitter query per schema entry per run, instead of
    // recompiling the pattern for every file the walk reaches.
    let compiled_queries = compile_language_queries(&schema);

    // Collect walk entries in deterministic order first; processing happens
    // in parallel below.
    let mut entries: Vec<WalkedEntry> = Vec::new();

    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                unreadable += 1;
                if unreadable <= 5 {
                    eprintln!("Could not read part of {:?}: {}", dir_path, e);
                }
                continue;
            }
        };

        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();

        if !is_ingest_eligible_path(path, &dir_path) {
            continue;
        }

        let abs_path = if path.is_absolute() {
            path.to_string_lossy().to_string()
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(path)
                .to_string_lossy()
                .to_string()
        };

        let node_path = node_id_for(&key_root, path);

        let parent_id = if path == dir_path {
            None
        } else if let Some(p) = path.parent() {
            if p == key_root || p.strip_prefix(&key_root).is_ok() {
                let p_str = node_id_for(&key_root, p);
                if p_str != "." && p_str != "" && p_str != node_path {
                    Some(p_str)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut contained_by = Vec::new();
        if let Some(pid) = parent_id {
            contained_by.push(generation_scoped_id(&namespace, generation, &pid));
        }

        let is_dir = entry.file_type().map_or(false, |ft| ft.is_dir());
        let is_file = entry.file_type().map_or(false, |ft| ft.is_file());

        let mem_type = if is_dir {
            "directory"
        } else if is_file {
            if path_str.ends_with(".md") {
                "markdown"
            } else {
                "file"
            }
        } else {
            continue;
        };

        let node_id = generation_scoped_id(&namespace, generation, &node_path);

        entries.push(WalkedEntry {
            path: path.to_path_buf(),
            path_str,
            abs_path,
            node_path,
            node_id,
            contained_by,
            is_file,
            mem_type: mem_type.to_string(),
        });
    }

    // Process the entries in parallel and replay their side effects in
    // deterministic walk order. The stream is bounded: a worker may not run
    // more than `INGEST_REPLAY_WINDOW` indices ahead of the replay, so the
    // reorder buffer holds a bounded number of results however large the tree
    // is.
    let worker_count = ingest_worker_count(entries.len());
    let entries_ref = &entries;
    let ext_to_lang_ref = &ext_to_lang;
    let schema_ref = &schema;
    let compiled_queries_ref = &compiled_queries;
    let prev_file_map_ref = &prev_file_map;
    let mut replay_state = ReplayState {
        entries: &entries,
        current_paths: &mut current_paths,
        current_kinds: &mut current_kinds,
        current_hashes: &mut current_hashes,
        raw_imports: &mut raw_imports,
        tx: &tx,
        observer: &observer,
    };
    let stats = stream_in_walk_order(
        entries.len(),
        worker_count,
        INGEST_REPLAY_WINDOW,
        || {
            let mut parsers_by_lang: HashMap<String, Parser> = HashMap::new();
            move |idx: usize| {
                process_walked_entry(
                    &entries_ref[idx],
                    ext_to_lang_ref,
                    schema_ref,
                    compiled_queries_ref,
                    prev_file_map_ref,
                    force_reparse,
                    &mut parsers_by_lang,
                )
            }
        },
        |idx, result| replay_processed_entry(&mut replay_state, idx, result),
    )?;
    drop(replay_state);
    tracing::debug!(
        peak_buffered = stats.peak_buffered,
        "ingest replay stream drained"
    );

    if unreadable > 0 {
        eprintln!(
            "WARNING: {} path(s) under {:?} could not be read, so they are missing from this ingest of namespace {}.",
            unreadable, dir_path, namespace
        );
    }

    // Resolve raw imports to in-repo file paths now that the full file set is known.
    let mut resolved = std::collections::HashMap::new();
    for (src, imports) in raw_imports {
        let targets = resolve_dependency_paths(&src, &imports, &current_paths);
        if !targets.is_empty() {
            resolved.insert(src, targets);
        }
    }
    Ok((current_paths, current_hashes, resolved, current_kinds))
}
/// Stamps the per-node `relative_path` onto a copy of the shared source
/// identity. The identity is otherwise identical across a tree, but the path
/// it governs differs per node, so each row records its own.
fn with_relative_path(
    source_identity: &serde_json::Value,
    relative_path: &str,
) -> serde_json::Value {
    let mut si = source_identity.clone();
    if let Some(obj) = si.as_object_mut() {
        obj.insert(
            "relative_path".to_string(),
            serde_json::json!(relative_path),
        );
    }
    si
}

/// Async consumer: receives owned items from the producer, embeds symbols,
/// and upserts everything to the vector store.
async fn run_consumer(
    mut rx: mpsc::Receiver<IngestItem>,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
    observer: Option<Arc<dyn IngestObserver>>,
    zero_vector: Vec<f32>,
    generation: u64,
    source_identity: serde_json::Value,
    ingest_version: &str,
    node_set: &[String],
) -> anyhow::Result<ConsumerOutcome> {
    // Items that are not a bare directory node: a file, section or symbol was
    // read again, so this walk changed something.
    let mut content_items = 0usize;
    let mut staged_symbols = Vec::new();
    // The embeddings the provider refused: a warning, not an abort.
    let mut degraded = DegradedEmbeddings::default();
    while let Some(item) = rx.recv().await {
        // A SkippedFile counts only when it replaced previous-generation rows;
        // a never-indexed skip is not content and must not defeat `unchanged`.
        match &item {
            IngestItem::NodeUpsert { mem_type, .. } if mem_type == "directory" => {}
            IngestItem::SkippedFile { .. } => {}
            _ => content_items += 1,
        }
        match item {
            IngestItem::NodeUpsert {
                node_id,
                node_path,
                contained_by,
                mem_type,
                abs_path,
                file_sha256,
            } => {
                let mut metadata = serde_json::Map::new();
                metadata.insert("absolute_path".to_string(), serde_json::json!(abs_path));
                metadata.insert("contained_by".to_string(), serde_json::json!(contained_by));
                metadata.insert(
                    "source_identity".to_string(),
                    with_relative_path(&source_identity, &node_path),
                );
                metadata.insert(
                    "index_generation".to_string(),
                    serde_json::json!(generation),
                );
                if let Some(hash) = file_sha256 {
                    metadata.insert("file_sha256".to_string(), serde_json::json!(hash));
                }
                metadata.insert(
                    "ingest_version".to_string(),
                    serde_json::json!(ingest_version),
                );
                metadata.insert("node_set".to_string(), serde_json::json!(node_set));

                let payload = MemoryPayload {
                    content: format!("Path: {}", node_path),
                    location: node_path,
                    location_lines: String::new(),
                    memory_type: mem_type,
                    metadata: serde_json::Value::Object(metadata),
                    user_id: "auto-ingestor".to_string(),
                    agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                };

                // A failed upsert aborts the staging generation rather than
                // reporting success over a graph with a hole in it: the error
                // propagates to `ingest_directory`, which calls
                // `abort_generation` and leaves the previous generation active.
                vector_store
                    .upsert(namespace, &node_id, zero_vector.clone(), payload)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("failed to upsert graph node {}: {}", node_id, e)
                    })?;
            }
            IngestItem::ParsedSymbols {
                node_path,
                lang_name,
                symbols,
                path_display,
                file_sha256,
            } => {
                let mut symbols_stored = 0usize;
                let file_symbol_ids: std::collections::HashMap<String, String> = symbols
                    .iter()
                    .map(|s| {
                        (
                            s.name.clone(),
                            generation_scoped_id(
                                namespace,
                                generation,
                                &symbol_id(&node_path, &s.kind, &s.name, &s.span_sha256),
                            ),
                        )
                    })
                    .collect();
                if !symbols.is_empty() {
                    let summaries: Vec<String> = symbols
                        .iter()
                        .map(|s| {
                            format!(
                                "{} {} in {}\nlines {}-{}\n\n{}",
                                s.kind, s.name, node_path, s.start_line, s.end_line, s.body
                            )
                        })
                        .collect();
                    let outcome = embedder
                        .embed_batch_degradable(&summaries)
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to embed symbol batch: {}", e))?;
                    if outcome.embeddings.len() != summaries.len() {
                        anyhow::bail!(
                            "embedder returned {} embeddings for {} symbols",
                            outcome.embeddings.len(),
                            summaries.len()
                        );
                    }
                    degraded.note(&outcome);
                    for ((symbol, summary), embedding) in
                        symbols.into_iter().zip(summaries).zip(outcome.embeddings)
                    {
                        // A provider refusal is a warning, not an abort: the
                        // item is skipped and the generation continues.
                        let embedding = match embedding {
                            Some(embedding) => embedding,
                            None => continue,
                        };
                        let id = generation_scoped_id(
                            namespace,
                            generation,
                            &symbol_id(&node_path, &symbol.kind, &symbol.name, &symbol.span_sha256),
                        );
                        let lines = format!("{}-{}", symbol.start_line, symbol.end_line);

                        let mut metadata = serde_json::Map::new();
                        metadata.insert("domain".to_string(), serde_json::json!("code_ast"));
                        metadata.insert("symbol".to_string(), serde_json::json!(symbol.name));
                        metadata.insert("symbol_kind".to_string(), serde_json::json!(symbol.kind));
                        metadata.insert("language".to_string(), serde_json::json!(lang_name));
                        metadata.insert(
                            "contained_by".to_string(),
                            serde_json::json!([generation_scoped_id(
                                namespace, generation, &node_path
                            )]),
                        );
                        metadata.insert(
                            "refs".to_string(),
                            serde_json::json!([
                                { "file": node_path.clone(), "lines": lines.clone() }
                            ]),
                        );
                        metadata.insert("file_sha256".to_string(), serde_json::json!(file_sha256));
                        metadata.insert(
                            "ingest_version".to_string(),
                            serde_json::json!(ingest_version),
                        );
                        metadata.insert("node_set".to_string(), serde_json::json!(node_set));
                        metadata.insert(
                            "span_sha256".to_string(),
                            serde_json::json!(symbol.span_sha256),
                        );
                        metadata.insert(
                            "source_identity".to_string(),
                            with_relative_path(&source_identity, &node_path),
                        );
                        metadata.insert(
                            "index_generation".to_string(),
                            serde_json::json!(generation),
                        );

                        if !symbol.calls.is_empty() {
                            metadata.insert("calls".to_string(), serde_json::json!(symbol.calls));
                            let called_ids: Vec<String> = symbol
                                .calls
                                .iter()
                                .filter_map(|c| file_symbol_ids.get(c))
                                .filter(|cid| *cid != &id)
                                .cloned()
                                .collect();
                            if !called_ids.is_empty() {
                                metadata.insert(
                                    "calls_resolved".to_string(),
                                    serde_json::json!(called_ids.clone()),
                                );
                                metadata.insert(
                                    "depends_on".to_string(),
                                    serde_json::json!(called_ids),
                                );
                            }
                        }

                        staged_symbols.push(StagedSymbolInfo {
                            id: id.clone(),
                            file_path: node_path.clone(),
                            name: symbol.name.clone(),
                            kind: symbol.kind.clone(),
                            calls: symbol.calls.clone(),
                        });

                        let payload = MemoryPayload {
                            content: summary.clone(),
                            location: node_path.clone(),
                            location_lines: lines,
                            memory_type: "code_ast".to_string(),
                            metadata: serde_json::Value::Object(metadata),
                            user_id: "auto-ingestor".to_string(),
                            agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                        };

                        // An embed or store failure aborts the staging generation:
                        // swallowing it here would hand back a graph whose symbols
                        // are silently missing, which is exactly the half-built
                        // tree staged publication exists to prevent. The batch
                        // embed above already failed the run before any row landed.
                        vector_store
                            .upsert(namespace, &id, embedding, payload)
                            .await
                            .map_err(|e| {
                                anyhow::anyhow!(
                                    "failed to store symbol {} from {}: {}",
                                    symbol.name,
                                    path_display,
                                    e
                                )
                            })?;
                        symbols_stored += 1;
                    }
                }

                if symbols_stored > 0 {
                    println!("Ingested {} symbols from {}", symbols_stored, path_display);
                    if let Some(observer) = &observer {
                        observer.file_ingested(&path_display, symbols_stored);
                    }
                }
            }
            IngestItem::SkippedFile {
                path_display,
                reason,
                had_previous,
            } => {
                if had_previous {
                    content_items += 1;
                }
                println!("Skipped {} during ingest: {}", path_display, reason);
                if let Some(observer) = &observer {
                    observer.file_skipped(&path_display, &reason);
                }
            }
            IngestItem::DocSections {
                node_path,
                sections,
                file_sha256,
            } => {
                if !sections.is_empty() {
                    let full_contents: Vec<String> = sections
                        .iter()
                        .map(|section| format!("{}\n\n{}", section.heading, section.text))
                        .collect();
                    let embed_inputs: Vec<String> = full_contents
                        .iter()
                        .map(|c| truncate_for_embedding(c))
                        .collect();
                    let outcome = embedder
                        .embed_batch_degradable(&embed_inputs)
                        .await
                        .map_err(|e| anyhow::anyhow!("failed to embed doc section batch: {}", e))?;
                    if outcome.embeddings.len() != full_contents.len() {
                        anyhow::bail!(
                            "embedder returned {} embeddings for {} doc sections",
                            outcome.embeddings.len(),
                            full_contents.len()
                        );
                    }
                    degraded.note(&outcome);
                    for ((section, summary), embedding) in sections
                        .into_iter()
                        .zip(full_contents)
                        .zip(outcome.embeddings)
                    {
                        // A provider refusal is a warning, not an abort.
                        let embedding = match embedding {
                            Some(embedding) => embedding,
                            None => continue,
                        };
                        let id = generation_scoped_id(
                            namespace,
                            generation,
                            &format!("{}#section:{}@{}", node_path, section.slug, section.sha256),
                        );
                        let mut metadata = serde_json::Map::new();
                        metadata.insert("domain".to_string(), serde_json::json!("docs"));
                        metadata.insert("heading".to_string(), serde_json::json!(section.heading));
                        metadata.insert(
                            "heading_level".to_string(),
                            serde_json::json!(section.level),
                        );
                        metadata.insert("slug".to_string(), serde_json::json!(section.slug));
                        metadata.insert(
                            "backticked".to_string(),
                            serde_json::json!(section.backticked),
                        );
                        metadata.insert("links".to_string(), serde_json::json!(section.links));
                        metadata.insert("file_sha256".to_string(), serde_json::json!(file_sha256));
                        metadata.insert(
                            "ingest_version".to_string(),
                            serde_json::json!(ingest_version),
                        );
                        metadata.insert("node_set".to_string(), serde_json::json!(node_set));
                        metadata.insert(
                            "contained_by".to_string(),
                            serde_json::json!([generation_scoped_id(
                                namespace, generation, &node_path
                            )]),
                        );
                        metadata.insert(
                            "source_identity".to_string(),
                            with_relative_path(&source_identity, &node_path),
                        );
                        metadata.insert(
                            "index_generation".to_string(),
                            serde_json::json!(generation),
                        );

                        let payload = MemoryPayload {
                            content: summary.clone(),
                            location: node_path.clone(),
                            location_lines: String::new(),
                            memory_type: "doc_section".to_string(),
                            metadata: serde_json::Value::Object(metadata),
                            user_id: "auto-ingestor".to_string(),
                            agent_name: Some("neurostrata-mcp-ingestor".to_string()),
                        };

                        vector_store
                            .upsert(namespace, &id, embedding, payload)
                            .await
                            .map_err(|e| {
                                anyhow::anyhow!("failed to store doc section {}: {}", id, e)
                            })?;
                    }
                }
            }
        }
    }

    Ok(ConsumerOutcome {
        content_items,
        staged_symbols,
        degraded,
    })
}

pub async fn ingest_directory(
    dir_path: &Path,
    schema: &ParserSchema,
    embedder: Arc<dyn Embedder>,
    vector_store: Arc<dyn VectorStore>,
    namespace: &str,
    observer: Option<Arc<dyn IngestObserver>>,
    node_set: &[String],
) -> anyhow::Result<()> {
    // Reserved destinations are refused before anything else happens: before
    // the node-set normalization, the directory probe, the previous-generation
    // read, any embedding work, or `begin_generation`. A namespace that belongs
    // to the internal staging machinery must never be treated as a project, and
    // the ordering is what keeps the refusal free.
    if crate::traits::is_reserved_ingest_destination(namespace) {
        anyhow::bail!(
            "ingest destination uses the reserved internal staging namespace '{}'; choose a different namespace",
            namespace
        );
    }

    // Normalize node_set: sorted + deduped (C2 deterministic contract).
    let node_set: Vec<String> = {
        let mut v = node_set.to_vec();
        v.sort();
        v.dedup();
        v
    };

    // Prove the root is a readable directory before anything is staged. A
    // missing or mistyped path used to clear the namespace, walk nothing -- the
    // walker's errors were skipped -- and report success over an emptied graph.
    std::fs::read_dir(dir_path).map_err(|e| {
        anyhow::anyhow!(
            "Refusing to ingest {:?} into namespace {}: it is not a readable directory, so nothing was staged: {}",
            dir_path,
            namespace,
            e
        )
    })?;

    // Two worktrees of one repository ingest into the same namespace. Their
    // nodes are kept apart by a generation, and the source identity records
    // which worktree wrote them (worktree_id = the canonical directory path;
    // repo/revision resolution is Task 2's `source::resolve`). The generation
    // pointer is keyed by this source, so publishing one worktree cannot
    // overwrite the other's active pointer.
    let worktree_id = source_key_for(dir_path);
    let source_key = worktree_id.clone();
    let ingest_start = std::time::Instant::now();
    match crate::registry::enrichment_start(&vector_store, Some(&embedder), namespace, &source_key)
        .await
    {
        Ok(status) => {
            if status.get("acquired").and_then(|v| v.as_bool()) == Some(false) {
                tracing::warn!("enrichment lock already held for namespace '{namespace}'");
            }
        }
        Err(e) => {
            tracing::warn!("enrichment registry update failed: {e}");
        }
    }
    // A process killed after publication but before relinking leaves the
    // pointer live with its declared edges missing. Complete that relink
    // before this ingest stages new rows; stores without a pending marker
    // return 0. Warn-first: a recovery failure must not strand later ingests.
    if let Err(e) = vector_store
        .recover_pending_relink(namespace, &source_key)
        .await
    {
        tracing::warn!(
            "could not finish a relink left pending by a crash in namespace '{namespace}': {e}"
        );
    }
    let source_identity = crate::context::model::SourceIdentity {
        worktree_id,
        ..Default::default()
    };
    let source_identity_json = serde_json::to_value(&source_identity)?;

    // Previous active generation for this source (for incremental carry-forward).
    let prev_gen = vector_store
        .active_generation(namespace, &source_key)
        .await
        .unwrap_or(0);

    // Build prev_file_map: node_path -> file_sha256 for files in prev gen.
    let mut prev_file_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut prev_stamps: Vec<String> = Vec::new();
    // Every directory, file and markdown node of the previous generation with
    // its kind, so a walk that finds exactly these nodes (same path, same kind)
    // and re-reads none of them can leave that generation active instead of
    // copying it. Paths alone are not enough: a file replaced by an empty
    // directory of the same name keeps its path.
    let mut prev_node_kinds: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // A failed summary leaves prev_stamps empty, which used to read as "no
    // offending stamp" and disable the version gate; the carry pass then
    // re-staged pre-fix rows over fresh ones and stamped them current. Treat a
    // summary failure as a gate trip: re-parsing is the safe answer.
    let mut summary_failed = false;
    if prev_gen > 0 {
        // Only this generation's rows, and only the fields read below: the
        // full `list` carried every row's text and cost ~100 MB per call on a
        // large namespace.
        match vector_store.generation_summary(namespace, prev_gen).await {
            Ok(rows) => {
                for r in rows {
                    let (_ns, gen, tail) = match crate::parser::ingest::split_scoped_id(&r.id) {
                        Some(x) => x,
                        None => continue,
                    };
                    if gen != prev_gen {
                        continue;
                    }
                    if matches!(r.memory_type.as_str(), "directory" | "file" | "markdown") {
                        prev_node_kinds.insert(r.location.clone(), r.memory_type.clone());
                    }
                    if let Some(h) = r.file_sha256 {
                        prev_file_map.insert(tail.to_string(), h);
                    }
                    prev_stamps.push(r.ingest_version);
                }
            }
            Err(e) => {
                summary_failed = true;
                tracing::warn!("vector_store.generation_summary failed while building prev_file_map for '{namespace}': {e}; forcing a full re-parse");
            }
        }
    }

    // Allocate a fresh generation. Nothing is deleted: the previous active
    // generation stays visible until this one publishes, so a failed build
    // leaves the graph exactly as it was.
    let generation = vector_store
        .begin_generation(namespace, &source_key)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Refusing to ingest: could not begin a staging generation for namespace {}: {}",
                namespace,
                e
            )
        })?;

    let ext_to_lang = build_ext_map(schema);
    let zero_vector = vec![0.0; embedder.dimensions()];
    let namespace_owned = namespace.to_string();
    let dir_path_owned = dir_path.to_path_buf();
    let schema_clone = schema.clone();

    // Version gate: a changed (or missing) ingest_version stamps a full re-parse.
    // A mixed previous generation (any row missing or differing) trips too.
    let current_version = ingest_version(&schema.fingerprint());
    let offending_stamp = prev_stamps
        .iter()
        .find(|s| {
            crate::parser::generation::version_gate_tripped(
                if s.is_empty() { None } else { Some(s.as_str()) },
                &current_version,
            )
        })
        .cloned();
    // An empty stamp list after a summary that claimed rows is just as
    // suspicious as an offending stamp: it is the shape a failed/short read
    // leaves behind, and carrying from it would publish pre-fix rows as
    // current. `summary_failed` is explicit; the emptiness check covers a
    // succeeded-but-empty read.
    let force_reparse =
        prev_gen > 0 && (offending_stamp.is_some() || summary_failed || prev_stamps.is_empty());
    if force_reparse {
        let warn_prev = offending_stamp
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("(none)");
        if let Some(obs) = &observer {
            obs.version_gate_tripped(warn_prev, &current_version);
        }
        tracing::warn!(prev = warn_prev, current = %current_version, "ingest version changed; re-parsing all files");
        // A reparse stages a second copy before it publishes. Refuse on a
        // host that cannot hold it rather than failing halfway through.
        if let Some(store) = crate::traits::VectorStore::as_any(vector_store.as_ref())
            .downcast_ref::<crate::store::ladybug::LadybugStore>()
        {
            let store_path = store.local_path();
            let store_bytes = std::fs::metadata(store_path).map(|m| m.len()).unwrap_or(0);
            let projected = crate::storage::preflight::projected_reindex_bytes(store_bytes);
            if let Err(e) = crate::storage::preflight::check_free_space(store_path, projected) {
                // Record the failed upgrade before returning: the registry is
                // the only trace the operator/doctor has of a refused reparse.
                if let Err(record) = crate::registry::enrichment_fail(
                    &vector_store,
                    Some(&embedder),
                    namespace,
                    "ingest",
                    &e.to_string(),
                )
                .await
                {
                    tracing::warn!("enrichment registry update failed: {record}");
                }
                return Err(e);
            }
        }
    }

    let (tx, rx) = mpsc::channel::<IngestItem>(256);

    // The blocking producer walks the filesystem and parses files on a
    // blocking thread, streaming owned results through the channel.
    let prev_file_map_clone = prev_file_map.clone();
    let observer_clone = observer.clone();
    let cached_root = crate::registry::cached_project_root(&vector_store, namespace)
        .await
        .unwrap_or(None);
    let producer = tokio::task::spawn_blocking(move || {
        run_producer(
            dir_path_owned,
            ext_to_lang,
            schema_clone,
            tx,
            namespace_owned,
            generation,
            prev_file_map_clone,
            force_reparse,
            observer_clone,
            cached_root,
        )
    });

    // The async consumer embeds and stores each item on the async runtime.
    let observer_clone = observer.clone();
    let consumer_result = run_consumer(
        rx,
        embedder.clone(),
        vector_store.clone(),
        namespace,
        observer_clone,
        zero_vector,
        generation,
        source_identity_json,
        &current_version,
        &node_set,
    )
    .await;

    let producer_result = producer.await;

    // Any failure aborts the staging generation: the previous active generation
    // stays reachable, and this one's half-built tree is discarded.
    let mut current_hashes = std::collections::HashMap::new();
    let mut current_paths = std::collections::HashSet::new();
    let mut current_kinds: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut dependency_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut staged_symbols: Vec<StagedSymbolInfo> = Vec::new();
    let mut degraded_embeddings = DegradedEmbeddings::default();
    let mut content_items = usize::MAX;
    let failure: Option<anyhow::Error> = match (consumer_result, producer_result) {
        (Ok(outcome), Ok(Ok((paths, hashes, deps, kinds)))) => {
            content_items = outcome.content_items;
            staged_symbols = outcome.staged_symbols;
            degraded_embeddings = outcome.degraded;
            current_paths = paths;
            current_hashes = hashes;
            dependency_map = deps;
            current_kinds = kinds;
            None
        }
        (Err(e), _) => Some(e),
        (Ok(_), Ok(Err(e))) => Some(e),
        (Ok(_), Err(e)) => Some(anyhow::anyhow!("ingest producer panicked: {}", e)),
    };
    if let Some(e) = failure {
        if let Err(abort) = vector_store
            .abort_generation(namespace, &source_key, generation)
            .await
        {
            eprintln!(
                "Failed to abort staging generation {} of namespace {}: {}",
                generation, namespace, abort
            );
        }
        if let Err(e) = crate::registry::enrichment_fail(
            &vector_store,
            Some(&embedder),
            namespace,
            "ingest",
            &e.to_string(),
        )
        .await
        {
            tracing::warn!("enrichment registry update failed: {e}");
        }
        return Err(e);
    }

    // The generation summary: the embeddings the provider refused. A refusal
    // is a warning, not an abort -- the generation continued without those
    // items, and the operator has to know which ones are missing.
    if let Some(summary) = degraded_embeddings.summary() {
        println!("{}", summary.trim_end());
    }

    // Nothing to publish: no file or section was read again, and the walk found
    // exactly the nodes the active generation already holds (same paths, each
    // of the same kind), so a new generation would be a copy of it. Keep the
    // active one, discard the staged directory nodes, and go on to the steps
    // that do not depend on it.
    //
    // The kinds matter because `content_items` does not count directory nodes:
    // a file replaced by an empty directory at the same path stages no content,
    // and comparing paths alone would call that unchanged and keep serving the
    // deleted file's node, symbols and sections.
    //
    // The `node_set` stamped on the nodes is deliberately not compared. A node
    // that is carried forward keeps the stamp it was first staged with, and
    // only directory nodes are staged again, so after a node-set change the
    // previous generation holds mixed stamps and a comparison would call every
    // later ingest a change. Making a node-set change visible here would mean
    // re-stamping carried nodes, which is a separate change.
    let unchanged =
        prev_gen > 0 && !force_reparse && content_items == 0 && current_kinds == prev_node_kinds;
    if unchanged {
        if let Err(abort) = vector_store
            .abort_generation(namespace, &source_key, generation)
            .await
        {
            tracing::warn!(
                "could not discard the empty staging generation {} of namespace {}: {}",
                generation,
                namespace,
                abort
            );
        }
        println!(
            "Namespace {} is unchanged since generation {}; kept it active",
            namespace, prev_gen
        );
    }

    // Carry forward unchanged nodes from the previous generation so the
    // new generation contains the full tree without re-embedding.
    // A forced re-parse (version trip) stages every current node itself, so
    // no previous row is carried in that mode.
    //
    // A failed carry is not a partial carry: unchanged files are skipped by
    // the producer, so this pass is their only route into the new generation.
    // Publishing past a failure here would flip the active pointer onto a tree
    // with holes in it, so the first carry error (summary, fetch or re-stage)
    // aborts the staging generation exactly like a staging failure above.
    if prev_gen > 0 && !force_reparse && !unchanged {
        let mut carried = 0usize;
        let mut carry_failure: Option<anyhow::Error> = None;
        let rows = match vector_store.generation_summary(namespace, prev_gen).await {
            Ok(rows) => rows,
            Err(e) => {
                carry_failure = Some(anyhow::anyhow!(
                    "carry-forward: could not list previous-generation rows of namespace {}: {}",
                    namespace,
                    e
                ));
                Vec::new()
            }
        };
        for r in rows {
            let id = r.id.clone();
            let (_ns, gen, tail) = match crate::parser::ingest::split_scoped_id(&id) {
                Some(x) => x,
                None => continue,
            };
            if gen != prev_gen {
                continue;
            }
            // A row whose path key is not one the current walk produces
            // belongs to an older key scheme (the same file ingested from a
            // subdirectory before a marker pinned keys to its root). The
            // producer re-stages that file under the current key, so carrying
            // the old id too would leave a duplicate, stale path here.
            let key_path = tail.split('#').next().unwrap_or(&tail);
            if !current_paths.contains(key_path) {
                continue;
            }
            // The row's `location` is the file it came from (stamped at
            // ingest time), which survives `#` in file names unlike
            // splitting the symbol id.
            let file_path = r.location.clone();
            let hash = r.file_sha256.clone();
            // Carry a previous row only when its file is still present with
            // the same bytes, or (for no-hash rows) when the path is still
            // visited. Deleted files and stale symbols drop out.
            let carry = if current_paths.contains(&file_path) {
                match &hash {
                    Some(h) => current_hashes.get(&file_path) == Some(h),
                    None => true,
                }
            } else {
                false
            };
            if !carry {
                continue;
            }
            // A carried node and the node staged for this generation get the
            // same scoped id when they share a path, so carrying a row whose
            // kind no longer matches the path would overwrite the staged node
            // with the old one: a directory turned file would be put back as a
            // directory, and the next ingest would repeat it. Directory rows
            // carry no hash, so the hash test above cannot catch this.
            let row_kind = r.memory_type.as_str();
            let current_kind = current_kinds.get(&file_path).map(String::as_str);
            let is_node_row = matches!(row_kind, "directory" | "file" | "markdown");
            if is_node_row && current_kind != Some(row_kind) {
                continue;
            }
            // Symbols and sections belong to a file; none of them survive the
            // path becoming a directory.
            if !is_node_row && current_kind == Some("directory") {
                continue;
            }
            match vector_store.get(namespace, &id).await {
                Ok(Some((vec, mut payload))) => {
                    let old_gen_str = format!(
                        "{}{}{}{}",
                        namespace,
                        crate::parser::generation::NAMESPACE_SEPARATOR,
                        prev_gen,
                        crate::parser::generation::NAMESPACE_SEPARATOR
                    );
                    let new_gen_str = format!(
                        "{}{}{}{}",
                        namespace,
                        crate::parser::generation::NAMESPACE_SEPARATOR,
                        generation,
                        crate::parser::generation::NAMESPACE_SEPARATOR
                    );
                    payload.metadata.as_object_mut().map(|o| {
                        o.insert(
                            "index_generation".to_string(),
                            serde_json::json!(generation),
                        );
                        o.insert(
                            "ingest_version".to_string(),
                            serde_json::json!(current_version),
                        );
                        // Fix contained_by generation prefixes (F2).
                        if let Some(serde_json::Value::Array(arr)) = o.get("contained_by").cloned()
                        {
                            let updated: Vec<serde_json::Value> = arr
                                .iter()
                                .filter_map(|v| v.as_str())
                                .map(|s| {
                                    serde_json::Value::String(s.replace(&old_gen_str, &new_gen_str))
                                })
                                .collect();
                            o.insert(
                                "contained_by".to_string(),
                                serde_json::Value::Array(updated),
                            );
                        }
                        // Fix depends_on and calls_resolved generation prefixes (F2):
                        // For `code_ast` symbols, keep only same-file targets here and strip
                        // stale cross-file targets (`cross_file_callees` / `cross_file_callers`);
                        // `resolve_cross_file_calls_for_generation` rebuilds cross-file edges
                        // from the current generation's `staged_symbols` so removed or renamed
                        // callees never leave dangling dependencies.
                        // For `file` rows, `depends_on` is rebuilt from `imports` filtered by
                        // `current_paths` below so deleted target files are dropped.
                        if row_kind == "code_ast" {
                            let same_file_prefix = format!("{new_gen_str}{file_path}#");
                            if let Some(serde_json::Value::Array(arr)) =
                                o.get("depends_on").cloned()
                            {
                                let updated: Vec<serde_json::Value> = arr
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .map(|s| s.replace(&old_gen_str, &new_gen_str))
                                    .filter(|s| s.starts_with(&same_file_prefix))
                                    .map(serde_json::Value::String)
                                    .collect();
                                if updated.is_empty() {
                                    o.remove("depends_on");
                                } else {
                                    o.insert(
                                        "depends_on".to_string(),
                                        serde_json::Value::Array(updated),
                                    );
                                }
                            }
                            if let Some(serde_json::Value::Array(arr)) =
                                o.get("calls_resolved").cloned()
                            {
                                let updated: Vec<serde_json::Value> = arr
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .map(|s| s.replace(&old_gen_str, &new_gen_str))
                                    .filter(|s| s.starts_with(&same_file_prefix))
                                    .map(serde_json::Value::String)
                                    .collect();
                                if updated.is_empty() {
                                    o.remove("calls_resolved");
                                } else {
                                    o.insert(
                                        "calls_resolved".to_string(),
                                        serde_json::Value::Array(updated),
                                    );
                                }
                            }
                            o.remove("cross_file_callees");
                            o.remove("cross_file_callers");
                        } else if row_kind == "file" {
                            if let Some(serde_json::Value::Array(arr)) =
                                o.get("depends_on").cloned()
                            {
                                let updated: Vec<serde_json::Value> = arr
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .filter(|s| {
                                        s.strip_prefix(&old_gen_str)
                                            .map(|rel| current_paths.contains(rel))
                                            .unwrap_or(false)
                                    })
                                    .map(|s| {
                                        serde_json::Value::String(
                                            s.replace(&old_gen_str, &new_gen_str),
                                        )
                                    })
                                    .collect();
                                if updated.is_empty() {
                                    o.remove("depends_on");
                                } else {
                                    o.insert(
                                        "depends_on".to_string(),
                                        serde_json::Value::Array(updated),
                                    );
                                }
                            }
                            if let Some(serde_json::Value::Array(arr)) = o.get("imports").cloned() {
                                let updated: Vec<serde_json::Value> = arr
                                    .iter()
                                    .filter_map(|v| v.as_str())
                                    .filter(|p| current_paths.contains(*p))
                                    .map(|p| serde_json::Value::String(p.to_string()))
                                    .collect();
                                if updated.is_empty() {
                                    o.remove("imports");
                                } else {
                                    o.insert(
                                        "imports".to_string(),
                                        serde_json::Value::Array(updated),
                                    );
                                }
                            }
                        } else if let Some(serde_json::Value::Array(arr)) =
                            o.get("depends_on").cloned()
                        {
                            let updated: Vec<serde_json::Value> = arr
                                .iter()
                                .filter_map(|v| v.as_str())
                                .map(|s| {
                                    serde_json::Value::String(s.replace(&old_gen_str, &new_gen_str))
                                })
                                .collect();
                            o.insert("depends_on".to_string(), serde_json::Value::Array(updated));
                        }
                    });
                    let new_id =
                        crate::parser::ingest::generation_scoped_id(namespace, generation, &tail);
                    if row_kind == "code_ast" {
                        let sym_name = payload
                            .metadata
                            .get("symbol")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let sym_kind = payload
                            .metadata
                            .get("symbol_kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let sym_calls: Vec<String> = payload
                            .metadata
                            .get("calls")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !sym_name.is_empty() {
                            staged_symbols.push(StagedSymbolInfo {
                                id: new_id.clone(),
                                file_path: file_path.clone(),
                                name: sym_name,
                                kind: sym_kind,
                                calls: sym_calls,
                            });
                        }
                    } else if row_kind == "file" && !dependency_map.contains_key(&file_path) {
                        if let Some(arr) =
                            payload.metadata.get("imports").and_then(|v| v.as_array())
                        {
                            let targets: Vec<String> = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .filter(|p| current_paths.contains(p))
                                .collect();
                            if !targets.is_empty() {
                                dependency_map.insert(file_path.clone(), targets);
                            }
                        }
                    }
                    if let Err(e) = vector_store.upsert(namespace, &new_id, vec, payload).await {
                        carry_failure = Some(anyhow::anyhow!(
                            "carry-forward: failed to re-stage node {} into generation {}: {}",
                            new_id,
                            generation,
                            e
                        ));
                        break;
                    }
                    carried += 1;
                }
                Ok(None) => {}
                Err(e) => {
                    carry_failure = Some(anyhow::anyhow!(
                        "carry-forward: vector_store.get failed for node {id}: {e}"
                    ));
                    break;
                }
            }
        }
        if let Some(e) = carry_failure {
            if let Err(abort) = vector_store
                .abort_generation(namespace, &source_key, generation)
                .await
            {
                eprintln!(
                    "Failed to abort staging generation {} of namespace {}: {}",
                    generation, namespace, abort
                );
            }
            // Record carry-forward abort in enrichment registry (bead neurostrata-sij).
            if let Err(e) = crate::registry::enrichment_fail(
                &vector_store,
                Some(&embedder),
                namespace,
                "ingest",
                &e.to_string(),
            )
            .await
            {
                tracing::warn!("enrichment registry update failed: {e}");
            }
            return Err(e);
        }
        println!("Carried forward {} unchanged nodes", carried);
    }

    // Stamp DEPENDS_ON targets onto file nodes (best-effort, warn-first).
    for (src_path, targets) in dependency_map.iter().filter(|_| !unchanged) {
        let src_id = crate::parser::ingest::generation_scoped_id(namespace, generation, src_path);
        match vector_store.get(namespace, &src_id).await {
            Ok(Some((vec, mut payload))) => {
                let target_ids: Vec<String> = targets
                    .iter()
                    .map(|t| crate::parser::ingest::generation_scoped_id(namespace, generation, t))
                    .collect();
                if let Some(obj) = payload.metadata.as_object_mut() {
                    obj.insert("depends_on".to_string(), serde_json::json!(target_ids));
                    obj.insert("imports".to_string(), serde_json::json!(targets));
                }
                if let Err(e) = vector_store.upsert(namespace, &src_id, vec, payload).await {
                    tracing::warn!(
                        "depends_on stamping: failed to upsert file node {} for namespace {}: {}",
                        src_id,
                        namespace,
                        e
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("vector_store.get failed for depends_on source '{src_id}': {e}");
            }
        }
    }

    // Tier-1 pre-publish workspace cross-file call graph resolution (`neurostrata-rbde`).
    if !unchanged && !staged_symbols.is_empty() {
        resolve_cross_file_calls_for_generation(
            &vector_store,
            namespace,
            &staged_symbols,
            &dependency_map,
        )
        .await;
    }

    // Capture the rollback target before publication flips the pointer. If
    // `relink_edges` then fails, the freshly published generation has no rule
    // edges and must not stay live: the previous generation is restored.
    //
    // `None` covers both a first ingest and an unchanged ingest, which differ
    // after a relink failure: an unchanged ingest never moves the pointer (its
    // staging generation was already discarded), while a first ingest
    // published generation 1 and has no earlier generation to restore.
    let previous_generation = if unchanged {
        None
    } else {
        match vector_store.active_generation(namespace, &source_key).await {
            Ok(previous) if previous > 0 => Some(previous),
            Ok(_) => None,
            Err(e) => {
                // Without the target there is nothing safe to roll back to,
                // so publishing would be an unrecoverable risk. Clean up the
                // staging generation exactly like a publish failure.
                if let Err(abort) = vector_store
                    .abort_generation(namespace, &source_key, generation)
                    .await
                {
                    eprintln!(
                        "Failed to abort staging generation {} of namespace {}: {}",
                        generation, namespace, abort
                    );
                }
                // `enrichment_start` already marked this run as running. Fail
                // it before returning, or the registry reports a stale active
                // ingest until the lock expires (review finding on the read
                // error branch).
                if let Err(fail) = crate::registry::enrichment_fail(
                    &vector_store,
                    Some(&embedder),
                    namespace,
                    "ingest",
                    &e.to_string(),
                )
                .await
                {
                    tracing::warn!("enrichment registry update failed: {fail}");
                }
                return Err(anyhow::anyhow!(
                    "stored the nodes of namespace {} but could not read its previous active generation before publishing generation {}: {}",
                    namespace,
                    generation,
                    e
                ));
            }
        }
    };

    // Atomically flip the active pointer onto this generation. Only now are its
    // nodes reachable; the previous generation remains as history.
    if let Err(e) = if unchanged {
        Ok(())
    } else {
        vector_store
            .publish_generation(namespace, &source_key, generation)
            .await
    } {
        // Publication failed: abort the staged generation so its rows do not
        // leak as orphaned staging (same cleanup as the failure branch above).
        if let Err(abort) = vector_store
            .abort_generation(namespace, &source_key, generation)
            .await
        {
            eprintln!(
                "Failed to abort staging generation {} of namespace {}: {}",
                generation, namespace, abort
            );
        }
        return Err(anyhow::anyhow!(
            "stored the nodes of namespace {} but could not make generation {} active: {}",
            namespace,
            generation,
            e
        ));
    }

    // A rule is not rewritten just because its file came back, so replay what
    // the memories still declare now that the nodes they point at are the
    // active generation.
    //
    // A failure here fails the ingest rather than warning about it: reporting
    // success would hand back a graph whose rules reach none of their code.
    // The new generation is already live at this point, so failing loudly is
    // not enough: roll the pointer back to the captured generation and abort
    // the generation whose edges could not be rebuilt.
    let linked = match vector_store.relink_edges(namespace, &source_key).await {
        Ok(linked) => linked,
        Err(relink_error) => {
            if unchanged {
                // The pointer never moved and the unchanged active generation
                // still serves the tree. The staging generation was discarded
                // when `unchanged` was detected; repeat the abort so a failed
                // first discard cannot leak its rows or its staging marker.
                let cleanup = vector_store
                    .abort_generation(namespace, &source_key, generation)
                    .await;
                let cleanup_note = match cleanup {
                    Ok(()) => format!("discarded staging generation {generation}"),
                    Err(e) => format!("could not discard staging generation {generation}: {e}"),
                };
                return Err(anyhow::anyhow!(
                    "namespace {} is unchanged since active generation {prev_gen} but could not relink the edges its memories declare, so rules do not reach their code; the unchanged active generation {prev_gen} is untouched and its staging generation was never published; cleanup: {}; relink error: {}",
                    namespace,
                    cleanup_note,
                    relink_error
                ));
            }
            match previous_generation {
                Some(previous_generation) => {
                    // Restore only while this ingest's published generation is
                    // still active. A later ingest may have published over it;
                    // its tree is the truth and a stale rollback target must
                    // never replace it.
                    match vector_store
                        .publish_generation_if_active(
                            namespace,
                            &source_key,
                            generation,
                            previous_generation,
                        )
                        .await
                    {
                        Ok(true) => {
                            // The pointer is back on the previous generation,
                            // so this abort only deletes the failed rows.
                            let abort = vector_store
                                .abort_generation(namespace, &source_key, generation)
                                .await;
                            let abort_note = match abort {
                                Ok(()) => format!("aborted generation {generation}"),
                                Err(e) => format!("could not abort generation {generation}: {e}"),
                            };
                            return Err(anyhow::anyhow!(
                                "stored the nodes of namespace {} but could not relink the edges its memories declare, so rules do not reach their code; rollback restored generation {}; {}; relink error: {}",
                                namespace,
                                previous_generation,
                                abort_note,
                                relink_error
                            ));
                        }
                        Ok(false) => {
                            // Another ingest owns the pointer now. Aborting
                            // would clear a generation that is not ours, so
                            // leave both the pointer and its tree intact.
                            return Err(anyhow::anyhow!(
                                "stored the nodes of namespace {} but could not relink the edges its memories declare, so rules do not reach their code; a newer generation is active so stale rollback target {} was left untouched and generation {} was not aborted; relink error: {}",
                                namespace,
                                previous_generation,
                                generation,
                                relink_error
                            ));
                        }
                        Err(restore_error) => {
                            // Restore failed, so the pointer may still name
                            // generation N. Aborting N clears the active
                            // pointer and leaves every reader with no tree.
                            return Err(anyhow::anyhow!(
                                "stored the nodes of namespace {} but could not relink the edges its memories declare, so rules do not reach their code; could not restore generation {}: {}; generation {} was not aborted so its active pointer stays intact; relink error: {}",
                                namespace,
                                previous_generation,
                                restore_error,
                                generation,
                                relink_error
                            ));
                        }
                    }
                }
                None => {
                    // No prior generation (a first ingest, or a namespace an
                    // earlier failed first ingest emptied): there is nothing
                    // to restore. Abort the generation just published, which
                    // also clears the pointer, so the documented post-failure
                    // state is "no active generation (0) and no nodes from
                    // the failed generation" -- never a live tree without
                    // edges.
                    let abort = vector_store
                        .abort_generation(namespace, &source_key, generation)
                        .await;
                    let abort_note = match abort {
                        Ok(()) => format!(
                            "aborted generation {generation}; no previous generation existed, so the namespace has no active generation (0) and none of generation {generation}'s nodes remain"
                        ),
                        Err(e) => format!(
                            "could not abort generation {generation}: {e}; the namespace may still serve generation {generation} without its rule edges"
                        ),
                    };
                    return Err(anyhow::anyhow!(
                        "stored the nodes of namespace {} but could not relink the edges its memories declare, so rules do not reach their code; rollback with no prior generation to restore: {}; relink error: {}",
                        namespace,
                        abort_note,
                        relink_error
                    ));
                }
            }
        }
    };
    println!(
        "Relinked {} declared edges in namespace {}",
        linked, namespace
    );
    if let Some(observer) = &observer {
        observer.relinked(linked);
    }

    // Cache project_root into the registry row (best-effort) for hook hygiene.
    crate::registry::cache_project_root_unless_marked(
        &vector_store,
        Some(&embedder),
        namespace,
        &dir_path.to_string_lossy(),
    )
    .await;

    // Watermark + stage telemetry on the namespace registry row (F12).
    // `current_version` was computed at the version gate above (AH-40: no re-shadow).
    // Compute deterministic source_digest over sorted (path, hash) pairs.
    let mut digest_entries: Vec<(String, String)> = current_hashes
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    digest_entries.sort_by(|a, b| a.0.cmp(&b.0));
    let digest_str = digest_entries
        .iter()
        .map(|(p, h)| format!("{}={}", p, h))
        .collect::<Vec<_>>()
        .join(";");
    let source_digest = sha256_hex(digest_str.as_bytes());
    let watermark = serde_json::json!({
        "last_enriched_at": chrono::Utc::now().to_rfc3339(),
        "enriched_count": current_paths.len(),
        "extractor_fingerprint": current_version,
        "source_digest": format!("sha256:{}", source_digest),
    });
    let telemetry = serde_json::json!({
        "stages": [{
            "name": "ingest",
            "count": current_paths.len(),
            "failures": 0,
            "latency_ms": ingest_start.elapsed().as_millis() as u64,
            "fingerprint": current_version,
        }]
    });
    if let Err(e) = crate::registry::enrichment_commit(
        &vector_store,
        Some(&embedder),
        namespace,
        watermark,
        telemetry,
    )
    .await
    {
        tracing::warn!("enrichment_commit failed for '{namespace}': {e}");
    }

    crate::telemetry::insights::sample_insights_after_ingest(
        &vector_store,
        namespace,
        Some(generation),
    )
    .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_from(json: &str) -> HashMap<String, String> {
        build_ext_map(&ParserSchema::load(json).expect("schema parses"))
    }

    /// The shipped src/schema.json declares extensions without a dot. The lookup
    /// used to prepend one, so nothing ever matched and no symbols were extracted.
    #[test]
    fn bare_schema_extension_matches_a_file_extension() {
        let map = map_from(r#"{"languages":{"rust":{"extensions":["rs"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("rs")), Some(&"rust".to_string()));
    }

    #[test]
    fn dotted_schema_extension_matches_too() {
        let map = map_from(r#"{"languages":{"rust":{"extensions":[".rs"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("rs")), Some(&"rust".to_string()));
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let map = map_from(r#"{"languages":{"python":{"extensions":["py"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("PY")), Some(&"python".to_string()));
    }

    /// The shipped schema once declared a language key ("javascript") that
    /// get_language() had no arm for, so those files silently produced nothing.
    #[test]
    fn every_language_in_the_shipped_schema_has_a_grammar() {
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        for lang in schema.languages.keys() {
            assert!(
                get_language(lang).is_some(),
                "schema.json declares language '{}' but get_language() has no arm for it",
                lang
            );
        }
    }

    /// And the reverse gap: a grammar nothing can reach, because no extension maps to it.
    #[test]
    fn every_shipped_extension_resolves_to_a_grammar() {
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let map = build_ext_map(&schema);
        for (ext, lang) in &map {
            assert!(
                get_language(lang).is_some(),
                "extension '{}' maps to '{}', which has no grammar",
                ext,
                lang
            );
        }
        assert_eq!(map.get(&normalize_ext("rs")), Some(&"rust".to_string()));
        assert_eq!(map.get(&normalize_ext("tsx")), Some(&"tsx".to_string()));
    }

    /// A project root that is absolute on the platform running the test.
    ///
    /// A literal `C:\dev\projects\neurostrata` is absolute only on Windows. On
    /// Linux and macOS it has no leading separator, so `node_id_for` would take
    /// its `is_relative()` branch and return the whole string instead of
    /// stripping the root.
    fn absolute_root() -> std::path::PathBuf {
        if cfg!(windows) {
            std::path::PathBuf::from(r"C:\dev\projects\neurostrata")
        } else {
            std::path::PathBuf::from("/dev/projects/neurostrata")
        }
    }

    /// Node ids double as edge targets, so a path an agent writes
    /// ("src/lib.rs") has to land on the same string the walker produced.
    #[test]
    fn a_node_id_is_relative_to_the_directory_being_ingested() {
        let root = absolute_root();

        assert_eq!(
            node_id_for(&root, &root.join("src").join("store").join("ladybug.rs")),
            "src/store/ladybug.rs",
            "an absolute walk must produce the same id as a relative one"
        );
        assert_eq!(
            node_id_for(Path::new("."), Path::new("./src/store/ladybug.rs")),
            "src/store/ladybug.rs"
        );
    }

    #[test]
    fn ingesting_a_subdirectory_keeps_the_documented_repo_relative_form() {
        // CLI-readme.md documents `neurostrata-mcp ingest ./src my-rust-project`,
        // and rules name files as "src/lib.rs". Stripping the ingest root would
        // leave "lib.rs" and quietly unlink every rule that names the file.
        assert_eq!(
            node_id_for(Path::new("./src"), Path::new("./src/store/ladybug.rs")),
            "src/store/ladybug.rs"
        );
        assert_eq!(
            node_id_for(Path::new("src"), Path::new("src/lib.rs")),
            "src/lib.rs"
        );
    }

    /// The daemon inherits the working directory of whatever launched it, and a
    /// parent of the project used to win over the ingest root.
    #[test]
    fn a_node_id_does_not_depend_on_the_working_directory() {
        let root = std::env::current_dir()
            .expect("working directory")
            .join("sub");
        assert_eq!(node_id_for(&root, &root.join("a.rs")), "a.rs");
    }

    #[test]
    fn the_ingest_root_is_named_rather_than_left_empty() {
        let root = absolute_root();
        assert_eq!(node_id_for(&root, &root), "neurostrata");
    }

    #[test]
    fn a_path_outside_everything_keeps_its_absolute_shape() {
        // Neither the working directory nor the ingest root is a prefix, so
        // there is nothing to strip. A usable absolute id beats a panic.
        assert_eq!(
            node_id_for(
                Path::new(r"C:\dev\projects\other"),
                Path::new(r"D:\elsewhere\a.rs")
            ),
            "D:/elsewhere/a.rs"
        );
    }

    #[test]
    fn node_paths_normalise_to_one_form() {
        assert_eq!(normalize_node_path(r".\src\lib.rs"), "src/lib.rs");
        assert_eq!(normalize_node_path("./src/lib.rs"), "src/lib.rs");
        assert_eq!(normalize_node_path("src/lib.rs"), "src/lib.rs");
        assert_eq!(normalize_node_path("src/"), "src");
    }

    /// The whole point: two projects both have a `src`, the Memory table has a
    /// single-column primary key, and the database is shared. Without the
    /// namespace in the id the second project ingested takes the first's node.
    /// doctor reports migration state from this, so a wrong answer either hides
    /// a namespace that is still collidable or nags about one that is not.
    #[test]
    fn an_id_is_qualified_only_by_its_own_namespace() {
        assert!(is_qualified("NeuroStrata", "NeuroStrata::src/main.rs"));
        // Written before ids carried a namespace.
        assert!(!is_qualified("NeuroStrata", "src/main.rs"));
        // Another project's node, which this namespace must not count as its own.
        assert!(!is_qualified("NeuroStrata", "Other::src/main.rs"));
        // A namespace that merely starts the same way is a different namespace.
        assert!(!is_qualified("Neuro", "NeuroStrata::src/main.rs"));
    }

    #[test]
    fn two_projects_sharing_a_path_get_different_ids() {
        assert_ne!(
            qualified_id("ProjectA", "src/main.rs"),
            qualified_id("ProjectB", "src/main.rs")
        );
        assert_eq!(qualified_id("NeuroStrata", "src"), "NeuroStrata::src");
    }

    /// A rule names a file the way a human writes it. The id it has to reach is
    /// qualified. If that bridge breaks, every GOVERNS edge silently stops
    /// forming and the graph looks fine while meaning nothing.
    #[test]
    fn a_bare_declaration_still_reaches_its_qualified_id() {
        let known = vec![
            qualified_id("NeuroStrata", "src/store/ladybug.rs"),
            qualified_id("NeuroStrata", "src/daemon.rs"),
        ];
        let index = crate::store::ladybug::KnownIds::new(known.iter().map(String::as_str));
        assert_eq!(
            index.resolve("src/store/ladybug.rs").as_deref(),
            Some("NeuroStrata::src/store/ladybug.rs")
        );
    }

    /// A suffix must not match a longer filename that merely ends the same way.
    #[test]
    fn a_suffix_does_not_match_a_longer_name() {
        let known = vec![qualified_id("NeuroStrata", "src/mylib.rs")];
        let index = crate::store::ladybug::KnownIds::new(known.iter().map(String::as_str));
        assert_eq!(index.resolve("lib.rs"), None);
    }

    /// The same path under two namespaces must resolve to neither. A wrong
    /// GOVERNS edge points a rule at another project's file, which is worse
    /// than the rule having no edge at all.
    #[test]
    fn a_path_held_by_two_namespaces_is_ambiguous() {
        let known = vec![
            qualified_id("ProjectA", "src/main.rs"),
            qualified_id("ProjectB", "src/main.rs"),
        ];
        let index = crate::store::ladybug::KnownIds::new(known.iter().map(String::as_str));
        assert_eq!(index.resolve("src/main.rs"), None);
    }

    #[test]
    fn symbol_ids_are_stable_and_distinguish_repeated_names() {
        let main = sha256_hex("fn main() {}".as_bytes());
        let other_body = sha256_hex("fn main() { loop {} }".as_bytes());
        let a = symbol_id("src/lib.rs", "functions", "main", &main);
        assert_eq!(a, symbol_id("src/lib.rs", "functions", "main", &main));
        assert_ne!(a, symbol_id("src/lib.rs", "functions", "main", &other_body));
        assert_ne!(a, symbol_id("src/other.rs", "functions", "main", &main));
        assert_ne!(a, symbol_id("src/lib.rs", "structs", "main", &main));
    }

    #[test]
    fn short_bodies_are_stored_verbatim_and_long_ones_are_cut() {
        let short = "fn main() {}";
        assert_eq!(truncate_for_embedding(short), short);

        let long = "x".repeat(MAX_SYMBOL_CHARS + 500);
        let cut = truncate_for_embedding(&long);
        assert!(cut.starts_with(&"x".repeat(MAX_SYMBOL_CHARS)));
        assert!(cut.contains("truncated"));
        assert!(cut.chars().filter(|c| *c == 'x').count() == MAX_SYMBOL_CHARS);
    }

    #[test]
    fn extract_called_identifiers_finds_function_calls_and_skips_keywords_and_self() {
        // Test that extract_called_identifiers finds function calls and skips keywords and self
        let body = "fn foo() { bar() }";
        let self_name = "foo";
        let result = extract_called_identifiers(body, self_name);
        // Should find "bar" as a function call, skip "foo" (self), and skip keywords
        assert!(result.iter().any(|s| s == "bar"));
        assert!(!result.iter().any(|s| s == "foo"));
        assert!(!result.iter().any(|s| s == "if"));
        assert!(!result.iter().any(|s| s == "fn"));
    }

    #[test]
    fn unknown_extension_has_no_language() {
        let map = map_from(r#"{"languages":{"rust":{"extensions":["rs"],"queries":{}}}}"#);
        assert_eq!(map.get(&normalize_ext("toml")), None);
    }

    struct ZeroEmbedder;

    #[async_trait::async_trait]
    impl Embedder for ZeroEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    /// Counts embeddings so a test can prove a refused ingest never reached
    /// the embedding stage.
    struct CountingEmbedder {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Embedder for CountingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![0.0; 4])
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    struct FailingEmbedder;

    #[async_trait::async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("synthetic embed failure")
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    /// A namespace that belongs to the internal staging machinery is refused
    /// as the very first thing ingest does: no walk, no embedding, no
    /// generation, and no change to rows already in it (issue #417).
    #[tokio::test]
    async fn reserved_ingest_destinations_are_rejected_before_staging_or_embedding() {
        let tmp = std::env::temp_dir().join(format!("ns-reserved-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("planted.rs"), "fn planted() {}").expect("write planted file");

        let db = std::env::temp_dir().join(format!("ns-reserved-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let reserved = "__neurostrata_ingest_stage__probe";
        store.init(reserved).await.expect("create the schema");

        let seed_id = qualified_id(reserved, "seeded.rs");
        store
            .upsert(
                reserved,
                &seed_id,
                vec![0.0; 4],
                MemoryPayload {
                    content: "Path: seeded.rs".to_string(),
                    user_id: "auto-ingestor".to_string(),
                    memory_type: "file".to_string(),
                    agent_name: None,
                    location: "seeded.rs".to_string(),
                    location_lines: String::new(),
                    metadata: serde_json::json!({}),
                },
            )
            .await
            .expect("seed a row in the reserved namespace");

        let embedder = Arc::new(CountingEmbedder {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let error = ingest_directory(
            &tmp,
            &schema,
            embedder.clone(),
            store.clone(),
            reserved,
            None,
            &[],
        )
        .await
        .expect_err("a reserved destination is refused");

        assert!(
            error
                .to_string()
                .contains("ingest destination uses the reserved internal staging namespace"),
            "the refusal names the reserved staging namespace: {error}"
        );
        assert_eq!(
            embedder.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "refusal happens before any embedding work"
        );
        assert_eq!(
            store
                .active_generation(reserved, &source_key_for(&tmp))
                .await
                .expect("read"),
            0,
            "refusal happens before a staging generation begins"
        );
        assert!(
            store
                .get(reserved, &seed_id)
                .await
                .expect("read seeded row")
                .is_some(),
            "the refusal leaves rows already in the namespace untouched"
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn a_missing_root_is_refused_before_anything_is_cleared() {
        let db = std::env::temp_dir().join(format!("ns-missing-root-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");

        let node = qualified_id("probe", "src/lib.rs");
        let previous = MemoryPayload {
            content: "Path: src/lib.rs".to_string(),
            user_id: "auto-ingestor".to_string(),
            memory_type: "file".to_string(),
            agent_name: None,
            location: "src/lib.rs".to_string(),
            location_lines: String::new(),
            metadata: serde_json::json!({}),
        };
        store
            .upsert("probe", &node, vec![0.0; 4], previous)
            .await
            .expect("seed a previous ingest");

        let missing = std::env::temp_dir().join(format!("ns-no-such-dir-{}", uuid::Uuid::new_v4()));
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let outcome = ingest_directory(
            &missing,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await;

        assert!(
            outcome.is_err(),
            "a root that does not exist is an error, not an empty ingest"
        );
        assert!(
            store
                .get("probe", &node)
                .await
                .expect("read back")
                .is_some(),
            "and the previous ingest is still there"
        );
    }

    /// A file replaced by an empty directory at the same path re-reads no
    /// file and visits the same path, but it is a change: the old file and its
    /// symbols must not stay active.
    #[tokio::test]
    async fn a_file_replaced_by_a_directory_publishes_a_new_generation() {
        let tmp = std::env::temp_dir().join(format!("ns-type-change-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("thing.rs"), "fn main() {}").expect("write test file");

        let db = std::env::temp_dir().join(format!("ns-type-change-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let ingest = || {
            ingest_directory(
                &tmp,
                &schema,
                Arc::new(ZeroEmbedder),
                store.clone(),
                "test-ns",
                None,
                &[],
            )
        };

        ingest().await.expect("first ingest");
        let key = source_key_for(&tmp);
        assert_eq!(
            store
                .active_generation("test-ns", &key)
                .await
                .expect("read"),
            1
        );

        std::fs::remove_file(tmp.join("thing.rs")).expect("remove the file");
        std::fs::create_dir(tmp.join("thing.rs")).expect("create a directory at the same path");
        ingest().await.expect("second ingest");
        assert_eq!(
            store
                .active_generation("test-ns", &key)
                .await
                .expect("read"),
            2,
            "a path that changed from a file to a directory publishes a new generation"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&db);
    }

    /// In a linked worktree the marker folder is spelled in the worktree,
    /// while `marker_dir()` maps it back to the main checkout. Keys must be
    /// stripped against the walk's own spelling or they become absolute.
    #[test]
    fn key_root_for_linked_worktree_uses_the_worktree_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        let src_main = repo.join("src");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::write(repo.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        // A linked worktree's common dir counts as a checkout only with HEAD
        // (paths::main_checkout_of_common); without it no marker is found.
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(src_main.join(".NeuroStrata")).unwrap();
        std::fs::write(
            src_main.join(".NeuroStrata/config.json"),
            r#"{"version":1,"namespace":"test-ns"}"#,
        )
        .unwrap();
        let wt = base.join("wt");
        let src_wt = wt.join("src");
        let nested_wt = src_wt.join("lib");
        std::fs::create_dir_all(&nested_wt).unwrap();
        std::fs::create_dir_all(src_main.join("lib")).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();

        let pinned_wt = dunce::canonicalize(&src_wt).unwrap();
        assert_eq!(
            key_root_for_ingest(&nested_wt, "test-ns", Some(&pinned_wt.to_string_lossy())),
            src_wt,
            "a nested ingest must key from the marker folder in the worktree's own spelling"
        );
        // A cached root that disagrees with the marker must not move the keys
        // onto a root path lookups do not use.
        assert_eq!(
            key_root_for_ingest(&nested_wt, "test-ns", Some(&repo.to_string_lossy())),
            nested_wt,
            "a marker that is not the cached root must not key from the marker"
        );
    }

    /// A marker pins the repo root; ingesting only its `src/` subfolder must
    /// still key nodes from the pinned root, or absolute lookups that start
    /// from the cached root can never reach them.
    #[tokio::test]
    async fn subdirectory_ingest_keys_nodes_from_the_marked_root() {
        let tmp = std::env::temp_dir().join(format!("ns-subdir-key-{}", uuid::Uuid::new_v4()));
        let repo = tmp.join("repo");
        let src = repo.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".NeuroStrata")).unwrap();
        std::fs::write(
            repo.join(".NeuroStrata").join("config.json"),
            r#"{"version":1,"namespace":"test-ns"}"#,
        )
        .unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn x() {}\n").unwrap();

        let db = tmp.join("db");
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.unwrap();
        let pinned = dunce::canonicalize(&repo).unwrap();
        crate::registry::cache_project_root(
            &store,
            Some(&(Arc::new(ZeroEmbedder) as Arc<dyn Embedder>)),
            "test-ns",
            &pinned.to_string_lossy(),
        )
        .await;
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        ingest_directory(
            &dunce::canonicalize(&src).unwrap(),
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            None,
            &[],
        )
        .await
        .expect("ingest succeeds");

        let locations: Vec<String> = store
            .list("test-ns", None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.payload.location)
            .collect();
        assert!(
            locations.iter().any(|p| p == "src/lib.rs"),
            "expected a root-relative key, got {locations:?}"
        );
        assert!(
            !locations.iter().any(|p| p == "lib.rs"),
            "an ingest-folder-relative key is unreachable from the pinned root: {locations:?}"
        );
    }

    /// A relative walk under an absolute marker root must key from that root,
    /// or lookups started at the marker cannot reach the stored node.
    #[test]
    fn a_relative_walk_under_an_absolute_marker_root_keys_from_the_marker() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            node_id_for(&cwd, std::path::Path::new("./src/lib.rs")),
            "src/lib.rs",
            "the walk's relative spelling must resolve through cwd to the marker root"
        );
    }

    /// A marker above a cached subfolder is not used for key spelling: path
    /// lookups still start at the cached root, so keys must agree with it.
    /// Re-ingesting after the cache moves to the marker root must not leave
    /// the obsolete subfolder-relative keys behind (carry-forward skip).
    #[tokio::test]
    async fn ingest_follows_the_cached_root_and_drops_obsolete_keys_on_reingest() {
        let tmp = std::env::temp_dir().join(format!("ns-key-scheme-{}", uuid::Uuid::new_v4()));
        let repo = tmp.join("repo");
        let src = repo.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".NeuroStrata")).unwrap();
        std::fs::write(
            repo.join(".NeuroStrata").join("config.json"),
            r#"{"version":1,"namespace":"test-ns"}"#,
        )
        .unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn x() {}\n").unwrap();

        let db = tmp.join("db");
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.unwrap();
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let embedder: Arc<dyn Embedder> = Arc::new(ZeroEmbedder);
        let pinned_repo = dunce::canonicalize(&repo).unwrap();
        let pinned_src = dunce::canonicalize(&src).unwrap();

        // Cached root disagrees with the marker: keys must be relative to the
        // cached subfolder, not to the marker root.
        crate::registry::cache_project_root(
            &store,
            Some(&embedder),
            "test-ns",
            &pinned_src.to_string_lossy(),
        )
        .await;
        ingest_directory(
            &pinned_src,
            &schema,
            embedder.clone(),
            store.clone(),
            "test-ns",
            None,
            &[],
        )
        .await
        .expect("first ingest succeeds");

        let locations = || async {
            store
                .list("test-ns", None, None)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.payload.location)
                .collect::<Vec<_>>()
        };
        let first = locations().await;
        assert!(
            first.iter().any(|p| p == "lib.rs"),
            "mismatched cached root must key from it: {first:?}"
        );
        assert!(
            !first.iter().any(|p| p == "src/lib.rs"),
            "the marker root is not the cached root, so its spelling is not used: {first:?}"
        );

        // Move the cached root to the marker root and re-ingest unchanged
        // files: keys become root-relative and the old keys must not linger.
        crate::registry::cache_project_root(
            &store,
            Some(&embedder),
            "test-ns",
            &pinned_repo.to_string_lossy(),
        )
        .await;
        ingest_directory(
            &pinned_src,
            &schema,
            embedder.clone(),
            store.clone(),
            "test-ns",
            None,
            &[],
        )
        .await
        .expect("second ingest succeeds");

        let second = locations().await;
        assert!(
            second.iter().any(|p| p == "src/lib.rs"),
            "now the marker root is cached: {second:?}"
        );
        assert!(
            !second.iter().any(|p| p == "lib.rs"),
            "carry-forward must not keep the obsolete subfolder key: {second:?}"
        );
    }

    #[tokio::test]
    async fn producer_consumer_streams_items_to_store() {
        // Create a temp directory with a parseable file.
        let tmp =
            std::env::temp_dir().join(format!("ns-producer-consumer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("hello.rs"), "fn main() {}").expect("write test file");

        let db =
            std::env::temp_dir().join(format!("ns-producer-consumer-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let outcome = ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            None,
            &[],
        )
        .await;

        assert!(outcome.is_ok(), "ingestion succeeded: {:?}", outcome.err());

        // The first ingest publishes generation 1.
        assert_eq!(
            store
                .active_generation("test-ns", &source_key_for(&tmp))
                .await
                .expect("read"),
            1,
            "the first ingest publishes generation 1"
        );

        // The file node should exist, generation-scoped.
        let file_id = generation_scoped_id("test-ns", 1, "hello.rs");
        let entry = store.get("test-ns", &file_id).await.expect("read back");
        assert!(entry.is_some(), "file node was stored");

        // The symbol node should exist (fn main), keyed by body hash.
        let sym_id = generation_scoped_id(
            "test-ns",
            1,
            &symbol_id(
                "hello.rs",
                "functions",
                "main",
                &sha256_hex("fn main() {}".as_bytes()),
            ),
        );
        let sym_entry = store.get("test-ns", &sym_id).await.expect("read back");
        assert!(sym_entry.is_some(), "symbol node was stored");

        // Clean up.
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn two_worktrees_ingest_the_same_namespace_into_distinct_generations() {
        // Two worktrees of one repository share a namespace but not a source.
        // Each must publish its own generation, never merge onto the other's.
        let worktree_a =
            std::env::temp_dir().join(format!("ns-worktree-a-{}", uuid::Uuid::new_v4()));
        let worktree_b =
            std::env::temp_dir().join(format!("ns-worktree-b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&worktree_a).expect("create worktree a");
        std::fs::create_dir_all(&worktree_b).expect("create worktree b");
        std::fs::write(worktree_a.join("hello.rs"), "fn main() {}").expect("write a");
        std::fs::write(worktree_b.join("hello.rs"), "fn main() { loop {} }").expect("write b");

        let db = std::env::temp_dir().join(format!("ns-worktrees-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("shared-ns").await.expect("create the schema");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");

        ingest_directory(
            &worktree_a,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "shared-ns",
            None,
            &[],
        )
        .await
        .expect("worktree a ingests");
        assert_eq!(
            store
                .active_generation("shared-ns", &source_key_for(&worktree_a))
                .await
                .expect("read"),
            1
        );

        ingest_directory(
            &worktree_b,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "shared-ns",
            None,
            &[],
        )
        .await
        .expect("worktree b ingests");
        assert_eq!(
            store
                .active_generation("shared-ns", &source_key_for(&worktree_b))
                .await
                .expect("read"),
            2
        );

        // Source scoping: B's publish must not overwrite A's active pointer.
        assert_eq!(
            store
                .active_generation("shared-ns", &source_key_for(&worktree_a))
                .await
                .expect("read"),
            1,
            "worktree B's publish must not overwrite worktree A's active generation"
        );

        // Each generation's file node coexists; the two worktrees never confluate.
        let a_id = generation_scoped_id("shared-ns", 1, "hello.rs");
        let b_id = generation_scoped_id("shared-ns", 2, "hello.rs");
        assert!(store.get("shared-ns", &a_id).await.expect("read").is_some());
        assert!(store.get("shared-ns", &b_id).await.expect("read").is_some());
        assert_ne!(a_id, b_id, "two generations must not share a physical id");

        std::fs::remove_dir_all(&worktree_a).ok();
        std::fs::remove_dir_all(&worktree_b).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn a_failed_staging_leaves_the_previous_generation_active() {
        // A missing file mid-walk fails the producer only after some nodes have
        // already staged. The failed generation must be discarded and the
        // previous active generation left untouched.
        let db = std::env::temp_dir().join(format!("ns-staging-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("stage-ns").await.expect("create the schema");

        // Publish generation 1 from a real directory.
        let first = std::env::temp_dir().join(format!("ns-stage-first-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&first).expect("create first dir");
        std::fs::write(first.join("hello.rs"), "fn main() {}").expect("write first");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        ingest_directory(
            &first,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "stage-ns",
            None,
            &[],
        )
        .await
        .expect("first ingest publishes");
        assert_eq!(
            store
                .active_generation("stage-ns", &source_key_for(&first))
                .await
                .expect("read"),
            1
        );

        // A missing root fails before a generation is even begun, so the active
        // generation stays 1 and no staging rows appear.
        let missing =
            std::env::temp_dir().join(format!("ns-stage-missing-{}", uuid::Uuid::new_v4()));
        let outcome = ingest_directory(
            &missing,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "stage-ns",
            None,
            &[],
        )
        .await;
        assert!(outcome.is_err(), "a missing root is refused");

        assert_eq!(
            store
                .active_generation("stage-ns", &source_key_for(&first))
                .await
                .expect("read"),
            1,
            "the previous active generation is untouched by a failed ingest"
        );
        let first_id = generation_scoped_id("stage-ns", 1, "hello.rs");
        assert!(
            store
                .get("stage-ns", &first_id)
                .await
                .expect("read")
                .is_some(),
            "the previous generation's nodes remain"
        );

        std::fs::remove_dir_all(&first).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn a_mid_stage_failure_aborts_and_keeps_the_previous_generation_active() {
        // A failure after staging has begun (an embed that fails on the first
        // symbol) must abort the staging generation: its half-built rows are
        // deleted and the previous active generation stays published. This is
        // the abort path the missing-root test cannot reach.
        let db = std::env::temp_dir().join(format!("ns-mid-stage-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("stage-ns").await.expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-mid-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn main() {}").expect("write file");

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&dir);

        // First ingest publishes generation 1.
        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "stage-ns",
            None,
            &[],
        )
        .await
        .expect("first ingest publishes");
        assert_eq!(
            store
                .active_generation("stage-ns", &source_key)
                .await
                .expect("read"),
            1
        );

        // Mutate the file so the incremental skip does not short-circuit the
        // staging path: the second ingest must actually stage + embed.
        std::fs::write(dir.join("hello.rs"), "fn main() { loop {} }").expect("rewrite file");

        // Second ingest begins generation 2, stages the file node, then the
        // symbol embed fails mid-stage.
        let outcome = ingest_directory(
            &dir,
            &schema,
            Arc::new(FailingEmbedder),
            store.clone(),
            "stage-ns",
            None,
            &[],
        )
        .await;
        assert!(
            outcome.is_err(),
            "a mid-stage embed failure aborts the ingest"
        );

        // The aborted generation's rows are gone and generation 1 stays active.
        assert_eq!(
            store
                .active_generation("stage-ns", &source_key)
                .await
                .expect("read"),
            1,
            "the previous active generation survives a mid-stage abort"
        );
        let gen1_id = generation_scoped_id("stage-ns", 1, "hello.rs");
        assert!(
            store
                .get("stage-ns", &gen1_id)
                .await
                .expect("read")
                .is_some(),
            "generation 1's nodes remain after the abort"
        );
        let gen2_id = generation_scoped_id("stage-ns", 2, "hello.rs");
        assert!(
            store
                .get("stage-ns", &gen2_id)
                .await
                .expect("read")
                .is_none(),
            "the aborted generation's staged nodes are deleted"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn version_gate_trips_on_schema_change_and_deletion_does_not_resurrect() {
        // First ingest publishes generation 1; nothing is skipped.
        let tmp = std::env::temp_dir().join(format!("ns-version-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("hello.rs"), "fn main() {}").expect("write file");
        let db = std::env::temp_dir().join(format!("ns-version-gate-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");

        struct Recorder(std::sync::Mutex<(usize, usize)>); // (skipped, gate trips)
        impl IngestObserver for Recorder {
            fn file_ingested(&self, _path: &str, _symbols: usize) {}
            fn file_skipped(&self, _path: &str, _reason: &str) {
                self.0.lock().unwrap().0 += 1;
            }
            fn relinked(&self, _edges: usize) {}
            fn version_gate_tripped(&self, _prev: &str, _current: &str) {
                self.0.lock().unwrap().1 += 1;
            }
        }

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let rec1 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            Some(rec1.clone()),
            &[],
        )
        .await
        .expect("first ingest");
        assert_eq!(
            rec1.0.lock().unwrap().0,
            0,
            "first ingest has nothing to skip"
        );
        assert_eq!(
            rec1.0.lock().unwrap().1,
            0,
            "first ingest does not trip the gate"
        );

        // Same schema: unchanged file is skipped and the gate stays quiet.
        let rec2 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            Some(rec2.clone()),
            &[],
        )
        .await
        .expect("second ingest");
        assert_eq!(rec2.0.lock().unwrap().0, 1, "unchanged file skipped");
        assert_eq!(rec2.0.lock().unwrap().1, 0, "version gate quiet");

        // Changed schema (fingerprint differs): gate trips once, nothing is skipped.
        let mut altered = schema.clone();
        altered
            .languages
            .get_mut("rust")
            .expect("shipped schema has rust")
            .extensions
            .push("rs2".to_string());
        let rec3 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(
            &tmp,
            &altered,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            Some(rec3.clone()),
            &[],
        )
        .await
        .expect("third ingest");
        assert_eq!(rec3.0.lock().unwrap().1, 1, "version gate tripped once");
        assert_eq!(
            rec3.0.lock().unwrap().0,
            0,
            "nothing skipped after a schema change"
        );

        // Deleting the file must not resurrect it in the next generation.
        std::fs::remove_file(tmp.join("hello.rs")).expect("remove file");
        let rec4 = Arc::new(Recorder(std::sync::Mutex::new((0, 0))));
        ingest_directory(
            &tmp,
            &altered,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            Some(rec4.clone()),
            &[],
        )
        .await
        .expect("fourth ingest");
        let gen4 = store
            .active_generation("probe", &source_key_for(&tmp))
            .await
            .expect("read active generation");
        let file_id = generation_scoped_id("probe", gen4, "hello.rs");
        assert!(
            store.get("probe", &file_id).await.expect("read").is_none(),
            "deleted file must not be resurrected by carry-forward"
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn hash_filename_with_hash_survives_carry_forward() {
        let tmp = std::env::temp_dir().join(format!("ns-hashname-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("a#b.rs"), "fn main() {}").expect("write file");
        let db = std::env::temp_dir().join(format!("ns-hashname-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");

        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("second ingest");

        let gen2 = store
            .active_generation("probe", &source_key_for(&tmp))
            .await
            .expect("read active generation");
        let file_id = generation_scoped_id("probe", gen2, "a#b.rs");
        assert!(
            store.get("probe", &file_id).await.expect("read").is_some(),
            "file node with '#' in name must survive carry-forward"
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    async fn kind_fixture(
        tag: &str,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        Arc<dyn VectorStore>,
        ParserSchema,
    ) {
        let tmp = std::env::temp_dir().join(format!("ns-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        let db = std::env::temp_dir().join(format!("ns-{tag}-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        (tmp, db, store, schema)
    }

    /// A directory replaced by a file of the same name must publish the file
    /// node. The old directory row has no hash, so carry-forward used to copy
    /// it over the staged file node (same scoped id) and the next ingest read
    /// the path as a directory again.
    #[tokio::test]
    async fn a_directory_replaced_by_a_file_publishes_the_file_node() {
        let (tmp, db, store, schema) = kind_fixture("dir-to-file").await;
        std::fs::create_dir_all(tmp.join("thing")).expect("create directory");
        std::fs::write(tmp.join("thing").join("inner.txt"), "inner").expect("write inner");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");
        let key = source_key_for(&tmp);
        let gen1 = store.active_generation("probe", &key).await.expect("gen1");

        std::fs::remove_dir_all(tmp.join("thing")).expect("remove directory");
        std::fs::write(tmp.join("thing"), "now a file").expect("write file");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("second ingest");
        let gen2 = store.active_generation("probe", &key).await.expect("gen2");
        assert!(
            gen2 > gen1,
            "a path that changed kind is a change: {gen1} then {gen2}"
        );

        let (_, node) = store
            .get("probe", &generation_scoped_id("probe", gen2, "thing"))
            .await
            .expect("read")
            .expect("the path has a node in the new generation");
        assert_eq!(
            node.memory_type, "file",
            "the published node is the file, not the old directory"
        );
        assert!(
            node.metadata.get("file_sha256").is_some(),
            "and it carries the file's hash"
        );
        assert!(
            store
                .get(
                    "probe",
                    &generation_scoped_id("probe", gen2, "thing/inner.txt")
                )
                .await
                .expect("read")
                .is_none(),
            "the old directory's child is not carried into the new generation"
        );

        // The state must be stable: a third ingest of the same tree changes nothing.
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("third ingest");
        let gen3 = store.active_generation("probe", &key).await.expect("gen3");
        assert_eq!(
            gen3, gen2,
            "the file node is now the previous generation, so nothing changed"
        );
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// A file replaced by an empty directory stages no content, so only the
    /// node kind tells the ingest anything changed. Comparing paths alone left
    /// the deleted file's node, symbols and sections served as live.
    #[tokio::test]
    async fn a_file_replaced_by_an_empty_directory_is_a_change() {
        let (tmp, db, store, schema) = kind_fixture("file-to-dir").await;
        std::fs::write(tmp.join("thing.rs"), "pub fn gone() {}").expect("write file");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");
        let key = source_key_for(&tmp);
        let gen1 = store.active_generation("probe", &key).await.expect("gen1");

        std::fs::remove_file(tmp.join("thing.rs")).expect("remove file");
        std::fs::create_dir_all(tmp.join("thing.rs")).expect("create directory");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("second ingest");
        let gen2 = store.active_generation("probe", &key).await.expect("gen2");
        assert!(
            gen2 > gen1,
            "a path that changed kind is a change: {gen1} then {gen2}"
        );

        let (_, node) = store
            .get("probe", &generation_scoped_id("probe", gen2, "thing.rs"))
            .await
            .expect("read")
            .expect("the path has a node in the new generation");
        assert_eq!(node.memory_type, "directory");
        let leftovers: Vec<String> = store
            .list("probe", None, None)
            .await
            .expect("list")
            .into_iter()
            .filter(|r| {
                split_scoped_id(&r.id)
                    .map(|(_, g, _)| g == gen2)
                    .unwrap_or(false)
            })
            .filter(|r| r.payload.location == "thing.rs" && r.payload.memory_type != "directory")
            .map(|r| r.id)
            .collect();
        assert!(
            leftovers.is_empty(),
            "the deleted file's rows are not served: {leftovers:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn pre_migration_generation_re_parses_and_stamps_every_row() {
        let tmp = std::env::temp_dir().join(format!("ns-premig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("hello.rs"), "fn main() {}").expect("write file");
        let db = std::env::temp_dir().join(format!("ns-premig-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&tmp);

        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");

        // Simulate a pre-migration generation by stripping ingest_version from
        // every row of the active generation.
        let gen1 = store
            .active_generation("probe", &source_key)
            .await
            .expect("read");
        let rows = store.list("probe", None, None).await.expect("list");
        for r in rows {
            let scoped = crate::parser::ingest::split_scoped_id(&r.id);
            if scoped.is_none() {
                continue;
            }
            let (_, g, _) = scoped.unwrap();
            if g != gen1 {
                continue;
            }
            if let Ok(Some((vec, mut payload))) = store.get("probe", &r.id).await {
                if let Some(obj) = payload.metadata.as_object_mut() {
                    obj.remove("ingest_version");
                }
                store
                    .upsert("probe", &r.id, vec, payload)
                    .await
                    .expect("upsert");
            }
        }

        // Re-ingest must trip the gate (missing stamps) and stamp every row.
        struct Rec(std::sync::Mutex<usize>);
        impl IngestObserver for Rec {
            fn file_ingested(&self, _p: &str, _s: usize) {}
            fn file_skipped(&self, _p: &str, _r: &str) {}
            fn relinked(&self, _e: usize) {}
            fn version_gate_tripped(&self, _p: &str, _c: &str) {
                *self.0.lock().unwrap() += 1;
            }
        }
        let rec = Arc::new(Rec(std::sync::Mutex::new(0)));
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            Some(rec.clone()),
            &[],
        )
        .await
        .expect("second ingest");
        assert_eq!(
            *rec.0.lock().unwrap(),
            1,
            "pre-migration generation trips the gate"
        );

        let gen2 = store
            .active_generation("probe", &source_key)
            .await
            .expect("read");
        let rows2 = store.list("probe", None, None).await.expect("list");
        for r in rows2 {
            let scoped = crate::parser::ingest::split_scoped_id(&r.id);
            if scoped.is_none() {
                continue;
            }
            let (_, g, _) = scoped.unwrap();
            if g != gen2 {
                continue;
            }
            let v = r
                .payload
                .metadata
                .get("ingest_version")
                .and_then(|x| x.as_str());
            assert!(
                v.is_some() && !v.unwrap().is_empty(),
                "every current row must be stamped"
            );
        }

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// The slim per-generation read must hand ingest exactly the rows `list`
    /// used to, minus the content.
    #[tokio::test]
    async fn generation_summary_matches_the_rows_list_gave_ingest() {
        use crate::traits::GenerationRow;
        let tmp = std::env::temp_dir().join(format!("ns-gensum-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("a.rs"), "fn a() {}\nfn b() {}").expect("write file");
        std::fs::write(tmp.join("notes.md"), "# Title\n\nbody\n").expect("write file");
        let db = std::env::temp_dir().join(format!("ns-gensum-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");

        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");
        // A second ingest after a change leaves a superseded generation behind.
        std::fs::write(tmp.join("a.rs"), "fn a() {}\nfn c() {}").expect("edit file");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("second ingest");
        let gen = store
            .active_generation("probe", &source_key_for(&tmp))
            .await
            .expect("read");

        let mut got = store
            .generation_summary("probe", gen)
            .await
            .expect("summary");
        let mut want: Vec<GenerationRow> = store
            .list("probe", None, None)
            .await
            .expect("list")
            .into_iter()
            .filter(|r| {
                crate::parser::ingest::split_scoped_id(&r.id).is_some_and(|(_, g, _)| g == gen)
            })
            .map(|r| GenerationRow::from_payload(r.id, &r.payload))
            .collect();
        got.sort_by(|a, b| a.id.cmp(&b.id));
        want.sort_by(|a, b| a.id.cmp(&b.id));
        assert!(
            got.len() > 2,
            "the active generation has directory, file and symbol rows"
        );
        assert_eq!(
            got, want,
            "same rows, same fields, as the full listing gave"
        );
        assert!(
            got.iter().any(|r| r.file_sha256.is_some()),
            "file rows carry their hash"
        );
        assert!(
            got.iter().all(|r| !r.ingest_version.is_empty()),
            "every current row is stamped"
        );

        let stale = store
            .generation_summary("probe", gen + 1000)
            .await
            .expect("summary");
        assert!(
            stale.is_empty(),
            "a generation that is not active yields nothing"
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// Two sources in one namespace. The generation counter is per namespace
    /// (see `begin_generation`), so two sources never share a generation
    /// number, but they do interleave: here source a is ingested, then source
    /// b, then source a again, which supersedes a's first generation. For each
    /// generation number the summary has to give the rows the full listing
    /// does, and only the rows of the source that owns it.
    #[tokio::test]
    async fn generation_summary_matches_list_for_two_sources_in_one_namespace() {
        use crate::traits::GenerationRow;
        let (tmp_a, db, store, schema) = kind_fixture("gensum-two-a").await;
        let tmp_b = std::env::temp_dir().join(format!("ns-gensum-two-b-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp_b).expect("create second source");
        std::fs::write(tmp_a.join("a.rs"), "fn a() {}").expect("write a");
        std::fs::write(tmp_b.join("b.rs"), "fn b() {}").expect("write b");

        ingest_directory(
            &tmp_a,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("ingest source a");
        ingest_directory(
            &tmp_b,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("ingest source b");
        std::fs::write(tmp_a.join("a.rs"), "fn a() {}\nfn a2() {}").expect("edit a");
        ingest_directory(
            &tmp_a,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("re-ingest source a");
        let gen_a = store
            .active_generation("probe", &source_key_for(&tmp_a))
            .await
            .expect("gen a");
        let gen_b = store
            .active_generation("probe", &source_key_for(&tmp_b))
            .await
            .expect("gen b");
        assert_eq!(
            (gen_b, gen_a),
            (2, 3),
            "b took the second number, a's re-ingest the third"
        );

        for gen in 1..=3u64 {
            let mut got = store
                .generation_summary("probe", gen)
                .await
                .expect("summary");
            let mut want: Vec<GenerationRow> = store
                .list("probe", None, None)
                .await
                .expect("list")
                .into_iter()
                .filter(|r| {
                    crate::parser::ingest::split_scoped_id(&r.id).is_some_and(|(_, g, _)| g == gen)
                })
                .map(|r| GenerationRow::from_payload(r.id, &r.payload))
                .collect();
            got.sort_by(|a, b| a.id.cmp(&b.id));
            want.sort_by(|a, b| a.id.cmp(&b.id));
            assert_eq!(got, want, "generation {gen}: same rows as the full listing");
        }
        let first = store
            .generation_summary("probe", 1)
            .await
            .expect("summary of generation 1");
        let second = store
            .generation_summary("probe", 2)
            .await
            .expect("summary of generation 2");
        let third = store
            .generation_summary("probe", 3)
            .await
            .expect("summary of generation 3");
        assert!(
            first.is_empty(),
            "a's superseded first generation yields nothing"
        );
        assert!(
            second.iter().all(|r| r.location != "a.rs"),
            "b's generation holds none of a's files"
        );
        assert!(
            third.iter().all(|r| r.location != "b.rs"),
            "a's generation holds none of b's files"
        );
        assert!(
            second.iter().any(|r| r.location == "b.rs"),
            "b's file is read"
        );
        assert!(
            third.iter().any(|r| r.location == "a.rs"),
            "a's file is read"
        );

        std::fs::remove_dir_all(&tmp_a).ok();
        std::fs::remove_dir_all(&tmp_b).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn mixed_stamps_trip_even_if_first_row_is_current() {
        let tmp = std::env::temp_dir().join(format!("ns-mixed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("hello.rs"), "fn main() {}").expect("write file");
        let db = std::env::temp_dir().join(format!("ns-mixed-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("probe").await.expect("create the schema");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&tmp);

        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");
        let gen1 = store
            .active_generation("probe", &source_key)
            .await
            .expect("read");
        let current = ingest_version(&schema.fingerprint());

        // Tamper with one row's stamp to a different value.
        let rows = store.list("probe", None, None).await.expect("list");
        let target = rows
            .iter()
            .find(|r| {
                crate::parser::ingest::split_scoped_id(&r.id)
                    .map(|x| x.1 == gen1)
                    .unwrap_or(false)
            })
            .expect("row")
            .id
            .clone();
        if let Ok(Some((vec, mut payload))) = store.get("probe", &target).await {
            if let Some(obj) = payload.metadata.as_object_mut() {
                obj.insert("ingest_version".to_string(), serde_json::json!("tampered"));
            }
            store
                .upsert("probe", &target, vec, payload)
                .await
                .expect("upsert");
        }
        assert_ne!(current, "tampered");

        struct Rec(std::sync::Mutex<usize>);
        impl IngestObserver for Rec {
            fn file_ingested(&self, _p: &str, _s: usize) {}
            fn file_skipped(&self, _p: &str, _r: &str) {}
            fn relinked(&self, _e: usize) {}
            fn version_gate_tripped(&self, _p: &str, _c: &str) {
                *self.0.lock().unwrap() += 1;
            }
        }
        let rec = Arc::new(Rec(std::sync::Mutex::new(0)));
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            Some(rec.clone()),
            &[],
        )
        .await
        .expect("second ingest");
        assert_eq!(*rec.0.lock().unwrap(), 1, "mixed stamps must trip the gate");

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[test]
    fn ingest_worker_count_respects_bounds() {
        assert_eq!(ingest_worker_count(0), 1);
        assert!(ingest_worker_count(3) <= 3);
        assert!(ingest_worker_count(100) <= 8);
        assert!(ingest_worker_count(100) >= 1);
    }

    /// The streaming producer replays the processed entries in walk order,
    /// however the workers happen to finish, and the replayed values are the
    /// ones a sequential run would produce.
    #[test]
    fn the_ingest_stream_replays_in_index_order() {
        let total = 64usize;
        let window = 8usize;
        let mut replayed: Vec<usize> = Vec::new();
        let mut values: Vec<usize> = Vec::new();
        let stats = stream_in_walk_order(
            total,
            4,
            window,
            || {
                move |idx: usize| -> anyhow::Result<usize> {
                    // The lower indices finish last, so the workers complete
                    // out of order.
                    std::thread::sleep(std::time::Duration::from_micros((total - idx) as u64));
                    Ok(idx * 2)
                }
            },
            |idx, result| {
                replayed.push(idx);
                values.push(result?);
                Ok(())
            },
        )
        .expect("the stream completes");
        assert_eq!(
            replayed,
            (0..total).collect::<Vec<_>>(),
            "the replay runs in index order"
        );
        assert_eq!(
            values,
            (0..total).map(|i| i * 2).collect::<Vec<_>>(),
            "the replayed values are the ones a sequential run produces"
        );
        assert!(
            stats.peak_buffered <= window,
            "the stream held {} results, the window is {}",
            stats.peak_buffered,
            window
        );
    }

    /// The stream bounds what it holds: no more than the window of results
    /// are ever in flight between the workers and the replay, however large
    /// the tree is.
    #[test]
    fn the_ingest_stream_bounds_what_it_holds() {
        let total = 500usize;
        let window = 16usize;
        let stats = stream_in_walk_order(
            total,
            8,
            window,
            || move |idx: usize| -> anyhow::Result<usize> { Ok(idx) },
            |_idx, _res| Ok(()),
        )
        .expect("the stream completes");
        assert!(
            stats.peak_buffered <= window,
            "the stream held {} results, the window is {}",
            stats.peak_buffered,
            window
        );
    }

    /// A worker that panics does not take the run with it: the panic becomes
    /// that entry's error, and the rest of the run still lands in order.
    #[test]
    fn a_panicking_worker_becomes_that_entrys_error() {
        let total = 32usize;
        let mut errors: Vec<(usize, String)> = Vec::new();
        let mut landed = 0usize;
        stream_in_walk_order(
            total,
            4,
            8,
            || {
                move |idx: usize| -> anyhow::Result<usize> {
                    if idx == 7 {
                        panic!("worker panic at index {idx}");
                    }
                    Ok(idx)
                }
            },
            |idx, result| {
                match result {
                    Ok(_) => landed += 1,
                    Err(e) => errors.push((idx, e.to_string())),
                }
                Ok(())
            },
        )
        .expect("the run completes despite the panic");
        assert_eq!(errors.len(), 1, "one entry failed");
        assert_eq!(
            errors[0].0, 7,
            "the entry that panicked is the one that failed"
        );
        assert!(
            errors[0].1.contains("panicked"),
            "the error names the panic: {}",
            errors[0].1
        );
        assert_eq!(landed, total - 1, "every other entry still landed");
    }

    /// A replay that fails ends the run instead of waiting for a replay that
    /// will never come.
    #[test]
    fn a_failing_replay_ends_the_stream() {
        let err = stream_in_walk_order(
            64usize,
            4,
            8,
            || move |idx: usize| -> anyhow::Result<usize> { Ok(idx) },
            |idx, _res| {
                if idx == 3 {
                    anyhow::bail!("replay refused index 3");
                }
                Ok(())
            },
        )
        .expect_err("the replay failure ends the stream");
        assert!(
            err.to_string().contains("replay refused"),
            "the error is the replay's own: {err}"
        );
    }

    /// The cancellation must be set under the lock the workers sleep on.
    ///
    /// A worker reads the abort flag and waits on the condvar while it holds
    /// the gate lock; the replay sets the flag and notifies. If the flag is
    /// set outside that lock, the two can interleave: the worker reads "not
    /// aborted", the replay sets the flag and notifies (the worker is not
    /// waiting yet, so the notification is lost), and the worker then sleeps
    /// through it -- the run waits for a wakeup that never comes.
    ///
    /// The test holds the gate lock (the worker is between its check and its
    /// wait) and asserts that the abort cannot get past it: an abort that
    /// takes the lock has to queue behind the waiter, an abort that does not
    /// takes it goes straight through and the worker sleeps through the
    /// notification.
    #[test]
    fn an_abort_waits_for_the_lock_the_worker_sleeps_on() {
        let gate = std::sync::Arc::new(ReplayGate::new());
        let held = gate.state.lock().expect("replay gate mutex");
        let (tx, rx) = std::sync::mpsc::channel();
        let gate_for_abort = gate.clone();
        let aborter = std::thread::spawn(move || {
            gate_for_abort.abort();
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "the abort must not pass a worker that holds the gate lock"
        );
        drop(held);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the abort completes once the lock is free");
        aborter.join().unwrap();
    }

    /// A worker waiting for the replay wakes up when the replay gives up.
    #[test]
    fn an_aborted_gate_wakes_the_worker_that_waits_for_it() {
        let gate = std::sync::Arc::new(ReplayGate::new());
        let gate_for_worker = gate.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let allowed = gate_for_worker.wait_for_slot(4, 1);
            let _ = tx.send(allowed);
        });
        // The worker is waiting for a slot the replay has not opened yet.
        std::thread::sleep(std::time::Duration::from_millis(50));
        gate.abort();
        let allowed = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the waiting worker wakes");
        assert!(!allowed, "an aborted gate lets no work through");
        worker.join().unwrap();
    }

    /// A replay that fails ends the run even when the workers are blocked
    /// handing their results back.
    ///
    /// The result channel is closed before the workers are joined, so a
    /// worker blocked in `send` fails and wakes instead of waiting for a
    /// receiver that will never read again. The channel is made the tighter
    /// bound here (capacity 1 against a replay window of 64), so the workers
    /// really do pile up on a full channel while the replay is over.
    ///
    /// The run happens on its own thread and is waited for with a timeout: a
    /// run that hangs fails this test instead of hanging the suite.
    #[test]
    fn a_failing_replay_ends_the_run_with_a_full_result_channel() {
        let (tx, rx) = std::sync::mpsc::channel();
        let runner = std::thread::spawn(move || {
            let outcome = stream_in_walk_order_bounded(
                512usize,
                4,
                64,
                1,
                || move |idx: usize| -> anyhow::Result<usize> { Ok(idx) },
                |idx, _res| {
                    if idx == 0 {
                        anyhow::bail!("the replay refused index 0");
                    }
                    Ok(())
                },
            );
            let _ = tx.send(outcome.is_err());
        });
        let ended = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the run ends instead of hanging");
        assert!(ended, "the replay failure is reported");
        runner.join().unwrap();
    }

    /// The refusal tally counts every refused embedding and keeps a bounded
    /// number of reasons, so the summary stays readable however many items
    /// the provider refused.
    #[test]
    fn degraded_embeddings_count_the_refusals_and_cap_the_samples() {
        let mut degraded = DegradedEmbeddings::default();
        assert!(
            degraded.summary().is_none(),
            "nothing refused, nothing said"
        );

        let outcome = crate::traits::BatchEmbedOutcome {
            embeddings: vec![Some(vec![0.0; 4]), None, None],
            warnings: (0..20).map(|i| format!("refusal {i}")).collect(),
        };
        degraded.note(&outcome);
        assert_eq!(degraded.count, 2, "both refusals are counted");
        assert_eq!(
            degraded.samples.len(),
            MAX_DEGRADED_EMBED_SAMPLES,
            "the reasons are sampled"
        );
        let summary = degraded.summary().expect("the summary names the refusals");
        assert!(
            summary.contains("2 embedding(s) were refused"),
            "the summary counts the refusals: {summary}"
        );
        assert!(
            summary.contains("refusal 0"),
            "the summary carries the reasons: {summary}"
        );
    }

    /// Refuses the first item of every batch and embeds the rest.
    struct RefusingEmbedder;

    #[async_trait::async_trait]
    impl Embedder for RefusingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }

        async fn embed_batch_degradable(
            &self,
            texts: &[String],
        ) -> anyhow::Result<crate::traits::BatchEmbedOutcome> {
            let mut embeddings: Vec<Option<Vec<f32>>> =
                texts.iter().map(|_| Some(vec![0.0; 4])).collect();
            if let Some(first) = embeddings.first_mut() {
                *first = None;
            }
            Ok(crate::traits::BatchEmbedOutcome {
                embeddings,
                warnings: vec![
                    "remote embedding refused one input (413): payload too large".to_string(),
                ],
            })
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    /// A provider that refuses one item does not cost the generation: the
    /// refused item is skipped, the rest of the generation lands, and the
    /// ingest reports the refusal instead of failing.
    #[tokio::test]
    async fn ingest_skips_a_refused_embedding_and_keeps_the_rest() {
        let tmp = std::env::temp_dir().join(format!("ns-degraded-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("lib.rs"), "pub fn alpha() {}\npub fn beta() {}\n")
            .expect("write rust file");
        let db = std::env::temp_dir().join(format!("ns-degraded-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("degraded-ns").await.expect("create the schema");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(RefusingEmbedder),
            store.clone(),
            "degraded-ns",
            None,
            &[],
        )
        .await
        .expect("a refused embedding does not fail the ingest");

        let rows = store.list("degraded-ns", None, None).await.expect("list");
        let symbols: Vec<String> = rows
            .iter()
            .filter(|r| r.payload.memory_type == "code_ast")
            .map(|r| r.payload.content.clone())
            .collect();
        assert_eq!(
            symbols.len(),
            1,
            "the refused symbol is skipped, the other one lands: {symbols:?}"
        );
        assert!(
            symbols[0].contains("beta"),
            "the symbol that was embedded is the one that landed: {symbols:?}"
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[test]
    fn markdown_sections_extract_headings_backticks_links_and_skip_empty() {
        let md = "# Heading\n\nbody with `sym` and [link](https://example.com)\n\n## Empty\n\n   \n\n## Second\nmore";
        let secs = parse_markdown_sections(md);
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].heading, "Heading");
        assert!(secs[0].backticked.contains(&"sym".to_string()));
        assert!(secs[0].links.contains(&"https://example.com".to_string()));
        assert_eq!(secs[1].heading, "Second");
    }

    /// F8 dry-run estimate and presort with all required fields present.
    #[tokio::test]
    async fn test_f8_dry_run_estimate_and_presort_zero_writes() {
        let tmp = std::env::temp_dir().join(format!("ns-f8-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("create");
        std::fs::write(tmp.join("foo.rs"), "pub fn bar() {}").unwrap();
        std::fs::write(tmp.join("README.md"), "# Hello\n\nSecret token x.\n").unwrap();
        // Duplicate file with same hash
        std::fs::write(tmp.join("dup.rs"), "pub fn bar() {}").unwrap();
        // Versioned file
        std::fs::write(tmp.join("foo_v2.rs"), "pub fn bar_v2() {}").unwrap();
        // Junk / bak file
        std::fs::write(tmp.join("foo.bak"), "old").unwrap();
        // Secret / PII content
        std::fs::write(
            tmp.join("secret.rs"),
            "const TOKEN: &str = \"secret123\";\nlet email = \"a@b.com\";\n",
        )
        .unwrap();

        let schema = ParserSchema::load(include_str!("../schema.json")).expect("schema parses");
        let est_report = dry_run_directory(&tmp, &schema, "estimate").await.unwrap();
        let pre_report = dry_run_directory(&tmp, &schema, "presort").await.unwrap();

        for (label, r) in [("estimate", &est_report), ("presort", &pre_report)] {
            assert!(
                r.contains(&format!("Dry-run ({})", label)),
                "{} mode header",
                label
            );
            assert!(r.contains("Files:"), "{} Files", label);
            assert!(r.contains("Parseable files:"), "{} Parseable", label);
            assert!(r.contains("Symbols:"), "{} Symbols", label);
            assert!(r.contains("Doc sections:"), "{} Doc sections", label);
            assert!(
                r.contains("Embeddings needed (est):"),
                "{} Embeddings",
                label
            );
            assert!(r.contains("Avoided embeddings:"), "{} Avoided", label);
            assert!(r.contains("Secret hits:"), "{} Secret hits", label);
            assert!(r.contains("PII warnings:"), "{} PII", label);
            assert!(r.contains("Junk candidates:"), "{} Junk candidates", label);
            assert!(r.contains("Duplicate candidates:"), "{} Duplicate", label);
            assert!(r.contains("Version candidates:"), "{} Version", label);
        }
        assert!(pre_report.contains("Junk files:"));
        assert!(pre_report.contains("Duplicate groups:"));
        assert!(pre_report.contains("Version candidate files:"));
        assert!(pre_report.contains("Proposed dataset groups:"));
    }

    /// F7 extract imports / resolve dependency paths for Rust/TS.
    #[test]
    fn test_f7_extract_and_resolve_dependencies() {
        let rust = r#"mod foo;
use crate::bar::baz;
use super::x;"#;
        let specs = extract_import_specifiers("rust", rust);
        assert!(specs.contains(&"mod:foo".to_string()));
        assert!(specs.contains(&"use:crate::bar::baz".to_string()));

        let ts = r#"import { x } from "./utils";\nrequire("./helper");"#;
        let ts_specs = extract_import_specifiers("typescript", ts);
        assert!(
            ts_specs.contains(&"js:./utils".to_string())
                || ts_specs.contains(&"js:./utils".to_string())
        );

        let all = std::collections::HashSet::from([
            "src/foo.rs".to_string(),
            "src/bar.rs".to_string(),
            "src/utils.ts".to_string(),
            "src/helper.js".to_string(),
        ]);
        // Resolve rust `mod foo;` from src/main.rs -> src/foo.rs
        let rust_deps = resolve_dependency_paths("src/main.rs", &specs, &all);
        assert!(rust_deps.contains(&"src/foo.rs".to_string()));
        // Resolve rust `use crate::bar::baz;` -> src/bar.rs
        assert!(rust_deps.contains(&"src/bar.rs".to_string()));

        // Resolve TypeScript imports
        let ts_deps = resolve_dependency_paths("src/app.ts", &ts_specs, &all);
        assert!(
            ts_deps.contains(&"src/utils.ts".to_string())
                || ts_deps.contains(&"src/helper.js".to_string())
        );
    }

    /// Memory probe, not a correctness test: ingests `NS_PROBE_DIR` twice into a
    /// store opened the way the daemon opens it (default 384 MB buffer-pool cap;
    /// set `NEUROSTRATA_BUFFER_POOL_MB=0` to measure the engine default) so peak
    /// working set can be read from outside the process. Run:
    ///   NS_PROBE_DIR=<dir> cargo test --bin neurostrata-mcp ingest_memory_probe -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn ingest_memory_probe() {
        let dir = std::path::PathBuf::from(std::env::var("NS_PROBE_DIR").expect("NS_PROBE_DIR"));
        let db = std::env::temp_dir().join(format!("ns-probe-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&db);
        // NS_PROBE_REAL=1 loads the daemon's embedder, so the model and its
        // runtime memory are part of what is measured.
        let embedder: Arc<dyn Embedder> = if std::env::var("NS_PROBE_REAL").is_ok() {
            Arc::new(crate::embed::FastEmbedder::new().expect("embedder"))
        } else {
            Arc::new(ZeroEmbedder)
        };
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::new(&db, embedder.dimensions()).expect("open"));
        store.init("probe").await.unwrap();
        let schema = ParserSchema::load(include_str!("../schema.json")).expect("schema");
        for pass in 1..=2 {
            let t = std::time::Instant::now();
            ingest_directory(
                &dir,
                &schema,
                embedder.clone(),
                store.clone(),
                "probe",
                None,
                &[],
            )
            .await
            .expect("ingest");
            println!("PROBE pass {pass} done in {:?}", t.elapsed());
        }
        // The probe owns a throwaway store; leave no multi-hundred-MB
        // directory behind after a measurement.
        drop(store);
        drop(embedder);
        let _ = std::fs::remove_dir_all(&db);
    }

    /// F2 incremental ingest + F12 watermark.
    #[tokio::test]
    async fn test_f2_f12_incremental_ingest_and_watermark() {
        let tmp = std::env::temp_dir().join(format!("ns-f2-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("create");
        std::fs::write(tmp.join("hello.rs"), "pub fn hello() {}").unwrap();
        let db = std::env::temp_dir().join(format!("ns-f2-db-{}", std::process::id()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open"));
        store.init("f2").await.unwrap();
        let schema = ParserSchema::load(include_str!("../schema.json")).expect("schema");
        let embedder = Arc::new(ZeroEmbedder);

        // First ingest -> generation 1, writes watermark.
        ingest_directory(
            &tmp,
            &schema,
            embedder.clone(),
            store.clone(),
            "f2",
            None,
            &[],
        )
        .await
        .expect("first");
        let source_key = source_key_for(&tmp);
        let gen1 = store.active_generation("f2", &source_key).await.unwrap();
        assert!(gen1 > 0, "first ingest produces a generation");

        // Check registry row has watermark and telemetry.
        let registry_id = crate::registry::registry_row_id("f2");
        let (_vec, payload) = store
            .get("f2", &registry_id)
            .await
            .unwrap()
            .expect("registry row");
        let meta = payload.metadata.as_object().expect("metadata");
        assert!(
            meta.contains_key("enrichment_state"),
            "enrichment_state present"
        );
        let state = meta.get("enrichment_state").expect("state");
        let wm = state.get("watermark").expect("watermark");
        assert!(wm.get("extractor_fingerprint").is_some(), "fingerprint");
        assert!(wm.get("source_digest").is_some(), "source_digest");
        assert_ne!(
            wm.get("source_digest")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "source_digest must not be empty hash"
        );
        assert!(wm.get("last_enriched_at").is_some(), "last_enriched_at");

        // Second ingest with unchanged files keeps generation 1 active: a new
        // generation would be a copy of it.
        ingest_directory(
            &tmp,
            &schema,
            embedder.clone(),
            store.clone(),
            "f2",
            None,
            &[],
        )
        .await
        .expect("second");
        let gen2 = store.active_generation("f2", &source_key).await.unwrap();
        assert_eq!(gen2, gen1, "an unchanged tree keeps the active generation");
        // Watermark updated (same fingerprint if schema unchanged, updated digest if files same).
        let (_vec2, payload2) = store
            .get("f2", &registry_id)
            .await
            .unwrap()
            .expect("registry row 2");
        let meta2 = payload2.metadata.as_object().expect("meta2");
        let wm2 = meta2
            .get("enrichment_state")
            .expect("state2")
            .get("watermark")
            .expect("wm2");
        assert!(wm2.get("source_digest").is_some(), "updated digest");

        // Modify file -> re-embeds changed file; old generation superseded.
        std::fs::write(tmp.join("hello.rs"), "pub fn hello_new() {}").unwrap();
        ingest_directory(
            &tmp,
            &schema,
            embedder.clone(),
            store.clone(),
            "f2",
            None,
            &[],
        )
        .await
        .expect("modified");
        let gen3 = store.active_generation("f2", &source_key).await.unwrap();
        assert!(gen3 > gen2, "a changed file publishes a newer generation");

        // A new file, with no change to any existing one, also publishes.
        std::fs::write(tmp.join("added.rs"), "pub fn added() {}").unwrap();
        ingest_directory(
            &tmp,
            &schema,
            embedder.clone(),
            store.clone(),
            "f2",
            None,
            &[],
        )
        .await
        .expect("added");
        let gen4 = store.active_generation("f2", &source_key).await.unwrap();
        assert!(gen4 > gen3, "an added file publishes a newer generation");

        // So does a deleted one.
        std::fs::remove_file(tmp.join("added.rs")).unwrap();
        ingest_directory(
            &tmp,
            &schema,
            embedder.clone(),
            store.clone(),
            "f2",
            None,
            &[],
        )
        .await
        .expect("deleted");
        let gen5 = store.active_generation("f2", &source_key).await.unwrap();
        assert!(gen5 > gen4, "a deleted file publishes a newer generation");
    }

    /// Records the documents the walk skipped, so a test asserts on the
    /// report rather than on what the walk printed.
    struct SkipRecorder {
        skipped: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    }

    impl IngestObserver for SkipRecorder {
        fn file_ingested(&self, _path: &str, _symbols: usize) {}

        fn relinked(&self, _edges: usize) {}

        fn file_skipped(&self, path: &str, reason: &str) {
            self.skipped
                .lock()
                .expect("recorder lock")
                .push((path.to_string(), reason.to_string()));
        }
    }

    /// PAR-03: a document whose text trips the scanner is skipped whole — no
    /// file node, no symbols — and reported with the scanner's reason, while
    /// the rest of the walk proceeds.
    #[tokio::test]
    async fn par03_secret_in_ingest_is_refused() {
        let tmp = std::env::temp_dir().join(format!("ns-ingest-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        // Secret-named fields with type annotations are ordinary source, not
        // credentials; they must still ingest.
        let good_source = "struct Config { api_key: String, token: String }\nfn clean() {}";
        std::fs::write(tmp.join("good.rs"), good_source).expect("write clean file");
        // Built at runtime so no secret-shaped literal sits in the source.
        let secret = format!("ghp_{}", "Ab1".repeat(12));
        let evil_source = format!("fn leak() {{\n    let key = \"{}\";\n}}\n", secret);
        std::fs::write(tmp.join("evil.rs"), &evil_source).expect("write leaking file");

        let db = std::env::temp_dir().join(format!("ns-ingest-secret-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");

        let skipped = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer: Arc<dyn IngestObserver> = Arc::new(SkipRecorder {
            skipped: skipped.clone(),
        });

        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let outcome = ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            Some(observer),
            &[],
        )
        .await;
        assert!(
            outcome.is_ok(),
            "the walk proceeds past the skipped document: {:?}",
            outcome.err()
        );

        // The clean document landed whole.
        let good_node = generation_scoped_id("test-ns", 1, "good.rs");
        assert!(
            store
                .get("test-ns", &good_node)
                .await
                .expect("read back")
                .is_some(),
            "clean file with secret-named typed fields was stored"
        );
        let good_sym = generation_scoped_id(
            "test-ns",
            1,
            &symbol_id(
                "good.rs",
                "functions",
                "clean",
                &sha256_hex("fn clean() {}".as_bytes()),
            ),
        );
        assert!(
            store
                .get("test-ns", &good_sym)
                .await
                .expect("read back")
                .is_some(),
            "clean symbol was stored"
        );

        // The tripping document contributed nothing: no file node, no symbol.
        let evil_node = generation_scoped_id("test-ns", 1, "evil.rs");
        assert!(
            store
                .get("test-ns", &evil_node)
                .await
                .expect("read back")
                .is_none(),
            "skipped document's file node was not stored"
        );
        let evil_sym = generation_scoped_id(
            "test-ns",
            1,
            &symbol_id(
                "evil.rs",
                "functions",
                "leak",
                &sha256_hex(evil_source.as_bytes()),
            ),
        );
        assert!(
            store
                .get("test-ns", &evil_sym)
                .await
                .expect("read back")
                .is_none(),
            "skipped document's symbol was not stored"
        );

        // And the skip was reported with the file and the scanner's reason.
        let skipped = skipped.lock().expect("recorder lock");
        assert_eq!(
            skipped.len(),
            1,
            "exactly one skip reported: {:?}",
            *skipped
        );
        assert!(
            skipped[0].0.ends_with("evil.rs"),
            "skip names the file: {}",
            skipped[0].0
        );
        assert!(
            skipped[0].1.contains("ERROR [SECURITY]"),
            "skip carries the scanner verdict: {}",
            skipped[0].1
        );
        assert!(
            skipped[0].1.contains("github_pat"),
            "skip carries the scanner category: {}",
            skipped[0].1
        );
        assert!(
            !skipped[0].1.contains(&secret),
            "skip must not echo the secret: {}",
            skipped[0].1
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// PAR-03 / C10: markdown has no tree-sitter grammar, so its bytes must be
    /// scanned independently of grammar availability. A credential in a .md
    /// file contributes no file node and no doc sections.
    #[tokio::test]
    async fn par03_secret_in_markdown_is_refused() {
        let tmp =
            std::env::temp_dir().join(format!("ns-ingest-md-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        // Built at runtime so no secret-shaped literal sits in the source.
        let secret = format!("ghp_{}", "Ab1".repeat(12));
        let md = format!("# Notes\n\nkey: {}\n", secret);
        std::fs::write(tmp.join("guide.md"), &md).expect("write leaking markdown");

        let db =
            std::env::temp_dir().join(format!("ns-ingest-md-secret-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");

        let skipped = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer: Arc<dyn IngestObserver> = Arc::new(SkipRecorder {
            skipped: skipped.clone(),
        });
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let outcome = ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            Some(observer),
            &[],
        )
        .await;
        assert!(
            outcome.is_ok(),
            "the walk proceeds past the skipped markdown: {:?}",
            outcome.err()
        );

        let file_id = generation_scoped_id("test-ns", 1, "guide.md");
        assert!(
            store
                .get("test-ns", &file_id)
                .await
                .expect("read back")
                .is_none(),
            "skipped markdown's file node was not stored"
        );
        for section in parse_markdown_sections(&md) {
            let sid = generation_scoped_id(
                "test-ns",
                1,
                &format!("guide.md#section:{}@{}", section.slug, section.sha256),
            );
            assert!(
                store
                    .get("test-ns", &sid)
                    .await
                    .expect("read back")
                    .is_none(),
                "skipped markdown section was not stored: {}",
                section.slug
            );
        }

        let skipped = skipped.lock().expect("recorder lock");
        assert_eq!(
            skipped.len(),
            1,
            "exactly one skip reported: {:?}",
            *skipped
        );
        assert!(
            skipped[0].0.ends_with("guide.md"),
            "skip names the file: {}",
            skipped[0].0
        );
        assert!(
            skipped[0].1.contains("ERROR [SECURITY]"),
            "skip carries the scanner verdict: {}",
            skipped[0].1
        );
        assert!(
            !skipped[0].1.contains(&secret),
            "skip must not echo the secret: {}",
            skipped[0].1
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn clean_markdown_ingests() {
        let tmp = std::env::temp_dir().join(format!("ns-md-clean-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        let md = "# Notes\n\nclean documentation without credentials\n";
        std::fs::write(tmp.join("guide.md"), md).expect("write clean markdown");
        let db = std::env::temp_dir().join(format!("ns-md-clean-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");
        let schema = ParserSchema::load(include_str!("../schema.json")).expect("schema");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "test-ns",
            None,
            &[],
        )
        .await
        .expect("clean markdown ingests");
        let node = generation_scoped_id("test-ns", 1, "guide.md");
        assert!(store.get("test-ns", &node).await.expect("read").is_some());
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// A skip for a path that was never indexed is not a change. Without this,
    /// one permanently rejected fixture forces a full carry on every ingest.
    #[tokio::test]
    async fn skipped_fixture_does_not_force_new_generation() {
        let tmp = std::env::temp_dir().join(format!("ns-skip-unchanged-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("good.rs"), "fn clean() {}").expect("write clean file");
        let secret = format!("ghp_{}", "Ab1".repeat(12));
        std::fs::write(tmp.join("skip.rs"), format!("let key = \"{secret}\";"))
            .expect("write skipped file");
        let db =
            std::env::temp_dir().join(format!("ns-skip-unchanged-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("test-ns").await.expect("create the schema");
        let schema = ParserSchema::load(include_str!("../schema.json")).expect("schema");
        let ingest = || {
            ingest_directory(
                &tmp,
                &schema,
                Arc::new(ZeroEmbedder),
                store.clone(),
                "test-ns",
                None,
                &[],
            )
        };
        ingest().await.expect("first ingest");
        let key = source_key_for(&tmp);
        assert_eq!(
            store
                .active_generation("test-ns", &key)
                .await
                .expect("read"),
            1
        );
        ingest().await.expect("second ingest");
        assert_eq!(
            store
                .active_generation("test-ns", &key)
                .await
                .expect("read"),
            1,
            "a never-indexed skipped fixture must not force a new generation"
        );
        let skipped_row = generation_scoped_id("test-ns", 1, "skip.rs");
        assert!(store
            .get("test-ns", &skipped_row)
            .await
            .expect("read")
            .is_none());
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// Which carry-forward store call a `CarryFailingStore` fault hits.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum CarryFault {
        Upsert,
        Get,
        List,
        /// Fail the first `generation_summary` call only, modelling a transient
        /// timeout: the later call (if the code reaches the carry pass) works.
        Summary,
    }

    /// Delegating store double whose carry-forward store calls fail while
    /// `fail` is set, so a test can break the carry-forward pass -- the
    /// previous-generation list, the row fetch, or the re-stage upsert --
    /// that runs between staging and publication and is an unchanged file's
    /// only route into the new generation.
    struct CarryFailingStore {
        inner: Arc<dyn VectorStore>,
        fail: std::sync::atomic::AtomicBool,
        fault: CarryFault,
        /// How many `generation_summary` calls the ingest has made. The
        /// pre-pass call is the first; the carry pass makes the second.
        summary_calls: std::sync::atomic::AtomicUsize,
        /// Only re-stage upserts whose scoped id carries this tail marker
        /// fail: the consumer also upserts the generation-scoped source node
        /// on every ingest, and failing that one would abort the ingest in
        /// `run_consumer` instead of exercising the carry-forward pass.
        upsert_marker: &'static str,
    }

    impl CarryFailingStore {
        fn armed(&self, fault: CarryFault) -> bool {
            self.fault == fault && self.fail.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl crate::traits::AsAny for CarryFailingStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[async_trait::async_trait]
    impl VectorStore for CarryFailingStore {
        async fn init(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.init(namespace).await
        }

        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            // Only re-staging a carried row into a later generation fails;
            // unscoped bookkeeping writes are never disturbed.
            if self.armed(CarryFault::Upsert)
                && id.contains(self.upsert_marker)
                && matches!(split_scoped_id(id), Some((_, gen, _)) if gen >= 2)
            {
                anyhow::bail!("injected carry upsert failure (test double)");
            }
            self.inner.upsert(namespace, id, vector, payload).await
        }

        async fn search(
            &self,
            namespace: &str,
            vector: Vec<f32>,
            limit: usize,
            include_tasks: bool,
            include_archived: bool,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner
                .search(namespace, vector, limit, include_tasks, include_archived)
                .await
        }

        async fn delete(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.delete(namespace, id).await
        }

        async fn clear_ingested(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.clear_ingested(namespace).await
        }

        async fn relink_edges(&self, namespace: &str, source_key: &str) -> anyhow::Result<usize> {
            self.inner.relink_edges(namespace, source_key).await
        }

        async fn begin_generation(&self, namespace: &str, source_key: &str) -> anyhow::Result<u64> {
            self.inner.begin_generation(namespace, source_key).await
        }

        async fn publish_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .publish_generation(namespace, source_key, generation)
                .await
        }

        async fn abort_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .abort_generation(namespace, source_key, generation)
                .await
        }

        async fn active_generation(
            &self,
            namespace: &str,
            source_key: &str,
        ) -> anyhow::Result<u64> {
            self.inner.active_generation(namespace, source_key).await
        }

        async fn generation_nodes(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<String>> {
            self.inner.generation_nodes(namespace, generation).await
        }

        async fn list(
            &self,
            namespace: &str,
            user_id: Option<&str>,
            memory_type: Option<&str>,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            if self.armed(CarryFault::List) {
                anyhow::bail!("injected carry list failure (test double)");
            }
            self.inner.list(namespace, user_id, memory_type).await
        }

        async fn generation_summary(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<crate::traits::GenerationRow>> {
            let call = self
                .summary_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // The List fault targets the carry pass's summary read: the
            // pre-pass read must succeed, or the new "a failed summary trips
            // the version gate" rule would re-parse and never reach carry.
            if self.armed(CarryFault::List) && call > 0 {
                anyhow::bail!("injected carry summary failure (test double)");
            }
            if self.armed(CarryFault::Summary) {
                // Fail once, then recover: a transient timeout, not an outage.
                self.fail.store(false, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("injected transient summary failure (test double)");
            }
            self.inner.generation_summary(namespace, generation).await
        }

        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            // The only generation-scoped fetches in the ingest flow are the
            // carry-forward pass and the DEPENDS_ON stamping after it; the
            // carry fetch is the first of them.
            if self.armed(CarryFault::Get) && split_scoped_id(id).is_some() {
                anyhow::bail!("injected carry get failure (test double)");
            }
            self.inner.get(namespace, id).await
        }

        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.inner.relocate(id, from, to).await
        }

        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }

        async fn export_graph(
            &self,
            include_retired: bool,
            include_archived: bool,
            project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
            anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner
                .export_graph(
                    include_retired,
                    include_archived,
                    project_contexts,
                    anchor_override,
                )
                .await
        }

        async fn increment_access_count(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.increment_access_count(namespace, id).await
        }

        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }

        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.inner.checkpoint().await
        }
    }

    /// Delegating store double whose `relink_edges` fails while `fail` is
    /// armed, so a test can break the pass that runs after publication and
    /// prove the ingest rolls the pointer back instead of leaving a published
    /// generation without its rule edges (issue #417).
    struct RelinkFailingStore {
        inner: Arc<dyn VectorStore>,
        fail: std::sync::atomic::AtomicBool,
    }

    impl RelinkFailingStore {
        fn armed(&self) -> bool {
            self.fail.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl crate::traits::AsAny for RelinkFailingStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[async_trait::async_trait]
    impl VectorStore for RelinkFailingStore {
        async fn init(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.init(namespace).await
        }

        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            self.inner.upsert(namespace, id, vector, payload).await
        }

        async fn search(
            &self,
            namespace: &str,
            vector: Vec<f32>,
            limit: usize,
            include_tasks: bool,
            include_archived: bool,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner
                .search(namespace, vector, limit, include_tasks, include_archived)
                .await
        }

        async fn delete(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.delete(namespace, id).await
        }

        async fn clear_ingested(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.clear_ingested(namespace).await
        }

        async fn relink_edges(&self, namespace: &str, source_key: &str) -> anyhow::Result<usize> {
            if self.armed() {
                self.fail.store(false, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("injected relink failure (test double)");
            }
            self.inner.relink_edges(namespace, source_key).await
        }

        async fn begin_generation(&self, namespace: &str, source_key: &str) -> anyhow::Result<u64> {
            self.inner.begin_generation(namespace, source_key).await
        }

        async fn publish_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .publish_generation(namespace, source_key, generation)
                .await
        }

        async fn abort_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            self.inner
                .abort_generation(namespace, source_key, generation)
                .await
        }

        async fn active_generation(
            &self,
            namespace: &str,
            source_key: &str,
        ) -> anyhow::Result<u64> {
            self.inner.active_generation(namespace, source_key).await
        }

        async fn generation_nodes(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<String>> {
            self.inner.generation_nodes(namespace, generation).await
        }

        async fn list(
            &self,
            namespace: &str,
            user_id: Option<&str>,
            memory_type: Option<&str>,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner.list(namespace, user_id, memory_type).await
        }

        async fn generation_summary(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<crate::traits::GenerationRow>> {
            self.inner.generation_summary(namespace, generation).await
        }

        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            self.inner.get(namespace, id).await
        }

        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.inner.relocate(id, from, to).await
        }

        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }

        async fn export_graph(
            &self,
            include_retired: bool,
            include_archived: bool,
            project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
            anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner
                .export_graph(
                    include_retired,
                    include_archived,
                    project_contexts,
                    anchor_override,
                )
                .await
        }

        async fn increment_access_count(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.increment_access_count(namespace, id).await
        }

        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }

        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.inner.checkpoint().await
        }
    }

    /// Delegating store double for the worst relink-rollback case: the
    /// post-publication relink fails, the restore of the previous generation
    /// fails, and the abort of the failed generation fails too (issue #417).
    ///
    /// The poison targets are fixed deliberately: the driver publishes
    /// generation 1 before arming, so the failed generation is 2 and the
    /// restore target is 1.
    struct RelinkAndRollbackFailingStore {
        inner: Arc<dyn VectorStore>,
        fail_relink: std::sync::atomic::AtomicBool,
        fail_restore: std::sync::atomic::AtomicBool,
        fail_abort: std::sync::atomic::AtomicBool,
    }

    impl crate::traits::AsAny for RelinkAndRollbackFailingStore {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[async_trait::async_trait]
    impl VectorStore for RelinkAndRollbackFailingStore {
        async fn init(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.init(namespace).await
        }

        async fn upsert(
            &self,
            namespace: &str,
            id: &str,
            vector: Vec<f32>,
            payload: MemoryPayload,
        ) -> anyhow::Result<crate::traits::UpsertReport> {
            self.inner.upsert(namespace, id, vector, payload).await
        }

        async fn search(
            &self,
            namespace: &str,
            vector: Vec<f32>,
            limit: usize,
            include_tasks: bool,
            include_archived: bool,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner
                .search(namespace, vector, limit, include_tasks, include_archived)
                .await
        }

        async fn delete(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.delete(namespace, id).await
        }

        async fn clear_ingested(&self, namespace: &str) -> anyhow::Result<()> {
            self.inner.clear_ingested(namespace).await
        }

        async fn relink_edges(&self, namespace: &str, source_key: &str) -> anyhow::Result<usize> {
            if self
                .fail_relink
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                anyhow::bail!("injected relink failure (test double)");
            }
            self.inner.relink_edges(namespace, source_key).await
        }

        async fn begin_generation(&self, namespace: &str, source_key: &str) -> anyhow::Result<u64> {
            self.inner.begin_generation(namespace, source_key).await
        }

        async fn publish_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            // Generation 1 is only ever published as the rollback restore
            // here: generation 1's original publication happened before the
            // double was armed.
            if generation == 1 && self.fail_restore.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("injected rollback restore failure (test double)");
            }
            self.inner
                .publish_generation(namespace, source_key, generation)
                .await
        }

        async fn abort_generation(
            &self,
            namespace: &str,
            source_key: &str,
            generation: u64,
        ) -> anyhow::Result<()> {
            if generation == 2 && self.fail_abort.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("injected rollback abort failure (test double)");
            }
            self.inner
                .abort_generation(namespace, source_key, generation)
                .await
        }

        async fn active_generation(
            &self,
            namespace: &str,
            source_key: &str,
        ) -> anyhow::Result<u64> {
            self.inner.active_generation(namespace, source_key).await
        }

        async fn generation_nodes(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<String>> {
            self.inner.generation_nodes(namespace, generation).await
        }

        async fn list(
            &self,
            namespace: &str,
            user_id: Option<&str>,
            memory_type: Option<&str>,
        ) -> anyhow::Result<Vec<crate::traits::SearchResult>> {
            self.inner.list(namespace, user_id, memory_type).await
        }

        async fn generation_summary(
            &self,
            namespace: &str,
            generation: u64,
        ) -> anyhow::Result<Vec<crate::traits::GenerationRow>> {
            self.inner.generation_summary(namespace, generation).await
        }

        async fn get(
            &self,
            namespace: &str,
            id: &str,
        ) -> anyhow::Result<Option<(Vec<f32>, MemoryPayload)>> {
            self.inner.get(namespace, id).await
        }

        async fn relocate(
            &self,
            id: &str,
            from: &str,
            to: &str,
        ) -> anyhow::Result<crate::traits::RelocateOutcome> {
            self.inner.relocate(id, from, to).await
        }

        async fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
            self.inner.list_namespaces().await
        }

        async fn export_graph(
            &self,
            include_retired: bool,
            include_archived: bool,
            project_contexts: &std::collections::HashMap<String, std::path::PathBuf>,
            anchor_override: Option<&crate::source_context::ScopedAnchorOverride>,
        ) -> anyhow::Result<serde_json::Value> {
            self.inner
                .export_graph(
                    include_retired,
                    include_archived,
                    project_contexts,
                    anchor_override,
                )
                .await
        }

        async fn increment_access_count(&self, namespace: &str, id: &str) -> anyhow::Result<()> {
            self.inner.increment_access_count(namespace, id).await
        }

        async fn export_database(&self, dir: &str) -> anyhow::Result<()> {
            self.inner.export_database(dir).await
        }

        async fn checkpoint(&self) -> anyhow::Result<()> {
            self.inner.checkpoint().await
        }
    }

    /// A relink failure happens after publication. Without a rollback the new
    /// generation stays active with none of its rule edges; the previous
    /// generation, which the store still holds, is the safe thing to restore.
    #[tokio::test]
    async fn a_failed_relink_rolls_back_to_the_previous_generation() {
        let db = std::env::temp_dir().join(format!("ns-relink-db-{}", uuid::Uuid::new_v4()));
        let inner: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let failing = Arc::new(RelinkFailingStore {
            inner: inner.clone(),
            fail: std::sync::atomic::AtomicBool::new(false),
        });
        let store: Arc<dyn VectorStore> = failing.clone();
        store.init("relink-ns").await.expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-relink-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn hello() {}").expect("write file");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&dir);

        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-ns",
            None,
            &[],
        )
        .await
        .expect("first ingest publishes generation 1");
        assert_eq!(
            inner
                .active_generation("relink-ns", &source_key)
                .await
                .expect("read"),
            1
        );

        // A changed file stages and publishes generation 2; the injected
        // relink failure then hits before the rule edges are rebuilt.
        std::fs::write(dir.join("hello.rs"), "fn hello_again() {}").expect("change file");
        failing
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let error = ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-ns",
            None,
            &[],
        )
        .await
        .expect_err("a failed relink fails the ingest");

        assert!(
            format!("{error}").contains("relink"),
            "the error names the relink failure: {error}"
        );
        assert_eq!(
            inner
                .active_generation("relink-ns", &source_key)
                .await
                .expect("read"),
            1,
            "the previous generation is restored after a failed relink"
        );
        assert!(
            inner
                .generation_nodes("relink-ns", 2)
                .await
                .expect("read")
                .is_empty(),
            "the generation that failed relink is aborted, not left staged"
        );
        let gen1_id = generation_scoped_id("relink-ns", 1, "hello.rs");
        assert!(
            inner
                .get("relink-ns", &gen1_id)
                .await
                .expect("read")
                .is_some(),
            "the restored generation is still reachable"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// On a first ingest there is no earlier generation to restore. The
    /// documented post-failure state is a clean empty namespace: no active
    /// generation and none of generation 1's rows or staging marker left
    /// behind, so a retry starts from zero rather than half a tree (#417).
    #[tokio::test]
    async fn a_failed_relink_on_the_first_ingest_leaves_no_generation_active() {
        let db = std::env::temp_dir().join(format!("ns-relink-first-db-{}", uuid::Uuid::new_v4()));
        let concrete =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let inner: Arc<dyn VectorStore> = concrete.clone();
        let failing = Arc::new(RelinkFailingStore {
            inner: inner.clone(),
            fail: std::sync::atomic::AtomicBool::new(false),
        });
        let store: Arc<dyn VectorStore> = failing.clone();
        store
            .init("relink-first-ns")
            .await
            .expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-relink-first-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn hello() {}").expect("write file");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");

        // Armed for the first ingest: generation 1 publishes, then relink
        // fails with no previous generation to fall back to.
        failing
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let error = ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-first-ns",
            None,
            &[],
        )
        .await
        .expect_err("a failed relink fails the first ingest");

        let text = format!("{error}");
        assert!(
            text.contains("no prior generation"),
            "the error documents the no-prior-generation rollback state: {text}"
        );
        assert!(
            text.contains("injected relink failure"),
            "the error keeps the relink failure: {text}"
        );
        assert_eq!(
            inner
                .active_generation("relink-first-ns", &source_key_for(&dir))
                .await
                .expect("read"),
            0,
            "a first ingest whose relink fails leaves no active generation"
        );
        assert!(
            inner
                .generation_nodes("relink-first-ns", 1)
                .await
                .expect("read")
                .is_empty(),
            "none of the failed generation's nodes remain reachable"
        );
        assert!(
            inner
                .get(
                    "relink-first-ns",
                    &generation_scoped_id("relink-first-ns", 1, "hello.rs")
                )
                .await
                .expect("read")
                .is_none(),
            "the failed generation's rows are gone, not merely unreferenced"
        );
        assert!(
            concrete
                .staging_generations_for_test("relink-first-ns")
                .await
                .expect("read staging markers")
                .is_empty(),
            "the failed generation's staging marker is removed"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// An unchanged ingest never moves the pointer: its staging generation is
    /// discarded before publication. When relink then fails, the previously
    /// active generation must stay active and the discarded generation must
    /// leave no staging marker behind (#417).
    #[tokio::test]
    async fn a_failed_relink_on_an_unchanged_ingest_leaves_it_active_and_no_staging() {
        let db = std::env::temp_dir().join(format!("ns-relink-same-db-{}", uuid::Uuid::new_v4()));
        let concrete =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let inner: Arc<dyn VectorStore> = concrete.clone();
        let failing = Arc::new(RelinkFailingStore {
            inner: inner.clone(),
            fail: std::sync::atomic::AtomicBool::new(false),
        });
        let store: Arc<dyn VectorStore> = failing.clone();
        store
            .init("relink-same-ns")
            .await
            .expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-relink-same-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn hello() {}").expect("write file");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");

        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-same-ns",
            None,
            &[],
        )
        .await
        .expect("first ingest publishes generation 1");
        let source_key = source_key_for(&dir);

        // No file changes: the second ingest stages nothing, discards its
        // generation, and then hits the injected relink failure.
        failing
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let error = ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-same-ns",
            None,
            &[],
        )
        .await
        .expect_err("a failed relink fails the unchanged ingest");

        let text = format!("{error}");
        assert!(
            text.contains("unchanged") && text.contains("active generation 1"),
            "the error documents the unchanged-generation state: {text}"
        );
        assert!(
            text.contains("injected relink failure"),
            "the error keeps the relink failure: {text}"
        );
        assert_eq!(
            inner
                .active_generation("relink-same-ns", &source_key)
                .await
                .expect("read"),
            1,
            "the unchanged active generation is not rolled back"
        );
        assert!(
            inner
                .generation_nodes("relink-same-ns", 2)
                .await
                .expect("read")
                .is_empty(),
            "the discarded generation has no rows"
        );
        assert!(
            concrete
                .staging_generations_for_test("relink-same-ns")
                .await
                .expect("read staging markers")
                .is_empty(),
            "the discarded generation leaks no staging marker"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// When restoring the previous generation fails, the error must name both
    /// the relink failure and the restore failure, and the failed generation
    /// must NOT be aborted: aborting clears the active pointer and leaves every
    /// reader with no tree at all (#417).
    #[tokio::test]
    async fn a_failed_relink_reports_the_rollback_failures_too() {
        let db =
            std::env::temp_dir().join(format!("ns-relink-combined-db-{}", uuid::Uuid::new_v4()));
        let inner: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let failing = Arc::new(RelinkAndRollbackFailingStore {
            inner: inner.clone(),
            fail_relink: std::sync::atomic::AtomicBool::new(false),
            fail_restore: std::sync::atomic::AtomicBool::new(false),
            fail_abort: std::sync::atomic::AtomicBool::new(false),
        });
        let store: Arc<dyn VectorStore> = failing.clone();
        store
            .init("relink-combined-ns")
            .await
            .expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-relink-combined-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn hello() {}").expect("write file");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&dir);

        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-combined-ns",
            None,
            &[],
        )
        .await
        .expect("first ingest publishes generation 1");

        std::fs::write(dir.join("hello.rs"), "fn hello_again() {}").expect("change file");
        failing
            .fail_relink
            .store(true, std::sync::atomic::Ordering::SeqCst);
        failing
            .fail_restore
            .store(true, std::sync::atomic::Ordering::SeqCst);
        failing
            .fail_abort
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let error = ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "relink-combined-ns",
            None,
            &[],
        )
        .await
        .expect_err("a failed relink with a failed rollback fails the ingest");

        let text = format!("{error}");
        for expected in [
            "injected relink failure",
            "could not restore generation 1",
            "injected rollback restore failure",
            "generation 2 was not aborted",
        ] {
            assert!(
                text.contains(expected),
                "the combined error names {expected:?}: {text}"
            );
        }
        assert_eq!(
            inner
                .active_generation("relink-combined-ns", &source_key)
                .await
                .expect("read"),
            2,
            "with both rollback steps failing the unrelinked generation stays active"
        );
        assert!(
            !inner
                .generation_nodes("relink-combined-ns", 2)
                .await
                .expect("read")
                .is_empty(),
            "the failed abort also leaves generation 2's rows in place"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// Shared driver for the carry-fault tests: publish generation 1 of a
    /// one-file directory, arm one carry-forward fault, then ingest the
    /// unchanged directory again. The unchanged file is skipped by the
    /// producer, so the carry-forward pass is its only route into generation
    /// 2 -- exactly the route the fault breaks.
    async fn ingest_twice_with_carry_fault(
        fault: CarryFault,
    ) -> (
        anyhow::Result<()>,
        Arc<dyn VectorStore>,
        String,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let db = std::env::temp_dir().join(format!("ns-carry-db-{}", uuid::Uuid::new_v4()));
        let inner: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let failing = Arc::new(CarryFailingStore {
            inner: inner.clone(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fault,
            summary_calls: std::sync::atomic::AtomicUsize::new(0),
            upsert_marker: "hello.rs",
        });
        let store: Arc<dyn VectorStore> = failing.clone();
        store.init("carry-ns").await.expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-carry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn main() {}").expect("write file");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        let source_key = source_key_for(&dir);

        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "carry-ns",
            None,
            &[],
        )
        .await
        .expect("first ingest publishes generation 1");
        assert_eq!(
            inner
                .active_generation("carry-ns", &source_key)
                .await
                .expect("read"),
            1,
            "the first ingest publishes generation 1"
        );

        // Make the namespace changed so the carry pass runs; `hello.rs` itself
        // is unchanged and is the row the fault injection targets.
        std::fs::write(dir.join("other.rs"), "fn other() {}").expect("write changed file");
        failing
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let outcome = ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "carry-ns",
            None,
            &[],
        )
        .await;
        (outcome, inner, source_key, dir, db)
    }

    /// The assertions every carry fault must satisfy: the ingest fails naming
    /// the carry-forward failure, and generation 1 stays active with its file
    /// node reachable -- nothing is published over a hole.
    fn assert_carry_fault_aborted(outcome: anyhow::Result<()>) -> anyhow::Error {
        let err = outcome.expect_err("a failed carry-forward step must fail the ingest");
        assert!(
            format!("{}", err).contains("carry-forward"),
            "the error names the carry-forward failure: {}",
            err
        );
        err
    }

    async fn assert_generation_1_survives(inner: &Arc<dyn VectorStore>, source_key: &str) {
        assert_eq!(
            inner
                .active_generation("carry-ns", source_key)
                .await
                .expect("read"),
            1,
            "the previous generation stays active after a failed carry"
        );
        let gen1_id = generation_scoped_id("carry-ns", 1, "hello.rs");
        assert!(
            inner
                .get("carry-ns", &gen1_id)
                .await
                .expect("read")
                .is_some(),
            "generation 1's file node remains reachable"
        );
    }

    #[tokio::test]
    async fn par04_failed_carry_forward_upsert_aborts_the_ingest() {
        let (outcome, inner, source_key, dir, db) =
            ingest_twice_with_carry_fault(CarryFault::Upsert).await;
        assert_carry_fault_aborted(outcome);
        assert_generation_1_survives(&inner, &source_key).await;
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn par04_failed_carry_forward_list_aborts_the_ingest() {
        // Largest blast radius: a failed list of the previous generation's
        // rows used to carry nothing at all and still publish.
        let (outcome, inner, source_key, dir, db) =
            ingest_twice_with_carry_fault(CarryFault::List).await;
        assert_carry_fault_aborted(outcome);
        assert_generation_1_survives(&inner, &source_key).await;
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn par04_failed_carry_forward_get_aborts_the_ingest() {
        // At the row fetch: a failed get used to drop that node from the new
        // generation and still publish.
        let (outcome, inner, source_key, dir, db) =
            ingest_twice_with_carry_fault(CarryFault::Get).await;
        assert_carry_fault_aborted(outcome);
        assert_generation_1_survives(&inner, &source_key).await;
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// A transient summary failure used to leave `prev_stamps` empty, which
    /// read as "no offending stamp": the version gate stayed off, the producer
    /// skipped unchanged files, and the carry pass (whose second summary
    /// succeeds) copied the pre-fix rows over the fresh ones and stamped them
    /// current. The failure must now trip the gate and re-parse.
    #[tokio::test]
    async fn transient_summary_failure_forces_reparse() {
        let db = std::env::temp_dir().join(format!("ns-transient-db-{}", uuid::Uuid::new_v4()));
        let inner: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        let failing = Arc::new(CarryFailingStore {
            inner: inner.clone(),
            fail: std::sync::atomic::AtomicBool::new(false),
            fault: CarryFault::Summary,
            summary_calls: std::sync::atomic::AtomicUsize::new(0),
            upsert_marker: "hello.rs",
        });
        let store: Arc<dyn VectorStore> = failing.clone();
        store.init("probe").await.expect("create the schema");

        let dir = std::env::temp_dir().join(format!("ns-transient-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("hello.rs"), "fn hello() {}").expect("write file");
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");
        let key = source_key_for(&dir);
        let gen1 = inner.active_generation("probe", &key).await.expect("gen1");

        // Simulate a pre-fix extraction: stale content, an old version stamp,
        // and the file hash unchanged (so the producer would skip it).
        let mut mutated = 0usize;
        for row in inner.list("probe", None, None).await.expect("list") {
            let Some((_, generation, _)) = split_scoped_id(&row.id) else {
                continue;
            };
            if generation != gen1 {
                continue;
            }
            let Some((vector, mut payload)) = inner.get("probe", &row.id).await.expect("read")
            else {
                continue;
            };
            payload.content = "STALE PRE-FIX EXTRACTION".to_string();
            if let Some(obj) = payload.metadata.as_object_mut() {
                obj.insert(
                    "ingest_version".to_string(),
                    serde_json::json!("0.0.0-stale"),
                );
            }
            inner
                .upsert("probe", &row.id, vector, payload)
                .await
                .expect("mutate the previous generation");
            mutated += 1;
        }
        assert!(mutated > 0, "the fixture mutated the previous generation");

        // The tree changed (a new file), so the carry pass would run; the
        // summary fails once before it can answer.
        failing
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(dir.join("other.rs"), "fn other() {}").expect("write changed file");
        ingest_directory(
            &dir,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("the re-parse ingests");

        let gen2 = inner.active_generation("probe", &key).await.expect("gen2");
        assert!(gen2 > gen1, "a changed tree publishes a new generation");
        let current: Vec<_> = inner
            .list("probe", None, None)
            .await
            .expect("list")
            .into_iter()
            .filter(|r| split_scoped_id(&r.id).is_some_and(|(_, g, _)| g == gen2))
            .collect();
        assert!(!current.is_empty(), "the current generation has rows");
        assert!(
            current
                .iter()
                .all(|r| r.payload.content != "STALE PRE-FIX EXTRACTION"),
            "a transient summary failure must not carry pre-fix rows into the current generation: {:?}",
            current
                .iter()
                .filter(|r| r.payload.content == "STALE PRE-FIX EXTRACTION")
                .map(|r| r.id.clone())
                .collect::<Vec<_>>()
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// The read helper rejects a path that is not a readable file, under every
    /// uid: a directory read is EISDIR even for root.
    #[test]
    fn read_ingest_file_rejects_a_directory() {
        let dir = std::env::temp_dir().join(format!("ns-read-dir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let err = read_ingest_file(&dir).expect_err("a directory is not a file");
        assert!(
            format!("{err}").contains("failed to read"),
            "the error names the read failure: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file the walk found but cannot read aborts the ingest instead of
    /// publishing a generation with its content silently missing.
    #[cfg(unix)]
    #[tokio::test]
    async fn read_failure_aborts_generation() {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, db, store, schema) = kind_fixture("read-failure").await;
        let file = tmp.join("locked.rs");
        std::fs::write(&file, "fn locked() {}").expect("write file");
        let mut perms = std::fs::metadata(&file).expect("stat").permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&file, perms).expect("chmod 000");

        // A root test process can read a mode-000 file (DAC override). Check
        // the fixture before asserting on it; the helper test above still
        // proves the error path in that environment.
        let fixture_unreadable = std::fs::read(&file).is_err();
        if fixture_unreadable {
            let err = ingest_directory(
                &tmp,
                &schema,
                Arc::new(ZeroEmbedder),
                store.clone(),
                "probe",
                None,
                &[],
            )
            .await
            .expect_err("an unreadable file must abort the ingest");
            assert!(
                format!("{err}").contains("failed to read"),
                "the error names the read failure: {err}"
            );
            assert_eq!(
                store
                    .active_generation("probe", &source_key_for(&tmp))
                    .await
                    .expect("read"),
                0,
                "no generation is published"
            );
        }

        let mut perms = std::fs::metadata(&file).expect("stat").permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&file, perms).ok();
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// The file-to-directory invariant: after the path changes kind, the new
    /// generation serves the directory node and none of the deleted file's
    /// rows.
    #[tokio::test]
    async fn file_to_directory_asserts_old_rows_dead() {
        let (tmp, db, store, schema) = kind_fixture("file-to-dir-dead").await;
        std::fs::write(tmp.join("thing.rs"), "pub fn gone() {}").expect("write file");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("first ingest");
        let key = source_key_for(&tmp);

        std::fs::remove_file(tmp.join("thing.rs")).expect("remove file");
        std::fs::create_dir_all(tmp.join("thing.rs")).expect("create directory");
        ingest_directory(
            &tmp,
            &schema,
            Arc::new(ZeroEmbedder),
            store.clone(),
            "probe",
            None,
            &[],
        )
        .await
        .expect("second ingest");
        let gen2 = store.active_generation("probe", &key).await.expect("gen2");

        let rows: Vec<_> = store
            .list("probe", None, None)
            .await
            .expect("list")
            .into_iter()
            .filter(|r| split_scoped_id(&r.id).is_some_and(|(_, g, _)| g == gen2))
            .collect();
        assert!(
            rows.iter()
                .any(|r| r.id == generation_scoped_id("probe", gen2, "thing.rs")
                    && r.payload.memory_type == "directory"),
            "the directory node is current"
        );
        assert!(
            rows.iter().all(|r| r.payload.location != "thing.rs"
                || r.payload.memory_type == "directory"),
            "no deleted file row is served: {:?}",
            rows.iter()
                .filter(|r| r.payload.location == "thing.rs" && r.payload.memory_type != "directory")
                .map(|r| r.id.clone())
                .collect::<Vec<_>>()
        );
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// Records every embed_batch payload so tests can observe how the
    /// consumer batches symbol and doc-section embeddings.
    struct BatchRecordingEmbedder {
        batch_calls: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }

    #[async_trait::async_trait]
    impl Embedder for BatchRecordingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.0; 4])
        }

        async fn embed_batch(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.batch_calls.lock().unwrap().push(texts.to_vec());
            Ok(texts.iter().map(|_| vec![0.0; 4]).collect())
        }

        fn dimensions(&self) -> usize {
            4
        }
    }

    fn batch_recording_embedder() -> (Arc<dyn Embedder>, Arc<std::sync::Mutex<Vec<Vec<String>>>>) {
        let batch_calls = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let embedder: Arc<dyn Embedder> = Arc::new(BatchRecordingEmbedder {
            batch_calls: Arc::clone(&batch_calls),
        });
        (embedder, batch_calls)
    }

    /// The producer compiles one tree-sitter query per schema entry up front
    /// instead of recompiling the pattern for every file.
    #[test]
    fn compile_language_queries_compiles_every_configured_query() {
        let schema = ParserSchema::load(
            r#"{"languages":{"rust":{"extensions":["rs"],"queries":{"functions":"(function_item name: (identifier) @name) @func"}}}}"#,
        )
        .expect("the fixture schema parses");
        let compiled = compile_language_queries(&schema);
        assert!(
            compiled.contains_key("rust:functions"),
            "the compiled cache is keyed by language:query, got {compiled:?}"
        );
    }

    /// One file carrying several symbols is embedded as a single batch, not one
    /// embed call per symbol.
    #[tokio::test]
    async fn ingest_batches_multi_symbol_files_into_one_embed_batch() {
        let tmp = std::env::temp_dir().join(format!("ns-batch-symbols-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(tmp.join("lib.rs"), "pub fn alpha() {}\npub fn beta() {}\n")
            .expect("write rust file");
        let db = std::env::temp_dir().join(format!("ns-batch-symbols-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("batch-ns").await.expect("create the schema");
        let (embedder, batch_calls) = batch_recording_embedder();
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        ingest_directory(
            &tmp,
            &schema,
            embedder,
            store.clone(),
            "batch-ns",
            None,
            &[],
        )
        .await
        .expect("ingest batches the multi-symbol file");

        let batches = batch_calls.lock().unwrap();
        assert_eq!(
            batches.len(),
            1,
            "one file with symbols is one batch: {batches:?}"
        );
        assert_eq!(
            batches[0].len(),
            2,
            "both symbols share one batch: {batches:?}"
        );
        assert!(
            batches[0][0].contains("alpha"),
            "the batch carries the first symbol: {}",
            batches[0][0]
        );
        assert!(
            batches[0][1].contains("beta"),
            "the batch carries the second symbol: {}",
            batches[0][1]
        );
        drop(batches);

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    /// One markdown file carrying several sections is embedded as a single
    /// batch.
    #[tokio::test]
    async fn ingest_batches_markdown_sections_into_one_embed_batch() {
        let tmp = std::env::temp_dir().join(format!("ns-batch-md-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        std::fs::write(
            tmp.join("guide.md"),
            "# Guide\n\nIntro.\n\n## Setup\n\nSetup.\n\n## Usage\n\nUsage.",
        )
        .expect("write markdown");
        let db = std::env::temp_dir().join(format!("ns-batch-md-db-{}", uuid::Uuid::new_v4()));
        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).expect("open temp database"));
        store.init("batch-ns").await.expect("create the schema");
        let (embedder, batch_calls) = batch_recording_embedder();
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");
        ingest_directory(
            &tmp,
            &schema,
            embedder,
            store.clone(),
            "batch-ns",
            None,
            &[],
        )
        .await
        .expect("ingest batches the multi-section markdown");

        let batches = batch_calls.lock().unwrap();
        assert_eq!(
            batches.len(),
            1,
            "one markdown file is one batch: {batches:?}"
        );
        assert_eq!(
            batches[0].len(),
            3,
            "all sections share one batch: {batches:?}"
        );
        drop(batches);

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    #[tokio::test]
    async fn incremental_ingest_clears_dangling_cross_file_depends_on_when_callee_removed() {
        let tmp = std::env::temp_dir().join(format!(
            "ns-inc-callee-removed-src-{}",
            uuid::Uuid::new_v4()
        ));
        let db =
            std::env::temp_dir().join(format!("ns-inc-callee-removed-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        std::fs::write(
            tmp.join("caller.rs"),
            "use crate::callee::compute_target;\npub fn invoke_caller() { compute_target(); }\n",
        )
        .unwrap();
        std::fs::write(tmp.join("callee.rs"), "pub fn compute_target() {}\n").unwrap();

        let store: Arc<dyn VectorStore> =
            Arc::new(crate::store::LadybugStore::for_testing(&db, 4).unwrap());
        store.init("inc-deps-ns").await.unwrap();
        let (embedder, _) = batch_recording_embedder();
        let schema =
            ParserSchema::load(include_str!("../schema.json")).expect("shipped schema parses");

        ingest_directory(
            &tmp,
            &schema,
            embedder.clone(),
            store.clone(),
            "inc-deps-ns",
            None,
            &[],
        )
        .await
        .expect("gen 1 ingest");

        let gen1_rows = store
            .list("inc-deps-ns", None, Some("code_ast"))
            .await
            .unwrap();
        let caller_gen1 = gen1_rows
            .iter()
            .find(|r| {
                r.payload.metadata.get("symbol").and_then(|v| v.as_str()) == Some("invoke_caller")
            })
            .expect("invoke_caller in gen 1");
        let gen1_deps = caller_gen1
            .payload
            .metadata
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(gen1_deps, 1, "gen 1 caller must depend on compute_target");

        // Rename `compute_target` in `callee.rs` without touching `caller.rs`.
        std::fs::write(tmp.join("callee.rs"), "pub fn renamed_other_fn() {}\n").unwrap();

        ingest_directory(
            &tmp,
            &schema,
            embedder,
            store.clone(),
            "inc-deps-ns",
            None,
            &[],
        )
        .await
        .expect("gen 2 incremental ingest");

        let gen2_rows = store
            .list("inc-deps-ns", None, Some("code_ast"))
            .await
            .unwrap();
        let caller_gen2 = gen2_rows
            .iter()
            .find(|r| {
                r.payload.metadata.get("symbol").and_then(|v| v.as_str()) == Some("invoke_caller")
            })
            .expect("invoke_caller carried into gen 2");
        let gen2_deps = caller_gen2
            .payload
            .metadata
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let gen2_calls_resolved = caller_gen2
            .payload
            .metadata
            .get("calls_resolved")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(
            gen2_deps, 0,
            "carried caller must not retain dangling depends_on after callee removal: {:?}",
            caller_gen2.payload.metadata
        );
        assert_eq!(
            gen2_calls_resolved, 0,
            "carried caller must not retain dangling calls_resolved after callee removal: {:?}",
            caller_gen2.payload.metadata
        );
        assert!(
            caller_gen2
                .payload
                .metadata
                .get("cross_file_callees")
                .is_none(),
            "carried caller must clear stale cross_file_callees"
        );

        std::fs::remove_dir_all(&tmp).ok();
        std::fs::remove_dir_all(&db).ok();
    }

    // Task 1: shared ingest eligibility predicate backing both the ingest
    // walker and the watcher event filter.

    #[test]
    fn eligibility_rejects_every_skip_dir_name() {
        let root = std::path::Path::new("/tmp/fresh-root");
        for skip_dir in SKIPPED_DIRS {
            let skipped_path = format!("/tmp/fresh-root/apps/gen/{}/src/lib.rs", skip_dir);
            assert!(
                !is_ingest_eligible_path(std::path::Path::new(&skipped_path), root),
                "skip dir {skip_dir} must be rejected: {skipped_path}"
            );
        }
    }

    /// Issue #355: daemon session/plugin state under `.NeuroStrata` is a
    /// self-write — the watcher must not treat it as an ingest trigger.
    #[test]
    fn eligibility_rejects_neurostrata_session_state() {
        let root = std::path::Path::new("/tmp/fresh-root");
        for rel in [
            ".NeuroStrata/session_queries.json",
            ".NeuroStrata/active_session.json",
            ".NeuroStrata/plugins/opencode/index.js",
            ".NeuroStrata/docs/a-rule.md",
        ] {
            let path = format!("/tmp/fresh-root/{rel}");
            assert!(
                !is_ingest_eligible_path(std::path::Path::new(&path), root),
                "daemon state {rel} must not be an ingest trigger"
            );
        }
        // Store-artifact behavior is unchanged: still rejected.
        let db_path = "/tmp/fresh-root/.NeuroStrata/store.db";
        assert!(!is_ingest_eligible_path(
            std::path::Path::new(db_path),
            root
        ));
        // Normal source outside the daemon state is still eligible.
        assert!(is_ingest_eligible_path(
            std::path::Path::new("/tmp/fresh-root/src/lib.rs"),
            root
        ));
    }

    #[test]
    fn eligibility_rejects_database_and_store_artifacts() {
        let root = std::path::Path::new("/tmp/fresh-root");
        for artifact in ["app.db", "app.wal", "app.lock", "app.checkpoint"] {
            let db_path = format!("/tmp/fresh-root/data/{}", artifact);
            assert!(
                !is_ingest_eligible_path(std::path::Path::new(&db_path), root),
                "store artifact {artifact} must be rejected"
            );
        }
    }

    #[test]
    fn eligibility_accepts_a_plain_source_file_under_root() {
        let root = std::path::Path::new("/tmp/fresh-root");
        assert!(is_ingest_eligible_path(
            std::path::Path::new("/tmp/fresh-root/src/lib.rs"),
            root
        ));
    }

    #[test]
    fn eligibility_rejects_paths_outside_root() {
        let root = std::path::Path::new("/tmp/fresh-root");
        assert!(!is_ingest_eligible_path(
            std::path::Path::new("/tmp/other/src/lib.rs"),
            root
        ));
    }

    #[test]
    fn eligibility_rejects_dotdot_escape_outside_root() {
        let root = std::path::Path::new("/tmp/fresh-root");
        assert!(!is_ingest_eligible_path(
            std::path::Path::new("/tmp/fresh-root/src/../../outside.rs"),
            root,
        ));
        assert!(!is_ingest_eligible_path(
            std::path::Path::new("/tmp/fresh-root/../../../etc/passwd"),
            root,
        ));
    }

    #[test]
    fn eligibility_accepts_relative_root_paths() {
        assert!(is_ingest_eligible_path(
            std::path::Path::new("src/lib.rs"),
            std::path::Path::new("src"),
        ));
        assert!(!is_ingest_eligible_path(
            std::path::Path::new("/tmp/fresh-root/src/lib.rs"),
            std::path::Path::new("."),
        ));
    }

    /// Windows-native spelling: `\` and `/` are both components to `Path`.
    #[cfg(windows)]
    #[test]
    fn skip_dirs_are_rejected_in_backslash_separated_paths() {
        let root = std::path::Path::new(r"C:\repo");
        for rel in [
            r".git\config",
            r"target\debug\x.rs",
            r"web\node_modules\a.js",
        ] {
            let path = format!(r"C:\repo\{rel}");
            assert!(
                !is_ingest_eligible_path(std::path::Path::new(&path), root),
                "{path} must be rejected"
            );
        }
        assert!(is_ingest_eligible_path(
            std::path::Path::new(r"C:\repo\src\lib.rs"),
            root
        ));
    }

    /// Review on #389: on Unix a backslash is an ordinary filename character.
    /// `src\vendor\module.rs` names one file, not a path inside `vendor`.
    #[cfg(not(windows))]
    #[test]
    fn backslashes_in_unix_filenames_do_not_imply_a_skipped_dir() {
        let root = std::path::Path::new("/tmp/fresh-root");
        assert!(is_ingest_eligible_path(
            std::path::Path::new("/tmp/fresh-root/src/vendor\\module.rs"),
            root
        ));
    }

    /// Review on #389: skipped directories are case-insensitive on Windows.
    #[cfg(windows)]
    #[test]
    fn skipped_dirs_match_case_insensitively_on_windows() {
        let root = std::path::Path::new(r"C:\repo");
        for rel in [
            r"TARGET\debug\x.rs",
            r".GIT\config",
            r"src\NODE_MODULES\a.js",
        ] {
            let path = format!(r"C:\repo\{rel}");
            assert!(
                !is_ingest_eligible_path(std::path::Path::new(&path), root),
                "{path} must be rejected"
            );
        }
    }

    /// Review on #389: the walked/watched path can be the skipped directory
    /// itself; a `/name/` substring test misses the final component.
    #[test]
    fn a_path_ending_at_a_skipped_dir_is_rejected() {
        let root = std::path::Path::new("/tmp/fresh-root");
        for skip_dir in SKIPPED_DIRS {
            let path = format!("/tmp/fresh-root/src/{skip_dir}");
            assert!(
                !is_ingest_eligible_path(std::path::Path::new(&path), root),
                "the skipped directory itself must be rejected: {path}"
            );
        }
    }

    /// Review on #386: `\server\share\repo\x` (rooted, no drive) is not
    /// under the UNC root `\\server\share\repo`, though both normalize to the
    /// same components.
    #[cfg(windows)]
    #[test]
    fn a_driveless_rooted_path_is_not_under_a_unc_root() {
        assert!(!is_ingest_eligible_path(
            std::path::Path::new(r"\server\share\repo\src\lib.rs"),
            std::path::Path::new(r"\\server\share\repo"),
        ));
    }
}
/// Dry-run ingest: walk the tree and produce a report without writing or
/// downloading anything.
pub async fn dry_run_directory(
    dir_path: &std::path::Path,
    schema: &crate::parser::schema::ParserSchema,
    mode: &str,
) -> anyhow::Result<String> {
    let ext_to_lang = build_ext_map(schema);

    let mut files = 0usize;
    let mut parseable = 0usize;
    let mut symbols = 0usize;
    let mut doc_sections = 0usize;
    let mut secret_hits = 0usize;
    let mut file_hashes: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut junk: Vec<String> = Vec::new();
    let mut pii_warnings: Vec<String> = Vec::new();
    let mut groups: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut all_file_paths: Vec<String> = Vec::new();
    let mut avoided_embeddings: usize = 0;
    let mut version_candidates: Vec<String> = Vec::new();

    // Pattern: version suffixes in file stem; used after loop too.
    let v_pattern = regex::Regex::new(r"(_v[0-9]+|-v[0-9]+|\.bak$|\.old$|_old$|_copy$)").unwrap();

    let walker = ignore::WalkBuilder::new(dir_path).build();
    for result in walker {
        let entry = match result {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("dry_run walk entry error: {e}");
                continue;
            }
        };
        let path = entry.path();
        let path_str = path.to_string_lossy();
        if !is_ingest_eligible_path(path, dir_path) {
            continue;
        }
        let is_file = entry.file_type().map_or(false, |ft| ft.is_file());
        if !is_file {
            continue;
        }
        files += 1;
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("dry_run failed to read {}: {e}", path.display());
                continue;
            }
        };
        let hash = sha256_hex(&bytes);
        file_hashes
            .entry(hash.clone())
            .or_default()
            .push(path_str.to_string());

        // Version-candidate detection: collect paths for post-loop analysis.
        all_file_paths.push(path_str.to_string());

        let lang_name = path
            .extension()
            .map(|e| normalize_ext(&e.to_string_lossy()))
            .and_then(|e| ext_to_lang.get(&e).cloned())
            .filter(|ln| get_language(ln).is_some());

        let content = String::from_utf8_lossy(&bytes).to_string();

        if crate::secrets::scan_entry_point(&content, &serde_json::json!({}), "dry_run").is_some() {
            secret_hits += 1;
        }

        if mode == "presort" {
            if content.contains('@')
                && content
                    .split_whitespace()
                    .any(|w| w.contains('@') && w.contains('.'))
            {
                pii_warnings.push(format!("{}: possible email", path_str));
            }
            let mut found_ssn = false;
            for window in content.as_bytes().windows(11) {
                let s = String::from_utf8_lossy(window);
                if s.chars().nth(3) == Some('-') && s.chars().nth(6) == Some('-') {
                    found_ssn = true;
                }
            }
            if found_ssn {
                pii_warnings.push(format!("{}: possible SSN", path_str));
            }
            let digits: String = content.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 16 {
                pii_warnings.push(format!("{}: possible credit card digits", path_str));
            }
            if content.trim().len() < 20 {
                junk.push(path_str.to_string());
            }
        }

        if let Some(parent) = path.parent() {
            let g = parent.to_string_lossy().to_string();
            *groups.entry(g).or_insert(0) += 1;
        }

        if path_str.ends_with(".md") {
            let secs = parse_markdown_sections(&content);
            doc_sections += secs.len();
            parseable += 1;
        } else if let Some(ln) = lang_name {
            if let Some(ts_lang) = get_language(&ln) {
                parseable += 1;
                let mut parser = tree_sitter::Parser::new();
                if parser.set_language(&ts_lang).is_ok() {
                    if let Some(tree) = parser.parse(&content, None) {
                        let schema_lang = &schema.languages[&ln];
                        for (_qname, qstr) in &schema_lang.queries {
                            if let Ok(q) = tree_sitter::Query::new(&ts_lang, qstr) {
                                let mut cursor = tree_sitter::QueryCursor::new();
                                let mut iter =
                                    cursor.matches(&q, tree.root_node(), content.as_bytes());
                                while let Some(_) = iter.next() {
                                    symbols += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut duplicates: Vec<String> = Vec::new();
    for (h, paths) in &file_hashes {
        if paths.len() > 1 {
            duplicates.push(format!("{} files share hash {}", paths.len(), &h[..12]));
            // Avoided embeddings = extra copies of identical content.
            avoided_embeddings += paths.len() - 1;
        }
    }

    // Version-candidate detection from patterns and from shared normalized base stems.
    // Pattern-based was tracked during loop; now add shared-stem detection.
    let mut stem_dir_map: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for path_str in &all_file_paths {
        let p = std::path::Path::new(path_str);
        // Pattern candidates must test the full file name, not the stem alone:
        // `.bak`/`.old` suffixes strip off in file_stem() and would never match (AH-17).
        if p.file_name()
            .and_then(|s| s.to_str())
            .map_or(false, |name| v_pattern.is_match(name))
            && !version_candidates.contains(path_str)
        {
            version_candidates.push(path_str.clone());
        }
        if let Some(stem) = p.file_stem() {
            let stem_str = stem.to_string_lossy().to_string();
            let normalized = v_pattern.replace_all(&stem_str, "");
            if let Some(parent) = p.parent() {
                let parent_key = parent.to_string_lossy().to_string();
                let dir_stem_key = format!("{}::{}", parent_key, normalized);
                stem_dir_map
                    .entry(dir_stem_key.clone())
                    .or_default()
                    .insert(path_str.clone());
            }
        }
    }
    for (_key, path_set) in &stem_dir_map {
        if path_set.len() > 1 {
            for path_str in path_set {
                if !version_candidates.contains(path_str) {
                    version_candidates.push(path_str.clone());
                }
            }
        }
    }
    version_candidates.sort();
    version_candidates.dedup();

    let mut report = String::new();
    report.push_str(&format!("Dry-run ({}) report:\n", mode));
    report.push_str(&format!("Files: {}\n", files));
    report.push_str(&format!("Parseable files: {}\n", parseable));
    report.push_str(&format!("Symbols: {}\n", symbols));
    report.push_str(&format!("Doc sections: {}\n", doc_sections));
    report.push_str(&format!(
        "Embeddings needed (est): {}\n",
        symbols + doc_sections
    ));
    report.push_str(&format!("Avoided embeddings: {}\n", avoided_embeddings));
    report.push_str(&format!("Secret hits: {}\n", secret_hits));
    report.push_str(&format!("PII warnings: {}\n", pii_warnings.len()));
    report.push_str(&format!("Junk candidates: {}\n", junk.len()));
    report.push_str(&format!("Duplicate candidates: {}\n", duplicates.len()));
    report.push_str(&format!(
        "Version candidates: {}\n",
        version_candidates.len()
    ));
    if mode == "presort" {
        report.push_str(&format!("Junk files: {}\n", junk.len()));
        for j in &junk {
            report.push_str(&format!("  junk: {}\n", j));
        }
        report.push_str(&format!("Duplicate groups: {}\n", duplicates.len()));
        for d in &duplicates {
            report.push_str(&format!("  dup: {}\n", d));
        }
        report.push_str(&format!(
            "Version candidate files: {}\n",
            version_candidates.len()
        ));
        for v in &version_candidates {
            report.push_str(&format!("  version: {}\n", v));
        }
        for w in &pii_warnings {
            report.push_str(&format!("  pii: {}\n", w));
        }
        report.push_str("Proposed dataset groups:\n");
        for (g, c) in &groups {
            report.push_str(&format!("  {} ({} files)\n", g, c));
        }
    }
    Ok(report)
}

/// Best-effort extraction of import/module specifiers from source text.
pub fn extract_import_specifiers(lang: &str, content: &str) -> Vec<String> {
    let mut out = Vec::new();
    match lang {
        "rust" => {
            // `mod foo;`
            for cap in regex::Regex::new(r"(?m)\bmod\s+([a-zA-Z_][a-zA-Z0-9_]*)\s*;")
                .unwrap()
                .captures_iter(content)
            {
                out.push(format!("mod:{}", &cap[1]));
            }
            // `use a::b::c;` or `use a::b::{x, y};`
            for cap in regex::Regex::new(r"(?m)\buse\s+([a-zA-Z0-9_:\.]+)")
                .unwrap()
                .captures_iter(content)
            {
                out.push(format!("use:{}", &cap[1]));
            }
            // `extern crate foo;`
            for cap in regex::Regex::new(r"(?m)\bextern\s+crate\s+([a-zA-Z_][a-zA-Z0-9_]*)")
                .unwrap()
                .captures_iter(content)
            {
                out.push(format!("extern:{}", &cap[1]));
            }
        }
        "python" => {
            for line in content.lines() {
                let t = line.trim();
                if let Some(caps) =
                    regex::Regex::new(r"^from\s+(\.+[A-Za-z0-9_\.]*|[A-Za-z0-9_\.]+)\s+import")
                        .unwrap()
                        .captures(t)
                {
                    out.push(format!("from:{}", &caps[1]));
                } else if let Some(caps) = regex::Regex::new(r"^import\s+([A-Za-z0-9_\.]+)")
                    .unwrap()
                    .captures(t)
                {
                    out.push(format!("import:{}", &caps[1]));
                }
            }
        }
        "typescript" | "tsx" | "javascript" => {
            for cap in regex::Regex::new(r#"(?m)(?:from|require)\s*\(?['"]([^'"]+)['"]"#)
                .unwrap()
                .captures_iter(content)
            {
                out.push(format!("js:{}", &cap[1]));
            }
        }
        "go" => {
            for cap in regex::Regex::new(r#"(?m)"([^"]+)""#)
                .unwrap()
                .captures_iter(content)
            {
                // only inside import blocks is hard; accept quoted paths
                let p = &cap[1];
                if p.contains('/') || p.contains('.') {
                    out.push(format!("go:{}", p));
                }
            }
        }
        "java" => {
            for cap in regex::Regex::new(r"(?m)^\s*import\s+([A-Za-z0-9_\.]+)\s*;")
                .unwrap()
                .captures_iter(content)
            {
                out.push(format!("java:{}", &cap[1]));
            }
        }
        _ => {}
    }
    out.sort();
    out.dedup();
    out
}

pub fn resolve_dependency_paths(
    current_rel: &str,
    imports: &[String],
    all_paths: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let current = std::path::Path::new(current_rel);
    let parent = current.parent().unwrap_or(std::path::Path::new(""));
    let exts = ["rs", "py", "ts", "tsx", "js", "jsx", "go", "java"];

    for imp in imports {
        let mut candidates: Vec<String> = Vec::new();
        if let Some(spec) = imp.strip_prefix("js:") {
            if spec.starts_with('.') {
                let base = parent.join(spec);
                push_candidates(&mut candidates, &base, &exts);
            } else {
                // bare specifier: try as relative path anyway
                push_candidates(&mut candidates, std::path::Path::new(spec), &exts);
            }
        } else if let Some(spec) = imp.strip_prefix("from:") {
            if spec.starts_with('.') {
                // python relative: .foo -> foo.py in same package, ..foo -> parent
                let dots = spec.bytes().take_while(|b| *b == b'.').count();
                let rest = &spec[dots..];
                let mut base = parent.to_path_buf();
                for _ in 1..dots {
                    base = base.parent().unwrap_or(&base).to_path_buf();
                }
                let segments: Vec<&str> = rest.split('.').filter(|s| !s.is_empty()).collect();
                if segments.is_empty() {
                    // package import: __init__.py
                    candidates.push(
                        base.join("__init__.py")
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                } else {
                    for len in 1..=segments.len() {
                        let sub = segments[..len].join("/");
                        push_candidates(&mut candidates, &base.join(&sub), &exts);
                    }
                }
            } else {
                let segments: Vec<&str> = spec.split('.').filter(|s| !s.is_empty()).collect();
                for len in 1..=segments.len() {
                    let base = segments[..len].join("/");
                    push_candidates(&mut candidates, std::path::Path::new(&base), &exts);
                    push_candidates(
                        &mut candidates,
                        std::path::Path::new("src").join(&base).as_path(),
                        &exts,
                    );
                }
            }
        } else if let Some(spec) = imp.strip_prefix("import:") {
            let segments: Vec<&str> = spec.split('.').filter(|s| !s.is_empty()).collect();
            for len in 1..=segments.len() {
                let base = segments[..len].join("/");
                push_candidates(&mut candidates, std::path::Path::new(&base), &exts);
                push_candidates(
                    &mut candidates,
                    std::path::Path::new("src").join(&base).as_path(),
                    &exts,
                );
            }
        } else if let Some(spec) = imp.strip_prefix("mod:") {
            let base = parent.join(spec);
            push_candidates(&mut candidates, &base, &exts);
            // mod foo; could be foo/mod.rs
            candidates.push(
                parent
                    .join(spec)
                    .join("mod.rs")
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        } else if let Some(spec) = imp.strip_prefix("use:") {
            let spec = spec.trim_end_matches(';');
            if let Some(rest) = spec.strip_prefix("crate::") {
                let segments: Vec<&str> = rest.split("::").filter(|s| !s.is_empty()).collect();
                for len in 1..=segments.len() {
                    let base = segments[..len].join("/");
                    push_candidates(
                        &mut candidates,
                        std::path::Path::new("src").join(&base).as_path(),
                        &exts,
                    );
                    push_candidates(&mut candidates, std::path::Path::new(&base), &exts);
                }
            } else if let Some(rest) = spec.strip_prefix("super::") {
                let segments: Vec<&str> = rest.split("::").filter(|s| !s.is_empty()).collect();
                let parent_parent = parent.parent().unwrap_or(parent);
                for len in 1..=segments.len() {
                    let base = segments[..len].join("/");
                    push_candidates(&mut candidates, &parent.join(&base), &exts);
                    push_candidates(&mut candidates, &parent_parent.join(&base), &exts);
                }
            } else if let Some(rest) = spec.strip_prefix("self::") {
                let segments: Vec<&str> = rest.split("::").filter(|s| !s.is_empty()).collect();
                for len in 1..=segments.len() {
                    let base = segments[..len].join("/");
                    push_candidates(&mut candidates, &parent.join(&base), &exts);
                }
            } else if spec.contains("::") {
                // external crate or module path; try as path under src, parent, and root
                let segments: Vec<&str> = spec.split("::").filter(|s| !s.is_empty()).collect();
                for len in 1..=segments.len() {
                    let base = segments[..len].join("/");
                    push_candidates(
                        &mut candidates,
                        std::path::Path::new("src").join(&base).as_path(),
                        &exts,
                    );
                    push_candidates(&mut candidates, &parent.join(&base), &exts);
                    push_candidates(&mut candidates, std::path::Path::new(&base), &exts);
                }
            }
        } else if let Some(spec) = imp.strip_prefix("extern:") {
            push_candidates(
                &mut candidates,
                std::path::Path::new("src").join(spec).as_path(),
                &exts,
            );
        } else if let Some(spec) = imp.strip_prefix("go:") {
            let base = spec.trim_matches('"');
            push_candidates(&mut candidates, std::path::Path::new(base), &exts);
        } else if let Some(spec) = imp.strip_prefix("java:") {
            let base = spec.replace('.', "/");
            push_candidates(&mut candidates, std::path::Path::new(&base), &exts);
            push_candidates(
                &mut candidates,
                std::path::Path::new("src").join(&base).as_path(),
                &exts,
            );
        }

        for cand in candidates {
            let norm = normalize_rel_path(&cand);
            if all_paths.contains(&norm) && norm != current_rel && !out.contains(&norm) {
                out.push(norm);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn is_common_std_ident(ident: &str) -> bool {
    matches!(
        ident,
        "new"
            | "default"
            | "from"
            | "into"
            | "clone"
            | "to_string"
            | "as_str"
            | "as_ref"
            | "unwrap"
            | "expect"
            | "ok"
            | "err"
            | "some"
            | "none"
            | "map"
            | "and_then"
            | "or_else"
            | "filter"
            | "collect"
            | "iter"
            | "into_iter"
            | "len"
            | "is_empty"
            | "push"
            | "pop"
            | "insert"
            | "remove"
            | "get"
            | "contains"
            | "fmt"
            | "write"
            | "read"
            | "open"
            | "close"
            | "init"
            | "main"
            | "test"
    )
}

/// Tier-1 Workspace Symbol Table cross-file call graph linker (`neurostrata-rbde`).
///
/// Uses the Lean 4 verified `resolve_cross_file_call` priority order:
/// 1. Same-file definitions (`SameFile`)
/// 2. Imported-file definitions from `dependency_map` (`ImportedFile`)
/// 3. Unambiguous workspace definitions (`WorkspaceUnique`, `ident_len >= 4`, `!is_common_std_ident`)
pub async fn resolve_cross_file_calls_for_generation(
    vector_store: &Arc<dyn VectorStore>,
    namespace: &str,
    staged_symbols: &[StagedSymbolInfo],
    dependency_map: &std::collections::HashMap<String, Vec<String>>,
) -> usize {
    use crate::formal_kernel::{resolve_cross_file_call, CrossFileResolution};

    let mut by_file_and_name: std::collections::HashMap<(String, String), Vec<String>> =
        std::collections::HashMap::new();
    let mut by_name: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();

    for sym in staged_symbols {
        by_file_and_name
            .entry((sym.file_path.clone(), sym.name.clone()))
            .or_default()
            .push(sym.id.clone());
        by_name
            .entry(sym.name.clone())
            .or_default()
            .push((sym.file_path.clone(), sym.id.clone()));
    }

    let mut resolved_targets_by_caller: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut cross_file_callees_by_caller: std::collections::HashMap<
        String,
        Vec<serde_json::Value>,
    > = std::collections::HashMap::new();
    let mut cross_file_callers_by_callee: std::collections::HashMap<
        String,
        Vec<serde_json::Value>,
    > = std::collections::HashMap::new();
    let mut cross_file_edges = 0usize;

    for caller in staged_symbols {
        if caller.calls.is_empty() {
            continue;
        }
        let imported_files = dependency_map
            .get(&caller.file_path)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

        for callee_name in &caller.calls {
            let same_file_ids: Vec<String> = by_file_and_name
                .get(&(caller.file_path.clone(), callee_name.clone()))
                .map(|ids| ids.iter().filter(|id| *id != &caller.id).cloned().collect())
                .unwrap_or_default();
            let in_same_file = !same_file_ids.is_empty();

            let mut imported_matches: Vec<(String, String)> = Vec::new();
            for imp_path in imported_files {
                if let Some(ids) = by_file_and_name.get(&(imp_path.clone(), callee_name.clone())) {
                    for tid in ids {
                        if tid != &caller.id {
                            imported_matches.push((imp_path.clone(), tid.clone()));
                        }
                    }
                }
            }
            let in_imported_file = !imported_matches.is_empty();

            let workspace_matches: Vec<(String, String)> = by_name
                .get(callee_name)
                .map(|entries| {
                    entries
                        .iter()
                        .filter(|(_, tid)| tid != &caller.id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let ws_count = workspace_matches.len() as u32;
            let is_self_call =
                callee_name == &caller.name && !in_same_file && !in_imported_file && ws_count == 0;

            let resolution = resolve_cross_file_call(
                is_self_call,
                in_same_file,
                in_imported_file,
                ws_count,
                callee_name.len() as u32,
                is_common_std_ident(callee_name),
            );

            match resolution {
                CrossFileResolution::SameFile => {
                    let entry = resolved_targets_by_caller
                        .entry(caller.id.clone())
                        .or_default();
                    for tid in same_file_ids {
                        if !entry.contains(&tid) {
                            entry.push(tid);
                        }
                    }
                }
                CrossFileResolution::ImportedFile => {
                    for (target_path, tid) in imported_matches {
                        let entry = resolved_targets_by_caller
                            .entry(caller.id.clone())
                            .or_default();
                        if !entry.contains(&tid) {
                            entry.push(tid.clone());
                        }
                        if target_path != caller.file_path {
                            cross_file_edges += 1;
                            cross_file_callees_by_caller
                                .entry(caller.id.clone())
                                .or_default()
                                .push(serde_json::json!({
                                    "symbol_id": tid,
                                    "symbol_name": callee_name,
                                    "file_path": target_path,
                                    "resolution": resolution.as_tag(),
                                }));
                            cross_file_callers_by_callee.entry(tid).or_default().push(
                                serde_json::json!({
                                    "symbol_id": caller.id,
                                    "symbol_name": caller.name,
                                    "symbol_kind": caller.kind,
                                    "file_path": caller.file_path,
                                    "resolution": resolution.as_tag(),
                                }),
                            );
                        }
                    }
                }
                CrossFileResolution::WorkspaceUnique => {
                    if let Some((target_path, tid)) = workspace_matches.into_iter().next() {
                        let entry = resolved_targets_by_caller
                            .entry(caller.id.clone())
                            .or_default();
                        if !entry.contains(&tid) {
                            entry.push(tid.clone());
                        }
                        if target_path != caller.file_path {
                            cross_file_edges += 1;
                            cross_file_callees_by_caller
                                .entry(caller.id.clone())
                                .or_default()
                                .push(serde_json::json!({
                                    "symbol_id": tid,
                                    "symbol_name": callee_name,
                                    "file_path": target_path,
                                    "resolution": resolution.as_tag(),
                                }));
                            cross_file_callers_by_callee.entry(tid).or_default().push(
                                serde_json::json!({
                                    "symbol_id": caller.id,
                                    "symbol_name": caller.name,
                                    "symbol_kind": caller.kind,
                                    "file_path": caller.file_path,
                                    "resolution": resolution.as_tag(),
                                }),
                            );
                        }
                    }
                }
                CrossFileResolution::UnresolvedSelfCall
                | CrossFileResolution::UnresolvedAmbiguousOrStd => {}
            }
        }
    }

    let mut dirty_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in cross_file_callees_by_caller.keys() {
        dirty_ids.insert(id.clone());
    }
    for id in cross_file_callers_by_callee.keys() {
        dirty_ids.insert(id.clone());
    }

    for sym_id in dirty_ids {
        match vector_store.get(namespace, &sym_id).await {
            Ok(Some((vec, mut payload))) => {
                if let Some(obj) = payload.metadata.as_object_mut() {
                    if let Some(targets) = resolved_targets_by_caller.get(&sym_id) {
                        let mut merged_deps: Vec<String> = obj
                            .get("depends_on")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        for t in targets {
                            if !merged_deps.contains(t) {
                                merged_deps.push(t.clone());
                            }
                        }
                        merged_deps.sort();
                        merged_deps.dedup();
                        obj.insert(
                            "depends_on".to_string(),
                            serde_json::json!(merged_deps.clone()),
                        );
                        obj.insert("calls_resolved".to_string(), serde_json::json!(merged_deps));
                    }
                    if let Some(callees) = cross_file_callees_by_caller.get(&sym_id) {
                        obj.insert("cross_file_callees".to_string(), serde_json::json!(callees));
                    }
                    if let Some(callers) = cross_file_callers_by_callee.get(&sym_id) {
                        obj.insert("cross_file_callers".to_string(), serde_json::json!(callers));
                    }
                }
                if let Err(e) = vector_store.upsert(namespace, &sym_id, vec, payload).await {
                    tracing::warn!(
                        "cross_file_call stamping: failed to upsert symbol {} in {}: {}",
                        sym_id,
                        namespace,
                        e
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "cross_file_call stamping: vector_store.get failed for {}: {}",
                    sym_id,
                    e
                );
            }
        }
    }

    cross_file_edges
}

fn push_candidates(out: &mut Vec<String>, base: &std::path::Path, exts: &[&str]) {
    let s = base.to_string_lossy().replace('\\', "/");
    // strip leading ./
    let s = s.trim_start_matches("./");
    out.push(s.to_string());
    for ext in exts {
        out.push(format!("{}.{}", s, ext));
    }
    // index files
    for ext in exts {
        out.push(format!("{}/index.{}", s, ext));
        out.push(format!("{}/mod.{}", s, ext));
        out.push(format!("{}/__init__.py", s));
    }
}

fn normalize_rel_path(p: &str) -> String {
    // Resolve . and .. lexically.
    let mut parts: Vec<String> = Vec::new();
    for part in p.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part.to_string());
        }
    }
    parts.join("/")
}
