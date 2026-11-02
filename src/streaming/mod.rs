use serde::{Deserialize, Serialize};

pub const STRIPPED_PROCEDURE_COUNTERS: [&str; 5] = [
    "remaining_fires",
    "performance_count",
    "previous_performance_count",
    "last_performed_at",
    "last_episodic_pointer",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchOp {
    Add,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delta {
    pub op: PatchOp,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub graph: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    pub events: Vec<Delta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MalformedLine { line: usize, message: String },
    UnknownOp { line: usize, op: String },
    InvalidTerm { line: usize, term: String },
    PrivacyViolation { line: usize, reason: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MalformedLine { line, message } => {
                write!(f, "line {}: malformed: {}", line, message)
            }
            ParseError::UnknownOp { line, op } => {
                write!(f, "line {}: unknown op: {}", line, op)
            }
            ParseError::InvalidTerm { line, term } => {
                write!(f, "line {}: invalid term: {}", line, term)
            }
            ParseError::PrivacyViolation { line, reason } => {
                write!(f, "line {}: privacy violation: {}", line, reason)
            }
        }
    }
}

impl std::error::Error for ParseError {}

fn check_privacy(line_content: &str, line: usize) -> Result<(), ParseError> {
    for counter in STRIPPED_PROCEDURE_COUNTERS {
        if line_content.contains(counter) {
            return Err(ParseError::PrivacyViolation {
                line,
                reason: format!("stripped procedure counter '{}'", counter),
            });
        }
    }
    if line_content.contains("\"eligibility\":\"private\"")
        || line_content.contains("ns:eligibility \"private\"")
    {
        return Err(ParseError::PrivacyViolation {
            line,
            reason: "private eligibility".to_string(),
        });
    }
    Ok(())
}

pub fn format_iri(iri: &str) -> Result<String, ParseError> {
    let inner = if iri.starts_with('<') && iri.ends_with('>') {
        &iri[1..iri.len() - 1]
    } else {
        iri
    };
    // Reject chars: < > " { } | ^ \ ` and control chars U+0000..=U+0020
    for ch in inner.chars() {
        let c = ch as u32;
        if c <= 0x0020 {
            if ch != '<' && ch != '>' {
                // Reject control chars directly
                return Err(ParseError::InvalidTerm {
                    line: 0,
                    term: format!("<{}>", iri),
                });
            }
        }
        match ch {
            '<' | '>' | '"' | '{' | '}' | '|' | '^' | '\\' | '`' => {
                return Err(ParseError::InvalidTerm {
                    line: 0,
                    term: format!("<{}>", iri),
                });
            }
            _ => {}
        }
    }
    // Actually build the escaped version
    let mut escaped = String::new();
    for ch in inner.chars() {
        let c = ch as u32;
        if (c > 0x0020 && c < 0x007F)
            && ch != '<'
            && ch != '>'
            && ch != '"'
            && ch != '{'
            && ch != '}'
            && ch != '|'
            && ch != '^'
            && ch != '\\'
            && ch != '`'
        {
            escaped.push(ch);
        } else if c > 0x007F || c <= 0x0020 {
            if ch == '<' || ch == '>' || ch == '"' || ch == '{' || ch == '}' || ch == '|'
                || ch == '^' || ch == '\\' || ch == '`'
            {
                return Err(ParseError::InvalidTerm {
                    line: 0,
                    term: format!("<{}>", iri),
                });
            }
            // Per N-Triples spec: escape control/non-ASCII via \uXXXX, and
            // codepoints beyond the BMP via \UXXXXXXXX (AO-5).
            if c > 0xFFFF {
                escaped.push_str(&format!("\\U{:08X}", c));
            } else {
                escaped.push_str(&format!("\\u{:04X}", c));
            }
        } else {
            escaped.push(ch);
        }
    }
    Ok(format!("<{}>", escaped))
}

pub fn format_literal(
    value: &str,
    datatype: Option<&str>,
    lang: Option<&str>,
) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => {
                let c = ch as u32;
                if c <= 0x001F || c == 0x007F {
                    escaped.push_str(&format!("\\u{:04X}", c));
                } else {
                    escaped.push(ch);
                }
            }
        }
    }
    let mut result = format!("\"{}\"", escaped);
    if let Some(dt) = datatype {
        result.push_str(&format!("^^{}", format_iri(dt).unwrap_or_else(|_| format!("<{}>", dt))));
    }
    if let Some(l) = lang {
        result.push_str(&format!("@{}", l));
    }
    result
}

pub fn format_term(raw: &str) -> Result<String, ParseError> {
    if raw.starts_with('<') && raw.ends_with('>') {
        return format_iri(raw);
    }
    if raw.starts_with("_:") {
        // Blank node
        return Ok(raw.to_string());
    }
    if raw.starts_with('"') {
        // Quoted literal - assume well-formed for format_term
        return Ok(raw.to_string());
    }
    // Plain IRI: starts with urn:, http://, or https://
    if raw.starts_with("urn:") || raw.starts_with("http://") || raw.starts_with("https://") {
        return format_iri(raw);
    }
    // Try to treat as literal? Or IRI by wrapping?
    // Per spec: handles <iri>, _:bnode, or "literal"...
    // For plain unquoted non-IRI, wrap as literal
    Ok(format_literal(raw, None, None))
}

impl Stream {
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    pub fn push(&mut self, delta: Delta) {
        self.events.push(delta);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn to_patch(&self) -> String {
        let mut lines = Vec::new();
        for delta in &self.events {
            let op_char = match delta.op {
                PatchOp::Add => "A",
                PatchOp::Delete => "D",
            };
            let s = match format_iri(&delta.subject) {
                Ok(formatted) => formatted,
                Err(e) => {
                    tracing::warn!("streaming to_patch: malformed subject IRI {:?}: {}", delta.subject, e);
                    delta.subject.clone()
                }
            };
            let p = match format_iri(&delta.predicate) {
                Ok(formatted) => formatted,
                Err(e) => {
                    tracing::warn!("streaming to_patch: malformed predicate IRI {:?}: {}", delta.predicate, e);
                    delta.predicate.clone()
                }
            };
            let o = if delta.object.starts_with('"') || delta.object.starts_with("_") || delta.object.starts_with('<') {
                delta.object.clone()
            } else {
                match format_term(&delta.object) {
                    Ok(formatted) => formatted,
                    Err(e) => {
                        tracing::warn!("streaming to_patch: malformed object term {:?}: {}", delta.object, e);
                        delta.object.clone()
                    }
                }
            };
            if let Some(g) = &delta.graph {
                let g_term = match format_iri(g) {
                    Ok(formatted) => formatted,
                    Err(e) => {
                        tracing::warn!("streaming to_patch: malformed graph IRI {:?}: {}", g, e);
                        g.clone()
                    }
                };
                lines.push(format!("{} {} {} {} {} .", op_char, s, p, o, g_term));
            } else {
                lines.push(format!("{} {} {} {} .", op_char, s, p, o));
            }
        }
        lines.join("\n")
    }

    pub fn parse_patch(input: &str) -> Result<Vec<Delta>, ParseError> {
        let mut results = Vec::new();
        for (line_idx, line) in input.lines().enumerate() {
            let line_num = line_idx + 1;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            check_privacy(trimmed, line_num)?;

            // Tokenize the line
            // Expected format: A <s> <p> <o> [<g>] .
            // or D <s> <p> <o> [<g>] .
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }
            let op_token = parts[0];
            let op = match op_token {
                "A" => PatchOp::Add,
                "D" => PatchOp::Delete,
                _ => {
                    return Err(ParseError::UnknownOp {
                        line: line_num,
                        op: op_token.to_string(),
                    });
                }
            };
            // Find the trailing "."
            if parts.last() != Some(&".") {
                return Err(ParseError::MalformedLine {
                    line: line_num,
                    message: "missing trailing '.'".to_string(),
                });
            }
            // The rest between op and . are the terms. Count them only after
            // quote-aware tokenization below — a whitespace split flags a
            // quoted literal containing spaces as extra terms (AO-9).
            // Re-tokenize respecting quotes
            let content = trimmed.trim_start_matches(op_token).trim();
            // Remove trailing .
            let content = content.strip_suffix('.').unwrap_or(content).trim();
            let mut parsed_terms: Vec<String> = Vec::new();
            let chars: Vec<char> = content.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                if i >= chars.len() {
                    break;
                }
                let mut term_chars = Vec::new();
                if chars[i] == '"' {
                    // Quoted literal
                    term_chars.push(chars[i]);
                    i += 1;
                    while i < chars.len() {
                        term_chars.push(chars[i]);
                        if chars[i] == '"' && (i == 0 || chars[i - 1] != '\\') {
                            // Check for unescaped quote
                            // Actually need to check if previous char in term_chars is backslash
                            // Since we push sequentially, check previous char
                            if term_chars.len() >= 2 && term_chars[term_chars.len() - 2] == '\\' {
                                // Escaped quote, continue
                            } else {
                                i += 1;
                                break;
                            }
                        }
                        i += 1;
                    }
                    // Consume optional datatype (^^<...>) or language tag (@lang) suffix.
                    if i < chars.len() && chars[i] == '^' && i + 1 < chars.len() && chars[i + 1] == '^' {
                        term_chars.push(chars[i]);
                        term_chars.push(chars[i + 1]);
                        i += 2;
                        if i < chars.len() && chars[i] == '<' {
                            term_chars.push(chars[i]);
                            i += 1;
                            while i < chars.len() && chars[i] != '>' {
                                term_chars.push(chars[i]);
                                i += 1;
                            }
                            if i < chars.len() && chars[i] == '>' {
                                term_chars.push(chars[i]);
                                i += 1;
                            }
                        }
                    } else if i < chars.len() && chars[i] == '@' {
                        term_chars.push(chars[i]);
                        i += 1;
                        while i < chars.len() && !chars[i].is_whitespace() {
                            term_chars.push(chars[i]);
                            i += 1;
                        }
                    }
                } else if chars[i] == '<' {
                    // IRI
                    term_chars.push(chars[i]);
                    i += 1;
                    while i < chars.len() && chars[i] != '>' {
                        term_chars.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '>' {
                        term_chars.push(chars[i]);
                        i += 1;
                    } else {
                        return Err(ParseError::InvalidTerm {
                            line: line_num,
                            term: term_chars.iter().collect::<String>(),
                        });
                    }
                } else if chars[i] == '_' {
                    // Blank node
                    term_chars.push(chars[i]);
                    i += 1;
                    while i < chars.len() && !chars[i].is_whitespace() {
                        term_chars.push(chars[i]);
                        i += 1;
                    }
                } else {
                    // Should not reach here for well-formed input
                    return Err(ParseError::InvalidTerm {
                        line: line_num,
                        term: chars[i..].iter().collect::<String>(),
                    });
                }
                parsed_terms.push(term_chars.iter().collect::<String>());
            }
            if parsed_terms.len() != 3 && parsed_terms.len() != 4 {
                return Err(ParseError::MalformedLine {
                    line: line_num,
                    message: format!("expected 3 or 4 terms, got {}", parsed_terms.len()),
                });
            }
            let subject = format_term(&parsed_terms[0])?;
            let predicate = format_term(&parsed_terms[1])?;
            let object_raw = parsed_terms[2].clone();
            let object = if object_raw.starts_with('"') {
                // Quoted literal - keep as is with any datatype/lang
                object_raw.to_string()
            } else {
                format_term(&object_raw)?
            };
            let graph = if parsed_terms.len() == 4 {
                Some(format_term(&parsed_terms[3])?)
            } else {
                None
            };

            results.push(Delta {
                op,
                subject: subject.trim_start_matches('<').trim_end_matches('>').to_string(),
                predicate: predicate.trim_start_matches('<').trim_end_matches('>').to_string(),
                object: object,
                graph,
            });
        }
        Ok(results)
    }

    pub fn from_nquads(nquads: &str, op: PatchOp) -> Result<Self, ParseError> {
        let mut stream = Stream::new();
        for (line_idx, line) in nquads.lines().enumerate() {
            let line_num = line_idx + 1;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            check_privacy(trimmed, line_num)?;
            // N-Quads is s p o g .  (or s p o . for default graph)
            // But here we assume canonical N-Quads with 4 terms + . for simplicity,
            // or 3 terms + .
            // We'll tokenize similarly to parse_patch
            let content = trimmed.trim_end_matches('.').trim();
            let mut terms: Vec<String> = Vec::new();
            let chars: Vec<char> = content.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                if i >= chars.len() {
                    break;
                }
                let mut term_chars = Vec::new();
                if chars[i] == '"' {
                    term_chars.push(chars[i]);
                    i += 1;
                    while i < chars.len() {
                        term_chars.push(chars[i]);
                        if chars[i] == '"' && (i == 0 || chars[i - 1] != '\\') {
                            if term_chars.len() >= 2 && term_chars[term_chars.len() - 2] == '\\' {
                                // Escaped quote
                            } else {
                                i += 1;
                                break;
                            }
                        }
                        i += 1;
                    }
                    // Consume optional datatype (^^<...>) or language tag (@lang) suffix.
                    if i < chars.len() && chars[i] == '^' && i + 1 < chars.len() && chars[i + 1] == '^' {
                        term_chars.push(chars[i]);
                        term_chars.push(chars[i + 1]);
                        i += 2;
                        if i < chars.len() && chars[i] == '<' {
                            term_chars.push(chars[i]);
                            i += 1;
                            while i < chars.len() && chars[i] != '>' {
                                term_chars.push(chars[i]);
                                i += 1;
                            }
                            if i < chars.len() && chars[i] == '>' {
                                term_chars.push(chars[i]);
                                i += 1;
                            }
                        }
                    } else if i < chars.len() && chars[i] == '@' {
                        term_chars.push(chars[i]);
                        i += 1;
                        while i < chars.len() && !chars[i].is_whitespace() {
                            term_chars.push(chars[i]);
                            i += 1;
                        }
                    }
                } else if chars[i] == '<' {
                    term_chars.push(chars[i]);
                    i += 1;
                    while i < chars.len() && chars[i] != '>' {
                        term_chars.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() && chars[i] == '>' {
                        term_chars.push(chars[i]);
                        i += 1;
                    } else {
                        return Err(ParseError::InvalidTerm {
                            line: line_num,
                            term: term_chars.iter().collect::<String>(),
                        });
                    }
                } else if chars[i] == '_' {
                    term_chars.push(chars[i]);
                    i += 1;
                    while i < chars.len() && !chars[i].is_whitespace() {
                        term_chars.push(chars[i]);
                        i += 1;
                    }
                } else {
                    return Err(ParseError::InvalidTerm {
                        line: line_num,
                        term: chars[i..].iter().collect::<String>(),
                    });
                }
                terms.push(term_chars.iter().collect::<String>());
            }
            // A quad is `s p o g .` — fewer than 3 or more than 4 terms is a
            // malformed line, not silently ignored or loosely accepted junk.
            if terms.len() < 3 || terms.len() > 4 {
                return Err(ParseError::MalformedLine {
                    line: line_num,
                    message: format!("expected 3 or 4 terms, got {}", terms.len()),
                });
            }
            let subject_raw = terms[0].clone();
            let predicate_raw = terms[1].clone();
            let object_raw = terms[2].clone();
            let graph_raw = if terms.len() >= 4 {
                Some(terms[3].clone())
            } else {
                None
            };
            let subject = if subject_raw.starts_with('<') && subject_raw.ends_with('>') {
                subject_raw[1..subject_raw.len()-1].to_string()
            } else {
                subject_raw
            };
            let predicate = if predicate_raw.starts_with('<') && predicate_raw.ends_with('>') {
                predicate_raw[1..predicate_raw.len()-1].to_string()
            } else {
                predicate_raw
            };
            let object = if object_raw.starts_with('"') {
                object_raw.to_string()
            } else {
                object_raw
            };
            let graph = graph_raw.as_ref().map(|g| {
                if g.starts_with('<') && g.ends_with('>') {
                    g[1..g.len()-1].to_string()
                } else {
                    g.clone()
                }
            });
            stream.push(Delta {
                op,
                subject,
                predicate,
                object,
                graph,
            });
        }
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_patch_op_roundtrip_add() {
        let mut stream = Stream::new();
        stream.push(Delta {
            op: PatchOp::Add,
            subject: "http://example.org/s".to_string(),
            predicate: "http://example.org/p".to_string(),
            object: "\"hello\"".to_string(),
            graph: None,
        });
        let patch = stream.to_patch();
        assert!(patch.contains("A "));
        assert!(patch.contains("."));
        let parsed = Stream::parse_patch(&patch).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].op, PatchOp::Add);
    }

    #[test]
    fn test_patch_op_roundtrip_delete_with_graph() {
        let mut stream = Stream::new();
        stream.push(Delta {
            op: PatchOp::Delete,
            subject: "urn:test:s1".to_string(),
            predicate: "urn:test:p1".to_string(),
            object: "\"world\"".to_string(),
            graph: Some("http://example.org/g".to_string()),
        });
        let patch = stream.to_patch();
        assert!(patch.contains("D "));
        assert!(patch.contains("http://example.org/g"));
        let parsed = Stream::parse_patch(&patch).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].op, PatchOp::Delete);
        assert_eq!(parsed[0].graph.as_ref().map(|_| true), Some(true));
    }

    #[test]
    fn test_roundtrip_add_delete_with_and_without_graph() {
        // Without graph
        let mut s = Stream::new();
        s.push(Delta {
            op: PatchOp::Add,
            subject: "<http://a/b>".to_string(),
            predicate: "<http://c/d>".to_string(),
            object: "<http://e/f>".to_string(),
            graph: None,
        });
        s.push(Delta {
            op: PatchOp::Delete,
            subject: "<http://a/b>".to_string(),
            predicate: "<http://c/d>".to_string(),
            object: "<http://e/f>".to_string(),
            graph: Some("<http://g/h>".to_string()),
        });
        let patch = s.to_patch();
        assert!(patch.contains("A "));
        assert!(patch.contains("D "));
        let parsed = Stream::parse_patch(&patch).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_literal_escaping() {
        let lit = format_literal("hello \"world\"", None, None);
        assert!(lit.contains("\\\""));
        let lit2 = format_literal("line1\nline2", None, None);
        assert!(lit2.contains("\\n"));
        let lit3 = format_literal("tab\there", None, None);
        assert!(lit3.contains("\\t"));
        let lit4 = format_literal("back\\slash", None, None);
        assert!(lit4.contains("\\\\"));
        let lit5 = format_literal("carriage\rreturn", None, None);
        assert!(lit5.contains("\\r"));
    }

    #[test]
    fn test_iri_escaping() {
        // Basic IRI
        let iri = format_iri("urn:test:s1").unwrap();
        assert!(iri.contains("<urn:test:s1>"));
    }

    #[test]
    fn test_blank_node() {
        let term = format_term("_:b0").unwrap();
        assert_eq!(term, "_:b0");
    }

    #[test]
    fn test_stripped_procedure_counters_rejected() {
        let input = "A <s> <p> \"remaining_fires\" .";
        let result = Stream::parse_patch(input);
        assert!(result.is_err());
        match result.unwrap_err() {
            ParseError::PrivacyViolation { line, .. } => assert_eq!(line, 1),
            other => panic!("Expected PrivacyViolation, got {:?}", other),
        }
    }
    #[test]
    fn test_stripped_counter_performance_count() {
        let input = "A <s> <p> \"performance_count\" .";
        assert!(Stream::parse_patch(input).is_err());
    }
    #[test]
    fn test_stripped_counter_last_performed_at() {
        let input = "A <s> <p> \"last_performed_at\" .";
        assert!(Stream::parse_patch(input).is_err());
    }
    #[test]
    fn test_stripped_counter_last_episodic_pointer() {
        let input = "A <s> <p> \"last_episodic_pointer\" .";
        assert!(Stream::parse_patch(input).is_err());
    }

    /// A quoted literal containing spaces is one term (AO-9): the whitespace
    /// split must not be used to count terms before quote-aware tokenizing.
    #[test]
    fn parse_patch_accepts_quoted_literal_with_spaces() {
        let patch = "A <urn:s> <urn:p> \"hello world\" .";
        let parsed = Stream::parse_patch(patch).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].object, "\"hello world\"");
    }

    #[test]
    fn test_malformed_line_unknown_op() {
        let input = "X <s> <p> <o> .";
        let result = Stream::parse_patch(input);
        assert!(result.is_err());
        match result.unwrap_err() {
            ParseError::UnknownOp { line, .. } => assert_eq!(line, 1),
            other => panic!("Expected UnknownOp, got {:?}", other),
        }
    }

    #[test]
    fn test_malformed_line_missing_dot() {
        let input = "A <s> <p> <o>";
        let result = Stream::parse_patch(input);
        assert!(result.is_err());
        match result.unwrap_err() {
            ParseError::MalformedLine { line, .. } => assert_eq!(line, 1),
            other => panic!("Expected MalformedLine, got {:?}", other),
        }
    }

    #[test]
    fn test_n_triples_escaping_unicode() {
        let lit = format_literal("hello\nworld\t!", None, None);
        assert!(lit.contains("\\n"));
        assert!(lit.contains("\\t"));
    }

    #[test]
    fn test_n_triples_escaping_quotes_backslash() {
        let value = r#"hello "world" \"#;
        let lit = format_literal(value, None, None);
        assert!(lit.contains(r#"\""#));
        assert!(lit.contains(r#"\\"#));
    }

    #[test]
    fn test_from_nquads() {
        let nquads = "<http://example.org/s> <http://example.org/p> \"test\" <http://example.org/g> .";
        let stream = Stream::from_nquads(nquads, PatchOp::Add).unwrap();
        assert_eq!(stream.len(), 1);
        assert_eq!(stream.events[0].op, PatchOp::Add);
    }
}
