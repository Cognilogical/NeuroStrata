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

#[derive(Debug, Clone, Default)]
pub struct OntologyIndex {
    pub domains: Vec<DomainEntry>,
    pub shacl_shapes: Vec<ShaclShapeDef>,
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
                memory_type == tc.split('/').last().unwrap_or(&tc)
                    || memory_type.to_lowercase() == tc.split('/').last().unwrap_or(&tc).to_lowercase()
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

    fn validate_path(&self, payload: &MemoryPayload, path: &str, shape: &ShaclShapeDef) -> Vec<ShaclViolation> {
        let mut out = Vec::new();
        let value = match payload.metadata.get(path.trim_start_matches("ns:")) {
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
