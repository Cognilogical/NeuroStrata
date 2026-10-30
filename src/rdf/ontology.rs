use crate::domain;
use crate::registry;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

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

#[derive(Debug, Clone, Default)]
pub struct OntologyIndex {
    pub domains: Vec<DomainEntry>,
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

        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for d in &domains {
            if let Some(parent) = &d.parent {
                children.entry(parent.clone()).or_default().push(d.slug.clone());
            }
        }

        let mut alias_map: BTreeMap<String, String> = BTreeMap::new();
        for d in &domains {
            for alias in &d.aliases {
                alias_map.insert(alias.to_lowercase(), d.slug.clone());
            }
            // slug itself maps to itself
            alias_map.insert(d.slug.to_lowercase(), d.slug.clone());
        }

        OntologyIndex {
            domains,
            children,
            alias_map,
            sub_properties: BTreeMap::new(),
            sub_classes: BTreeMap::new(),
            equivalent: BTreeMap::new(),
            // New fields for 2xe9 spec
            equivalent_class: BTreeMap::new(),
            equivalent_property: BTreeMap::new(),
            inverse_of: BTreeMap::new(),
            transitive_properties: BTreeSet::new(),
            symmetric_properties: BTreeSet::new(),
            alt_labels: BTreeMap::new(),
            exact_match: BTreeMap::new(),
            broader: BTreeMap::new(),
            narrower: BTreeMap::new(),
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

    // New methods for 2xe9 spec
    pub fn subclasses_of(&self, class: &str) -> BTreeSet<String> {
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
        // This would use the alias_map in a real implementation
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
        let mut result = ClosureResult { results: Vec::new(), evidence: Vec::new() };
        let mut queue = VecDeque::new();
        let mut visited = BTreeSet::new();

        queue.push_back(root.to_string());
        visited.insert(root.to_string());

        while let Some(cur) = queue.pop_front() {
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
        let mut result = ClosureResult { results: Vec::new(), evidence: Vec::new() };
        let mut visited = BTreeSet::new();

        for edge in edges {
            let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
            if parts.len() == 2 && parts[0] == root {
                let target = &parts[1];
                if !visited.contains(target) {
                    visited.insert(target.clone());
                    result.results.push(target.clone());
                }
            }
        }

        result
    }
}