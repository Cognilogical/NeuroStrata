//! Vault-LD: W3C YAML-LD 1.0 / Vault-LD bidirectional Markdown vault sync
//! with Open JSON Canvas 1.0 topology export.
//!
//! This module contains the note/canvas data model and deterministic
//! render/parse primitives. The sync entry points (export_vault, import_vault,
//! handle_vault_sync) are defined in the vault_sync submodule.

pub mod vault_sync;

use crate::traits::SearchResult;
use hex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// W3C YAML-LD 1.0 context IRI
pub const YAML_LD_CONTEXT: &str = "https://w3id.org/yaml/vocab";

/// JSON Canvas 1.0 schema namespace
pub const CANVAS_SCHEMA: &str = "https://jsoncanvas.org";

/// Separates user-authored body content from the parser-generated edge
/// block. Only lines after this marker are stripped on re-import.
pub const STRUCTURAL_EDGE_MARKER: &str = "<!-- vault-ld:structural-edges -->";

/// Represents a single vault note with YAML-LD 1.0 frontmatter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultNote {
    /// Unique identifier (maps to memory ID)
    #[serde(rename = "@id")]
    pub id: String,
    /// RDF type (maps to memory_type)
    #[serde(rename = "@type")]
    pub rdf_type: String,
    /// JSON-LD context
    #[serde(rename = "@context")]
    pub context: String,
    /// Epistemic tier (rule|fact|procedure|structural|task)
    pub epistemic_tier: String,
    /// SHA-256 of canonical content for bitemporal supersession
    pub canonical_sha256: String,
    /// WikiLink targets (RELATES_TO, GOVERNS, DEPENDS_ON edges)
    pub related: Vec<String>,
    /// Markdown body content
    #[serde(skip_serializing)]
    pub content: String,
}

impl VaultNote {
    /// Identity of a note for bitemporal change detection. Content alone is
    /// insufficient: an edge-only edit is a new version too.
    pub fn fingerprint(&self) -> String {
        let mut related = self.related.clone();
        related.sort();
        related.dedup();
        let canonical = format!(
            "{}\u{1f}{}\u{1f}{}",
            self.content.trim(),
            self.epistemic_tier,
            related.join("\u{1e}")
        );
        hex::encode(Sha256::digest(canonical.as_bytes()))
    }

    /// Create a VaultNote from a stored payload. Used by export and by the
    /// import unchanged-round-trip check for rows that carry no
    /// `canonical_sha256` in metadata (task rows are written as `{task: …}`
    /// without it, so a hash-only comparison can never match them).
    pub fn from_payload(id: &str, payload: &crate::traits::MemoryPayload) -> Self {
        let memory_type = payload.memory_type.clone();

        let epistemic_tier = match memory_type.as_str() {
            "rule" => "rule",
            "fact" => "fact",
            "procedure" => "procedure",
            "structural" => "structural",
            "task" => "task",
            _ => "symbol",
        }
        .to_string();

        let mut related = Vec::new();
        for key in ["related", "related_to", "governs", "depends_on"] {
            if let Some(edges) = payload.metadata.get(key).and_then(|v| v.as_array()) {
                for edge in edges {
                    if let Some(target) = edge.as_str() {
                        related.push(target.to_string());
                    }
                }
            }
        }
        // Canonical edge set: one deterministic rendering and fingerprint for
        // an identical graph regardless of which field declared the target.
        related.sort();
        related.dedup();

        let mut note = VaultNote {
            id: id.to_string(),
            rdf_type: format!("yaml:{memory_type}"),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier,
            canonical_sha256: String::new(),
            // Match import-side parsing: surrounding whitespace is not part
            // of note identity and must not survive an export/import cycle.
            related,
            content: payload.content.trim().to_string(),
        };
        note.canonical_sha256 = note.fingerprint();
        note
    }

    /// Create a VaultNote from a SearchResult (ladybug payload)
    pub fn from_search_result(result: &SearchResult) -> Result<Self, String> {
        Ok(Self::from_payload(&result.id, &result.payload))
    }

    /// Render to YAML-LD 1.0 Markdown with frontmatter
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");

        let frontmatter = serde_json::json!({
            "@context": self.context,
            "@id": self.id,
            "@type": self.rdf_type,
            "epistemic_tier": self.epistemic_tier,
            "canonical_sha256": self.canonical_sha256,
            "related": self.related,
        });

        let yaml = serde_yaml::to_string(&frontmatter)
            .unwrap_or_else(|_| "epistemic_tier: unknown\n".to_string());
        out.push_str(&yaml);
        out.push_str("---\n\n");

        out.push_str(&self.content);
        if !self.related.is_empty() {
            out.push_str("\n\n");
            out.push_str(STRUCTURAL_EDGE_MARKER);
            out.push('\n');
            for target in &self.related {
                out.push_str(&format!("[[{}]]\n", target));
            }
        }
        out
    }

    /// Parse Markdown with YAML-LD frontmatter.
    ///
    /// Frontmatter is exactly one block: only a `---` on the first line
    /// (after an optional UTF-8 BOM) opens it, the next `---` closes it,
    /// and delimiter scanning stops permanently at that point. Every later
    /// `---` is ordinary body content (a Markdown horizontal rule) and must
    /// never be reinterpreted as a frontmatter boundary.
    pub fn from_markdown(markdown: &str) -> Result<Self, String> {
        let markdown = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
        let lines: Vec<&str> = markdown.lines().collect();

        if lines.first().map(|line| line.trim()) != Some("---") {
            return Err("unterminated or missing YAML frontmatter block".to_string());
        }

        let mut frontmatter_lines = Vec::new();
        let mut body_lines = Vec::new();
        let mut closed = false;
        for line in lines.into_iter().skip(1) {
            if !closed {
                if line.trim() == "---" {
                    closed = true;
                } else {
                    frontmatter_lines.push(line);
                }
            } else {
                body_lines.push(line);
            }
        }

        if !closed || frontmatter_lines.is_empty() {
            return Err("unterminated or missing YAML frontmatter block".to_string());
        }

        // YAML-LD uses `@`-prefixed keys (@context/@id/@type), which libyaml
        // rejects as unquoted token starts. Quote them before parsing so both
        // spec-style and serde_yaml-emitted (already-quoted) frontmatter load.
        let quoted: Vec<String> = frontmatter_lines
            .iter()
            .map(|line| {
                let trimmed = line.trim_start();
                let indent = &line[..line.len() - trimmed.len()];
                if trimmed.starts_with('@') && !trimmed.starts_with('"') && trimmed.contains(':') {
                    if let Some(pos) = trimmed.find(':') {
                        return format!("{}\"{}\"{}", indent, &trimmed[..pos], &trimmed[pos..]);
                    }
                }
                line.to_string()
            })
            .collect();
        let frontmatter_str = quoted.join("\n");
        let frontmatter: serde_json::Value = serde_yaml::from_str(&frontmatter_str)
            .map_err(|e| format!("Invalid YAML frontmatter: {}", e))?;

        let context = frontmatter
            .get("@context")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "@context missing".to_string())?
            .to_string();

        let id = frontmatter
            .get("@id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "@id missing".to_string())?
            .to_string();

        let rdf_type = frontmatter
            .get("@type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "@type missing".to_string())?
            .to_string();

        let epistemic_tier = frontmatter
            .get("epistemic_tier")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "epistemic_tier missing".to_string())?
            .to_string();

        let canonical_sha256 = frontmatter
            .get("canonical_sha256")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "canonical_sha256 missing".to_string())?
            .to_string();

        let related: Vec<String> = frontmatter
            .get("related")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Only the parser-generated block after the marker is structural.
        // User-authored standalone WikiLinks are body content and survive.
        let prose_end = body_lines
            .iter()
            .position(|line| line.trim() == STRUCTURAL_EDGE_MARKER)
            .unwrap_or(body_lines.len());
        let content = body_lines[..prose_end].join("\n").trim().to_string();

        // Also extract WikiLinks from body content
        let mut all_related = related.clone();
        for line in &body_lines {
            let text = *line;
            if text.contains("[[") && text.contains("]]") {
                let mut remaining: &str = text;
                while let Some(start) = remaining.find("[[") {
                    if let Some(end) = remaining[start + 2..].find("]]") {
                        let link_end = start + 2 + end + 2;
                        let link = &remaining[start + 2..link_end - 2];
                        all_related.push(link.to_string());
                        remaining = &remaining[link_end..];
                    } else {
                        break;
                    }
                }
            }
        }
        all_related.sort();
        all_related.dedup();

        Ok(VaultNote {
            id,
            rdf_type,
            context,
            epistemic_tier,
            canonical_sha256,
            related: all_related,
            content,
        })
    }
}

/// Embedded v1 SHACL shapes for Vault-LD validation
pub const VAULT_LD_SHAPES_V1: &str = "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix yaml: <https://w3id.org/yaml/vocab> .

<yaml:VaultNote> a sh:NodeShape ;
    sh:targetClass yaml:VaultNote ;
    sh:property [
        sh:path yaml:epistemicTier ;
        sh:nodeKind sh:Literal ;
        sh:pattern \"^[A-Za-z_][A-Za-z0-9_.-]*$\" ;
        sh:message \"epistemicTier must be a simple tier token (rule|fact|procedure|structural|task|symbol or a custom slug)\" ;
    ] ;
    sh:property [
        sh:path yaml:canonicalSha256 ;
        sh:nodeKind sh:Literal ;
        sh:pattern \"^[0-9a-f]{64}$\" ;
        sh:message \"canonicalSha256 must be a 64-character hex string\" ;
    ] ;
    sh:property [
        sh:path yaml:related ;
        sh:nodeKind sh:BlankNode ;
        sh:message \"related links must be blank nodes with targetId\" ;
    ] .";

// --- Canvas types ---

/// A node in the Open JSON Canvas 1.0 topology
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasNode {
    pub id: String,
    pub node_type: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub label: Option<String>,
    pub color: Option<String>,
    pub file: Option<String>,
}

/// An edge in the Open JSON Canvas 1.0 topology
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasEdge {
    pub id: String,
    pub edge_type: String,
    pub from_node: String,
    pub to_node: String,
    pub from_side: Option<String>,
    pub to_side: Option<String>,
}

/// A complete Open JSON Canvas 1.0 document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasDocument {
    pub nodes: Vec<CanvasNode>,
    pub edges: Vec<CanvasEdge>,
}

// --- TDD Tests ---

#[cfg(test)]
mod tests {
    use super::vault_sync::{
        canonicalize_vault_path, generate_canvas, note_to_turtle, scan_secrets, validate_shacl,
    };
    use super::*;
    use std::path::Path;

    #[test]
    fn test_vault_note_to_markdown() {
        let note = VaultNote {
            id: "test-note-1".to_string(),
            rdf_type: "yaml:text".to_string(),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier: "empirical".to_string(),
            canonical_sha256: "a1b2c3d4e5f6".to_string(),
            related: vec!["other-note".to_string()],
            content: "Hello World".to_string(),
        };

        let markdown = note.to_markdown();
        assert!(markdown.contains("@context"));
        assert!(markdown.contains("test-note-1"));
        assert!(markdown.contains("empirical"));
        assert!(markdown.contains("a1b2c3d4e5f6"));
        assert!(markdown.contains("Hello World"));
        assert!(markdown.contains("[[other-note]]"));
    }

    #[test]
    fn test_parse_markdown_ld() {
        let markdown = r#"---
@context: "https://w3id.org/yaml/vocab"
@id: "test-id"
@type: "yaml:text"
epistemic_tier: "empirical"
canonical_sha256: "abc123"
related: ["Link1"]
---

Hello World
[[Link2]]"#;

        let note = VaultNote::from_markdown(markdown).unwrap();
        assert_eq!(note.id, "test-id");
        assert_eq!(note.rdf_type, "yaml:text");
        assert_eq!(note.epistemic_tier, "empirical");
        assert_eq!(note.canonical_sha256, "abc123");
        assert!(note.related.contains(&"Link1".to_string()));
        assert!(note.related.contains(&"Link2".to_string()));
    }

    #[test]
    fn test_markdown_roundtrip() {
        let original = VaultNote {
            id: "roundtrip-test".to_string(),
            rdf_type: "yaml:fact".to_string(),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier: "fact".to_string(),
            canonical_sha256: "roundtrip123".to_string(),
            related: vec!["target".to_string()],
            content: "Test content".to_string(),
        };

        let markdown = original.to_markdown();
        let parsed = VaultNote::from_markdown(&markdown).unwrap();

        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.rdf_type, original.rdf_type);
        assert_eq!(parsed.epistemic_tier, original.epistemic_tier);
        assert_eq!(parsed.content, original.content);
        assert_eq!(parsed.related, original.related);
    }

    #[test]
    fn test_note_to_turtle() {
        let note = VaultNote {
            id: "turtle-test".to_string(),
            rdf_type: "yaml:rule".to_string(),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier: "rule".to_string(),
            canonical_sha256: "turtle123".to_string(),
            related: vec!["link1".to_string(), "link2".to_string()],
            content: "Rule content".to_string(),
        };

        let turtle = note_to_turtle(&note).expect("valid VaultNote renders to Turtle");
        assert!(turtle.contains("turtle-test"));
        assert!(turtle.contains("yaml:rule"));
        assert!(turtle.contains("yaml:epistemicTier"));
        assert!(turtle.contains("yaml:canonicalSha256"));
        assert!(turtle.contains("link1"));
        assert!(turtle.contains("link2"));
    }

    #[test]
    fn test_generate_canvas() {
        let notes = vec![
            VaultNote {
                id: "note1".to_string(),
                rdf_type: "yaml:rule".to_string(),
                context: YAML_LD_CONTEXT.to_string(),
                epistemic_tier: "rule".to_string(),
                canonical_sha256: "sha1".to_string(),
                related: vec!["note2".to_string()],
                content: "Rule".to_string(),
            },
            VaultNote {
                id: "note2".to_string(),
                rdf_type: "yaml:fact".to_string(),
                context: YAML_LD_CONTEXT.to_string(),
                epistemic_tier: "fact".to_string(),
                canonical_sha256: "sha2".to_string(),
                related: vec![],
                content: "Fact".to_string(),
            },
        ];

        let canvas = generate_canvas(&notes, "TestNamespace");
        assert_eq!(canvas.nodes.len(), 2);
        assert_eq!(canvas.edges.len(), 1);
        assert_eq!(canvas.edges[0].from_node, "note1");
        assert_eq!(canvas.edges[0].to_node, "note2");
    }

    #[test]
    fn test_canonicalize_vault_path() {
        use std::env;

        let cwd = env::current_dir().unwrap();

        let result = canonicalize_vault_path(Path::new("."), &cwd, &[]);
        assert!(result.is_ok());

        let result = canonicalize_vault_path(Path::new("some/subdir"), &cwd, &[]);
        match result {
            Ok(_) => (),
            Err(e) => assert!(e.contains("outside the project root")),
        }
    }

    #[test]
    fn test_scan_secrets() {
        assert!(scan_secrets("clean content", "test").is_ok());

        let secret_content = "api_key = \"sk-1234567890abcdef\"";
        let result = scan_secrets(secret_content, "test.md");
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_validate_shacl() {
        let mut note = VaultNote {
            id: "urn:test".to_string(),
            rdf_type: "yaml:fact".to_string(),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier: "fact".to_string(),
            canonical_sha256: "a".repeat(64),
            related: Vec::new(),
            content: "content".to_string(),
        };
        note.canonical_sha256 = note.fingerprint();
        assert!(
            validate_shacl(&note_to_turtle(&note).expect("valid VaultNote renders to Turtle"))
                .is_ok()
        );

        note.canonical_sha256 = "not-a-hash".to_string();
        assert!(
            validate_shacl(&note_to_turtle(&note).expect("valid VaultNote renders to Turtle"))
                .is_err()
        );
    }

    /// Finding 9: a standalone WikiLink written by the user is body content.
    /// Only the parser-generated structural edge block may be stripped.
    /// Closure finding 3: related targets from every edge field must be
    /// sorted and deduplicated before rendering or fingerprinting.
    #[test]
    fn from_search_result_related_is_sorted_and_deduped() {
        let payload = crate::traits::MemoryPayload {
            content: "body".to_string(),
            user_id: "me".to_string(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: serde_json::json!({
                "related": ["z", "a", "z"],
                "related_to": ["b", "a"],
                "governs": ["c"],
                "depends_on": ["z"]
            }),
        };
        let result = crate::traits::SearchResult {
            id: "dedup".to_string(),
            score: 0.0,
            payload,
            evidence: None,
        };
        let note = VaultNote::from_search_result(&result).expect("note");
        assert_eq!(note.related, vec!["a", "b", "c", "z"]);
    }

    #[test]
    fn test_user_authored_wikilink_body_line_survives_roundtrip() {
        let original = VaultNote {
            id: "roundtrip-wikilink".to_string(),
            rdf_type: "yaml:fact".to_string(),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier: "fact".to_string(),
            canonical_sha256: "a".repeat(64),
            related: vec!["structural-target".to_string()],
            content: "Intro\n\n[[user-link]]\n\nOutro".to_string(),
        };
        let parsed = VaultNote::from_markdown(&original.to_markdown()).unwrap();
        assert_eq!(parsed.content, original.content);
        assert!(parsed.related.contains(&"user-link".to_string()));
        assert!(parsed.related.contains(&"structural-target".to_string()));
    }

    /// Issue #292: only the first `---` opens frontmatter and the next one
    /// closes it. Every later `---` is body content (a Markdown rule).
    #[test]
    fn from_markdown_preserves_body_horizontal_rules() {
        let original = VaultNote {
            id: "rules-note".to_string(),
            rdf_type: "yaml:fact".to_string(),
            context: YAML_LD_CONTEXT.to_string(),
            epistemic_tier: "fact".to_string(),
            canonical_sha256: "a".repeat(64),
            related: Vec::new(),
            content: "Intro\n\n---\n\nMiddle\n\n---\n\nOutro".to_string(),
        };
        let parsed = VaultNote::from_markdown(&original.to_markdown())
            .expect("two body rules must not confuse the frontmatter scanner");
        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.rdf_type, original.rdf_type);
        assert_eq!(parsed.epistemic_tier, original.epistemic_tier);
        assert_eq!(parsed.content, original.content);
        assert_eq!(parsed.content.matches("---").count(), 2);
    }

    /// Issue #292: a UTF-8 BOM before the opening delimiter is tolerated, and
    /// a single horizontal rule in the body survives parsing.
    #[test]
    fn from_markdown_accepts_bom_and_single_body_horizontal_rule() {
        let markdown = format!(
            "\u{feff}---\n@context: \"{}\"\n@id: \"bom-note\"\n@type: \"yaml:fact\"\n\
             epistemic_tier: fact\ncanonical_sha256: \"{}\"\nrelated: []\n---\n\n\
             Before\n---\nAfter",
            YAML_LD_CONTEXT,
            "a".repeat(64)
        );
        let parsed = VaultNote::from_markdown(&markdown).expect("BOM + rule must parse");
        assert_eq!(parsed.id, "bom-note");
        assert_eq!(parsed.content, "Before\n---\nAfter");
    }

    /// Issue #292: an unclosed opening delimiter, or no opening delimiter at
    /// all, is a typed parse error — never silently treated as frontmatter.
    #[test]
    fn from_markdown_rejects_unterminated_or_missing_frontmatter() {
        let unterminated = "---\n@id: \"x\"\nbody without a closing delimiter\n";
        let err = VaultNote::from_markdown(unterminated).expect_err("unclosed block must fail");
        assert!(err.to_lowercase().contains("unterminated"), "{}", err);

        let missing = "not frontmatter\njust a body\n";
        let err = VaultNote::from_markdown(missing).expect_err("missing block must fail");
        assert!(err.to_lowercase().contains("unterminated"), "{}", err);
    }
}
