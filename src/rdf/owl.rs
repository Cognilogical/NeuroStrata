//! OWL 2 RL / RDFS forward-chaining helpers (in-house).
use std::collections::{BTreeMap, BTreeSet};

/// Materialize subclass closure: for each class, include all superclasses.
pub fn materialize_subclass_closure(
    sub: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for k in sub.keys() {
        let mut seen = BTreeSet::new();
        let mut stack = vec![k.clone()];
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur.clone()) { continue; }
            if let Some(parents) = sub.get(&cur) {
                for p in parents {
                    if !seen.contains(p) { stack.push(p.clone()); }
                }
            }
        }
        seen.remove(k);
        out.insert(k.clone(), seen);
    }
    out
}

/// Apply RDFS rule: if (x rdf:type C) and (C rdfs:subClassOf D) then (x rdf:type D).
pub fn type_closure(
    types: &BTreeMap<String, BTreeSet<String>>,
    subclass: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = types.clone();
    for (inst, classes) in types {
        let mut expanded = BTreeSet::new();
        for c in classes {
            expanded.insert(c.clone());
            if let Some(supers) = subclass.get(c) {
                for s in supers { expanded.insert(s.clone()); }
            }
        }
        out.insert(inst.clone(), expanded);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subclass_closure() {
        let mut sub = BTreeMap::new();
        sub.insert("a".into(), BTreeSet::from(["b".into()]));
        sub.insert("b".into(), BTreeSet::from(["c".into()]));
        let cl = materialize_subclass_closure(&sub);
        assert!(cl.get("a").unwrap().contains("c"));
    }
    #[test]
    fn type_closure_propagates() {
        let mut types = BTreeMap::new();
        types.insert("x".into(), BTreeSet::from(["a".into()]));
        let mut sub = BTreeMap::new();
        sub.insert("a".into(), BTreeSet::from(["b".into()]));
        let out = type_closure(&types, &sub);
        assert!(out.get("x").unwrap().contains("b"));
    }
}

/// Materialize inferred triples from a base triple set using RDFS subclass rule.
pub fn materialize_triples(triples: &[(String,String,String)]) -> Vec<(String,String,String)> {
    let mut out: Vec<(String,String,String)> = triples.to_vec();
    // subclass propagation: if (x rdf:type C) and (C rdfs:subClassOf D) then (x rdf:type D)
    let mut subclasses: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (s,p,o) in triples {
        if p == "rdfs:subClassOf" { subclasses.entry(s.clone()).or_default().push(o.clone()); }
    }
    for (s,p,o) in triples {
        if p == "rdf:type" {
            if let Some(supers) = subclasses.get(o) {
                for sup in supers {
                    let t = (s.clone(), "rdf:type".to_string(), sup.clone());
                    if !out.contains(&t) { out.push(t); }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod owl_extra_tests {
    use super::*;
    #[test]
    fn materializes() {
        let base = vec![
            ("x".into(),"rdf:type".into(),"A".into()),
            ("A".into(),"rdfs:subClassOf".into(),"B".into()),
        ];
        let out = materialize_triples(&base);
        assert!(out.iter().any(|t| t == &("x".into(),"rdf:type".into(),"B".into())));
    }
}
