use crate::domain;
use crate::registry;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
        }
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
        let mut out = vec![rel.to_string()];
        if let Some(supers) = self.sub_properties.get(rel) {
            for s in supers {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
        }
        out
    }

    pub fn add_sub_property(&mut self, child: &str, parent: &str) {
        self.sub_properties
            .entry(child.to_string())
            .or_default()
            .push(parent.to_string());
    }
}