//! In-house multi-field lexical engine.
//!
//! This module is deliberately `std`-only: the native store and the
//! `crates/neurostrata-wasm` read-only engine compile it from this path, so it
//! must not grow a `crate::` dependency. The ranking rule is one auditable
//! BM25-style formula (see `SearchOptions` and `bm25_term`), not a fusion of
//! rankers.
//!
//! Score polarity follows the rest of NeuroStrata: `LexicalHit::score` is a
//! distance in `[0, 1]`, lower is better. The conversion happens once, after
//! the maximum score in the result set is known.

use std::collections::{BTreeMap, BTreeSet};

/// The three searchable fields, in weight order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Content,
    Symbol,
    Location,
    Namespace,
    MemoryType,
}

impl Field {
    pub fn tokenize(self, text: &str) -> Vec<String> {
        match self {
            Field::Content => tokenize_whitespace(text),
            Field::Symbol | Field::Location | Field::Namespace => tokenize_code(text),
            Field::MemoryType => tokenize_whitespace(text),
        }
    }

    fn weight_index(self) -> usize {
        match self {
            Field::Content => 0,
            Field::Symbol => 1,
            Field::Location => 2,
            Field::Namespace => 3,
            Field::MemoryType => 4,
        }
    }
}

pub const ALL_FIELDS: [Field; 5] = [Field::Content, Field::Symbol, Field::Location, Field::Namespace, Field::MemoryType];
pub const DEFAULT_FIELD_WEIGHTS: [f64; 5] = [1.0, 2.0, 1.5, 3.0, 0.5];
const K1: f64 = 1.2;
const B: f64 = 0.75;
const MAX_FUZZY_EXPANSIONS: usize = 50;

/// A document as the lexical index sees it. The store derives the filter flags
/// from metadata when it builds the document; the engine never parses JSON.
#[derive(Debug, Clone)]
pub struct LexicalDoc {
    pub id: String,
    pub namespace: String,
    pub memory_type: String,
    pub content: String,
    pub symbol: String,
    pub location: String,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub archived: bool,
    pub user_id: String,
    pub visible_to: Vec<String>,
    pub domains: Vec<String>,
    /// Structural rows (`directory`, `file`, `markdown`, `freshness_flag`,
    /// `guard_event`) are never recall material.
    pub structural: bool,
    /// Registry rows are control-plane rows, not memories.
    pub registry: bool,
    /// SynapticMesh conflict placeholders stay out of every read surface.
    pub conflicting: bool,
}

impl LexicalDoc {
    pub fn new(
        id: impl Into<String>,
        namespace: impl Into<String>,
        memory_type: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let memory_type = memory_type.into();
        let structural = matches!(
            memory_type.as_str(),
            "directory" | "file" | "markdown" | "freshness_flag" | "guard_event"
        );
        Self {
            id: id.into(),
            namespace: namespace.into(),
            memory_type,
            content: content.into(),
            symbol: String::new(),
            location: String::new(),
            valid_from: None,
            valid_to: None,
            archived: false,
            user_id: String::new(),
            visible_to: Vec::new(),
            domains: Vec::new(),
            structural,
            registry: false,
            conflicting: false,
        }
    }

    /// Build the tokenized view of every searchable field.
    /// Returns (field, tokens) pairs in field order.
    pub fn field_tokens(&self) -> Vec<(Field, Vec<String>)> {
        vec![
            (Field::Content, Field::Content.tokenize(&self.content)),
            (Field::Symbol, Field::Symbol.tokenize(&self.symbol)),
            (Field::Location, Field::Location.tokenize(&self.location)),
            (Field::Namespace, Field::Namespace.tokenize(&self.namespace)),
            (Field::MemoryType, Field::MemoryType.tokenize(&self.memory_type)),
        ]
    }

    fn key(&self) -> String {
        format!("{}::{}", self.namespace, self.id)
    }
}

#[derive(Debug, Clone)]
pub struct Posting {
    pub doc_id: String,
    pub field: Field,
    pub tf: usize,
    pub positions: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct NamespaceIndex {
    pub docs: BTreeMap<String, LexicalDoc>,
    pub postings: BTreeMap<String, Vec<Posting>>,
    pub doc_freq: BTreeMap<String, usize>,
    pub doc_len: BTreeMap<String, usize>,
    pub avgdl: f64,
    pub field_weights: [f64; 5],
    total_len: usize,
}

impl NamespaceIndex {
    pub fn new() -> Self {
        Self {
            field_weights: DEFAULT_FIELD_WEIGHTS,
            ..Self::default()
        }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    fn add(&mut self, doc: LexicalDoc) {
        let key = doc.key();
        if self.docs.contains_key(&key) {
            self.remove_key(&key);
        }

        let mut terms: BTreeSet<String> = BTreeSet::new();
        let mut doc_len = 0usize;
        for (field, tokens) in doc.field_tokens() {
            doc_len += tokens.len();

            let mut by_term: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (position, token) in tokens.into_iter().enumerate() {
                by_term.entry(token).or_default().push(position);
            }
            for (term, positions) in by_term {
                terms.insert(term.clone());
                self.postings.entry(term).or_default().push(Posting {
                    doc_id: key.clone(),
                    field,
                    tf: positions.len(),
                    positions,
                });
            }
        }

        for term in terms {
            *self.doc_freq.entry(term).or_insert(0) += 1;
        }
        self.doc_len.insert(key.clone(), doc_len);
        self.total_len += doc_len;
        self.docs.insert(key, doc);
        self.recompute_avgdl();
    }

    fn remove_key(&mut self, key: &str) -> Option<LexicalDoc> {
        let doc = self.docs.remove(key)?;
        let mut removed_terms: BTreeSet<String> = BTreeSet::new();
        self.postings.retain(|term, postings| {
            let before = postings.len();
            postings.retain(|p| p.doc_id != key);
            if postings.is_empty() {
                removed_terms.insert(term.clone());
                false
            } else {
                if postings.len() != before {
                    removed_terms.insert(term.clone());
                }
                true
            }
        });
        for term in removed_terms {
            if let Some(df) = self.doc_freq.get_mut(&term) {
                *df = df.saturating_sub(1);
                if *df == 0 {
                    self.doc_freq.remove(&term);
                }
            }
        }
        if let Some(len) = self.doc_len.remove(key) {
            self.total_len = self.total_len.saturating_sub(len);
        }
        self.recompute_avgdl();
        Some(doc)
    }

    fn recompute_avgdl(&mut self) {
        self.avgdl = if self.docs.is_empty() {
            0.0
        } else {
            self.total_len as f64 / self.docs.len() as f64
        };
    }
}

impl Default for NamespaceIndex {
    fn default() -> Self {
        Self {
            docs: BTreeMap::new(),
            postings: BTreeMap::new(),
            doc_freq: BTreeMap::new(),
            doc_len: BTreeMap::new(),
            avgdl: 0.0,
            field_weights: DEFAULT_FIELD_WEIGHTS,
            total_len: 0,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct LexicalIndex {
    pub namespaces: BTreeMap<String, NamespaceIndex>,
}

impl LexicalIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, doc: LexicalDoc) {
        // Non-recall material never enters the index → keeps BM25 N/df/avgdl
        // computed over visible recall rows only (§5.6 filtering at build time).
        if doc.structural || doc.registry || doc.conflicting {
            return;
        }
        let namespace = doc.namespace.clone();
        self.namespaces
            .entry(namespace)
            .or_insert_with(NamespaceIndex::new)
            .add(doc);
    }

    /// Insert or replace a document, returning whether it replaced one.
    pub fn upsert(&mut self, doc: LexicalDoc) -> bool {
        let key = doc.key();
        let existed = self
            .namespaces
            .get(&doc.namespace)
            .map(|ns| ns.docs.contains_key(&key))
            .unwrap_or(false);
        self.add(doc);
        existed
    }

    pub fn remove(&mut self, namespace: &str, id: &str) -> bool {
        let key = format!("{}::{}", namespace, id);
        let removed = self
            .namespaces
            .get_mut(namespace)
            .and_then(|ns| ns.remove_key(&key))
            .is_some();
        if self
            .namespaces
            .get(namespace)
            .map(|ns| ns.is_empty())
            .unwrap_or(false)
        {
            self.namespaces.remove(namespace);
        }
        removed
    }

    /// Remove every document in a namespace that matches `predicate`.
    pub fn remove_where<F>(&mut self, namespace: &str, mut predicate: F) -> usize
    where
        F: FnMut(&LexicalDoc) -> bool,
    {
        let keys: Vec<String> = match self.namespaces.get(namespace) {
            Some(ns) => ns
                .docs
                .iter()
                .filter(|(_, doc)| predicate(doc))
                .map(|(key, _)| key.clone())
                .collect(),
            None => Vec::new(),
        };
        let mut removed = 0;
        if let Some(ns) = self.namespaces.get_mut(namespace) {
            for key in keys {
                if ns.remove_key(&key).is_some() {
                    removed += 1;
                }
            }
        }
        if self
            .namespaces
            .get(namespace)
            .map(|ns| ns.is_empty())
            .unwrap_or(false)
        {
            self.namespaces.remove(namespace);
        }
        removed
    }

    pub fn clear_namespace(&mut self, namespace: &str) {
        self.namespaces.remove(namespace);
    }

    pub fn clear(&mut self) {
        self.namespaces.clear();
    }

    pub fn rebuild(&mut self, docs: Vec<LexicalDoc>) {
        self.clear();
        let mut docs = docs;
        docs.sort_by(|a, b| a.namespace.cmp(&b.namespace).then_with(|| a.id.cmp(&b.id)));
        for doc in docs {
            self.add(doc);
        }
    }

    pub fn len(&self) -> usize {
        self.namespaces.values().map(|ns| ns.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.namespaces.values().all(|ns| ns.is_empty())
    }

    pub fn search(
        &self,
        namespace: &str,
        query: &str,
        opts: &SearchOptions,
    ) -> LexicalSearchOutcome {
        let Some(ns) = self.namespaces.get(namespace) else {
            return LexicalSearchOutcome::default();
        };
        let parsed = parse_query(query, opts.fuzzy_max_distance);
        if parsed.groups.is_empty() {
            return LexicalSearchOutcome::default();
        }

        let domain_requested = !opts.domains.is_empty();
        let mut hits = score_namespace(ns, &parsed, opts, domain_requested);
        let mut domain_filter_applied = domain_requested;
        if domain_requested && hits.is_empty() {
            hits = score_namespace(ns, &parsed, opts, false);
            domain_filter_applied = false;
        }

        finalize(hits, opts.limit)
            .into_iter()
            .map(|hit| LexicalHit {
                id: hit.id,
                score: hit.score,
            })
            .collect::<Vec<_>>()
            .into_outcome(domain_filter_applied)
    }
}

#[derive(Debug, Clone)]
pub struct LexicalHit {
    pub id: String,
    pub score: f32,
}

#[derive(Debug, Default, Clone)]
pub struct LexicalSearchOutcome {
    pub hits: Vec<LexicalHit>,
    /// True when a requested domain filter actually selected the rows.
    /// False covers both "no filter asked for" and the documented fallback.
    pub domain_filter_applied: bool,
}

trait IntoOutcome {
    fn into_outcome(self, domain_filter_applied: bool) -> LexicalSearchOutcome;
}

impl IntoOutcome for Vec<LexicalHit> {
    fn into_outcome(self, domain_filter_applied: bool) -> LexicalSearchOutcome {
        LexicalSearchOutcome {
            hits: self,
            domain_filter_applied,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub now: i64,
    pub as_of: Option<i64>,
    pub include_tasks: bool,
    pub include_archived: bool,
    pub principal: String,
    pub domains: Vec<String>,
    pub fuzzy_max_distance: usize,
    pub limit: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            now: 0,
            as_of: None,
            include_tasks: false,
            include_archived: false,
            principal: "admin".to_string(),
            domains: Vec::new(),
            fuzzy_max_distance: 2,
            limit: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clause {
    Term(String),
    Phrase(String),
    Fuzzy { token: String, distance: usize },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    /// Clauses inside one group are ANDed; groups are ORed.
    pub groups: Vec<Vec<Clause>>,
}

pub fn parse_query(query: &str, fuzzy_max_distance: usize) -> ParsedQuery {
    let fuzzy_max = fuzzy_max_distance.min(2);
    let mut groups: Vec<Vec<Clause>> = vec![Vec::new()];
    let mut current = String::new();
    let mut in_quote = false;

    for ch in query.chars() {
        match ch {
            '"' => {
                if in_quote {
                    push_phrase(&mut groups, &current);
                    current.clear();
                    in_quote = false;
                } else {
                    push_token(&mut groups, &current, fuzzy_max);
                    current.clear();
                    in_quote = true;
                }
            }
            c if c.is_whitespace() && !in_quote => {
                push_token(&mut groups, &current, fuzzy_max);
                current.clear();
            }
            c => current.push(c),
        }
    }
    if in_quote {
        push_phrase(&mut groups, &current);
    } else {
        push_token(&mut groups, &current, fuzzy_max);
    }

    groups.retain(|group| !group.is_empty());
    ParsedQuery { groups }
}

fn push_phrase(groups: &mut [Vec<Clause>], raw: &str) {
    let phrase = raw.trim();
    if phrase.is_empty() {
        return;
    }
    if let Some(group) = groups.last_mut() {
        group.push(Clause::Phrase(phrase.to_string()));
    }
}

fn push_token(groups: &mut Vec<Vec<Clause>>, raw: &str, fuzzy_max: usize) {
    let token = raw.trim();
    if token.is_empty() {
        return;
    }
    if token.eq_ignore_ascii_case("OR") {
        if groups.last().map(|group| !group.is_empty()).unwrap_or(false) {
            groups.push(Vec::new());
        }
        return;
    }
    if let Some((stem, distance)) = parse_fuzzy(token, fuzzy_max) {
        if let Some(group) = groups.last_mut() {
            group.push(Clause::Fuzzy {
                token: normalize_query_token(&stem),
                distance,
            });
        }
    } else if let Some(group) = groups.last_mut() {
        group.push(Clause::Term(token.to_string()));
    }
}

fn parse_fuzzy(token: &str, fuzzy_max: usize) -> Option<(String, usize)> {
    let tilde = token.rfind('~')?;
    let stem = &token[..tilde];
    if stem.is_empty() {
        return None;
    }
    let suffix = &token[tilde + 1..];
    let distance = if suffix.is_empty() {
        1
    } else {
        suffix.parse::<usize>().ok()?
    };
    Some((stem.to_string(), distance.min(fuzzy_max).min(2)))
}

fn normalize_query_token(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Split on whitespace, lowercase, and strip punctuation from the edges.
pub fn tokenize_whitespace(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|raw| {
            let token = raw
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            if token.is_empty() {
                None
            } else {
                Some(token)
            }
        })
        .collect()
}

/// Split code identifiers on path/namespace separators and camelCase
/// boundaries, lowercase, and strip punctuation from the edges.
pub fn tokenize_code(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for segment in text.split(|c: char| matches!(c, '/' | '\\' | '.' | ':' | '_' | '-')) {
        let segment = segment.trim_matches(|c: char| !c.is_alphanumeric());
        if segment.is_empty() {
            continue;
        }
        let chars: Vec<char> = segment.chars().collect();
        let mut start = 0usize;
        for i in 1..chars.len() {
            let prev = chars[i - 1];
            let current = chars[i];
            let next = chars.get(i + 1).copied();
            let camel_boundary = (prev.is_lowercase() && current.is_uppercase())
                || (prev.is_uppercase()
                    && current.is_uppercase()
                    && next.map(|c| c.is_lowercase()).unwrap_or(false));
            if camel_boundary {
                let token: String = chars[start..i].iter().collect();
                push_lowercase(&mut out, &token);
                start = i;
            }
        }
        let token: String = chars[start..].iter().collect();
        push_lowercase(&mut out, &token);
    }
    out
}

fn push_lowercase(out: &mut Vec<String>, token: &str) {
    let token = token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    if !token.is_empty() {
        out.push(token);
    }
}

/// ReBAC v1 viewer predicate, duplicated here (not imported) so the module
/// stays portable to the WASM crate. `src/rebac.rs` delegates to this function
/// for the same reason: one semantics, two call sites.
pub fn acl_allows(user: &str, resource_owner: &str, relation: &str, acl: &[String]) -> bool {
    if user.is_empty() {
        return false;
    }
    if user == "admin" {
        return true;
    }
    if !resource_owner.is_empty() && user == resource_owner {
        return true;
    }
    match relation {
        "viewer" => acl.is_empty() || acl.iter().any(|entry| entry == "*" || entry == user),
        "editor" | "owner" => false,
        _ => false,
    }
}

fn doc_visible(doc: &LexicalDoc, opts: &SearchOptions) -> bool {
    if doc.structural || doc.registry || doc.conflicting {
        return false;
    }
    if doc.archived && !opts.include_archived {
        return false;
    }
    if doc.memory_type == "task" && !opts.include_tasks {
        return false;
    }
    if !retirement_visible(doc, opts.now, opts.as_of) {
        return false;
    }
    acl_allows(&opts.principal, &doc.user_id, "viewer", &doc.visible_to)
}

fn retirement_visible(doc: &LexicalDoc, now: i64, as_of: Option<i64>) -> bool {
    match as_of {
        None => doc.valid_to.map_or(true, |v| v > 0 && v > now),
        Some(ts) => {
            doc.valid_from.unwrap_or(0) <= ts
                && doc.valid_to.map_or(true, |v| v > 0 && ts < v)
        }
    }
}

fn domain_matches(doc: &LexicalDoc, domains: &[String]) -> bool {
    domains.is_empty() || domains.iter().any(|d| doc.domains.iter().any(|own| own == d))
}

#[derive(Debug)]
struct ScoredDoc {
    id: String,
    score: f32,
}

fn score_namespace(
    ns: &NamespaceIndex,
    parsed: &ParsedQuery,
    opts: &SearchOptions,
    apply_domains: bool,
) -> Vec<ScoredDoc> {
    let n = ns.docs.len() as f64;
    if n == 0.0 {
        return Vec::new();
    }
    let mut scored = Vec::new();
    for (key, doc) in &ns.docs {
        if !doc_visible(doc, opts) {
            continue;
        }
        if apply_domains && !domain_matches(doc, &opts.domains) {
            continue;
        }
        if let Some(score) = eval_query(ns, key, parsed, n) {
            if score > 0.0 {
                scored.push(ScoredDoc {
                    id: doc.id.clone(),
                    score: score as f32,
                });
            }
        }
    }
    scored
}

fn eval_query(ns: &NamespaceIndex, key: &str, parsed: &ParsedQuery, n: f64) -> Option<f64> {
    let mut total = 0.0;
    let mut matched = false;
    for group in &parsed.groups {
        if let Some(score) = eval_group(ns, key, group, n) {
            total += score;
            matched = true;
        }
    }
    if matched {
        Some(total)
    } else {
        None
    }
}

fn eval_group(ns: &NamespaceIndex, key: &str, group: &[Clause], n: f64) -> Option<f64> {
    let mut total = 0.0;
    for clause in group {
        total += score_clause(ns, key, clause, n)?;
    }
    Some(total)
}

fn score_clause(ns: &NamespaceIndex, key: &str, clause: &Clause, n: f64) -> Option<f64> {
    match clause {
        Clause::Term(raw) => score_term_clause(ns, key, raw, n),
        Clause::Phrase(raw) => score_phrase_clause(ns, key, raw, n),
        Clause::Fuzzy { token, distance } => {
            if token.is_empty() {
                return None;
            }
            let mut expansions: Vec<(String, usize)> = ns
                .postings
                .keys()
                .filter_map(|term| {
                    levenshtein_bounded(term, token, *distance).map(|d| (term.clone(), d))
                })
                .collect();
            expansions.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            expansions.truncate(MAX_FUZZY_EXPANSIONS);

            let mut best: Option<f64> = None;
            for (term, edit_distance) in expansions {
                let contribution = score_term_across_fields(ns, key, &term, n);
                if contribution <= 0.0 {
                    continue;
                }
                let decayed = contribution * 0.5_f64.powi(edit_distance as i32);
                best = Some(best.map_or(decayed, |current| current.max(decayed)));
            }
            best
        }
    }
}

fn score_term_clause(ns: &NamespaceIndex, key: &str, raw: &str, n: f64) -> Option<f64> {
    let mut total = 0.0;
    let mut matched = false;
    for field in ALL_FIELDS {
        let tokens = field.tokenize(raw);
        if tokens.is_empty() {
            continue;
        }
        if tokens
            .iter()
            .all(|token| field_tf(ns, key, token, field).is_some())
        {
            matched = true;
            for token in &tokens {
                total += bm25_term(ns, key, token, field, n);
            }
        }
    }
    if matched {
        Some(total)
    } else {
        None
    }
}

fn score_phrase_clause(ns: &NamespaceIndex, key: &str, raw: &str, n: f64) -> Option<f64> {
    let mut total = 0.0;
    let mut matched = false;
    for field in ALL_FIELDS {
        let tokens = field.tokenize(raw);
        match tokens.len() {
            0 => continue,
            1 => {
                if field_tf(ns, key, &tokens[0], field).is_some() {
                    matched = true;
                    total += bm25_term(ns, key, &tokens[0], field, n);
                }
            }
            _ => {
                if phrase_adjacent(ns, key, &tokens, field) {
                    matched = true;
                    let mut idf_sum = 0.0;
                    for token in &tokens {
                        total += bm25_term(ns, key, token, field, n);
                        idf_sum += idf_term(ns, token, n);
                    }
                    total += 2.0 * idf_sum;
                }
            }
        }
    }
    if matched {
        Some(total)
    } else {
        None
    }
}

fn score_term_across_fields(ns: &NamespaceIndex, key: &str, term: &str, n: f64) -> f64 {
    ALL_FIELDS
        .iter()
        .map(|field| bm25_term(ns, key, term, *field, n))
        .sum()
}

fn field_tf(ns: &NamespaceIndex, key: &str, term: &str, field: Field) -> Option<usize> {
    ns.postings.get(term).and_then(|postings| {
        postings
            .iter()
            .find(|p| p.doc_id == key && p.field == field)
            .map(|p| p.tf)
    })
}

fn phrase_adjacent(ns: &NamespaceIndex, key: &str, terms: &[String], field: Field) -> bool {
    let Some(first) = ns.postings.get(&terms[0]) else {
        return false;
    };
    let Some(first_positions) = first
        .iter()
        .find(|p| p.doc_id == key && p.field == field)
        .map(|p| p.positions.as_slice())
    else {
        return false;
    };

    for start in first_positions {
        let mut adjacent = true;
        for (offset, term) in terms.iter().enumerate().skip(1) {
            let expected = start + offset;
            let present = ns
                .postings
                .get(term)
                .and_then(|postings| {
                    postings
                        .iter()
                        .find(|p| p.doc_id == key && p.field == field)
                })
                .map(|p| p.positions.binary_search(&expected).is_ok())
                .unwrap_or(false);
            if !present {
                adjacent = false;
                break;
            }
        }
        if adjacent {
            return true;
        }
    }
    false
}

fn idf_term(ns: &NamespaceIndex, term: &str, n: f64) -> f64 {
    let df = ns.doc_freq.get(term).copied().unwrap_or(0) as f64;
    if df <= 0.0 {
        return 0.0;
    }
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

fn bm25_term(ns: &NamespaceIndex, key: &str, term: &str, field: Field, n: f64) -> f64 {
    let Some(tf) = field_tf(ns, key, term, field) else {
        return 0.0;
    };
    let df = ns.doc_freq.get(term).copied().unwrap_or(0) as f64;
    if df <= 0.0 {
        return 0.0;
    }
    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
    let dl = ns.doc_len.get(key).copied().unwrap_or(1) as f64;
    let avgdl = if ns.avgdl > 0.0 { ns.avgdl } else { 1.0 };
    let tf = tf as f64;
    let tf_norm = (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avgdl));
    idf * tf_norm * ns.field_weights[field.weight_index()]
}

fn finalize(mut scored: Vec<ScoredDoc>, limit: usize) -> Vec<ScoredDoc> {
    if scored.is_empty() {
        return Vec::new();
    }
    let max_score = scored
        .iter()
        .map(|hit| hit.score)
        .fold(0.0_f32, f32::max);
    if max_score > 0.0 {
        for hit in &mut scored {
            hit.score = (1.0 - hit.score / max_score).clamp(0.0, 1.0);
        }
    } else {
        for hit in &mut scored {
            hit.score = 0.0;
        }
    }
    scored.sort_by(|a, b| {
        a.score
            .total_cmp(&b.score)
            .then_with(|| a.id.cmp(&b.id))
    });
    let limit = limit.max(1);
    scored.truncate(limit);
    scored
}

/// Levenshtein distance capped at `max`; returns `None` when the true distance
/// exceeds the cap. The banded DP keeps the fuzzy expansion bounded.
fn levenshtein_bounded(a: &str, b: &str, max: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        let mut row_min = current[0];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
            row_min = row_min.min(current[j + 1]);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[b.len()];
    if distance <= max {
        Some(distance)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_is_bounded() {
        assert_eq!(levenshtein_bounded("color", "colour", 1), Some(1));
        assert_eq!(levenshtein_bounded("color", "color", 0), Some(0));
        assert_eq!(levenshtein_bounded("color", "banana", 2), None);
    }

    #[test]
    fn parse_query_keeps_phrases_fuzzy_and_or_groups() {
        let parsed = parse_query("alpha \"beta gamma\" delta~2 OR epsilon", 2);
        assert_eq!(parsed.groups.len(), 2);
        assert_eq!(parsed.groups[0].len(), 3);
        assert_eq!(parsed.groups[1].len(), 1);
        assert!(matches!(parsed.groups[0][1], Clause::Phrase(_)));
        assert!(matches!(
            parsed.groups[0][2],
            Clause::Fuzzy { distance: 2, .. }
        ));
    }
}
