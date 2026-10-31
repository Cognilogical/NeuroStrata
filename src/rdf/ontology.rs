use crate::domain;
use crate::registry;
use crate::rdf::shacl::{ShaclViolation, validate_memory};
use crate::traits::MemoryPayload;
use serde_json::Value;
use std::path::{Path, PathBuf};
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
            shacl_shapes: Vec::new(),
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
    }

    pub fn add_inverse_of(&mut self, a: &str, b: &str) {
        self.inverse_of
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string());
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
            if start.elapsed().as_millis() as u64 > budget.max_ms {
                break;
            }
            if result.results.len() >= budget.max_nodes {
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
        while current_hops < max_hops && start.elapsed().as_millis() as u64 <= budget.max_ms {
            if result.results.len() >= budget.max_nodes {
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

pub fn load_shacl_shapes(&mut self, schemas_dir: &Path) -> anyhow::Result<usize> {
    let ttl_files: Vec<PathBuf> = std::fs::read_dir(schemas_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("ttl"))
        .map(|e| e.path())
        .collect();

    let mut total = 0usize;
    for path in ttl_files {
        let content = std::fs::read_to_string(&path)?;
        let parser = oxttl::TurtleParser::new();
        let triples = parser.feed(content.as_bytes())?;
        while let Some(Ok(q)) = parser.parse_next() {
        
        let mut cur_shape: Option<ShaclShapeDef> = None;
        for q in triples {
            let s = q.subject().to_string();
            let p = q.predicate().to_string();
            let o = q.object().to_string();

            // sh:NodeShape declaration
            if p == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                && o == "http://www.w3.org/ns/shacl#NodeShape" {
                if let Some(sh) = cur_shape.take() {
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
                }
            }

            // sh:path
            if p == "http://www.w3.org/ns/shacl#path" {
                if let Some(last) = cur_shape.as_mut().and_then(|sh| sh.property_constraints.last_mut()) {
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
                if let Some(shape_ref) = cur_shape.as_mut() {
                    if let Some(last) = shape_ref.property_constraints.last_mut() {
                        shape_ref.in_values.insert(last.0.clone(), Vec::new());
                    }
                }
            }
        }

        if let Some(sh) = cur_shape.take() {
            self.shacl_shapes.push(sh);
            total += 1;
        }
    }

    Ok(total)
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

            // Only evaluate MemoryShape (targetClass = Memory) or matching type shape
            if shape.target_class.is_none() {
                // MemoryShape — core validation applies to all memories
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
            match value {
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
                serde_json::Value::Number(n) => {
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
}
