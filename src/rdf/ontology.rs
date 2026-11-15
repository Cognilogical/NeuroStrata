use crate::domain;
use crate::registry;
use crate::rdf::shacl::ShaclViolation;
use oxrdf::NamedOrBlankNode;
use oxrdf::Term;
use crate::traits::MemoryPayload;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, Copy, Default)]
pub struct QueryBudget {
    pub max_ms: u64,
    pub max_nodes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ClosureResult {
    pub results: Vec<String>,
    pub evidence: Vec<crate::traits::SearchEvidence>,
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainEntry {
    pub slug: String,
    pub parent: Option<String>,
    pub aliases: Vec<String>,
    pub concept_iri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShaclShapeDef {
    pub shape_iri: String,
    pub target_class: Option<String>,
    pub property_constraints: Vec<(String, Option<String>)>,
    pub min_counts: BTreeMap<String, u32>,
    pub max_counts: BTreeMap<String, u32>,
    pub datatypes: BTreeMap<String, String>,
    pub in_values: BTreeMap<String, Vec<String>>,
    pub node_kinds: BTreeMap<String, String>,
    pub messages: BTreeMap<String, String>,
}

/// Inner `[ ... ]` property-constraint data accumulated while the oxttl
/// parser still has not emitted the enclosing `sh:property` push.
#[derive(Debug, Default)]
struct PendingShapeProp {
    path: Option<String>,
    in_lists: Vec<String>,
    in_tokens: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ConceptIndexEntry {
    pub memory_id: String,
    pub slug: String,
    pub pref_label: String,
    pub alt_labels: Vec<String>,
    pub definition: String,
    pub domain: Option<String>,
    pub concept_iri: Option<String>,
    pub broader: Vec<String>,
    pub related: Vec<String>,
    pub contrasts_with: Vec<String>,
    pub realization_status: String,
}

#[derive(Debug, Clone, Default)]
pub struct OntologyIndex {
    pub domains: Vec<DomainEntry>,
    pub shacl_shapes: Vec<ShaclShapeDef>,
    pub action_types: BTreeMap<String, crate::rdf::action::ActionTypeDefinition>,
    children: BTreeMap<String, Vec<String>>,
    alias_map: BTreeMap<String, String>,
    sub_properties: BTreeMap<String, Vec<String>>,
    sub_classes: BTreeMap<String, BTreeSet<String>>,
    equivalent: BTreeMap<String, String>,
    // New fields for 2xe9 spec
    equivalent_class: BTreeMap<String, BTreeSet<String>>,
    equivalent_property: BTreeMap<String, BTreeSet<String>>,
    inverse_of: BTreeMap<String, BTreeSet<String>>,
    transitive_properties: BTreeSet<String>,
    symmetric_properties: BTreeSet<String>,
    alt_labels: BTreeMap<String, BTreeSet<String>>,
    exact_match: BTreeMap<String, BTreeSet<String>>,
    broader: BTreeMap<String, BTreeSet<String>>,
    narrower: BTreeMap<String, BTreeSet<String>>,
    // Concept taxonomy fields (Task 3)
    pub concepts: BTreeMap<String, ConceptIndexEntry>,
    pub concept_aliases: BTreeMap<String, BTreeSet<String>>,
    pub related: BTreeMap<String, BTreeSet<String>>,
    pub contrasts_with: BTreeMap<String, BTreeSet<String>>,
}

impl OntologyIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_registry_metadata(metadata: &Value) -> Self {
        let entries_raw = registry::registry_domain_entries(metadata);
        let mut domains = Vec::new();
        for v in entries_raw {
            let slug_raw = v.get("slug").and_then(|s| s.as_str()).unwrap_or("");
            let slug = match domain::normalize_domain_slug(slug_raw) {
                Some(s) => s,
                None => continue,
            };
            let parent = v
                .get("parent")
                .and_then(|p| p.as_str())
                .and_then(domain::normalize_domain_slug);
            let aliases = v
                .get("aliases")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let concept_iri = v
                .get("concept_iri")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            domains.push(DomainEntry {
                slug,
                parent,
                aliases,
                concept_iri,
            });
        }

        // Cycle guard: drop parent links that would create a cycle.
        let slug_set: BTreeSet<String> = domains.iter().map(|d| d.slug.clone()).collect();
        let mut parent_map: BTreeMap<String, Option<String>> = BTreeMap::new();
        for d in &domains {
            parent_map.insert(d.slug.clone(), d.parent.clone());
        }
        // Detect cycles via DFS.
        for start in &slug_set {
            let mut seen = BTreeSet::new();
            let mut cur = Some(start.clone());
            while let Some(s) = cur {
                if !seen.insert(s.clone()) {
                    // cycle: drop parent of start
                    parent_map.insert(start.clone(), None);
                    break;
                }
                cur = parent_map.get(&s).cloned().flatten();
                if cur.as_ref().map(|c| !slug_set.contains(c)).unwrap_or(false) {
                    // parent not in set; stop
                    break;
                }
            }
        }

        // Rebuild domains with cleaned parents.
        for d in &mut domains {
            d.parent = parent_map.get(&d.slug).cloned().flatten();
        }

        Self::from_entries(domains)
    }

    /// Build an index from domain entries, registering each entry's parent,
    /// concept IRI, and aliases as explicit ontology relations.
    pub fn from_entries(entries: Vec<DomainEntry>) -> Self {
        let mut index = if entries.is_empty() {
            Self::empty()
        } else {
            OntologyIndex {
                domains: entries,
                children: BTreeMap::new(),
                alias_map: BTreeMap::new(),
                sub_properties: BTreeMap::new(),
                sub_classes: BTreeMap::new(),
                equivalent: BTreeMap::new(),
                equivalent_class: BTreeMap::new(),
                equivalent_property: BTreeMap::new(),
                inverse_of: BTreeMap::new(),
                transitive_properties: BTreeSet::new(),
                symmetric_properties: BTreeSet::new(),
                alt_labels: BTreeMap::new(),
                exact_match: BTreeMap::new(),
                broader: BTreeMap::new(),
                narrower: BTreeMap::new(),
                shacl_shapes: Vec::new(),
                action_types: BTreeMap::new(),
                concepts: BTreeMap::new(),
                concept_aliases: BTreeMap::new(),
                related: BTreeMap::new(),
                contrasts_with: BTreeMap::new(),
            }
        };

        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut alias_map: BTreeMap<String, String> = BTreeMap::new();
        for d in index.domains.clone() {
            for alias in &d.aliases {
                alias_map.insert(alias.to_lowercase(), d.slug.clone());
                index.add_alt_label(&d.slug, alias);
            }
            // slug itself maps to itself
            alias_map.insert(d.slug.to_lowercase(), d.slug.clone());
            if let Some(parent) = &d.parent {
                children
                    .entry(parent.clone())
                    .or_default()
                    .push(d.slug.clone());
                index.add_sub_class(&d.slug, parent);
                index.add_broader(&d.slug, parent);
            }
            if let Some(iri) = &d.concept_iri {
                index.add_equivalent(&d.slug, iri);
                index.add_exact_match(&d.slug, iri);
            }
        }
        index.children = children;
        index.alias_map = alias_map;
        index
    }

    /// Register one more domain on an already-built index, keeping the alias,
    /// sub-class, broader, children, and concept-IRI relations in sync. A slug
    /// already present is a no-op, so repeated refreshes stay idempotent.
    pub fn add_domain(&mut self, entry: DomainEntry) {
        if self.domains.iter().any(|d| d.slug == entry.slug) {
            return;
        }
        for alias in &entry.aliases {
            self.alias_map.insert(alias.to_lowercase(), entry.slug.clone());
            self.add_alt_label(&entry.slug, alias);
        }
        self.alias_map
            .insert(entry.slug.to_lowercase(), entry.slug.clone());
        if let Some(parent) = &entry.parent {
            self.children
                .entry(parent.clone())
                .or_default()
                .push(entry.slug.clone());
            self.add_sub_class(&entry.slug, parent);
            self.add_broader(&entry.slug, parent);
        }
        if let Some(iri) = &entry.concept_iri {
            self.add_equivalent(&entry.slug, iri);
            self.add_exact_match(&entry.slug, iri);
        }
        self.domains.push(entry);
    }

    /// The registered domain entries, in registration order.
    pub fn domains(&self) -> &[DomainEntry] {
        &self.domains
    }

    /// One registered domain by slug.
    pub fn domain(&self, slug: &str) -> Option<&DomainEntry> {
        self.domains.iter().find(|d| d.slug == slug)
    }

    /// Direct children of a registered domain (empty for unknown slugs; the
    /// root domain is `"global"`).
    pub fn children_of(&self, slug: &str) -> Vec<String> {
        self.children.get(slug).cloned().unwrap_or_default()
    }

    /// Whether a property is declared symmetric (e.g. RELATES_TO).
    pub fn is_symmetric(&self, prop: &str) -> bool {
        self.symmetric_properties.contains(prop)
    }

    /// Populate the index from a vocabulary JSON object. Recognised optional
    /// keys: `sub_classes`, `equivalents`, `sub_properties`,
    /// `equivalent_properties`, `inverse_of`, `transitive_properties`,
    /// `symmetric_properties`, `broader`, `exact_match`, `alt_labels`.
    pub fn load_vocabulary_value(&mut self, vocab: &Value) {
        if let Some(arr) = vocab.get("sub_classes").and_then(Value::as_array) {
            for pair in arr {
                if let Some((child, parent)) = vocabulary_pair(pair) {
                    self.add_sub_class(child, parent);
                }
            }
        }
        if let Some(arr) = vocab.get("equivalents").and_then(Value::as_array) {
            for pair in arr {
                if let Some((a, b)) = vocabulary_pair(pair) {
                    self.add_equivalent(a, b);
                    self.add_equivalent_class(a, b);
                }
            }
        }
        if let Some(arr) = vocab.get("sub_properties").and_then(Value::as_array) {
            for pair in arr {
                if let Some((child, parent)) = vocabulary_pair(pair) {
                    self.add_sub_property(child, parent);
                }
            }
        }
        if let Some(arr) = vocab
            .get("equivalent_properties")
            .and_then(Value::as_array)
        {
            for pair in arr {
                if let Some((a, b)) = vocabulary_pair(pair) {
                    self.add_equivalent_property(a, b);
                }
            }
        }
        if let Some(arr) = vocab.get("inverse_of").and_then(Value::as_array) {
            for pair in arr {
                if let Some((a, b)) = vocabulary_pair(pair) {
                    self.add_inverse_of(a, b);
                }
            }
        }
        if let Some(arr) = vocab
            .get("transitive_properties")
            .and_then(Value::as_array)
        {
            for p in arr {
                if let Some(s) = p.as_str() {
                    self.add_transitive_property(s);
                }
            }
        }
        if let Some(arr) = vocab
            .get("symmetric_properties")
            .and_then(Value::as_array)
        {
            for p in arr {
                if let Some(s) = p.as_str() {
                    self.add_symmetric_property(s);
                }
            }
        }
        if let Some(arr) = vocab.get("broader").and_then(Value::as_array) {
            for pair in arr {
                if let Some((narrow, broad)) = vocabulary_pair(pair) {
                    self.add_broader(narrow, broad);
                }
            }
        }
        if let Some(arr) = vocab.get("exact_match").and_then(Value::as_array) {
            for pair in arr {
                if let Some((a, b)) = vocabulary_pair(pair) {
                    self.add_exact_match(a, b);
                }
            }
        }
        if let Some(arr) = vocab.get("alt_labels").and_then(Value::as_array) {
            for pair in arr {
                if let Some((symbol, label)) = vocabulary_pair(pair) {
                    self.add_alt_label(symbol, label);
                }
            }
        }
    }

    pub fn add_sub_class(&mut self, child: &str, parent: &str) {
        self.sub_classes
            .entry(child.to_string())
            .or_default()
            .insert(parent.to_string());
    }

    pub fn super_classes(&self, class: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::new();
        if let Some(parents) = self.sub_classes.get(class) {
            for p in parents {
                queue.push_back(p.clone());
            }
        }
        while let Some(cur) = queue.pop_front() {
            if !out.insert(cur.clone()) {
                continue;
            }
            if let Some(parents) = self.sub_classes.get(&cur) {
                for p in parents {
                    if !out.contains(p) {
                        queue.push_back(p.clone());
                    }
                }
            }
        }
        out
    }

    pub fn expand_class(&self, class: &str) -> Vec<String> {
        let mut out = BTreeSet::new();
        out.insert(class.to_string());
        for p in self.super_classes(class) {
            out.insert(p);
        }
        // equivalence: include equivalent classes
        if let Some(rep) = self.equivalent_of(class) {
            out.insert(rep.clone());
        }
        let mut v: Vec<String> = out.into_iter().collect();
        v.sort();
        v
    }

    pub fn add_equivalent(&mut self, a: &str, b: &str) {
        self.equivalent.insert(a.to_string(), b.to_string());
        self.equivalent.insert(b.to_string(), a.to_string());
        // Mirror into equivalent_class so equivalents_of() also sees concept
        // IRIs registered via add_equivalent (not only via add_equivalent_class).
        self.equivalent_class
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        self.equivalent_class
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    pub fn equivalent_of(&self, class: &str) -> Option<String> {
        self.equivalent.get(class).cloned()
    }

    pub fn add_sub_property(&mut self, child: &str, parent: &str) {
        self.sub_properties
            .entry(child.to_string())
            .or_default()
            .push(parent.to_string());
    }

    // New ontology mutators (2xe9 spec §5.1 / §11.4–11.6)
    pub fn add_equivalent_class(&mut self, a: &str, b: &str) {
        self.equivalent_class
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        self.equivalent_class
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    pub fn add_equivalent_property(&mut self, a: &str, b: &str) {
        self.equivalent_property
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        // Symmetric reverse so equivalent_property(b) works too.
        self.equivalent_property
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    pub fn add_inverse_of(&mut self, a: &str, b: &str) {
        self.inverse_of
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        // Symmetric reverse so inverse_of(b) works too.
        self.inverse_of
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    pub fn add_transitive_property(&mut self, prop: &str) {
        self.transitive_properties.insert(prop.to_string());
    }

    pub fn add_symmetric_property(&mut self, prop: &str) {
        self.symmetric_properties.insert(prop.to_string());
    }

    pub fn add_broader(&mut self, narrow: &str, broad: &str) {
        self.broader
            .entry(narrow.to_string())
            .or_default()
            .insert(broad.to_string());
        self.narrower
            .entry(broad.to_string())
            .or_default()
            .insert(narrow.to_string());
    }

    pub fn add_exact_match(&mut self, a: &str, b: &str) {
        self.exact_match
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        self.exact_match
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    pub fn add_alt_label(&mut self, symbol: &str, label: &str) {
        self.alt_labels
            .entry(symbol.to_string())
            .or_default()
            .insert(label.to_string());
    }

    // New methods for 2xe9 spec
    pub fn subclasses_of(&self, class: &str) -> BTreeSet<String> {
        // Traverse DOWNWARD: find every child in sub_classes whose parent set
        // names `class`, then recurse below those children.
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::new();
        for (child, parents) in &self.sub_classes {
            if parents.contains(class) {
                queue.push_back(child.clone());
            }
        }
        while let Some(cur) = queue.pop_front() {
            if !out.insert(cur.clone()) {
                continue;
            }
            for (child, parents) in &self.sub_classes {
                if parents.contains(&cur) && !out.contains(child) {
                    queue.push_back(child.clone());
                }
            }
        }
        out
    }

    pub fn superclasses_of(&self, class: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::new();
        if let Some(parents) = self.sub_classes.get(class) {
            for p in parents {
                queue.push_back(p.clone());
            }
        }
        while let Some(cur) = queue.pop_front() {
            if !out.insert(cur.clone()) {
                continue;
            }
            if let Some(parents) = self.sub_classes.get(&cur) {
                for p in parents {
                    if !out.contains(p) {
                        queue.push_back(p.clone());
                    }
                }
            }
        }
        out
    }

    pub fn subproperties_of(&self, iri: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some(sups) = self.sub_properties.get(iri) {
            for s in sups {
                out.insert(s.clone());
            }
        }
        out
    }

    pub fn equivalents_of(&self, iri: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some(eqs) = self.equivalent_class.get(iri) {
            for e in eqs {
                out.insert(e.clone());
            }
        }
        if let Some(eqs) = self.equivalent_property.get(iri) {
            for e in eqs {
                out.insert(e.clone());
            }
        }
        out
    }

    pub fn inverse_of(&self, iri: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some(inv) = self.inverse_of.get(iri) {
            for i in inv {
                out.insert(i.clone());
            }
        }
        out
    }

    pub fn is_transitive(&self, iri: &str) -> bool {
        self.transitive_properties.contains(iri)
    }

    pub fn aliases_for(&self, symbol: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some(slug) = self.canonical_slug(symbol) {
            out.insert(slug.clone());
            if let Some(aliases) = self.alt_labels.get(&slug) {
                for a in aliases {
                    out.insert(a.clone());
                }
            }
            // Also include aliases mapped via alias_map
            if let Some(slug_from_alias) = self.alias_map.get(&symbol.to_lowercase()) {
                out.insert(slug_from_alias.clone());
            }
        }
        out
    }

    pub fn alt_labels_for(&self, symbol: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some(slug) = self.canonical_slug(symbol) {
            if let Some(alt) = self.alt_labels.get(&slug) {
                for a in alt { out.insert(a.clone()); }
            }
        }
        // Direct lookup for symbols not yet registered as domain slugs
        if let Some(alt) = self.alt_labels.get(symbol) {
            for a in alt { out.insert(a.clone()); }
        }
        out
    }

    pub fn expand_domain(&self, slug: &str) -> Vec<String> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(slug.to_string());
        while let Some(cur) = queue.pop_front() {
            if !out.insert(cur.clone()) {
                continue;
            }
            if let Some(kids) = self.children.get(&cur) {
                for k in kids {
                    if !out.contains(k) {
                        queue.push_back(k.clone());
                    }
                }
            }
        }
        let mut v: Vec<String> = out.into_iter().collect();
        v.sort();
        v
    }

    /// Add related concepts to the ontology index.
    pub fn add_related(&mut self, a: &str, b: &str) {
        self.related
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        self.related
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    /// Add contrast relationships to the ontology index.
    pub fn add_contrasts_with(&mut self, a: &str, b: &str) {
        self.contrasts_with
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
        self.contrasts_with
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string());
    }

    /// Get all broader concepts (ancestors) for a concept, excluding the concept itself.
    pub fn concept_broader_of(&self, slug: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::new();
        let mut visited = BTreeSet::new();
        
        queue.push_back(slug.to_string());
        visited.insert(slug.to_string());
        
        while let Some(cur) = queue.pop_front() {
            if let Some(broader) = self.broader.get(&cur) {
                for b in broader {
                    if !visited.contains(b) {
                        visited.insert(b.clone());
                        out.insert(b.clone());
                        queue.push_back(b.clone());
                    }
                }
            }
        }
        
        out
    }

    /// Get all narrower concepts (descendants) for a concept, excluding the concept itself.
    pub fn concept_narrower_of(&self, slug: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::new();
        let mut visited = BTreeSet::new();
        
        queue.push_back(slug.to_string());
        visited.insert(slug.to_string());
        
        while let Some(cur) = queue.pop_front() {
            if let Some(narrower) = self.narrower.get(&cur) {
                for n in narrower {
                    if !visited.contains(n) {
                        visited.insert(n.clone());
                        out.insert(n.clone());
                        queue.push_back(n.clone());
                    }
                }
            }
        }
        
        out
    }

    /// Expand concept query terms to include the concept itself, its direct narrower/related concepts,
    /// and their direct labels and alternative labels.
    pub fn expand_concept_query_terms(&self, query: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return result;
        }
        let mut matched_slugs = BTreeSet::new();
        let normalized_query = trimmed.to_lowercase();

        if self.concepts.contains_key(&normalized_query) {
            matched_slugs.insert(normalized_query.clone());
        }
        if let Some(slug) = domain::normalize_domain_slug(trimmed) {
            if self.concepts.contains_key(&slug) {
                matched_slugs.insert(slug.clone());
            }
            if let Some(targets) = self.concept_aliases.get(&slug) {
                matched_slugs.extend(targets.iter().cloned());
            }
        }
        if let Some(targets) = self.concept_aliases.get(&normalized_query) {
            matched_slugs.extend(targets.iter().cloned());
        }
        if let Some(targets) = self.concept_aliases.get(trimmed) {
            matched_slugs.extend(targets.iter().cloned());
        }
        for tok in normalized_query
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
            .filter(|t| t.len() >= 2)
        {
            if self.concepts.contains_key(tok) {
                matched_slugs.insert(tok.to_string());
            }
            if let Some(targets) = self.concept_aliases.get(tok) {
                matched_slugs.extend(targets.iter().cloned());
            }
        }

        for slug in matched_slugs {
            result.insert(slug.clone());
            if let Some(concept) = self.concepts.get(&slug) {
                if !concept.pref_label.is_empty() {
                    result.insert(concept.pref_label.clone());
                }
                for alt in &concept.alt_labels {
                    if !alt.is_empty() {
                        result.insert(alt.clone());
                    }
                }
            }
            if let Some(narrower) = self.narrower.get(&slug) {
                for n in narrower {
                    result.insert(n.clone());
                    if let Some(nc) = self.concepts.get(n) {
                        if !nc.pref_label.is_empty() {
                            result.insert(nc.pref_label.clone());
                        }
                        for alt in &nc.alt_labels {
                            if !alt.is_empty() {
                                result.insert(alt.clone());
                            }
                        }
                    }
                }
            }
            if let Some(related) = self.related.get(&slug) {
                for r in related {
                    result.insert(r.clone());
                    if let Some(rc) = self.concepts.get(r) {
                        if !rc.pref_label.is_empty() {
                            result.insert(rc.pref_label.clone());
                        }
                        for alt in &rc.alt_labels {
                            if !alt.is_empty() {
                                result.insert(alt.clone());
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// Load concept rows from memory payloads, filtering out archived, retired, conflicting, and non-public concepts.
    /// Validates taxonomy DAG and SKOS disjointness constraints.
    pub fn load_concept_rows(&mut self, rows: &[(String, MemoryPayload)], now_ts: i64) {
        let mut active_rows: Vec<(i64, String, MemoryPayload)> = rows
            .iter()
            .filter_map(|(id, payload)| {
                if payload
                    .metadata
                    .get("archived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    return None;
                }
                if let Some(v) = payload.metadata.get("valid_to") {
                    if !v.is_null() {
                        let retired = v.as_i64().map(|ts| ts > 0 && ts <= now_ts).unwrap_or(true);
                        if retired {
                            return None;
                        }
                    }
                }
                if crate::synaptic_mesh::conflict::is_conflicting_metadata(&payload.metadata) {
                    return None;
                }
                // Privacy-safe OntologyIndex hydration: exclude any concept row with a non-public access_policy.
                if let Some(ap) = payload.metadata.get("access_policy") {
                    if !ap.is_null() {
                        let is_public = match ap.as_object() {
                            Some(obj) if obj.is_empty() => true,
                            Some(obj) => {
                                obj.get("public").and_then(|v| v.as_bool()) == Some(true)
                                    && obj
                                        .get("allowed_users")
                                        .and_then(|v| v.as_array())
                                        .map(|a| a.is_empty())
                                        .unwrap_or(true)
                                    && obj
                                        .get("allowed_roles")
                                        .and_then(|v| v.as_array())
                                        .map(|a| a.is_empty())
                                        .unwrap_or(true)
                            }
                            None => false,
                        };
                        if !is_public {
                            return None;
                        }
                    }
                }
                let vf = payload
                    .metadata
                    .get("valid_from")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                Some((vf, id.clone(), payload.clone()))
            })
            .collect();

        // Sort ascending by (valid_from, id) so later active rows overwrite earlier ones for the same slug.
        active_rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let mut deduped_by_slug: BTreeMap<String, (String, MemoryPayload)> = BTreeMap::new();
        for (_vf, id, payload) in active_rows {
            let Some(c_obj) = payload.metadata.get("concept").and_then(|v| v.as_object()) else {
                continue;
            };
            let Some(raw_slug) = c_obj.get("slug").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(slug) = domain::normalize_domain_slug(raw_slug) else {
                tracing::warn!("skipping concept row '{}' with invalid slug '{}'", id, raw_slug);
                continue;
            };
            deduped_by_slug.insert(slug, (id, payload));
        }

        let mut slug_to_num: BTreeMap<String, u32> = BTreeMap::new();
        let mut next_id: u32 = 1;
        let mut intern_slug = |s: &str| -> u32 {
            if let Some(&id) = slug_to_num.get(s) {
                id
            } else {
                let id = next_id;
                next_id += 1;
                slug_to_num.insert(s.to_string(), id);
                id
            }
        };

        // Pass 1: validate broader DAG incrementally and collect accepted broader edges per slug.
        let mut accepted_broader_pairs: Vec<(u32, u32)> = Vec::new();
        let mut accepted_broader_by_slug: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut rejected_cyclic_slugs: BTreeSet<String> = BTreeSet::new();

        for (slug, (row_id, payload)) in &deduped_by_slug {
            let Some(c_obj) = payload.metadata.get("concept").and_then(|v| v.as_object()) else {
                continue;
            };
            let u = intern_slug(slug);
            let mut valid_parents = Vec::new();
            if let Some(arr) = c_obj.get("broader").and_then(|v| v.as_array()) {
                for item in arr {
                    let Some(raw_p) = item.as_str() else { continue };
                    let Some(p_slug) = domain::normalize_domain_slug(raw_p) else { continue };
                    let v = intern_slug(&p_slug);
                    let mut candidate = accepted_broader_pairs.clone();
                    candidate.push((u, v));
                    if crate::formal_kernel::validate_taxonomy_dag(&candidate) {
                        accepted_broader_pairs = candidate;
                        valid_parents.push(p_slug);
                    } else {
                        tracing::warn!(
                            "skipping cyclic SKOS broader edge '{}' -> '{}' on concept row '{}'",
                            slug,
                            p_slug,
                            row_id
                        );
                        rejected_cyclic_slugs.insert(slug.clone());
                    }
                }
            }
            accepted_broader_by_slug.insert(slug.clone(), valid_parents);
        }

        // Pass 2: validate SKOS S27 disjointness for related and contrasts_with and populate OntologyIndex.
        for (slug, (row_id, payload)) in &deduped_by_slug {
            let Some(c_obj) = payload.metadata.get("concept").and_then(|v| v.as_object()) else {
                continue;
            };
            let valid_broader = accepted_broader_by_slug
                .get(slug)
                .cloned()
                .unwrap_or_default();
            if rejected_cyclic_slugs.contains(slug) && valid_broader.is_empty() {
                continue;
            }
            let u = intern_slug(slug);

            for parent in &valid_broader {
                // Canonical SKOS broader only — never add_sub_class!
                self.add_broader(slug, parent);
            }

            let mut valid_related = Vec::new();
            if let Some(arr) = c_obj.get("related").and_then(|v| v.as_array()) {
                for item in arr {
                    let Some(raw_r) = item.as_str() else { continue };
                    let Some(r_slug) = domain::normalize_domain_slug(raw_r) else { continue };
                    let v = intern_slug(&r_slug);
                    if crate::formal_kernel::validate_skos_disjointness(
                        &accepted_broader_pairs,
                        &[(u, v)],
                        &[],
                    ) {
                        self.add_related(slug, &r_slug);
                        valid_related.push(r_slug);
                    } else {
                        tracing::warn!(
                            "skipping SKOS S27 conflicting related edge '{}' <-> '{}' on concept row '{}'",
                            slug,
                            r_slug,
                            row_id
                        );
                    }
                }
            }

            let mut valid_contrasts = Vec::new();
            if let Some(arr) = c_obj.get("contrasts_with").and_then(|v| v.as_array()) {
                for item in arr {
                    let Some(raw_c) = item.as_str() else { continue };
                    let Some(c_slug) = domain::normalize_domain_slug(raw_c) else { continue };
                    let v = intern_slug(&c_slug);
                    if crate::formal_kernel::validate_skos_disjointness(
                        &accepted_broader_pairs,
                        &[],
                        &[(u, v)],
                    ) {
                        self.add_contrasts_with(slug, &c_slug);
                        valid_contrasts.push(c_slug);
                    } else {
                        tracing::warn!(
                            "skipping SKOS S27 conflicting contrasts_with edge '{}' <-> '{}' on concept row '{}'",
                            slug,
                            c_slug,
                            row_id
                        );
                    }
                }
            }

            let pref_label = c_obj
                .get("pref_label")
                .and_then(|v| v.as_str())
                .unwrap_or(slug.as_str())
                .to_string();
            let alt_labels: Vec<String> = c_obj
                .get("alt_labels")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let definition = c_obj
                .get("definition")
                .and_then(|v| v.as_str())
                .unwrap_or(payload.content.as_str())
                .to_string();
            let domain_opt = c_obj
                .get("domain")
                .and_then(|v| v.as_str())
                .and_then(domain::normalize_domain_slug);
            let concept_iri = c_obj
                .get("concept_iri")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string());
            let realization_status = c_obj
                .get("realization_status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // Multi-target concept_aliases collision preservation (both lowercased and raw)
            self.concept_aliases
                .entry(slug.to_lowercase())
                .or_default()
                .insert(slug.clone());
            if !pref_label.trim().is_empty() {
                self.concept_aliases
                    .entry(pref_label.trim().to_lowercase())
                    .or_default()
                    .insert(slug.clone());
                self.concept_aliases
                    .entry(pref_label.trim().to_string())
                    .or_default()
                    .insert(slug.clone());
            }
            for alt in &alt_labels {
                let trimmed = alt.trim();
                if !trimmed.is_empty() {
                    self.concept_aliases
                        .entry(trimmed.to_lowercase())
                        .or_default()
                        .insert(slug.clone());
                    self.concept_aliases
                        .entry(trimmed.to_string())
                        .or_default()
                        .insert(slug.clone());
                    self.add_alt_label(slug, trimmed);
                }
            }
            if let Some(ref iri) = concept_iri {
                self.add_exact_match(slug, iri);
            }

            self.concepts.insert(
                slug.clone(),
                ConceptIndexEntry {
                    memory_id: row_id.clone(),
                    slug: slug.clone(),
                    pref_label,
                    alt_labels,
                    definition,
                    domain: domain_opt,
                    concept_iri,
                    broader: valid_broader,
                    related: valid_related,
                    contrasts_with: valid_contrasts,
                    realization_status,
                },
            );
        }
    }

    pub fn canonical_slug(&self, raw: &str) -> Option<String> {
        let slug = domain::normalize_domain_slug(raw)?;
        if self.domains.iter().any(|d| d.slug == slug) {
            return Some(slug);
        }
        // Normalize lookup key: try slug then lowercased raw then original lowercased.
        self.alias_map
            .get(&slug)
            .or_else(|| self.alias_map.get(&raw.to_lowercase()))
            .cloned()
    }

    pub fn expand_relation(&self, rel: &str) -> Vec<String> {
        // Transitive closure over subPropertyOf (RDFS).
        let mut out = BTreeSet::new();
        let mut queue = VecDeque::new();
        out.insert(rel.to_string());
        queue.push_back(rel.to_string());
        while let Some(cur) = queue.pop_front() {
            if let Some(supers) = self.sub_properties.get(&cur) {
                for s in supers {
                    if out.insert(s.clone()) {
                        queue.push_back(s.clone());
                    }
                }
            }
        }
        let mut v: Vec<String> = out.into_iter().collect();
        v.sort();
        v
    }

    pub fn closure_subclasses(&self, root: &str, budget: &QueryBudget) -> ClosureResult {
        let mut result = ClosureResult::default();
        let mut queue = VecDeque::new();
        let mut visited = BTreeSet::new();
        let start = std::time::Instant::now();

        queue.push_back(root.to_string());
        visited.insert(root.to_string());

        while let Some(cur) = queue.pop_front() {
            if budget.max_ms > 0 && start.elapsed().as_millis() as u64 > budget.max_ms {
                break;
            }
            if budget.max_nodes > 0 && result.results.len() >= budget.max_nodes {
                break;
            }
            if let Some(kids) = self.children.get(&cur) {
                for k in kids {
                    if !visited.contains(k) {
                        visited.insert(k.clone());
                        queue.push_back(k.clone());
                        result.results.push(k.clone());
                    }
                }
            }
        }

        result
    }

    pub fn closure_governs(&self, root: &str, edges: &[String], budget: &QueryBudget) -> ClosureResult {
        let mut result = ClosureResult::default();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut frontier: VecDeque<String> = VecDeque::new();
        let start = std::time::Instant::now();
        let max_hops = 2usize;

        // Hop 0: direct outgoing edges from root
        for edge in edges {
            let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
            if parts.len() == 2 && parts[0] == root {
                let target = &parts[1];
                if !visited.contains(target) {
                    visited.insert(target.clone());
                    result.results.push(target.clone());
                    frontier.push_back(target.clone());
                }
            }
        }

        // BFS for further bounded transitive hops (default 2 hops)
        let mut current_hops = 1usize;
        while current_hops < max_hops
            && (budget.max_ms == 0 || start.elapsed().as_millis() as u64 <= budget.max_ms)
        {
            if budget.max_nodes > 0 && result.results.len() >= budget.max_nodes {
                break;
            }
            let frontier_len = frontier.len();
            for _ in 0..frontier_len {
                let node = match frontier.pop_front() {
                    Some(n) => n,
                    None => break,
                };
                for edge in edges {
                    let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
                    if parts.len() == 2 && parts[0] == node {
                        let target = &parts[1];
                        if !visited.contains(target) {
                            visited.insert(target.clone());
                            result.results.push(target.clone());
                            frontier.push_back(target.clone());
                        }
                    }
                }
            }
            current_hops += 1;
        }

        result
    }

pub fn load_shacl_shapes(&mut self, ttl: &str) -> anyhow::Result<usize> {
    let mut total = 0usize;
    let parser = oxttl::TurtleParser::new();
    let mut cur_shape: Option<ShaclShapeDef> = None;
    // Turtle `sh:in ( "a" "b" )` collections expand into rdf:first/rdf:rest
    // triples on list blank nodes; record them so sh:in can resolve values.
    let mut list_first: BTreeMap<String, String> = BTreeMap::new();
    let mut list_rest: BTreeMap<String, String> = BTreeMap::new();
    // sh:in triples whose object blank node was not yet attached to any
    // rdf:first/rdf:rest chain, deferred until the owning shape finalizes.
    let mut deferred_in_lists: Vec<(String, String, String)> = Vec::new();
    // oxttl emits inner `[ sh:path ... ; sh:in ... ]` triples BEFORE the
    // enclosing `sh:property` push, so blank-node property constraint data is
    // accumulated and applied when the push finally arrives.
    let mut pending_props: BTreeMap<String, PendingShapeProp> = BTreeMap::new();

    /// Extract list values from a `sh:in` object string: quoted string
    /// literals (honoring backslash escapes) and <IRI> tokens.
    fn parse_shacl_in_values(obj: &str) -> Vec<String> {
        let mut out = Vec::new();
        let chars: Vec<char> = obj.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_whitespace() {
                i += 1;
            } else if chars[i] == '"' {
                i += 1;
                let mut val = String::new();
                while i < chars.len() {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        val.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        i += 1;
                        break;
                    }
                    val.push(chars[i]);
                    i += 1;
                }
                out.push(val);
            } else if chars[i] == '<' {
                i += 1;
                let mut val = String::new();
                while i < chars.len() && chars[i] != '>' {
                    val.push(chars[i]);
                    i += 1;
                }
                i += 1;
                out.push(val);
            } else {
                i += 1;
            }
        }
        out
    }

    /// Walk an RDF list (rdf:first / rdf:rest chain) collecting value tokens.
    fn resolve_sh_in_list(
        head: &str,
        first_map: &BTreeMap<String, String>,
        rest_map: &BTreeMap<String, String>,
    ) -> Vec<String> {
        let mut values = Vec::new();
        let mut cur = head.to_string();
        let mut hops = 0usize;
        while !cur.is_empty() && hops < 10_000 {
            match first_map.get(&cur) {
                Some(v) => values.push(v.clone()),
                None => break,
            }
            match rest_map.get(&cur) {
                Some(next) if !next.is_empty() => cur = next.clone(),
                _ => break,
            }
            hops += 1;
        }
        values
    }

    /// Drain deferred sh:in blank-node lists whose rdf:first/rdf:rest triples
    /// are now known, filling the matching (empty) in_values entry.
    fn resolve_deferred_in_lists(
        shape: &mut ShaclShapeDef,
        deferred: &mut Vec<(String, String, String)>,
        first_map: &BTreeMap<String, String>,
        rest_map: &BTreeMap<String, String>,
    ) {
        let mut i = 0;
        while i < deferred.len() {
            let (shape_iri, path, head) = deferred[i].clone();
            if shape_iri != shape.shape_iri {
                i += 1;
                continue;
            }
            let values = resolve_sh_in_list(&head, first_map, rest_map);
            if values.is_empty() {
                i += 1;
                continue;
            }
            shape
                .in_values
                .entry(path)
                .and_modify(|existing| {
                    if existing.is_empty() {
                        *existing = values.clone();
                    }
                })
                .or_insert_with(|| values.clone());
            deferred.remove(i);
        }
    }

    for q in parser.for_reader(ttl.as_bytes()) {
        let q = q?;
        let s = match q.subject {
            NamedOrBlankNode::NamedNode(nn) => nn.as_str().to_string(),
            _ => q.subject.to_string(),
        };
        let p = q.predicate.as_str().to_string();
        let o = match &q.object {
            Term::NamedNode(nn) => nn.as_str().to_string(),
            Term::Literal(lit) => lit.value().to_string(),
            _ => q.object.to_string(),
        };

        // RDF list collection tracking (rdf:first / rdf:rest chain nodes)
        if p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#first" {
            list_first.entry(s.clone()).or_insert_with(|| o.clone());
        } else if p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest" {
            match &q.object {
                Term::BlankNode(bn) => {
                    list_rest.insert(s.clone(), format!("_:{}", bn.as_str()));
                }
                _ => {
                    list_rest.insert(s.clone(), String::new());
                }
            }
        }

        // sh:NodeShape declaration
        if p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
            && o == "http://www.w3.org/ns/shacl#NodeShape" {
            if let Some(mut sh) = cur_shape.take() {
                resolve_deferred_in_lists(&mut sh, &mut deferred_in_lists, &list_first, &list_rest);
                self.shacl_shapes.push(sh);
                total += 1;
            }
            cur_shape = Some(ShaclShapeDef {
                shape_iri: s.clone(),
                ..Default::default()
            });
        }

        // sh:targetClass
        if p == "http://www.w3.org/ns/shacl#targetClass" {
            if let Some(sh) = cur_shape.as_mut() {
                sh.target_class = Some(o.clone());
            }
        }

        // sh:message
        if p == "http://www.w3.org/ns/shacl#message" {
            if let Some(sh) = cur_shape.as_mut() {
                sh.messages.insert(o.clone(), o.clone());
            }
        }

        // sh:property
        if p == "http://www.w3.org/ns/shacl#property" {
            if let Some(sh) = cur_shape.as_mut() {
                sh.property_constraints.push((o.clone(), None));
                let shape_iri = sh.shape_iri.clone();
                // Apply accumulated inner `[ ... ]` data that oxttl emitted
                // before this push (subject blank node = the push object).
                if let Some(pend) = pending_props.remove(&o) {
                    let path_key = pend.path.clone().unwrap_or_else(|| o.clone());
                    if let Some(last) = sh.property_constraints.last_mut() {
                        last.0 = path_key.clone();
                    }
                    let mut values: Vec<String> = pend.in_tokens;
                    for head in &pend.in_lists {
                        let resolved = resolve_sh_in_list(head, &list_first, &list_rest);
                        if resolved.is_empty() {
                            deferred_in_lists.push((
                                shape_iri.clone(),
                                path_key.clone(),
                                head.clone(),
                            ));
                        } else {
                            values.extend(resolved);
                        }
                    }
                    if !values.is_empty() {
                        sh.in_values.insert(path_key, values);
                    }
                }
            }
        }

        // sh:path
        if p == "http://www.w3.org/ns/shacl#path" {
            if s.starts_with("_:") && cur_shape.is_some() {
                // Inner constraint blank node: record the declared path so it
                // can key the constraint once sh:property pushes it.
                pending_props.entry(s.clone()).or_default().path = Some(o.clone());
            } else if let Some(last) = cur_shape.as_mut().and_then(|sh| sh.property_constraints.last_mut()) {
                last.0 = o.clone();
            }
        }

        // sh:minCount
        if p == "http://www.w3.org/ns/shacl#minCount" {
            let val: u32 = o.parse().unwrap_or(0);
            if let Some(shape_ref) = cur_shape.as_mut() {
                if let Some(last) = shape_ref.property_constraints.last_mut() {
                    shape_ref.min_counts.insert(last.0.clone(), val);
                }
            }
        }

        // sh:maxCount
        if p == "http://www.w3.org/ns/shacl#maxCount" {
            let val: u32 = o.parse().unwrap_or(u32::MAX);
            if let Some(shape_ref) = cur_shape.as_mut() {
                if let Some(last) = shape_ref.property_constraints.last_mut() {
                    shape_ref.max_counts.insert(last.0.clone(), val);
                }
            }
        }

        // sh:datatype
        if p == "http://www.w3.org/ns/shacl#datatype" {
            if let Some(shape_ref) = cur_shape.as_mut() {
                if let Some(last) = shape_ref.property_constraints.last_mut() {
                    shape_ref.datatypes.insert(last.0.clone(), o.clone());
                }
            }
        }

        // sh:nodeKind
        if p == "http://www.w3.org/ns/shacl#nodeKind" {
            if let Some(shape_ref) = cur_shape.as_mut() {
                if let Some(last) = shape_ref.property_constraints.last_mut() {
                    shape_ref.node_kinds.insert(last.0.clone(), o.clone());
                }
            }
        }

        // sh:in
        if p == "http://www.w3.org/ns/shacl#in" {
            if s.starts_with("_:") && cur_shape.is_some() {
                // Inner constraint blank node: buffer the list reference or
                // token values; resolved/applied when sh:property pushes.
                let pend = pending_props.entry(s.clone()).or_default();
                match &q.object {
                    Term::BlankNode(bn) => {
                        let head = format!("_:{}", bn.as_str());
                        pend.in_lists.push(head);
                    }
                    _ => {
                        let toks = parse_shacl_in_values(&o);
                        pend.in_tokens.extend(toks);
                    }
                }
            } else {
                let values: Vec<String>;
                let mut deferred_head: Option<String> = None;
                match &q.object {
                    Term::BlankNode(bn) => {
                        let head = format!("_:{}", bn.as_str());
                        let resolved = resolve_sh_in_list(&head, &list_first, &list_rest);
                        if resolved.is_empty() {
                            values = Vec::new();
                            deferred_head = Some(head);
                        } else {
                            values = resolved;
                        }
                    }
                    _ => values = parse_shacl_in_values(&o),
                }
                if let Some(shape_ref) = cur_shape.as_mut() {
                    if let Some(last) = shape_ref.property_constraints.last_mut() {
                        shape_ref.in_values.insert(last.0.clone(), values);
                    }
                    if let Some(head) = deferred_head {
                        deferred_in_lists.push((
                            shape_ref.shape_iri.clone(),
                            shape_ref
                                .property_constraints
                                .last()
                                .map(|c| c.0.clone())
                                .unwrap_or_default(),
                            head,
                        ));
                    }
                }
            }
        }
    }

    if let Some(mut sh) = cur_shape.take() {
        resolve_deferred_in_lists(&mut sh, &mut deferred_in_lists, &list_first, &list_rest);
        self.shacl_shapes.push(sh);
        total += 1;
    }

    Ok(total)
}

/// Drop loaded SHACL shapes (used when a registry row stops shipping shapes).
pub fn clear_shacl_shapes(&mut self) {
    self.shacl_shapes.clear();
}

/// Validate a memory payload against loaded SHACL shapes.
/// Returns violations extracted from matching shapes; falls back to
/// hardcoded `crate::rdf::shacl::validate_memory` if no shapes are loaded.
    pub fn shacl_validate(&self, payload: &MemoryPayload) -> Vec<ShaclViolation> {
        if self.shacl_shapes.is_empty() {
            return crate::rdf::shacl::validate_memory(payload);
        }

        // Evaluate each matching shape against payload metadata
        let mut violations = Vec::new();
        let memory_type = payload.memory_type.clone();

        for shape in &self.shacl_shapes {
            // Match shape to memory_type via targetClass (ns:Rule, ns:Procedure, ns:Task, etc.)
            let shape_match = shape.target_class.as_deref().map_or(false, |tc| {
                let tail = tc.rsplit(|c| c == '/' || c == '#' || c == ':').next().unwrap_or(tc);
                memory_type == tail || memory_type.eq_ignore_ascii_case(tail)
            });

            if !shape_match && shape.target_class.is_some() {
                continue; // shape not applicable to this memory type
            }

            // Evaluate Memory target shapes (Memory) and matching type shapes
            if shape.target_class.as_deref().map_or(false, |tc| tc.ends_with("Memory")) || shape_match {
                for (path, _) in &shape.property_constraints {
                    let path_violations = self.validate_path(payload, path, shape);
                    violations.extend(path_violations);
                }
            }
        }

        violations
    }

    /// Strict post-image validation combining hardcoded SHACL, custom loaded SHACL shapes,
    /// and ontology domain/reasoning constraints (`ReasoningAdapter::validate_payload`).
    pub fn validate_strict_post_image(&self, payload: &MemoryPayload) -> Vec<ShaclViolation> {
        let mut violations = crate::rdf::shacl::validate_memory(payload);
        for v in self.shacl_validate(payload) {
            if !violations.contains(&v) {
                violations.push(v);
            }
        }
        let adapter = crate::rdf::reasoning::ReasoningAdapter::new();
        for v in adapter.validate_payload(self, payload) {
            if !violations.contains(&v) {
                violations.push(v);
            }
        }
        violations
    }

    fn validate_path(&self, payload: &MemoryPayload, path: &str, shape: &ShaclShapeDef) -> Vec<ShaclViolation> {
        let mut out = Vec::new();
        let meta_key = path
            .trim_start_matches("urn:neurostrata:ns#")
            .trim_start_matches("http://neurostrata.io/ns#")
            .trim_start_matches("https://neurostrata.io/ns#")
            .trim_start_matches("ns:");
        let value = match payload.metadata.get(meta_key).or_else(|| payload.metadata.get(path)) {
            Some(v) => v.clone(),
            None => {
                // Check min_count
                if let Some(min) = shape.min_counts.get(path) {
                    if *min > 0 {
                        out.push(ShaclViolation {
                            field: path.to_string(),
                            why: format!("{} is required (minCount {})", path, min),
                        });
                    }
                }
                return out;
            }
        };

        // max_count checks (object vs array)
        if let Some(max) = shape.max_counts.get(path) {
            match &value {
                serde_json::Value::Array(arr) => {
                    if arr.len() > *max as usize {
                        out.push(ShaclViolation {
                            field: path.to_string(),
                            why: format!("{} exceeds maxCount {}", path, max),
                        });
                    }
                }
                _ if *max == 0 => {}
                _ => {}
            }
        }

        // datatype check
        if let Some(dt) = shape.datatypes.get(path) {
            match &value {
                serde_json::Value::String(_) => {
                    if dt.contains("string") {
                        // ok
                    } else {
                        out.push(ShaclViolation {
                            field: path.to_string(),
                            why: format!("{} must be {}", path, dt),
                        });
                    }
                }
                serde_json::Value::Number(_n) => {
                    if dt.contains("integer") || dt.contains("decimal") {
                        // ok
                    } else {
                        out.push(ShaclViolation {
                            field: path.to_string(),
                            why: format!("{} must be {}", path, dt),
                        });
                    }
                }
                serde_json::Value::Object(_) if dt.contains("BlankNodeOrIRI") || dt.contains("BlankNode") => {
                    // ok
                }
                serde_json::Value::Null => {
                    if dt.contains("BlankNodeOrIRI") {
                        // ok, null allowed for BlankNodeOrIRI
                    } else {
                        out.push(ShaclViolation {
                            field: path.to_string(),
                            why: format!("{} must be {}", path, dt),
                        });
                    }
                }
                _ => {}
            }
        }

        // in values check
        if let Some(allowed) = shape.in_values.get(path) {
            if let Some(s) = value.as_str() {
                if !allowed.iter().any(|a| a == s) {
                    out.push(ShaclViolation {
                        field: path.to_string(),
                        why: format!("{} value '{}' not in allowed values {:?}", path, s, allowed),
                    });
                }
            } else if let Some(arr) = value.as_array() {
                for item in arr {
                    if let Some(item_s) = item.as_str() {
                        if !allowed.iter().any(|a| a == item_s) {
                            out.push(ShaclViolation {
                                field: path.to_string(),
                                why: format!("{} value '{}' not in allowed values {:?}", path, item_s, allowed),
                            });
                        }
                    }
                }
            }
        }

        out
    }

    pub fn shacl_shapes_loaded(&self) -> usize {
        self.shacl_shapes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subclasses_of_returns_descendants_and_superclasses_ancestors() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        // auth/mtls < auth/tls < auth < security
        idx.add_sub_class("auth", "security");
        idx.add_sub_class("auth/tls", "auth");
        idx.add_sub_class("auth/mtls", "auth/tls");

        let subs = idx.subclasses_of("auth");
        assert!(subs.contains("auth/tls"), "direct child missing: {:?}", subs);
        assert!(subs.contains("auth/mtls"), "transitive child missing: {:?}", subs);
        assert!(
            !subs.contains("auth") && !subs.contains("security"),
            "subclasses_of must not return the class itself or its ancestors: {:?}",
            subs
        );

        let supers = idx.superclasses_of("auth/mtls");
        assert!(supers.contains("auth/tls"), "direct parent missing: {:?}", supers);
        assert!(supers.contains("auth"), "transitive ancestor missing: {:?}", supers);
        assert!(
            !supers.contains("auth/mtls"),
            "superclasses_of must not return the class itself: {:?}",
            supers
        );
    }

    #[test]
    fn load_shacl_shapes_parses_sh_in_list_values() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        let ttl = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ns: <urn:neurostrata:ns#> .
<urn:neurostrata:shape:rule> a sh:NodeShape ;
    sh:targetClass ns:rule ;
    sh:property [
        sh:path ns:facets ;
        sh:in ( "ENFORCED" "PARTIAL" ) ;
    ] .
"#;
        idx.load_shacl_shapes(ttl).expect("shapes load");
        let shapes = &idx.shacl_shapes;
        assert_eq!(shapes.len(), 1, "expected one shape");
        let vals = shapes[0].in_values.get("urn:neurostrata:ns#facets");
        assert_eq!(
            vals.map(|v| v.as_slice()),
            Some(&["ENFORCED".to_string(), "PARTIAL".to_string()][..]),
            "sh:in list values must be parsed, not left empty: {:?}",
            shapes[0].in_values
        );
    }

    #[test]
    fn add_related_sets_symmetric_relationships() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.add_related("concept-a", "concept-b");
        
        assert_eq!(idx.related.get("concept-a"), Some(&BTreeSet::from(["concept-b".to_string()])));
        assert_eq!(idx.related.get("concept-b"), Some(&BTreeSet::from(["concept-a".to_string()])));
    }

    #[test]
    fn add_contrasts_with_sets_symmetric_relationships() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.add_contrasts_with("concept-a", "concept-b");
        
        assert_eq!(idx.contrasts_with.get("concept-a"), Some(&BTreeSet::from(["concept-b".to_string()])));
        assert_eq!(idx.contrasts_with.get("concept-b"), Some(&BTreeSet::from(["concept-a".to_string()])));
    }

    #[test]
    fn concept_broader_of_returns_all_ancestors() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.add_broader("child", "parent");
        idx.add_broader("grandchild", "child");
        
        let broader = idx.concept_broader_of("grandchild");
        assert!(broader.contains("child"));
        assert!(broader.contains("parent"));
        assert!(!broader.contains("grandchild"));
    }

    #[test]
    fn concept_narrower_of_returns_all_descendants() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.add_broader("child", "parent");
        idx.add_broader("grandchild", "child");
        
        let narrower = idx.concept_narrower_of("parent");
        assert!(narrower.contains("child"));
        assert!(narrower.contains("grandchild"));
        assert!(!narrower.contains("parent"));
    }

    #[test]
    fn expand_concept_query_terms_with_direct_match() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        let entry = ConceptIndexEntry {
            memory_id: "m1".to_string(),
            slug: "concept-a".to_string(),
            pref_label: "Concept Alpha".to_string(),
            alt_labels: vec!["Alpha Concept".to_string(), "Concept A".to_string()],
            definition: "A test concept".to_string(),
            domain: None,
            concept_iri: None,
            broader: Vec::new(),
            related: Vec::new(),
            contrasts_with: Vec::new(),
            realization_status: "unknown".to_string(),
        };
        idx.concepts.insert("concept-a".to_string(), entry);
        
        let expanded = idx.expand_concept_query_terms("concept-a");
        assert!(expanded.contains("concept-a"));
        assert!(expanded.contains("Concept Alpha"));
        assert!(expanded.contains("Alpha Concept"));
        assert!(expanded.contains("Concept A"));
    }

    #[test]
    fn expand_concept_query_terms_with_related() {
        let mut idx = OntologyIndex::from_entries(Vec::new());
        let entry1 = ConceptIndexEntry {
            memory_id: "m1".to_string(),
            slug: "concept-a".to_string(),
            pref_label: "Concept Alpha".to_string(),
            alt_labels: vec![],
            definition: "A test concept".to_string(),
            domain: None,
            concept_iri: None,
            broader: Vec::new(),
            related: Vec::new(),
            contrasts_with: Vec::new(),
            realization_status: "unknown".to_string(),
        };
        let entry2 = ConceptIndexEntry {
            memory_id: "m2".to_string(),
            slug: "concept-b".to_string(),
            pref_label: "Concept Beta".to_string(),
            alt_labels: vec!["Beta Concept".to_string()],
            definition: "Another test concept".to_string(),
            domain: None,
            concept_iri: None,
            broader: Vec::new(),
            related: Vec::new(),
            contrasts_with: Vec::new(),
            realization_status: "unknown".to_string(),
        };
        idx.concepts.insert("concept-a".to_string(), entry1);
        idx.concepts.insert("concept-b".to_string(), entry2);
        idx.add_related("concept-a", "concept-b");
        
        let expanded = idx.expand_concept_query_terms("concept-a");
        assert!(expanded.contains("concept-b"));
        assert!(expanded.contains("Concept Beta"));
        assert!(expanded.contains("Beta Concept"));
    }

    #[test]
    fn load_concept_rows_filters_archived() {
        let rows = vec![(
            "mem-1".to_string(),
            MemoryPayload {
                content: "test".to_string(),
                user_id: "user-1".to_string(),
                memory_type: "concept".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({
                    "archived": true,
                    "concept": {"slug": "archived-concept"}
                }),
            },
        )];
        
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.load_concept_rows(&rows, 1730000000);
        
        assert!(idx.concepts.is_empty());
    }

    #[test]
    fn load_concept_rows_filters_non_public_access_policy() {
        let rows = vec![(
            "mem-1".to_string(),
            MemoryPayload {
                content: "test".to_string(),
                user_id: "user-1".to_string(),
                memory_type: "concept".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({
                    "access_policy": {"public": false},
                    "concept": {"slug": "private-concept"}
                }),
            },
        )];
        
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.load_concept_rows(&rows, 1730000000);
        
        assert!(idx.concepts.is_empty());
    }

    #[test]
    fn load_concept_rows_validates_dag() {
        let rows = vec![(
            "mem-1".to_string(),
            MemoryPayload {
                content: "test".to_string(),
                user_id: "user-1".to_string(),
                memory_type: "concept".to_string(),
                agent_name: None,
                location: String::new(),
                location_lines: String::new(),
                metadata: json!({
                    "concept": {"slug": "concept-a", "broader": ["concept-a"]}
                }),
            },
        )];
        
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.load_concept_rows(&rows, 1730000000);
        
        // Concept with self-loop broader should not be loaded
        assert!(idx.concepts.is_empty());
    }

    #[test]
    fn load_concept_rows_preserves_multi_target_alias_collisions_and_separates_subclasses() {
        let rows = vec![
            (
                "mem-1".to_string(),
                MemoryPayload {
                    content: "Parent concept".to_string(),
                    user_id: "user-1".to_string(),
                    memory_type: "concept".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "concept": {
                            "slug": "consensus",
                            "pref_label": "Distributed Consensus",
                            "alt_labels": ["paxos-family"],
                            "definition": "Agreement protocol across fault-tolerant nodes."
                        }
                    }),
                },
            ),
            (
                "mem-2".to_string(),
                MemoryPayload {
                    content: "Child concept A".to_string(),
                    user_id: "user-1".to_string(),
                    memory_type: "concept".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "concept": {
                            "slug": "raft-consensus",
                            "pref_label": "Raft Consensus",
                            "alt_labels": ["shared-alias", "paxos-family"],
                            "broader": ["consensus"],
                            "related": ["consensus"],
                            "definition": "Leader-based consensus protocol."
                        }
                    }),
                },
            ),
            (
                "mem-3".to_string(),
                MemoryPayload {
                    content: "Child concept B".to_string(),
                    user_id: "user-1".to_string(),
                    memory_type: "concept".to_string(),
                    agent_name: None,
                    location: String::new(),
                    location_lines: String::new(),
                    metadata: json!({
                        "concept": {
                            "slug": "multi-paxos",
                            "pref_label": "Multi-Paxos",
                            "alt_labels": ["shared-alias", "paxos-family"],
                            "broader": ["consensus"],
                            "definition": "Multi-decree Paxos protocol."
                        }
                    }),
                },
            ),
        ];

        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.load_concept_rows(&rows, 1730000000);

        // Multi-target alias collision preserved (both raft-consensus and multi-paxos under "shared-alias")
        let shared_targets = idx
            .concept_aliases
            .get("shared-alias")
            .expect("shared-alias must exist");
        assert!(shared_targets.contains("raft-consensus"));
        assert!(shared_targets.contains("multi-paxos"));

        // Derived narrower from canonical broader
        let narrower = idx.concept_narrower_of("consensus");
        assert!(narrower.contains("raft-consensus"));
        assert!(narrower.contains("multi-paxos"));

        // Strict separation from RDFS subClassOf
        assert!(idx.subclasses_of("consensus").is_empty());

        // SKOS S27 disjointness: related edge between raft-consensus and its ancestor consensus is rejected
        assert!(
            idx.related
                .get("raft-consensus")
                .map(|s| !s.contains("consensus"))
                .unwrap_or(true)
        );
    }
}


/// Extract an `(a, b)` pair from a vocabulary entry that is either a
/// two-element `[a, b]` array or an object with `"child"`/`"parent"` (and
/// `"a"`/`"b"`) keys.
/// Extract an `(a, b)` pair from a vocabulary entry that is either a
/// two-element `[a, b]` array or an object with `"child"`/`"parent"` (and
/// `"a"`/`"b"`) keys.
fn vocabulary_pair(pair: &Value) -> Option<(&str, &str)> {
    match pair {
        Value::Array(a) if a.len() == 2 => Some((a[0].as_str()?, a[1].as_str()?)),
        Value::Object(o) => {
            let a = o
                .get("child")
                .and_then(Value::as_str)
                .or_else(|| o.get("a").and_then(Value::as_str));
            let b = o
                .get("parent")
                .and_then(Value::as_str)
                .or_else(|| o.get("b").and_then(Value::as_str));
            match (a, b) {
                (Some(a), Some(b)) => Some((a, b)),
                _ => None,
            }
        }
        _ => None,
    }
}
