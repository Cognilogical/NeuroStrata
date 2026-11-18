use crate::domain;
use crate::registry;
use crate::rdf::shacl::ShaclViolation;
use ontobug::validate::extract_shacl_shapes;
use crate::rdf::action::validate_sparql_ask_syntax_and_budget;
use crate::rdf::reasoning::{nt_triple, DerivationStep, MaterializationCertificate, ReasoningAdapter, ReasoningProfile};
use crate::traits::MemoryPayload;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

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
    pub shapes_ttl: Option<String>,
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

/// Governed SKOS/OWL vocabulary for a single domain: class terms and predicate
/// terms that a bounded classifier may emit for that domain. The vocabulary is
/// bounded by `QueryBudget`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainVocabulary {
    /// Class IRIs or normalized slugs that are governed for this domain.
    pub classes: BTreeSet<String>,
    /// Predicate IRIs (property IRIs) that are governed for this domain.
    pub predicates: BTreeSet<String>,
}

impl DomainVocabulary {
    /// A vocabulary is unusable when EITHER side is missing: without classes
    /// nothing can be typed, without predicates nothing can be asserted.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() || self.predicates.is_empty()
    }

    /// Determine whether a class term is governed. Exact match only — a
    /// normalized-slug fallback would let an invented absolute IRI collapse
    /// onto a governed slug and smuggle an ungoverned class into the graph.
    pub fn governs_class(&self, term: &str) -> bool {
        self.classes.contains(term)
    }

    /// Determine whether a predicate term (IRI) is governed.
    pub fn governs_predicate(&self, term: &str) -> bool {
        self.predicates.contains(term)
    }

}

/// Errors that can occur when selecting a domain vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VocabularySelectionError {
    UnknownDomain(String),
    BudgetExhausted { visited: usize, max_nodes: usize },
    EmptyVocabulary(String),
    MalformedIri(String),
    UnknownDatatype(String),
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
                shapes_ttl: None,
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

    /// Select the governed class and predicate vocabulary for `domain`,
    /// bounded by `budget`. Returns empty vocabulary (fails closed) when the
    /// domain has no registered concepts, no shapes, and no properties.
    pub fn select_domain_vocabulary(
        &self,
        domain: &str,
        budget: &QueryBudget,
    ) -> Result<DomainVocabulary, VocabularySelectionError> {
        // 1. Normalize the requested domain before resolving it, so case and
        // separator variants select the same governed vocabulary. A domain
        // that normalizes to nothing is rejected (fail closed).
        let normalized = domain::normalize_domain_slug(domain)
            .ok_or_else(|| VocabularySelectionError::UnknownDomain(domain.to_string()))?;
        let domain_entry = self.domain(&normalized).ok_or_else(|| {
            VocabularySelectionError::UnknownDomain(domain.to_string())
        })?;

        // 2. Budget tracking.
        let mut visited = 0usize;
        let start_time = std::time::Instant::now();

        // Helper to check budget. A non-positive limit fails closed: the
        // budget is finite on purpose, so zero means "no work allowed",
        // never "unlimited".
        let check_budget = |visited: usize| -> Result<(), VocabularySelectionError> {
            if budget.max_nodes == 0 || visited >= budget.max_nodes {
                return Err(VocabularySelectionError::BudgetExhausted {
                    visited,
                    max_nodes: budget.max_nodes,
                });
            }
            if budget.max_ms == 0
                || start_time.elapsed().as_millis() as u64 >= budget.max_ms
            {
                return Err(VocabularySelectionError::BudgetExhausted {
                    visited,
                    max_nodes: budget.max_nodes,
                });
            }
            Ok(())
        };

        // 3. Seed the class closure: domain slug, concept IRI, equivalents.
        let mut class_closure: BTreeSet<String> = BTreeSet::new();
        let mut predicate_closure: BTreeSet<String> = BTreeSet::new();
        let mut queue = VecDeque::new();

        let push_class = |set: &mut BTreeSet<String>, term: String, visited: &mut usize| -> Result<(), VocabularySelectionError> {
            if !set.contains(&term) {
                *visited += 1;
                check_budget(*visited)?;
                set.insert(term);
            }
            Ok(())
        };

        // Helper to enqueue a term for BFS expansion.
        let enqueue = |queue: &mut VecDeque<String>, term: String| {
            if !queue.contains(&term) {
                queue.push_back(term);
            }
        };

        // 4. Initial seeds from the domain entry.
        push_class(&mut class_closure, domain_entry.slug.clone(), &mut visited)?;
        if let Some(iri) = &domain_entry.concept_iri {
            push_class(&mut class_closure, iri.clone(), &mut visited)?;
        }

        // 5. Register concepts whose domain is this domain or its descendant.
        // Expand domain hierarchy (descendants) via self.expand_domain.
        let descendant_domains = self.expand_domain(&normalized);
        for concept in &self.concepts {
            let concept_domain = match &concept.1.domain {
                Some(d) => d,
                None => continue,
            };
            // Check if concept_domain is in descendant_domains (including itself).
            if !descendant_domains.iter().any(|d| d == concept_domain) {
                continue;
            }
            // Concept's own slug and concept_iri
            push_class(&mut class_closure, concept.1.slug.clone(), &mut visited)?;
            if let Some(iri) = &concept.1.concept_iri {
                push_class(&mut class_closure, iri.clone(), &mut visited)?;
            }
            // Enqueue for expansion (broader, narrower, related, equivalents, exact_match)
            enqueue(&mut queue, concept.1.slug.clone());
            if let Some(iri) = &concept.1.concept_iri {
                enqueue(&mut queue, iri.clone());
            }
        }

        // 6. BFS over class closure terms.
        while let Some(term) = queue.pop_front() {
            check_budget(visited)?;
            // Collect neighbors.
            let mut neighbors: Vec<String> = Vec::new();
            if let Some(parents) = self.sub_classes.get(&term) {
                for p in parents {
                    neighbors.push(p.clone());
                }
            }
            if let Some(children) = self.children.get(&term) {
                for c in children {
                    neighbors.push(c.clone());
                }
            }
            if let Some(broader) = self.broader.get(&term) {
                for b in broader {
                    neighbors.push(b.clone());
                }
            }
            if let Some(narrower) = self.narrower.get(&term) {
                for n in narrower {
                    neighbors.push(n.clone());
                }
            }
            if let Some(related) = self.related.get(&term) {
                for r in related {
                    neighbors.push(r.clone());
                }
            }
            for e in self.equivalents_of(&term) {
                neighbors.push(e);
            }
            if let Some(exact) = self.exact_match.get(&term) {
                for e in exact {
                    neighbors.push(e.clone());
                }
            }
            for n in neighbors {
                push_class(&mut class_closure, n.clone(), &mut visited)?;
                enqueue(&mut queue, n);
            }
        }

        // 7. Predicate closure: scoped to this domain. Seeds are the property
        // paths of SHACL shapes whose target_class is governed by the class
        // closure built above. The closure then expands over property
        // hierarchies (superproperties via `subproperties_of`), property
        // equivalences and inverse properties (registered bidirectionally),
        // bounded by the same budget as the class closure. Global property
        // maps are NOT unioned in: a property that is transitive or
        // symmetric in the global map but unreachable from a governed seed
        // stays out of the domain vocabulary.
        let mut predicate_queue: VecDeque<String> = VecDeque::new();
        for shape in &self.shacl_shapes {
            let Some(target) = &shape.target_class else {
                continue;
            };
            if !class_closure.contains(target) {
                continue;
            }
            for (path, _) in &shape.property_constraints {
                if predicate_closure.insert(path.clone()) {
                    visited += 1;
                    check_budget(visited)?;
                    predicate_queue.push_back(path.clone());
                }
            }
        }
        while let Some(term) = predicate_queue.pop_front() {
            check_budget(visited)?;
            let mut neighbors: Vec<String> = Vec::new();
            neighbors.extend(self.subproperties_of(&term));
            neighbors.extend(self.equivalents_of(&term));
            neighbors.extend(self.inverse_of(&term));
            for n in neighbors {
                if predicate_closure.insert(n.clone()) {
                    visited += 1;
                    check_budget(visited)?;
                    predicate_queue.push_back(n);
                }
            }
        }

        // 9. Final validation: reject empty vocabulary.
        if class_closure.is_empty() || predicate_closure.is_empty() {
            return Err(VocabularySelectionError::EmptyVocabulary(
                domain.to_string(),
            ));
        }

        Ok(DomainVocabulary {
            classes: class_closure,
            predicates: predicate_closure,
        })
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
    self.shapes_ttl = Some(ttl.to_string());
    let extracted = extract_shacl_shapes(ttl)
        .map_err(|e| anyhow::anyhow!("load_shacl_shapes: {}", e))?;
    let mut total = 0usize;

    for shape in extracted {
        let mut min_counts = BTreeMap::new();
        let mut max_counts = BTreeMap::new();
        let mut datatypes = BTreeMap::new();
        let mut node_kinds = BTreeMap::new();
        let mut in_values = BTreeMap::new();
        let mut messages = BTreeMap::new();
        let mut property_constraints = Vec::new();

        for prop in &shape.property_constraints {
            let path = prop.path.clone();
            property_constraints.push((path.clone(), None));
            min_counts.insert(path.clone(), prop.min_count.unwrap_or(0).max(0) as u32);
            max_counts.insert(path.clone(), prop.max_count.unwrap_or(u32::MAX as i64).max(0) as u32);
            if let Some(dt) = &prop.datatype {
                datatypes.insert(path.clone(), dt.clone());
            }
            if let Some(nk) = &prop.node_kind {
                node_kinds.insert(path.clone(), nk.clone());
            }
            if !prop.in_values.is_empty() {
                in_values.insert(path.clone(), prop.in_values.clone());
            }
            if let Some(msg) = &prop.message {
                messages.insert(path.clone(), msg.clone());
            }
        }

        self.shacl_shapes.push(ShaclShapeDef {
            shape_iri: shape.shape_iri,
            target_class: shape.target_class,
            property_constraints,
            min_counts,
            max_counts,
            datatypes,
            in_values,
            node_kinds,
            messages,
        });
        total += 1;
    }

    Ok(total)
}

/// Drop loaded SHACL shapes (used when a registry row stops shipping shapes).
/// Both the parsed shapes and the raw TTL must go: `validate_rdf_graph` reads
/// `shapes_ttl`, so leaving it behind keeps validating against stale shapes.
pub fn clear_shacl_shapes(&mut self) {
    self.shacl_shapes.clear();
    self.shapes_ttl = None;
}

/// Validate RDF data against loaded SHACL shapes using ontobug/rudof.
///
/// Uses the shapes TTL captured at load time; returns SHACL violations.
pub fn validate_rdf_graph(&self, data_ttl: &str) -> anyhow::Result<Vec<ShaclViolation>> {
    let Some(shapes_ttl) = self.shapes_ttl.clone() else {
        return Ok(Vec::new());
    };
    crate::rdf::shacl::validate_rdf_with_ontobug(data_ttl, &shapes_ttl)
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

// ---------------------------------------------------------------------------
// Capability B — zero-mutation ontology closure / blast-radius planner (spec §3)
// ---------------------------------------------------------------------------

/// Zero-mutation request for an ontology plan. The planner clones the active
/// namespace (or the named scenario overlay) into an ephemeral in-memory
/// dataset; nothing here is ever committed to a store, scenario, task or edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OntologyPlanRequest {
    pub namespace: String,
    /// Optional overlay scenario; when omitted the active namespace is cloned.
    pub scenario_id: Option<String>,
    /// Turtle / N-Quads additions staged in the ephemeral sandbox.
    pub add_turtle: Option<String>,
    /// Canonical N-Triple / N-Quad patterns retracted in the ephemeral sandbox.
    #[serde(default)]
    pub retract_triples: Vec<String>,
    /// Optional replacement SHACL shapes document validated against the post-image.
    pub candidate_shacl_turtle: Option<String>,
}

/// A SHACL finding flattened from `ontobug::validate::Report` for operator use.
/// Field order defines the canonical report ordering (focus/path/component/...).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ShaclViolationSummary {
    pub focus: String,
    pub path: Option<String>,
    pub component: String,
    pub message: Option<String>,
    pub severity: String,
}

/// Result of a zero-mutation ontology plan. Every entailment is a certified
/// Capability A `DerivationStep`; every impact entry is derived from an actual
/// changed governance fact or SPARQL ASK outcome — never fabricated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OntologyPlanReport {
    pub namespace: String,
    pub added_entailments: Vec<DerivationStep>,
    pub removed_entailments: Vec<String>,
    pub validation_conforms: bool,
    pub new_shacl_violations: Vec<ShaclViolationSummary>,
    pub consistency_violations: Vec<String>,
    pub impacted_memories: Vec<String>,
    pub impacted_files: Vec<String>,
    pub impacted_action_slugs: Vec<String>,
    /// Real DDL from `ontobug::diff_ontologies` + `generate_cypher_ddl`.
    pub schema_ddl_diff: Vec<String>,
}

/// Read-only, ReBAC-filtered inputs gathered at the store boundary. Kept as
/// plain data so the planner itself never touches a store and is trivially
/// testable in isolation.
#[derive(Debug, Clone, Default)]
pub struct OntologyPlanInput {
    /// Pre-image RDF graph (active namespace or scenario-merged) as Turtle.
    pub base_turtle: String,
    /// Currently governing SHACL shapes, used when no candidate is supplied.
    pub shapes_turtle: Option<String>,
    /// ReBAC-visible memory IDs.
    pub visible_memory_ids: Vec<String>,
    /// ReBAC-visible file paths.
    pub visible_file_paths: Vec<String>,
    /// ReBAC-visible action slugs with their SPARQL ASK criterion queries.
    pub visible_action_asks: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OntologyPlanError {
    Rdf(String),
    Reasoning(String),
    Shacl(String),
    Ask(String),
    Diff(String),
}

impl std::fmt::Display for OntologyPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rdf(m) => write!(f, "ontology plan RDF error: {m}"),
            Self::Reasoning(m) => write!(f, "ontology plan reasoning error: {m}"),
            Self::Shacl(m) => write!(f, "ontology plan SHACL error: {m}"),
            Self::Ask(m) => write!(f, "ontology plan SPARQL ASK error: {m}"),
            Self::Diff(m) => write!(f, "ontology plan DDL diff error: {m}"),
        }
    }
}
impl std::error::Error for OntologyPlanError {}

fn parse_turtle_triples(ttl: &str) -> Result<Vec<oxrdf::Triple>, OntologyPlanError> {
    let mut out = Vec::new();
    for parsed in oxttl::TurtleParser::new().for_slice(ttl.as_bytes()) {
        out.push(parsed.map_err(|e| OntologyPlanError::Rdf(format!("turtle parse: {e}")))?);
    }
    Ok(out)
}

fn parse_nquads_triples(s: &str) -> Result<Vec<oxrdf::Triple>, OntologyPlanError> {
    let mut out = Vec::new();
    for parsed in oxttl::NQuadsParser::new().for_slice(s.as_bytes()) {
        let q = parsed.map_err(|e| OntologyPlanError::Rdf(format!("n-quads parse: {e}")))?;
        out.push(oxrdf::Triple::new(q.subject, q.predicate, q.object));
    }
    Ok(out)
}

fn parse_add_triples(add: Option<&str>) -> Result<Vec<oxrdf::Triple>, OntologyPlanError> {
    let Some(s) = add else { return Ok(Vec::new()); };
    match parse_turtle_triples(s) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => parse_nquads_triples(s),
    }
}

fn parse_retract_triples(retract: &[String]) -> Result<Vec<oxrdf::Triple>, OntologyPlanError> {
    let mut out = Vec::new();
    for s in retract {
        let mut it = oxttl::NQuadsParser::new().for_slice(s.as_bytes());
        let q = it
            .next()
            .ok_or_else(|| OntologyPlanError::Rdf("empty retract triple".to_string()))?
            .map_err(|e| OntologyPlanError::Rdf(format!("retract n-quads parse: {e}")))?;
        // A retraction entry is exactly one quad. A second quad, or trailing
        // malformed content, is rejected wholesale — no partial retraction.
        match it.next() {
            Some(Ok(_)) => {
                return Err(OntologyPlanError::Rdf(
                    "retract entry must contain exactly one quad".to_string(),
                ))
            }
            Some(Err(e)) => {
                return Err(OntologyPlanError::Rdf(format!(
                    "retract entry has trailing malformed content: {e}"
                )))
            }
            None => {}
        }
        out.push(oxrdf::Triple::new(q.subject, q.predicate, q.object));
    }
    Ok(out)
}

fn triples_to_turtle(triples: &[oxrdf::Triple]) -> Result<String, OntologyPlanError> {
    let mut sorted = triples.to_vec();
    sorted.sort_by(|a, b| nt_triple(a).cmp(&nt_triple(b)));
    sorted.dedup_by(|a, b| nt_triple(a) == nt_triple(b));
    let mut writer = oxttl::TurtleSerializer::new().for_writer(Vec::new());
    for t in &sorted {
        writer
            .serialize_triple(t.as_ref())
            .map_err(|e| OntologyPlanError::Rdf(format!("turtle serialization: {e}")))?;
    }
    let bytes = writer
        .finish()
        .map_err(|e| OntologyPlanError::Rdf(format!("turtle finish: {e}")))?;
    String::from_utf8(bytes).map_err(|e| OntologyPlanError::Rdf(format!("turtle utf-8: {e}")))
}

/// Deterministic Turtle serialization of projected namespace quads into the
/// default graph, for use by the read-only scenario/port plan entry points.
pub fn projected_quads_to_turtle(
    quads: &[crate::rdf::types::Quad],
) -> Result<String, OntologyPlanError> {
    let mut triples = Vec::with_capacity(quads.len());
    for q in quads {
        if let Ok(oq) = crate::rdf::types_oxrdf::OxRdfBridge::neurostrata_quad_to_oxrdf(q) {
            triples.push(oxrdf::Triple::new(
                oq.inner.subject,
                oq.inner.predicate,
                oq.inner.object,
            ));
        }
    }
    triples_to_turtle(&triples)
}

fn triples_to_dataset(triples: &[oxrdf::Triple]) -> oxrdf::Dataset {
    // Mirror the production SPARQL projection: asserted data lives in a named
    // graph and ASK criteria run with the default graph set as the union of all
    // named graphs. Putting these triples in the default graph would make them
    // invisible once that union is selected.
    let graph = oxrdf::GraphName::NamedNode(
        oxrdf::NamedNode::new_unchecked("urn:neurostrata:graph:ontology-plan:sandbox"),
    );
    let mut ds = oxrdf::Dataset::new();
    for t in triples {
        ds.insert(&oxrdf::Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            graph.clone(),
        ));
    }
    ds
}

fn certified_closure(
    triples: &[oxrdf::Triple],
    namespace: &str,
) -> Result<MaterializationCertificate, OntologyPlanError> {
    ReasoningAdapter::new()
        .materialize_owl2_rl_with_proofs(triples, namespace, 50_000, ReasoningProfile::OwlRl)
        .map_err(|e| OntologyPlanError::Reasoning(e.to_string()))
}

fn shacl_summaries(
    data_ttl: &str,
    shapes_ttl: &str,
) -> Result<(bool, Vec<ShaclViolationSummary>), OntologyPlanError> {
    let report = ontobug::validate::shacl_turtle(data_ttl, shapes_ttl, &ontobug::Mode::Native)
        .map_err(|e| OntologyPlanError::Shacl(e.to_string()))?;
    let violations = report
        .results
        .iter()
        .filter(|f| f.severity == "Violation")
        .map(|f| ShaclViolationSummary {
            focus: f.focus.clone(),
            path: f.path.clone(),
            component: f.component.clone(),
            message: f.message.clone(),
            severity: f.severity.clone(),
        })
        .collect();
    Ok((report.conforms, violations))
}

fn subject_iri(s: &oxrdf::Subject) -> Option<String> {
    match s {
        oxrdf::Subject::NamedNode(n) => Some(n.as_str().to_string()),
        _ => None,
    }
}

fn term_iri(t: &oxrdf::Term) -> Option<String> {
    match t {
        oxrdf::Term::NamedNode(n) => Some(n.as_str().to_string()),
        _ => None,
    }
}

fn local_name(iri: &str) -> &str {
    iri.rsplit(['#', '/']).next().unwrap_or(iri)
}

fn is_governance_predicate(iri: &str) -> bool {
    local_name(iri).to_ascii_lowercase().contains("govern")
}

/// Hierarchy-cycle strings over `skos:broader` / `rdfs:subClassOf` edges.
fn hierarchy_cycles(triples: &[oxrdf::Triple]) -> Vec<String> {
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for t in triples {
        let local = local_name(t.predicate.as_str());
        if local == "broader" || local == "subClassOf" {
            if let (Some(s), Some(o)) = (subject_iri(&t.subject), term_iri(&t.object)) {
                adj.entry(s).or_default().insert(o);
            }
        }
    }
    fn dfs(
        node: &str,
        adj: &BTreeMap<String, BTreeSet<String>>,
        path: &mut Vec<String>,
        found: &mut BTreeSet<String>,
    ) {
        let Some(nexts) = adj.get(node) else { return; };
        for next in nexts {
            if let Some(pos) = path.iter().position(|n| n == next) {
                let mut cycle = path[pos..].to_vec();
                cycle.sort();
                found.insert(format!("hierarchy cycle: {}", cycle.join(" -> ")));
            } else {
                path.push(next.clone());
                dfs(next, adj, path, found);
                path.pop();
            }
        }
    }
    let mut found = BTreeSet::new();
    for node in adj.keys().cloned().collect::<Vec<_>>() {
        dfs(&node, &adj, &mut vec![node.clone()], &mut found);
    }
    found.into_iter().collect()
}

fn eval_ask(query: &spargebra::Query, ds: &oxrdf::Dataset) -> Result<bool, OntologyPlanError> {
    let evaluator = spareval::QueryEvaluator::new();
    let mut prepared = evaluator.prepare(query);
    prepared.dataset_mut().set_default_graph_as_union();
    match prepared.execute(ds) {
        Ok(spareval::QueryResults::Boolean(b)) => Ok(b),
        Ok(_) => Err(OntologyPlanError::Ask(
            "criterion did not return a boolean ASK result".to_string(),
        )),
        Err(e) => Err(OntologyPlanError::Ask(format!("ASK evaluation: {e}"))),
    }
}

fn ask_flips(
    action_asks: &[(String, Vec<String>)],
    pre: &oxrdf::Dataset,
    post: &oxrdf::Dataset,
) -> Result<Vec<String>, OntologyPlanError> {
    // BTreeSet makes the result sorted and duplicate-free regardless of the
    // order (or repetition) of action rows supplied at the store boundary.
    let mut impacted = BTreeSet::new();
    for (slug, asks) in action_asks {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for q in asks {
            let parsed = validate_sparql_ask_syntax_and_budget(q, 10_000)
                .map_err(|e| OntologyPlanError::Ask(format!("{slug}: {e}")))?;
            before.push(eval_ask(&parsed, pre)?);
            after.push(eval_ask(&parsed, post)?);
        }
        if before != after {
            impacted.insert(slug.clone());
        }
    }
    Ok(impacted.into_iter().collect())
}

/// Projected memory subject IRI used throughout `rdf::projection`.
fn memory_iri(id: &str) -> String {
    format!("urn:neurostrata:memory:{id}")
}

/// Token-boundary containment: `needle` must appear in `hay` without an
/// IRI/path continuation character directly before or after it. Plain
/// `str::contains` over-reports `mem-a` on `mem-ab` and `foo.rs` on
/// `foo.rs.bak`; a path at the end of a legitimate namespace IRI
/// (`urn:neurostrata:file:foo.rs`) still matches because `:` is a boundary on
/// the left and `>` a boundary on the right.
fn contains_token(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let is_continuation =
        |b: u8| is_word(b) || matches!(b, b'.' | b'/' | b':' | b'%' | b'\\' | b'~');
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() {
        return false;
    }
    for i in 0..=(h.len() - n.len()) {
        if &h[i..i + n.len()] != n {
            continue;
        }
        let before_ok = i == 0 || !is_word(h[i - 1]);
        let after_ok = i + n.len() == h.len() || !is_continuation(h[i + n.len()]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn governance_impact<'a>(
    changed: impl Iterator<Item = &'a oxrdf::Triple>,
    input: &OntologyPlanInput,
) -> (Vec<String>, Vec<String>) {
    let mut memories = BTreeSet::new();
    let mut files = BTreeSet::new();
    let mut terms = Vec::new();
    for t in changed {
        if is_governance_predicate(t.predicate.as_str()) {
            terms.push(t.subject.to_string());
            terms.push(t.object.to_string());
        }
    }
    for term in terms {
        for id in &input.visible_memory_ids {
            if contains_token(&term, &memory_iri(id)) {
                memories.insert(id.clone());
            }
        }
        for path in &input.visible_file_paths {
            if contains_token(&term, path) {
                files.insert(path.clone());
            }
        }
    }
    (memories.into_iter().collect(), files.into_iter().collect())
}

/// Canonicalize every order-sensitive report vector so equal semantic plans
/// serialize byte-identically regardless of upstream row/triple ordering.
fn deterministic_report_order(report: &mut OntologyPlanReport) {
    report.added_entailments.sort_by(|a, b| {
        a.conclusion
            .cmp(&b.conclusion)
            .then_with(|| a.rule.as_str().cmp(b.rule.as_str()))
            .then_with(|| a.premises.cmp(&b.premises))
    });
    report.removed_entailments.sort();
    report.removed_entailments.dedup();
    report.new_shacl_violations.sort();
    report.new_shacl_violations.dedup();
    report.consistency_violations.sort();
    report.consistency_violations.dedup();
    report.impacted_memories.sort();
    report.impacted_memories.dedup();
    report.impacted_files.sort();
    report.impacted_files.dedup();
    report.impacted_action_slugs.sort();
    report.impacted_action_slugs.dedup();
    report.schema_ddl_diff.sort();
    report.schema_ddl_diff.dedup();
}

impl OntologyIndex {
    /// Zero-mutation semantic-closure diff over an ephemeral sandbox dataset.
    ///
    /// Clones the pre-image RDF into memory, applies the staged additions and
    /// retractions, computes certified Capability A closures for both images,
    /// and reports entailment/validation/consistency/impact/DDL deltas. This
    /// never writes to a store, scenario, task, edge or the ontology cache.
    pub fn plan_mutation_diff(
        &self,
        request: &OntologyPlanRequest,
        input: &OntologyPlanInput,
    ) -> Result<OntologyPlanReport, OntologyPlanError> {
        let base = parse_turtle_triples(&input.base_turtle)?;
        let adds = parse_add_triples(request.add_turtle.as_deref())?;
        let rets = parse_retract_triples(&request.retract_triples)?;

        let retracted: HashSet<String> = rets.iter().map(nt_triple).collect();
        let mut post = base.clone();
        post.extend(adds.iter().cloned());
        post.retain(|t| !retracted.contains(&nt_triple(t)));

        let pre_turtle = triples_to_turtle(&base)?;
        let post_turtle = triples_to_turtle(&post)?;

        let pre_closure = certified_closure(&base, &request.namespace)?;
        let post_closure = certified_closure(&post, &request.namespace)?;

        let pre_conclusions: HashSet<String> = pre_closure
            .certificate
            .steps
            .iter()
            .map(|s| s.conclusion.clone())
            .collect();
        let post_conclusions: HashSet<String> = post_closure
            .certificate
            .steps
            .iter()
            .map(|s| s.conclusion.clone())
            .collect();

        let added_entailments: Vec<DerivationStep> = post_closure
            .certificate
            .steps
            .iter()
            .filter(|s| !pre_conclusions.contains(&s.conclusion))
            .cloned()
            .collect();
        let mut removed_entailments: Vec<String> = pre_conclusions
            .iter()
            .filter(|c| !post_conclusions.contains(*c))
            .cloned()
            .collect();
        removed_entailments.sort();

        let shapes = request
            .candidate_shacl_turtle
            .as_deref()
            .or(input.shapes_turtle.as_deref());
        let (validation_conforms, new_shacl_violations) = match shapes {
            Some(sh) => {
                let (_, pre_v) = shacl_summaries(&pre_turtle, sh)?;
                let (post_ok, post_v) = shacl_summaries(&post_turtle, sh)?;
                let pre_set: HashSet<ShaclViolationSummary> = pre_v.into_iter().collect();
                (post_ok, post_v.into_iter().filter(|v| !pre_set.contains(v)).collect())
            }
            None => (true, Vec::new()),
        };

        let pre_disjoint: HashSet<String> = pre_closure
            .certificate
            .disjointness_violations
            .iter()
            .map(|s| s.conclusion.clone())
            .collect();
        let pre_cycles: HashSet<String> = hierarchy_cycles(&base).into_iter().collect();
        let mut consistency_violations: Vec<String> = post_closure
            .certificate
            .disjointness_violations
            .iter()
            .map(|s| s.conclusion.clone())
            .filter(|c| !pre_disjoint.contains(c))
            .collect();
        consistency_violations.extend(
            hierarchy_cycles(&post)
                .into_iter()
                .filter(|c| !pre_cycles.contains(c)),
        );
        consistency_violations.sort();

        let pre_dataset = triples_to_dataset(&base);
        let post_dataset = triples_to_dataset(&post);
        let impacted_action_slugs = ask_flips(&input.visible_action_asks, &pre_dataset, &post_dataset)?;
        let (mut impacted_memories, mut impacted_files) =
            governance_impact(adds.iter().chain(rets.iter()), input);
        for nt in added_entailments
            .iter()
            .map(|s| s.conclusion.clone())
            .chain(removed_entailments.iter().cloned())
        {
            for id in &input.visible_memory_ids {
                if contains_token(&nt, &memory_iri(id)) {
                    impacted_memories.push(id.clone());
                }
            }
            for path in &input.visible_file_paths {
                if contains_token(&nt, path) {
                    impacted_files.push(path.clone());
                }
            }
        }
        impacted_memories.sort();
        impacted_memories.dedup();
        impacted_files.sort();
        impacted_files.dedup();

        let diff = ontobug::diff_ontologies(&pre_turtle, &post_turtle)
            .map_err(|e| OntologyPlanError::Diff(e.to_string()))?;
        let schema_ddl_diff = ontobug::generate_cypher_ddl(&diff, "DEFAULT")
            .map_err(|e| OntologyPlanError::Diff(e.to_string()))?;

        let mut report = OntologyPlanReport {
            namespace: request.namespace.clone(),
            added_entailments,
            removed_entailments,
            validation_conforms,
            new_shacl_violations,
            consistency_violations,
            impacted_memories,
            impacted_files,
            impacted_action_slugs,
            schema_ddl_diff,
        };
        deterministic_report_order(&mut report);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- Capability F review fixes: RED regressions -----------------------

    #[test]
    fn domain_vocabulary_is_empty_when_either_side_is_missing() {
        let classes_only = DomainVocabulary {
            classes: ["memory".to_string()].into_iter().collect(),
            predicates: BTreeSet::new(),
        };
        let predicates_only = DomainVocabulary {
            classes: BTreeSet::new(),
            predicates: ["urn:neurostrata:ns#hasType".to_string()]
                .into_iter()
                .collect(),
        };
        assert!(classes_only.is_empty(), "classes without predicates fail closed");
        assert!(predicates_only.is_empty(), "predicates without classes fail closed");
        assert!(DomainVocabulary::default().is_empty());
    }

    #[test]
    fn governs_class_accepts_only_exact_terms() {
        let vocab = DomainVocabulary {
            classes: ["memory".to_string(), "urn:neurostrata:ns#Memory".to_string()]
                .into_iter()
                .collect(),
            predicates: BTreeSet::new(),
        };
        assert!(vocab.governs_class("memory"));
        assert!(vocab.governs_class("urn:neurostrata:ns#Memory"));
        // A normalized-slug fallback would let an invented absolute IRI
        // such as `memory:` collapse to the governed slug `memory`.
        assert!(!vocab.governs_class("memory:"), "invented IRI must not match slug");
        assert!(!vocab.governs_class("Memory"));
        assert!(!vocab.governs_class("memory:Invented"));
    }

    #[test]
    fn clear_shacl_shapes_drops_the_shapes_ttl_too() {
        let ttl = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ns: <urn:neurostrata:ns#> .
<urn:neurostrata:shape:memory> a sh:NodeShape ;
    sh:targetClass ns:Memory ;
    sh:property [ sh:path ns:hasType ; sh:minCount 1 ] .
"#;
        let mut idx = OntologyIndex::from_entries(Vec::new());
        idx.load_shacl_shapes(ttl).expect("shapes load");
        assert!(idx.shapes_ttl.is_some(), "load must retain raw shapes TTL");
        idx.clear_shacl_shapes();
        assert!(idx.shacl_shapes.is_empty());
        assert!(idx.shapes_ttl.is_none(), "clear must not leave stale shapes TTL");
        assert!(
            idx.validate_rdf_graph("<s> <p> <o> .").expect("graph validation")
                .is_empty(),
            "no shapes means no graph-level violations (stale TTL would still fire)"
        );
    }

    #[test]
    fn select_domain_vocabulary_normalizes_the_requested_domain() {
        let mut idx = OntologyIndex::from_entries(vec![DomainEntry {
            slug: "memory".to_string(),
            parent: None,
            aliases: Vec::new(),
            concept_iri: Some("urn:neurostrata:ns#Memory".to_string()),
        }]);
        idx.load_shacl_shapes(
            r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix ns: <urn:neurostrata:ns#> .
<urn:neurostrata:shape:memory> a sh:NodeShape ;
    sh:targetClass ns:Memory ;
    sh:property [ sh:path ns:hasType ; sh:minCount 1 ] .
"#,
        )
        .expect("shapes load");
        let budget = QueryBudget { max_ms: 10_000, max_nodes: 10_000 };
        let lower = idx.select_domain_vocabulary("memory", &budget).expect("plain slug");
        let upper = idx.select_domain_vocabulary("  Memory  ", &budget).expect("case + whitespace");
        assert_eq!(lower, upper, "domain variants must resolve to the same vocabulary");
        assert!(
            idx.select_domain_vocabulary("!!!", &budget).is_err(),
            "a domain that normalizes to nothing must be rejected"
        );
    }

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

#[cfg(test)]
mod plan_tests {
    use super::*;

    const EX: &str = "urn:test:ex#";

    fn input(base: &str) -> OntologyPlanInput {
        OntologyPlanInput {
            base_turtle: base.to_string(),
            shapes_turtle: None,
            visible_memory_ids: Vec::new(),
            visible_file_paths: Vec::new(),
            visible_action_asks: Vec::new(),
        }
    }

    fn req(add: Option<&str>, retract: &[&str]) -> OntologyPlanRequest {
        OntologyPlanRequest {
            namespace: "test/ns".to_string(),
            scenario_id: None,
            add_turtle: add.map(str::to_string),
            retract_triples: retract.iter().map(|s| s.to_string()).collect(),
            candidate_shacl_turtle: None,
        }
    }

    #[test]
    fn plan_reports_added_entailment_without_mutating_inputs() {
        let base = format!(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             <{EX}A> rdfs:subClassOf <{EX}B> ."
        );
        let add = format!("<{EX}x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}A> .");
        let inp = input(&base);
        let idx = OntologyIndex::empty();
        let idx_before = format!("{idx:?}");
        let base_before = inp.base_turtle.clone();

        let report = idx
            .plan_mutation_diff(&req(Some(&add), &[]), &inp)
            .expect("plan must succeed");

        let found = report
            .added_entailments
            .iter()
            .any(|s| s.conclusion.contains(&format!("<{EX}B>")));
        assert!(found, "new subClassOf entailment must be reported: {:?}", report.added_entailments);
        // Zero mutation: neither the index nor the caller input changed.
        assert_eq!(base_before, inp.base_turtle);
        assert_eq!(idx_before, format!("{idx:?}"));
    }

    #[test]
    fn plan_reports_removed_entailment() {
        let base = format!(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             <{EX}A> rdfs:subClassOf <{EX}B> .\n\
             <{EX}x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}A> ."
        );
        let retract = format!(
            "<{EX}x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}A> ."
        );
        let inp = input(&base);
        let idx = OntologyIndex::empty();

        let report = idx
            .plan_mutation_diff(&req(None, &[&retract]), &inp)
            .expect("plan must succeed");

        assert!(
            report.removed_entailments.iter().any(|s| s.contains(&format!("<{EX}B>"))),
            "lost subClassOf entailment must be reported: {:?}",
            report.removed_entailments
        );
    }

    #[test]
    fn plan_reports_new_shacl_violations_after_candidate_shapes() {
        let base = format!("<{EX}x> a <{EX}Employee> . <{EX}x> <{EX}hasName> \"Ada\" .");
        let shapes = format!(
            "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
             <urn:test:shape:employee> a sh:NodeShape ;\n\
             \x20 sh:targetClass <{EX}Employee> ;\n\
             \x20 sh:property [ sh:path <{EX}hasName> ; sh:minCount 1 ] ."
        );
        let mut inp = input(&base);
        inp.shapes_turtle = Some(shapes.clone());
        let retract = format!("<{EX}x> <{EX}hasName> \"Ada\" .");
        let idx = OntologyIndex::empty();

        let report = idx
            .plan_mutation_diff(&req(None, &[&retract]), &inp)
            .expect("plan must succeed");

        assert!(!report.validation_conforms, "post-image must not conform");
        assert!(!report.new_shacl_violations.is_empty(), "new SHACL violation expected");
        assert!(report.new_shacl_violations.iter().any(|v| v.focus.contains("x")));
    }

    #[test]
    fn plan_produces_real_schema_ddl_diff_via_ontobug() {
        let base = "@prefix owl: <http://www.w3.org/2002/07/owl#> .\n<urn:test:ex#Employee> a owl:Class .";
        let add = "@prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
                   @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                   <urn:test:ex#Manager> a owl:Class ; rdfs:subClassOf <urn:test:ex#Employee> .";
        let idx = OntologyIndex::empty();

        let report = idx
            .plan_mutation_diff(&req(Some(add), &[]), &input(base))
            .expect("plan must succeed");

        assert!(!report.schema_ddl_diff.is_empty(), "ontobug must produce real DDL");
        assert!(report.schema_ddl_diff.iter().any(|s| s.contains("OntologyClass")),
            "DDL must reference OntologyClass statements: {:?}", report.schema_ddl_diff);
    }

    #[test]
    fn plan_reports_action_slug_when_sparql_ask_flips() {
        let base = format!("<{EX}x> a <{EX}Employee> .");
        let mut inp = input(&base);
        inp.visible_action_asks.push((
            "check-employee".to_string(),
            vec![format!("PREFIX ex: <{EX}> ASK {{ ex:x a ex:Employee }}")],
        ));
        let retract = format!(
            "<{EX}x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}Employee> ."
        );
        let idx = OntologyIndex::empty();

        let report = idx
            .plan_mutation_diff(&req(None, &[&retract]), &inp)
            .expect("plan must succeed");

        assert!(report.impacted_action_slugs.contains(&"check-employee".to_string()),
            "ASK flip must mark slug impacted: {:?}", report.impacted_action_slugs);
    }

    // --- Capability B independent-review regressions (RED-first) ----------

    #[test]
    fn plan_rejects_retract_entry_containing_two_quads() {
        let base = format!("<{EX}x> a <{EX}Employee> .");
        let two = format!(
            "<{EX}x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}Employee> .\n\
             <{EX}y> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}Employee> ."
        );
        let inp = input(&base);
        let base_before = inp.base_turtle.clone();
        let result = OntologyIndex::empty().plan_mutation_diff(&req(None, &[&two]), &inp);
        assert!(result.is_err(), "two quads in one retract entry must be rejected: {result:?}");
        assert_eq!(base_before, inp.base_turtle, "plan must not mutate caller input");
    }

    #[test]
    fn plan_rejects_retract_entry_with_valid_quad_and_malformed_tail() {
        let base = format!("<{EX}x> a <{EX}Employee> .");
        let bad = format!(
            "<{EX}x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}Employee> . not-a-triple"
        );
        let inp = input(&base);
        let result = OntologyIndex::empty().plan_mutation_diff(&req(None, &[&bad]), &inp);
        assert!(result.is_err(), "trailing malformed content must be rejected: {result:?}");
    }

    #[test]
    fn plan_impacted_action_slugs_are_sorted_unique_deterministic() {
        let base = format!("<{EX}x> a <{EX}Employee> .");
        let ask = format!("PREFIX ex: <{EX}> ASK {{ ex:x a ex:Employee }}");
        let retract = format!(
            "<{EX}x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}Employee> ."
        );
        let mut a = input(&base);
        a.visible_action_asks = vec![
            ("zeta".into(), vec![ask.clone()]),
            ("alpha".into(), vec![ask.clone()]),
            ("zeta".into(), vec![ask.clone()]),
        ];
        let mut b = input(&base);
        b.visible_action_asks = vec![
            ("alpha".into(), vec![ask.clone()]),
            ("zeta".into(), vec![ask]),
        ];
        let idx = OntologyIndex::empty();
        let ra = idx.plan_mutation_diff(&req(None, &[&retract]), &a).expect("plan a");
        let rb = idx.plan_mutation_diff(&req(None, &[&retract]), &b).expect("plan b");
        assert_eq!(
            serde_json::to_string(&ra.impacted_action_slugs).unwrap(),
            serde_json::to_string(&rb.impacted_action_slugs).unwrap(),
            "reordered/duplicate action rows must yield byte-identical slug JSON"
        );
        assert_eq!(
            ra.impacted_action_slugs,
            vec!["alpha".to_string(), "zeta".to_string()],
            "slug list must be sorted and unique: {:?}", ra.impacted_action_slugs
        );
    }

    #[test]
    fn plan_governance_impact_uses_exact_memory_iri_boundary() {
        let base = format!("<{EX}x> a <{EX}Thing> .");
        let add_false = format!("<{EX}g> <{EX}governs> <urn:neurostrata:memory:mem-ab> .");
        let add_true = format!("<{EX}g> <{EX}governs> <urn:neurostrata:memory:mem-a> .");
        let mut inp = input(&base);
        inp.visible_memory_ids = vec!["mem-a".to_string()];
        let idx = OntologyIndex::empty();

        let false_report = idx.plan_mutation_diff(&req(Some(&add_false), &[]), &inp).expect("plan false");
        assert!(
            !false_report.impacted_memories.iter().any(|m| m == "mem-a"),
            "mem-a must not false-positive on mem-ab: {:?}", false_report.impacted_memories
        );
        let true_report = idx.plan_mutation_diff(&req(Some(&add_true), &[]), &inp).expect("plan true");
        assert!(
            true_report.impacted_memories.iter().any(|m| m == "mem-a"),
            "exact memory IRI must impact: {:?}", true_report.impacted_memories
        );
    }

    #[test]
    fn plan_governance_impact_uses_exact_file_path_boundary() {
        let base = format!("<{EX}x> a <{EX}Thing> .");
        let add_false = format!("<{EX}g> <{EX}governs> \"foo.rs.bak\" .");
        let add_true = format!("<{EX}g> <{EX}governs> \"foo.rs\" .");
        let add_iri = format!("<{EX}g> <{EX}governs> <urn:neurostrata:file:foo.rs> .");
        let mut inp = input(&base);
        inp.visible_file_paths = vec!["foo.rs".to_string()];
        let idx = OntologyIndex::empty();

        let false_report = idx.plan_mutation_diff(&req(Some(&add_false), &[]), &inp).expect("plan false");
        assert!(
            !false_report.impacted_files.iter().any(|p| p == "foo.rs"),
            "foo.rs must not false-positive on foo.rs.bak: {:?}", false_report.impacted_files
        );
        let true_report = idx.plan_mutation_diff(&req(Some(&add_true), &[]), &inp).expect("plan true");
        assert!(
            true_report.impacted_files.iter().any(|p| p == "foo.rs"),
            "exact file path must impact: {:?}", true_report.impacted_files
        );
        let iri_report = idx.plan_mutation_diff(&req(Some(&add_iri), &[]), &inp).expect("plan iri");
        assert!(
            iri_report.impacted_files.iter().any(|p| p == "foo.rs"),
            "file path at a legitimate IRI boundary must impact: {:?}", iri_report.impacted_files
        );
    }

    #[test]
    fn plan_report_vectors_are_canonical_under_shuffled_input() {
        use crate::rdf::reasoning::Owl2RlRuleId;
        let step = |conclusion: &str| DerivationStep {
            rule: Owl2RlRuleId::PrpDom,
            premises: vec!["<urn:p> <urn:q> <urn:r> .".to_string()],
            conclusion: conclusion.to_string(),
        };
        let violation = |focus: &str, component: &str| ShaclViolationSummary {
            focus: focus.to_string(),
            path: None,
            component: component.to_string(),
            message: None,
            severity: "Violation".to_string(),
        };
        let build = |shuffled: bool| OntologyPlanReport {
            namespace: "test/ns".to_string(),
            added_entailments: if shuffled {
                vec![step("<urn:z>"), step("<urn:a>"), step("<urn:m>")]
            } else {
                vec![step("<urn:a>"), step("<urn:m>"), step("<urn:z>")]
            },
            removed_entailments: if shuffled {
                vec!["<urn:z>".into(), "<urn:a>".into()]
            } else {
                vec!["<urn:a>".into(), "<urn:z>".into()]
            },
            validation_conforms: false,
            new_shacl_violations: if shuffled {
                vec![
                    violation("<urn:z>", "MinCountConstraintComponent"),
                    violation("<urn:a>", "DatatypeConstraintComponent"),
                    violation("<urn:a>", "ClassConstraintComponent"),
                ]
            } else {
                vec![
                    violation("<urn:a>", "ClassConstraintComponent"),
                    violation("<urn:a>", "DatatypeConstraintComponent"),
                    violation("<urn:z>", "MinCountConstraintComponent"),
                ]
            },
            consistency_violations: vec![],
            impacted_memories: vec![],
            impacted_files: vec![],
            impacted_action_slugs: vec![],
            schema_ddl_diff: vec![],
        };

        let mut a = build(true);
        let mut b = build(false);
        deterministic_report_order(&mut a);
        deterministic_report_order(&mut b);

        let conclusions: Vec<&str> = a.added_entailments.iter().map(|s| s.conclusion.as_str()).collect();
        assert_eq!(conclusions, vec!["<urn:a>", "<urn:m>", "<urn:z>"]);
        let foci: Vec<(&str, &str)> = a.new_shacl_violations.iter()
            .map(|v| (v.focus.as_str(), v.component.as_str())).collect();
        assert_eq!(foci, vec![
            ("<urn:a>", "ClassConstraintComponent"),
            ("<urn:a>", "DatatypeConstraintComponent"),
            ("<urn:z>", "MinCountConstraintComponent"),
        ]);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "shuffled report inputs must serialize byte-identically"
        );
    }
}
