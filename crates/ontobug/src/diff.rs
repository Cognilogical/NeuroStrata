//! Capability B — structural ontology diff and deterministic LadybugDB Cypher DDL helpers.
//!
//! [`diff_ontologies`] compares two Turtle ontology documents and returns the symmetric
//! difference as deterministic, public structs (BTree orderings); [`generate_cypher_ddl`]
//! renders that diff as a list of idempotent, executable LadybugDB Cypher statements.
//!
//! The surface is pure: nothing in this module opens a database, no triple is mutated, and
//! no database state is read. Callers feed the statements to `LadybugSchemaStore::cypher`
//! (or `cypher_params`) if they want to apply them.

use crate::error::{Error, Result};
use oxrdf::{NamedOrBlankNode, Term};
use oxrdfio::{RdfFormat, RdfParser};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// Vocabulary IRIs.
// ---------------------------------------------------------------------------

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_CLASS: &str = "http://www.w3.org/2000/01/rdf-schema#Class";
const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const RDFS_COMMENT: &str = "http://www.w3.org/2000/01/rdf-schema#comment";
const RDFS_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const RDF_PROPERTY: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
const OWL_OBJECT_PROPERTY: &str = "http://www.w3.org/2002/07/owl#ObjectProperty";
const OWL_DATATYPE_PROPERTY: &str = "http://www.w3.org/2002/07/owl#DatatypeProperty";
const OWL_ANNOTATION_PROPERTY: &str = "http://www.w3.org/2002/07/owl#AnnotationProperty";
const OWL_EQUIVALENT_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const OWL_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const SDO_DOMAIN_INCLUDES: &str = "http://schema.org/domainIncludes";
const SDO_RANGE_INCLUDES: &str = "http://schema.org/rangeIncludes";
const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";

// ---------------------------------------------------------------------------
// Public diff types.
// ---------------------------------------------------------------------------

/// One added or removed class, carrying the annotations relevant to a plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OntologyClassDiff {
    pub iri: String,
    pub label: Option<String>,
    pub comment: Option<String>,
}

/// One added, removed or changed property, carrying the annotations relevant to a plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OntologyPropertyDiff {
    pub iri: String,
    /// The `rdf:type` IRI that declared the property (e.g. `owl:ObjectProperty`).
    pub kind: Option<String>,
    pub label: Option<String>,
    pub comment: Option<String>,
    /// XSD datatype IRIs named by `rdfs:range`, sorted. Mirrors the exact JSON array
    /// stored in the live `OntologyProperty.rangeDatatypes` column.
    pub range_datatypes: BTreeSet<String>,
}

/// One added or removed relationship between two class/property IRIs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OntologyEdgeDiff {
    pub from: String,
    pub to: String,
}

/// The structural difference between two ontology documents: added/removed classes,
/// properties, class hierarchy, equivalence and property domain/range edges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OntologyDiff {
    pub added_classes: BTreeSet<OntologyClassDiff>,
    pub removed_classes: BTreeSet<OntologyClassDiff>,
    /// Entities present in both documents whose label/comment (or property kind) changed.
    /// Values are the POST-image; `None` means "clear this property" in the DDL.
    pub changed_classes: BTreeSet<OntologyClassDiff>,
    pub changed_properties: BTreeSet<OntologyPropertyDiff>,
    pub added_properties: BTreeSet<OntologyPropertyDiff>,
    pub removed_properties: BTreeSet<OntologyPropertyDiff>,
    pub added_subclass_of: BTreeSet<OntologyEdgeDiff>,
    pub removed_subclass_of: BTreeSet<OntologyEdgeDiff>,
    pub added_equivalent_class: BTreeSet<OntologyEdgeDiff>,
    pub removed_equivalent_class: BTreeSet<OntologyEdgeDiff>,
    pub added_domain: BTreeSet<OntologyEdgeDiff>,
    pub removed_domain: BTreeSet<OntologyEdgeDiff>,
    pub added_range: BTreeSet<OntologyEdgeDiff>,
    pub removed_range: BTreeSet<OntologyEdgeDiff>,
}

impl OntologyDiff {
    /// True when nothing was added or removed in either document.
    pub fn is_empty(&self) -> bool {
        self.added_classes.is_empty()
            && self.removed_classes.is_empty()
            && self.changed_classes.is_empty()
            && self.changed_properties.is_empty()
            && self.added_properties.is_empty()
            && self.removed_properties.is_empty()
            && self.added_subclass_of.is_empty()
            && self.removed_subclass_of.is_empty()
            && self.added_equivalent_class.is_empty()
            && self.removed_equivalent_class.is_empty()
            && self.added_domain.is_empty()
            && self.removed_domain.is_empty()
            && self.added_range.is_empty()
            && self.removed_range.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Internal snapshot for diffing.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct OntologySnapshot {
    classes: BTreeMap<String, (Option<String>, Option<String>)>,
    properties: BTreeMap<String, (Option<String>, Option<String>, Option<String>)>,
    datatypes: BTreeMap<String, BTreeSet<String>>,
    subclass_of: BTreeSet<OntologyEdgeDiff>,
    equivalent_class: BTreeSet<OntologyEdgeDiff>,
    domain: BTreeSet<OntologyEdgeDiff>,
    range: BTreeSet<OntologyEdgeDiff>,
}

// ---------------------------------------------------------------------------
// Turtle parsing.
// ---------------------------------------------------------------------------

fn subject_iri(s: &NamedOrBlankNode) -> Option<String> {
    match s {
        NamedOrBlankNode::NamedNode(n) => Some(n.as_str().to_owned()),
        _ => None,
    }
}

fn object_iri(t: &Term) -> Option<String> {
    match t {
        Term::NamedNode(n) => Some(n.as_str().to_owned()),
        _ => None,
    }
}

fn literal_value(t: &Term) -> Option<String> {
    match t {
        Term::Literal(l) => Some(l.value().to_owned()),
        _ => None,
    }
}

/// Precedence of a property `rdf:type`. A generic `rdf:Property` never overrides a
/// specific OWL kind, whatever the statement order.
fn property_kind_rank(iri: &str) -> u8 {
    match iri {
        OWL_OBJECT_PROPERTY => 4,
        OWL_DATATYPE_PROPERTY => 3,
        OWL_ANNOTATION_PROPERTY => 2,
        RDF_PROPERTY => 1,
        _ => 0,
    }
}

/// Parse a Turtle document into a snapshot.
///
/// Blank nodes are intentionally ignored: anonymous classes and OWL restrictions have no
/// stable IRI and no meaningful cross-document identity, so they are skipped
/// deterministically (`subject_iri`/`object_iri` return `None`) rather than expanded into
/// arbitrary OWL restriction diffing.
///
/// Datatype ranges (`rdfs:range <http://www.w3.org/2001/XMLSchema#…>`) are metadata,
/// not edges: a literal datatype has no class node to link, so only named class ranges
/// become `RANGE` edges while XSD ranges are kept as sorted `rangeDatatypes` values.
fn parse_turtle(ttl: &str) -> Result<OntologySnapshot> {
    let parser = RdfParser::from_format(RdfFormat::Turtle);
    let mut quads = Vec::new();
    for q_result in parser.for_slice(ttl.as_bytes()) {
        // Never echo oxrdfio's error: it embeds the offending source fragment. Report
        // only a bounded progress count so arbitrary input cannot leak into logs.
        let q = q_result.map_err(|_| {
            Error::MalformedInput(format!(
                "RDF syntax error in Turtle input after {} statement(s)",
                quads.len()
            ))
        })?;
        quads.push(q);
    }

    let mut snap = OntologySnapshot::default();

    // Pass 1 — declarations, independent of statement order. Annotations written
    // before `rdf:type` therefore survive, and a specific OWL kind always supersedes a
    // generic `rdf:Property` in either order.
    for q in &quads {
        let Some(s) = subject_iri(&q.subject) else {
            continue;
        };
        if q.predicate.as_str() != RDF_TYPE {
            continue;
        }
        let Some(o) = object_iri(&q.object) else {
            continue;
        };
        match o.as_str() {
            RDFS_CLASS | OWL_CLASS => {
                snap.classes.entry(s).or_default();
            }
            OWL_OBJECT_PROPERTY
            | OWL_DATATYPE_PROPERTY
            | OWL_ANNOTATION_PROPERTY
            | RDF_PROPERTY => {
                let entry = snap.properties.entry(s).or_insert((None, None, None));
                let replace = match entry.2.as_deref() {
                    None => true,
                    Some(k) => {
                        let (nr, kr) = (property_kind_rank(&o), property_kind_rank(k));
                        nr > kr || (nr == kr && o.as_str() < k)
                    }
                };
                if replace {
                    entry.2 = Some(o);
                }
            }
            _ => {}
        }
    }

    // Pass 1b — stub every endpoint named only by an edge, mirroring the live
    // projection (`LadybugSchemaStore::project_ontology`). `or_insert`/`or_default`
    // leave declared metadata intact: a stub never overwrites a real declaration.
    for q in &quads {
        let p = q.predicate.as_str();
        let class_edge = matches!(p, OWL_SUBCLASS_OF | OWL_EQUIVALENT_CLASS);
        let prop_edge = matches!(
            p,
            RDFS_DOMAIN | RDFS_RANGE | SDO_DOMAIN_INCLUDES | SDO_RANGE_INCLUDES
        );
        if !class_edge && !prop_edge {
            continue;
        }
        if let Some(s) = subject_iri(&q.subject) {
            if class_edge {
                snap.classes.entry(s).or_default();
            } else {
                snap.properties.entry(s).or_insert((None, None, None));
            }
        }
        // XSD datatype ranges are intentional non-edges: no class node exists to stub.
        let datatype_range = matches!(p, RDFS_RANGE | SDO_RANGE_INCLUDES);
        if let Some(o) = object_iri(&q.object) {
            if !(datatype_range && o.starts_with(XSD_NS)) {
                snap.classes.entry(o).or_default();
            }
        }
    }

    // Pass 2 — annotations and edges, now that every declared term has an entry.
    for q in &quads {
        let Some(s) = subject_iri(&q.subject) else {
            continue;
        };
        let p = q.predicate.as_str();
        let o_iri = object_iri(&q.object);
        let o_lit = literal_value(&q.object);

        if p == RDFS_LABEL {
            if let Some(ref v) = o_lit {
                if let Some(e) = snap.classes.get_mut(&s) {
                    if e.0.is_none() {
                        e.0 = Some(v.clone());
                    }
                }
                if let Some(e) = snap.properties.get_mut(&s) {
                    if e.0.is_none() {
                        e.0 = Some(v.clone());
                    }
                }
            }
        }
        if p == RDFS_COMMENT {
            if let Some(ref v) = o_lit {
                if let Some(e) = snap.classes.get_mut(&s) {
                    if e.1.is_none() {
                        e.1 = Some(v.clone());
                    }
                }
                if let Some(e) = snap.properties.get_mut(&s) {
                    if e.1.is_none() {
                        e.1 = Some(v.clone());
                    }
                }
            }
        }

        if let Some(o) = o_iri {
            match p {
                OWL_SUBCLASS_OF => {
                    snap.subclass_of.insert(OntologyEdgeDiff {
                        from: s.clone(),
                        to: o,
                    });
                }
                OWL_EQUIVALENT_CLASS => {
                    snap.equivalent_class.insert(OntologyEdgeDiff {
                        from: s.clone(),
                        to: o,
                    });
                }
                RDFS_DOMAIN | SDO_DOMAIN_INCLUDES => {
                    snap.domain.insert(OntologyEdgeDiff {
                        from: s.clone(),
                        to: o,
                    });
                }
                RDFS_RANGE | SDO_RANGE_INCLUDES => {
                    if o.starts_with(XSD_NS) {
                        // Literal datatype ranges are metadata, not graph edges; keep
                        // the exact sorted set stored by the live projection.
                        snap.datatypes.entry(s.clone()).or_default().insert(o);
                    } else {
                        snap.range.insert(OntologyEdgeDiff {
                            from: s.clone(),
                            to: o,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(snap)
}

// ---------------------------------------------------------------------------
// diff_ontologies.
// ---------------------------------------------------------------------------

/// Parse `old_ttl` as the pre-image and `new_ttl` as the post-image, then return the
/// symmetric structural difference. Both documents must be valid Turtle; a syntax error
/// is reported as [`Error::MalformedInput`].
pub fn diff_ontologies(old_ttl: &str, new_ttl: &str) -> Result<OntologyDiff> {
    let old = parse_turtle(old_ttl)?;
    let new = parse_turtle(new_ttl)?;
    Ok(diff_snapshots(&old, &new))
}

fn diff_snapshots(old: &OntologySnapshot, new: &OntologySnapshot) -> OntologyDiff {
    let mut d = OntologyDiff::default();

    for (iri, (label, comment)) in &new.classes {
        match old.classes.get(iri) {
            None => {
                d.added_classes.insert(OntologyClassDiff {
                    iri: iri.clone(),
                    label: label.clone(),
                    comment: comment.clone(),
                });
            }
            // Annotation-only delta: values are the post-image. `None` clears a field.
            Some(before) if before != &(label.clone(), comment.clone()) => {
                d.changed_classes.insert(OntologyClassDiff {
                    iri: iri.clone(),
                    label: label.clone(),
                    comment: comment.clone(),
                });
            }
            Some(_) => {}
        }
    }
    for (iri, (label, comment)) in &old.classes {
        if !new.classes.contains_key(iri) {
            d.removed_classes.insert(OntologyClassDiff {
                iri: iri.clone(),
                label: label.clone(),
                comment: comment.clone(),
            });
        }
    }

    let datatypes_of =
        |snap: &OntologySnapshot, iri: &str| snap.datatypes.get(iri).cloned().unwrap_or_default();
    for (iri, (label, comment, kind)) in &new.properties {
        let after = (
            label.clone(),
            comment.clone(),
            kind.clone(),
            datatypes_of(&new, iri),
        );
        let before = old
            .properties
            .get(iri)
            .map(|(l, c, k)| (l.clone(), c.clone(), k.clone(), datatypes_of(&old, iri)));
        match before {
            None => {
                d.added_properties.insert(OntologyPropertyDiff {
                    iri: iri.clone(),
                    kind: kind.clone(),
                    label: label.clone(),
                    comment: comment.clone(),
                    range_datatypes: datatypes_of(&new, iri),
                });
            }
            Some(b) if b != after => {
                d.changed_properties.insert(OntologyPropertyDiff {
                    iri: iri.clone(),
                    kind: kind.clone(),
                    label: label.clone(),
                    comment: comment.clone(),
                    range_datatypes: datatypes_of(&new, iri),
                });
            }
            Some(_) => {}
        }
    }
    for (iri, (label, comment, kind)) in &old.properties {
        if !new.properties.contains_key(iri) {
            d.removed_properties.insert(OntologyPropertyDiff {
                iri: iri.clone(),
                kind: kind.clone(),
                label: label.clone(),
                comment: comment.clone(),
                range_datatypes: datatypes_of(&old, iri),
            });
        }
    }

    d.added_subclass_of = new
        .subclass_of
        .difference(&old.subclass_of)
        .cloned()
        .collect();
    d.removed_subclass_of = old
        .subclass_of
        .difference(&new.subclass_of)
        .cloned()
        .collect();
    d.added_equivalent_class = new
        .equivalent_class
        .difference(&old.equivalent_class)
        .cloned()
        .collect();
    d.removed_equivalent_class = old
        .equivalent_class
        .difference(&new.equivalent_class)
        .cloned()
        .collect();
    d.added_domain = new.domain.difference(&old.domain).cloned().collect();
    d.removed_domain = old.domain.difference(&new.domain).cloned().collect();
    d.added_range = new.range.difference(&old.range).cloned().collect();
    d.removed_range = old.range.difference(&new.range).cloned().collect();

    d
}

// ---------------------------------------------------------------------------
// generate_cypher_ddl.
// ---------------------------------------------------------------------------

/// The namespace of an IRI: up to and including its last `#`, `/` or `:`.
fn namespace_of(iri: &str) -> String {
    if iri.starts_with("_:") {
        return String::new();
    }
    match iri.rfind(['#', '/', ':']) {
        Some(i) if i >= 8 => iri[..i + 1].to_owned(),
        _ => String::new(),
    }
}

/// A Ladybug content node id: owning graph plus IRI, separated by `\x1f`.
///
/// A part that itself contains the separator would alias a different (graph, iri)
/// pair, so escape those parts first. Parts without `\x1f` stay byte-exact,
/// preserving every previously written id.
fn content_id(graph_label: &str, iri: &str) -> String {
    let escape_part = |part: &str| {
        if part.contains('\x1f') {
            part.replace('\\', "\\\\").replace('\x1f', "\x1e")
        } else {
            part.to_owned()
        }
    };
    format!("{}\x1f{}", escape_part(graph_label), escape_part(iri))
}

/// The `rangeDatatypes` column value: a sorted JSON array, exactly as written by
/// `LadybugSchemaStore::project_ontology`.
fn range_datatypes_json(dts: &BTreeSet<String>) -> Result<String> {
    Ok(serde_json::to_string(dts)?)
}

/// Escape a string for a Cypher single-quoted literal. Backslashes are escaped first so
/// the control-character escapes inserted afterwards are not doubled on a second pass.
pub(crate) fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\0', "\\0")
        .replace('\x1f', "\\u001f")
}

/// Render a structural diff as deterministic, idempotent LadybugDB Cypher statements.
/// Node and relationship writes use `MERGE` (never plain `CREATE`), so replaying a plan
/// does not duplicate or conflict. Every edge pattern pins *both* endpoints to the
/// target `graph`, preventing cross-graph matches.
///
/// Syntax follows the in-tree `lbug 0.22.3` usage: `MERGE (n:L {id: …}) ON CREATE SET …`
/// for nodes; endpoints use the proven `MATCH (a:L) WHERE a.iri = … AND a.graph = …`
/// form (never inline endpoint property maps), followed by `MERGE (a)-[:R]->(b)`;
/// this avoids the unsupported `MERGE`-inside-`UNWIND` form. Tables and columns match
/// `crate::ladybug` exactly: `OntologyClass`, `OntologyProperty`, `SUBCLASS_OF`,
/// `EQUIVALENT_CLASS`, `DOMAIN`, `RANGE`. Apply via `LadybugSchemaStore::cypher`.
///
/// `graph` is the label stored in each node's `graph` column (a graph IRI or `"DEFAULT"`).
pub fn generate_cypher_ddl(diff: &OntologyDiff, graph: &str) -> Result<Vec<String>> {
    let glabel = esc(graph);
    let mut stmts = Vec::new();

    // 1. Remove classes (DETACH DELETE also removes their edges). Node ids already
    //    embed the graph label, so this is naturally graph-scoped.
    for c in &diff.removed_classes {
        let id = esc(&content_id(graph, &c.iri));
        stmts.push(format!(
            "MATCH (n:OntologyClass) WHERE n.id = '{id}' DETACH DELETE n"
        ));
    }

    // 2. Remove properties.
    for p in &diff.removed_properties {
        let id = esc(&content_id(graph, &p.iri));
        stmts.push(format!(
            "MATCH (n:OntologyProperty) WHERE n.id = '{id}' DETACH DELETE n"
        ));
    }

    // 3. Add classes — MERGE on primary key, populate all columns on CREATE.
    for c in &diff.added_classes {
        let id = esc(&content_id(graph, &c.iri));
        let iri = esc(&c.iri);
        let ns = esc(&namespace_of(&c.iri));
        let label = esc(c.label.as_deref().unwrap_or(""));
        let comment = esc(c.comment.as_deref().unwrap_or(""));
        stmts.push(format!(
            "MERGE (n:OntologyClass {{id: '{id}'}}) ON CREATE SET n.iri = '{iri}', \
             n.label = '{label}', n.comment = '{comment}', n.namespace = '{ns}', \
             n.graph = '{glabel}'"
        ));
    }

    // 4. Add properties — same MERGE contract, with an empty datatype list.
    for p in &diff.added_properties {
        let id = esc(&content_id(graph, &p.iri));
        let iri = esc(&p.iri);
        let ns = esc(&namespace_of(&p.iri));
        let kind = esc(p.kind.as_deref().unwrap_or(""));
        let label = esc(p.label.as_deref().unwrap_or(""));
        let comment = esc(p.comment.as_deref().unwrap_or(""));
        let dts = esc(&range_datatypes_json(&p.range_datatypes)?);
        stmts.push(format!(
            "MERGE (n:OntologyProperty {{id: '{id}'}}) ON CREATE SET n.iri = '{iri}', \
             n.kind = '{kind}', n.label = '{label}', n.comment = '{comment}', \
             n.rangeDatatypes = '{dts}', n.namespace = '{ns}', n.graph = '{glabel}'"
        ));
    }

    // 5. Annotation-only deltas: SET the post-image; `None` clears the column to ''.
    //    ON CREATE SET also emits complete identity fields so a partial or fresh apply
    //    can never leave a half-populated node behind.
    for c in &diff.changed_classes {
        let id = esc(&content_id(graph, &c.iri));
        let iri = esc(&c.iri);
        let ns = esc(&namespace_of(&c.iri));
        let label = esc(c.label.as_deref().unwrap_or(""));
        let comment = esc(c.comment.as_deref().unwrap_or(""));
        stmts.push(format!(
            "MERGE (n:OntologyClass {{id: '{id}'}}) ON CREATE SET n.iri = '{iri}', \
             n.label = '{label}', n.comment = '{comment}', n.namespace = '{ns}', \
             n.graph = '{glabel}' ON MATCH SET n.label = '{label}', n.comment = '{comment}'"
        ));
    }
    for p in &diff.changed_properties {
        let id = esc(&content_id(graph, &p.iri));
        let iri = esc(&p.iri);
        let ns = esc(&namespace_of(&p.iri));
        let kind = esc(p.kind.as_deref().unwrap_or(""));
        let label = esc(p.label.as_deref().unwrap_or(""));
        let comment = esc(p.comment.as_deref().unwrap_or(""));
        let dts = esc(&range_datatypes_json(&p.range_datatypes)?);
        stmts.push(format!(
            "MERGE (n:OntologyProperty {{id: '{id}'}}) ON CREATE SET n.iri = '{iri}', \
             n.kind = '{kind}', n.label = '{label}', n.comment = '{comment}', \
             n.rangeDatatypes = '{dts}', n.namespace = '{ns}', n.graph = '{glabel}' \
             ON MATCH SET n.kind = '{kind}', n.label = '{label}', n.comment = '{comment}', \
             n.rangeDatatypes = '{dts}'"
        ));
    }

    // 6. Remove edges. Both endpoints are pinned to the target graph using the
    //    proven `MATCH ... WHERE` form; inline endpoint property maps are unproven
    //    in lbug 0.22.3.
    for e in &diff.removed_subclass_of {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (a:OntologyClass)-[r:SUBCLASS_OF]->(b:OntologyClass) \
             WHERE a.iri = '{from}' AND a.graph = '{glabel}' \
             AND b.iri = '{to}' AND b.graph = '{glabel}' DELETE r"
        ));
    }
    for e in &diff.removed_equivalent_class {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (a:OntologyClass)-[r:EQUIVALENT_CLASS]->(b:OntologyClass) \
             WHERE a.iri = '{from}' AND a.graph = '{glabel}' \
             AND b.iri = '{to}' AND b.graph = '{glabel}' DELETE r"
        ));
    }
    for e in &diff.removed_domain {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (p:OntologyProperty)-[r:DOMAIN]->(c:OntologyClass) \
             WHERE p.iri = '{from}' AND p.graph = '{glabel}' \
             AND c.iri = '{to}' AND c.graph = '{glabel}' DELETE r"
        ));
    }
    for e in &diff.removed_range {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (p:OntologyProperty)-[r:RANGE]->(c:OntologyClass) \
             WHERE p.iri = '{from}' AND p.graph = '{glabel}' \
             AND c.iri = '{to}' AND c.graph = '{glabel}' DELETE r"
        ));
    }

    // 7. Add edges — MATCH both graph-scoped endpoints via WHERE, then MERGE the
    //    relationship so replay cannot duplicate it.
    for e in &diff.added_subclass_of {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (a:OntologyClass) WHERE a.iri = '{from}' AND a.graph = '{glabel}' \
             MATCH (b:OntologyClass) WHERE b.iri = '{to}' AND b.graph = '{glabel}' \
             MERGE (a)-[:SUBCLASS_OF]->(b)"
        ));
    }
    for e in &diff.added_equivalent_class {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (a:OntologyClass) WHERE a.iri = '{from}' AND a.graph = '{glabel}' \
             MATCH (b:OntologyClass) WHERE b.iri = '{to}' AND b.graph = '{glabel}' \
             MERGE (a)-[:EQUIVALENT_CLASS]->(b)"
        ));
    }
    for e in &diff.added_domain {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (p:OntologyProperty) WHERE p.iri = '{from}' AND p.graph = '{glabel}' \
             MATCH (c:OntologyClass) WHERE c.iri = '{to}' AND c.graph = '{glabel}' \
             MERGE (p)-[:DOMAIN]->(c)"
        ));
    }
    for e in &diff.added_range {
        let (from, to) = (esc(&e.from), esc(&e.to));
        stmts.push(format!(
            "MATCH (p:OntologyProperty) WHERE p.iri = '{from}' AND p.graph = '{glabel}' \
             MATCH (c:OntologyClass) WHERE c.iri = '{to}' AND c.graph = '{glabel}' \
             MERGE (p)-[:RANGE]->(c)"
        ));
    }

    Ok(stmts)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ttl_a() -> &'static str {
        r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
ex:Person a rdfs:Class ;
    rdfs:label "Person" ;
    rdfs:comment "A person." .
ex:knows a owl:ObjectProperty ;
    rdfs:label "knows" ;
    rdfs:domain ex:Person ;
    rdfs:range ex:Person .
"#
    }

    fn ttl_b() -> &'static str {
        r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
ex:Person a rdfs:Class ;
    rdfs:label "Person" .
ex:Employee a rdfs:Class ;
    rdfs:label "Employee" ;
    rdfs:subClassOf ex:Person .
ex:knows a owl:ObjectProperty ;
    rdfs:domain ex:Employee ;
    rdfs:range ex:Person .
"#
    }

    // --- diff_ontologies ---

    #[test]
    fn diff_ontologies_identical_inputs_is_empty() {
        let d = diff_ontologies(ttl_a(), ttl_a()).unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn diff_ontologies_detects_added_class() {
        let d = diff_ontologies(ttl_a(), ttl_b()).unwrap();
        assert!(d.added_classes.contains(&OntologyClassDiff {
            iri: "http://example.org/Employee".to_owned(),
            label: Some("Employee".to_owned()),
            comment: None,
        }));
    }

    #[test]
    fn diff_ontologies_detects_removed_class() {
        let d = diff_ontologies(ttl_b(), ttl_a()).unwrap();
        assert!(d.removed_classes.contains(&OntologyClassDiff {
            iri: "http://example.org/Employee".to_owned(),
            label: Some("Employee".to_owned()),
            comment: None,
        }));
    }

    #[test]
    fn diff_ontologies_detects_subclass_edge() {
        let d = diff_ontologies(ttl_a(), ttl_b()).unwrap();
        assert!(d.added_subclass_of.contains(&OntologyEdgeDiff {
            from: "http://example.org/Employee".to_owned(),
            to: "http://example.org/Person".to_owned(),
        }));
    }

    #[test]
    fn diff_ontologies_detects_domain_change() {
        let d = diff_ontologies(ttl_a(), ttl_b()).unwrap();
        assert!(d.removed_domain.contains(&OntologyEdgeDiff {
            from: "http://example.org/knows".to_owned(),
            to: "http://example.org/Person".to_owned(),
        }));
        assert!(d.added_domain.contains(&OntologyEdgeDiff {
            from: "http://example.org/knows".to_owned(),
            to: "http://example.org/Employee".to_owned(),
        }));
    }

    // --- generate_cypher_ddl ---

    #[test]
    fn generate_cypher_ddl_added_class_creates_node() {
        let ttl = r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:C a rdfs:Class .
"#;
        let d = diff_ontologies("", ttl).unwrap();
        assert_eq!(
            generate_cypher_ddl(&d, "test").unwrap(),
            vec![
                "MERGE (n:OntologyClass {id: 'test\\u001fhttp://example.org/C'}) ON CREATE SET n.iri = 'http://example.org/C', n.label = '', n.comment = '', n.namespace = 'http://example.org/', n.graph = 'test'"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn generate_cypher_ddl_removed_class_deletes() {
        let ttl = r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:C a rdfs:Class .
"#;
        let d = diff_ontologies(ttl, "").unwrap();
        assert_eq!(
            generate_cypher_ddl(&d, "test").unwrap(),
            vec![
                "MATCH (n:OntologyClass) WHERE n.id = 'test\\u001fhttp://example.org/C' DETACH DELETE n"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn generate_cypher_ddl_escapes_single_quotes() {
        let ttl1 = r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/O'Reilly> a rdfs:Class .
"#;
        let ttl2 = r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
"#;
        let d = diff_ontologies(ttl1, ttl2).unwrap();
        let stmts = generate_cypher_ddl(&d, "test").unwrap();
        // LadybugDB escapes a literal apostrophe as `\'` (never `''`, which would
        // terminate the string and permit Cypher injection).
        assert!(stmts.iter().any(|s| s.contains("O\\'Reilly")));
    }

    #[test]
    fn content_id_escapes_unit_separator_to_avoid_aliasing() {
        // The graph label and IRI are the two parts joined by `\x1f`. If either part
        // itself contains `\x1f` it must be escaped, otherwise distinct nodes alias.
        assert_ne!(content_id("a\x1fb", "c"), content_id("a", "b\x1fc"));
        // Ordinary parts keep their exact historical id encoding.
        assert_eq!(
            content_id("g", "http://example.org/C"),
            "g\x1fhttp://example.org/C"
        );
    }

    #[test]
    fn generated_ddl_with_apostrophe_iri_executes_live() {
        use crate::LadybugSchemaStore;
        let store = LadybugSchemaStore::open_in_memory().unwrap();
        let ttl = r#"@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/O'Reilly> a rdfs:Class .
"#;
        let d = diff_ontologies("", ttl).unwrap();
        let stmts = generate_cypher_ddl(&d, "g").unwrap();
        for s in &stmts {
            store
                .cypher(s)
                .unwrap_or_else(|e| panic!("invalid DDL `{s}`: {e}"));
        }
        assert_eq!(
            store
                .cypher("MATCH (n:OntologyClass) RETURN n.iri")
                .unwrap(),
            vec![vec!["http://example.org/O'Reilly".to_owned()]]
        );
    }

    #[test]
    fn generate_cypher_ddl_deterministic_ordering() {
        let ttl1 = r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:A a rdfs:Class .
ex:B a rdfs:Class .
ex:C a rdfs:Class .
"#;
        let ttl2 = r#"@prefix ex: <http://example.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
"#;
        let d = diff_ontologies(ttl2, ttl1).unwrap();
        let stmts = generate_cypher_ddl(&d, "test").unwrap();
        // Exact ordered statements: one MERGE per class, in IRI order A, B, C.
        let expected: Vec<String> = ["A", "B", "C"]
            .iter()
            .map(|x| {
                let id = format!("test\\u001fhttp://example.org/{x}");
                format!(
                    "MERGE (n:OntologyClass {{id: '{id}'}}) ON CREATE SET n.iri = 'http://example.org/{x}', n.label = '', n.comment = '', n.namespace = 'http://example.org/', n.graph = 'test'"
                )
            })
            .collect();
        assert_eq!(stmts, expected);
    }

    // --- parse error ---

    #[test]
    fn diff_ontologies_malformed_input_returns_error() {
        let ttl = "not valid turtle { }";
        let err = diff_ontologies(ttl, "").unwrap_err();
        match err {
            Error::MalformedInput(_) => {}
            _ => panic!("expected MalformedInput, got {:?}", err),
        }
    }

    // ---- Regression tests for the consolidated v1.19.0 Capability B review ----

    fn prefix_head() -> &'static str {
        "@prefix ex: <http://example.org/> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         @prefix owl: <http://www.w3.org/2002/07/owl#> .\n"
    }

    #[test]
    fn annotations_preceding_rdf_type_are_captured_in_exact_slots() {
        let ttl = format!(
            "{}ex:p rdfs:label \"p-label\" ;\n\
             \x20    rdfs:comment \"p-comment\" ;\n\
             \x20    a owl:ObjectProperty .\n",
            prefix_head()
        );
        let d = diff_ontologies("", &ttl).unwrap();
        assert_eq!(d.added_properties.len(), 1);
        let p = d.added_properties.iter().next().unwrap();
        assert_eq!(p.iri, "http://example.org/p");
        assert_eq!(
            p.label.as_deref(),
            Some("p-label"),
            "label must be tuple slot 0"
        );
        assert_eq!(
            p.comment.as_deref(),
            Some("p-comment"),
            "comment must be tuple slot 1"
        );
        assert_eq!(
            p.kind.as_deref(),
            Some("http://www.w3.org/2002/07/owl#ObjectProperty"),
            "kind must be tuple slot 2 and never a comment"
        );
    }

    #[test]
    fn property_label_comment_kind_survive_even_when_comment_is_long() {
        let ttl = format!(
            "{}ex:p a owl:AnnotationProperty ;\n\
             \x20    rdfs:label \"L\" ; rdfs:comment \"C which is not a kind\" .\n",
            prefix_head()
        );
        let d = diff_ontologies("", &ttl).unwrap();
        let p = d.added_properties.iter().next().unwrap();
        assert_eq!(p.label.as_deref(), Some("L"));
        assert_eq!(p.comment.as_deref(), Some("C which is not a kind"));
        assert_eq!(
            p.kind.as_deref(),
            Some("http://www.w3.org/2002/07/owl#AnnotationProperty")
        );
    }

    #[test]
    fn generic_property_kind_upgrades_to_specific_regardless_of_statement_order() {
        let head = format!(
            "{}@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n",
            prefix_head()
        );
        let orders = [
            format!("{head}ex:p a rdf:Property, owl:ObjectProperty .\n"),
            format!("{head}ex:p a owl:ObjectProperty, rdf:Property .\n"),
            format!("{head}ex:p a rdf:Property, owl:DatatypeProperty .\n"),
            format!("{head}ex:p a owl:DatatypeProperty, rdf:Property .\n"),
        ];
        for (i, ttl) in orders.iter().enumerate() {
            let d = diff_ontologies("", ttl).unwrap();
            let p = d.added_properties.iter().next().unwrap();
            let expected = if i < 2 {
                "http://www.w3.org/2002/07/owl#ObjectProperty"
            } else {
                "http://www.w3.org/2002/07/owl#DatatypeProperty"
            };
            assert_eq!(p.kind.as_deref(), Some(expected), "order variant {i}");
        }
    }

    #[test]
    fn generic_property_kind_is_retained_when_no_specific_declaration() {
        let ttl = format!(
            "{}@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
             ex:p a rdf:Property .\n",
            prefix_head()
        );
        let d = diff_ontologies("", &ttl).unwrap();
        let p = d.added_properties.iter().next().unwrap();
        assert_eq!(
            p.kind.as_deref(),
            Some("http://www.w3.org/1999/02/22-rdf-syntax-ns#Property")
        );
    }

    #[test]
    fn added_nodes_and_edges_are_idempotent_merge_statements() {
        let d = diff_ontologies("", ttl_a()).unwrap();
        let stmts = generate_cypher_ddl(&d, "g").unwrap();
        assert!(
            stmts.iter().all(|s| !s.contains("CREATE (")),
            "no plain CREATE node/relationship pattern (non-idempotent): {:?}",
            stmts
        );
        assert!(stmts
            .iter()
            .any(|s| s.starts_with("MERGE (n:OntologyClass")));
        assert!(stmts
            .iter()
            .any(|s| s.starts_with("MERGE (n:OntologyProperty")));
        let edges: Vec<_> = stmts
            .iter()
            .filter(|s| s.contains("SUBCLASS_OF") || s.contains("DOMAIN") || s.contains("RANGE"))
            .collect();
        assert_eq!(
            edges.into_iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "MATCH (p:OntologyProperty) WHERE p.iri = 'http://example.org/knows' AND p.graph = 'g' MATCH (c:OntologyClass) WHERE c.iri = 'http://example.org/Person' AND c.graph = 'g' MERGE (p)-[:DOMAIN]->(c)",
                "MATCH (p:OntologyProperty) WHERE p.iri = 'http://example.org/knows' AND p.graph = 'g' MATCH (c:OntologyClass) WHERE c.iri = 'http://example.org/Person' AND c.graph = 'g' MERGE (p)-[:RANGE]->(c)",
            ],
            "exact ordered domain/range statements"
        );
        for s in stmts
            .iter()
            .filter(|s| s.contains("SUBCLASS_OF") || s.contains("DOMAIN") || s.contains("RANGE"))
        {
            assert!(
                s.contains("MERGE (a)-[:")
                    || s.contains("MERGE (p)-[:")
                    || s.contains("MERGE (c)-[:")
                    || s.contains(")-[:DOMAIN]->")
                    || s.contains(")-[:RANGE]->"),
                "edge must use MERGE: {s}"
            );
            assert!(!s.contains("CREATE ("), "edge must not CREATE: {s}");
        }
    }

    #[test]
    fn every_edge_statement_scopes_both_endpoints_to_target_graph() {
        let d = diff_ontologies(ttl_a(), ttl_b()).unwrap();
        let stmts = generate_cypher_ddl(&d, "mygraph").unwrap();
        let edge_stmts: Vec<_> = stmts
            .iter()
            .filter(|s| s.contains("DOMAIN") || s.contains("RANGE") || s.contains("SUBCLASS_OF"))
            .collect();
        assert!(!edge_stmts.is_empty());
        for s in edge_stmts {
            assert!(
                s.contains("WHERE "),
                "must use proven MATCH ... WHERE form: {s}"
            );
            assert!(
                !s.contains("{iri:"),
                "inline endpoint property maps are unproven: {s}"
            );
            assert_eq!(
                s.matches("graph = 'mygraph'").count(),
                2,
                "both endpoints must be graph-scoped: {s}"
            );
        }
    }

    #[test]
    fn edge_statements_use_the_exact_proven_match_where_syntax() {
        // ttl_b declares Employee subClassOf Person; reversing makes it a removal.
        let removed = diff_ontologies(ttl_b(), ttl_a()).unwrap();
        let removed_stmts = generate_cypher_ddl(&removed, "g").unwrap();
        let del = removed_stmts
            .iter()
            .find(|s| s.contains("SUBCLASS_OF") && s.contains("DELETE r"))
            .unwrap();
        assert_eq!(
            del,
            "MATCH (a:OntologyClass)-[r:SUBCLASS_OF]->(b:OntologyClass) \
             WHERE a.iri = 'http://example.org/Employee' AND a.graph = 'g' \
             AND b.iri = 'http://example.org/Person' AND b.graph = 'g' DELETE r"
        );

        let added = diff_ontologies("", ttl_a()).unwrap();
        let added_stmts = generate_cypher_ddl(&added, "g").unwrap();
        let domain = added_stmts
            .iter()
            .find(|s| s.contains("[:DOMAIN]->"))
            .unwrap();
        assert_eq!(
            domain,
            "MATCH (p:OntologyProperty) WHERE p.iri = 'http://example.org/knows' \
             AND p.graph = 'g' MATCH (c:OntologyClass) \
             WHERE c.iri = 'http://example.org/Person' AND c.graph = 'g' \
             MERGE (p)-[:DOMAIN]->(c)"
        );
    }

    #[test]
    fn undeclared_class_edge_endpoints_get_deterministic_stubs() {
        let ttl = format!(
            "{}ex:C a rdfs:Class ; rdfs:label \"C label\" .\n\
             ex:Base a rdfs:Class ; rdfs:label \"Base label\" .\n\
             ex:p a owl:ObjectProperty ; rdfs:domain ex:External ; rdfs:range ex:Other .\n\
             ex:C rdfs:subClassOf ex:Base ; owl:equivalentClass ex:AnonymousBase .\n",
            prefix_head()
        );
        let d = diff_ontologies("", &ttl).unwrap();
        // C + Base declared; External, Other and AnonymousBase are undeclared endpoints.
        assert_eq!(d.added_classes.len(), 5);
        for local in ["External", "Other", "AnonymousBase"] {
            let iri = format!("http://example.org/{local}");
            assert!(
                d.added_classes.contains(&OntologyClassDiff {
                    iri,
                    label: None,
                    comment: None,
                }),
                "missing stub for {local}"
            );
        }
        // A declared endpoint keeps its metadata; the stub must not overwrite it.
        let base = d
            .added_classes
            .iter()
            .find(|c| c.iri == "http://example.org/Base")
            .unwrap();
        assert_eq!(base.label.as_deref(), Some("Base label"));
        // Node emission precedes every edge that references the stub.
        let stmts = generate_cypher_ddl(&d, "g").unwrap();
        let node_idx = stmts
            .iter()
            .position(|s| s.starts_with("MERGE (n:OntologyClass") && s.contains("/External'"))
            .unwrap();
        let edge_idx = stmts
            .iter()
            .position(|s| s.contains("[:DOMAIN]->"))
            .unwrap();
        assert!(node_idx < edge_idx, "stub node must precede its edge");
    }

    #[test]
    fn changed_node_merges_create_complete_identity_on_fresh_apply() {
        let old = format!(
            "{}ex:C a rdfs:Class ; rdfs:label \"Old\" ; rdfs:comment \"Old comment\" .\n\
             ex:p a owl:ObjectProperty ; rdfs:label \"Old p\" ; rdfs:comment \"Old pc\" .\n",
            prefix_head()
        );
        let new = format!(
            "{}ex:C a rdfs:Class ; rdfs:label \"New\" .\n\
             ex:p a owl:DatatypeProperty ; rdfs:comment \"New pc\" .\n",
            prefix_head()
        );
        let d = diff_ontologies(&old, &new).unwrap();
        let stmts = generate_cypher_ddl(&d, "g").unwrap();
        let cid = format!("g\\u001fhttp://example.org/C");
        let pid = format!("g\\u001fhttp://example.org/p");
        assert_eq!(
            stmts,
            vec![
                format!(
                    "MERGE (n:OntologyClass {{id: '{cid}'}}) ON CREATE SET n.iri = 'http://example.org/C', n.label = 'New', n.comment = '', n.namespace = 'http://example.org/', n.graph = 'g' ON MATCH SET n.label = 'New', n.comment = ''"
                ),
                format!(
                    "MERGE (n:OntologyProperty {{id: '{pid}'}}) ON CREATE SET n.iri = 'http://example.org/p', n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty', n.label = '', n.comment = 'New pc', n.rangeDatatypes = '[]', n.namespace = 'http://example.org/', n.graph = 'g' ON MATCH SET n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty', n.label = '', n.comment = 'New pc', n.rangeDatatypes = '[]'"
                ),
            ]
        );
    }

    #[test]
    fn escaping_orders_backslash_then_quote_then_control_chars() {
        assert_eq!(
            esc("a\\b'c\nd\re\tf\x00g\x1fh"),
            "a\\\\b\\'c\\nd\\re\\tf\\0g\\u001fh"
        );
    }

    #[test]
    fn annotation_only_changes_are_visible_and_clear_removed_values() {
        let old = format!(
            "{}ex:C a rdfs:Class ; rdfs:label \"Old\" ; rdfs:comment \"Old comment\" .\n\
             ex:p a owl:ObjectProperty ; rdfs:label \"Old p\" ; rdfs:comment \"Old pc\" .\n",
            prefix_head()
        );
        let new = format!(
            "{}ex:C a rdfs:Class ; rdfs:label \"New\" .\n\
             ex:p a owl:DatatypeProperty ; rdfs:comment \"New pc\" .\n",
            prefix_head()
        );
        let d = diff_ontologies(&old, &new).unwrap();
        assert!(!d.is_empty(), "annotation-only changes must be visible");
        assert!(d.added_classes.is_empty() && d.removed_classes.is_empty());
        assert!(d.added_properties.is_empty() && d.removed_properties.is_empty());
        let stmts = generate_cypher_ddl(&d, "g").unwrap();
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("n.label = 'New'") && s.contains("n.comment = ''")),
            "class annotation update with cleared comment: {:?}",
            stmts
        );
        assert!(
            stmts.iter().any(|s| s
                .contains("n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty'")
                && s.contains("n.label = ''")
                && s.contains("n.comment = 'New pc'")),
            "property kind/label/comment update: {:?}",
            stmts
        );
    }

    #[test]
    fn domain_range_only_property_gets_a_stub_node_so_edges_materialize() {
        let ttl = format!(
            "{}ex:C a rdfs:Class .\n\
             ex:undeclared rdfs:domain ex:C ; rdfs:range ex:C .\n",
            prefix_head()
        );
        let d = diff_ontologies("", &ttl).unwrap();
        assert_eq!(
            d.added_properties.len(),
            1,
            "a property named only by domain/range must get a stub node"
        );
        assert_eq!(d.added_domain.len(), 1);
        assert_eq!(d.added_range.len(), 1);
        let stmts = generate_cypher_ddl(&d, "g").unwrap();
        assert!(
            stmts
                .iter()
                .any(|s| s.starts_with("MERGE (n:OntologyProperty")
                    && s.contains("http://example.org/undeclared")),
            "{:?}",
            stmts
        );
    }

    #[test]
    fn diff_and_ddl_are_deterministic_under_triple_reordering() {
        let t1 = format!(
            "{}ex:p rdfs:label \"L\" ; a owl:ObjectProperty .\n\
             ex:C a rdfs:Class .\n",
            prefix_head()
        );
        let t2 = format!(
            "{}ex:C a rdfs:Class .\n\
             ex:p a owl:ObjectProperty ; rdfs:label \"L\" .\n",
            prefix_head()
        );
        let d1 = diff_ontologies("", &t1).unwrap();
        let d2 = diff_ontologies("", &t2).unwrap();
        assert_eq!(d1, d2, "reordering must not change the diff");
        assert_eq!(
            generate_cypher_ddl(&d1, "g").unwrap(),
            generate_cypher_ddl(&d2, "g").unwrap()
        );
    }

    #[test]
    fn malformed_error_is_bounded_and_never_echoes_input_fragments() {
        let ttl = "not valid turtle { TOP_SECRET_FRAGMENT_9f3a2 }";
        let err = diff_ontologies(ttl, "").unwrap_err().to_string();
        assert!(
            !err.contains("TOP_SECRET_FRAGMENT_9f3a2"),
            "raw input leaked: {err}"
        );
        assert!(err.len() <= 128, "error must be bounded: {err}");
    }

    fn exact_new_ttl() -> String {
        format!(
            "{}ex:Person a rdfs:Class ; rdfs:label \"Person\" ; rdfs:comment \"A person.\" .\n\
             ex:knows a owl:ObjectProperty ; rdfs:label \"knows\" ; rdfs:comment \"A relation.\" ;\n\
             \x20    rdfs:domain ex:Person ; rdfs:range ex:Person .\n",
            prefix_head()
        )
    }

    #[test]
    fn generated_ddl_is_exact_ordered_with_exact_metadata() {
        let d = diff_ontologies("", &exact_new_ttl()).unwrap();
        let stmts = generate_cypher_ddl(&d, "g").unwrap();
        let cid = format!("g\\u001fhttp://example.org/Person");
        let pid = format!("g\\u001fhttp://example.org/knows");
        let expected = vec![
            format!(
                "MERGE (n:OntologyClass {{id: '{cid}'}}) ON CREATE SET n.iri = 'http://example.org/Person', n.label = 'Person', n.comment = 'A person.', n.namespace = 'http://example.org/', n.graph = 'g'"
            ),
            format!(
                "MERGE (n:OntologyProperty {{id: '{pid}'}}) ON CREATE SET n.iri = 'http://example.org/knows', n.kind = 'http://www.w3.org/2002/07/owl#ObjectProperty', n.label = 'knows', n.comment = 'A relation.', n.rangeDatatypes = '[]', n.namespace = 'http://example.org/', n.graph = 'g'"
            ),
            "MATCH (p:OntologyProperty) WHERE p.iri = 'http://example.org/knows' AND p.graph = 'g' MATCH (c:OntologyClass) WHERE c.iri = 'http://example.org/Person' AND c.graph = 'g' MERGE (p)-[:DOMAIN]->(c)".to_owned(),
            "MATCH (p:OntologyProperty) WHERE p.iri = 'http://example.org/knows' AND p.graph = 'g' MATCH (c:OntologyClass) WHERE c.iri = 'http://example.org/Person' AND c.graph = 'g' MERGE (p)-[:RANGE]->(c)".to_owned(),
        ];
        assert_eq!(stmts, expected);
    }

    // ---- Final review: live-engine verification + datatype fidelity ----

    fn xsd_prefix() -> &'static str {
        "@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n"
    }

    fn live_ttl_v1() -> String {
        format!(
            "{}{}ex:Person a rdfs:Class ; rdfs:label \"Person\" .\n\
             ex:Employee a rdfs:Class ; rdfs:subClassOf ex:Person .\n\
             ex:knows a owl:ObjectProperty ; rdfs:label \"knows\" ;\n\
             \x20   rdfs:domain ex:Person ; rdfs:range ex:Person ,\n\
             \x20   xsd:string , xsd:int .\n\
             ex:age a owl:DatatypeProperty ; rdfs:domain ex:Person ; rdfs:range xsd:integer .\n",
            prefix_head(),
            xsd_prefix()
        )
    }

    fn live_ttl_v2() -> String {
        format!(
            "{}{}ex:Person a rdfs:Class ; rdfs:label \"Person v2\" .\n\
             ex:knows a owl:DatatypeProperty ; rdfs:label \"knows v2\" ;\n\
             \x20   rdfs:domain ex:Person ; rdfs:range ex:Person , xsd:int .\n\
             ex:age a owl:DatatypeProperty ; rdfs:domain ex:Person ; rdfs:range xsd:integer .\n",
            prefix_head(),
            xsd_prefix()
        )
    }

    #[test]
    fn generated_ddl_executes_live_and_is_idempotent() {
        use crate::LadybugSchemaStore;
        let store = LadybugSchemaStore::open_in_memory().unwrap();

        // Addition plan applied twice: node/edge MERGEs must not duplicate rows.
        let add = diff_ontologies("", &live_ttl_v1()).unwrap();
        let add_stmts = generate_cypher_ddl(&add, "g").unwrap();
        for _ in 0..2 {
            for s in &add_stmts {
                store
                    .cypher(s)
                    .unwrap_or_else(|e| panic!("invalid DDL `{s}`: {e}"));
            }
        }
        let count = |q: &str| store.cypher(q).unwrap()[0][0].clone();
        assert_eq!(count("MATCH (n:OntologyClass) RETURN count(n)"), "2");
        assert_eq!(count("MATCH (n:OntologyProperty) RETURN count(n)"), "2");
        assert_eq!(count("MATCH ()-[r:SUBCLASS_OF]->() RETURN count(r)"), "1");
        assert_eq!(count("MATCH ()-[r:DOMAIN]->() RETURN count(r)"), "2");
        assert_eq!(count("MATCH ()-[r:RANGE]->() RETURN count(r)"), "1");

        // Raw U+001F inside the id literal survives the live engine round-trip:
        // schema `content_id = {graph}\x1f{iri}` stays exactly compatible.
        assert_eq!(
            store
                .cypher("MATCH (n:OntologyClass) RETURN n.id, n.iri ORDER BY n.iri")
                .unwrap(),
            vec![
                vec![
                    "g\u{1f}http://example.org/Employee".to_owned(),
                    "http://example.org/Employee".to_owned()
                ],
                vec![
                    "g\u{1f}http://example.org/Person".to_owned(),
                    "http://example.org/Person".to_owned()
                ],
            ]
        );

        // Changed + removal plan applied twice: identity updates in place, removed
        // class detaches its edge, stale datatype range disappears from ON MATCH.
        let delta = diff_ontologies(&live_ttl_v1(), &live_ttl_v2()).unwrap();
        let delta_stmts = generate_cypher_ddl(&delta, "g").unwrap();
        for _ in 0..2 {
            for s in &delta_stmts {
                store
                    .cypher(s)
                    .unwrap_or_else(|e| panic!("invalid DDL `{s}`: {e}"));
            }
        }
        assert_eq!(count("MATCH (n:OntologyClass) RETURN count(n)"), "1");
        assert_eq!(count("MATCH ()-[r:SUBCLASS_OF]->() RETURN count(r)"), "0");
        assert_eq!(
            store.cypher("MATCH (p:OntologyProperty) WHERE p.iri = 'http://example.org/knows' RETURN p.kind, p.label, p.rangeDatatypes").unwrap(),
            vec![vec![
                "http://www.w3.org/2002/07/owl#DatatypeProperty".to_owned(),
                "knows v2".to_owned(),
                "[\"http://www.w3.org/2001/XMLSchema#int\"]".to_owned(),
            ]]
        );
    }

    #[test]
    fn added_property_emits_exact_datatype_metadata() {
        let ttl = format!(
            "{}{}ex:p a owl:DatatypeProperty ; rdfs:range xsd:int , xsd:string .\n",
            prefix_head(),
            xsd_prefix()
        );
        let d = diff_ontologies("", &ttl).unwrap();
        let p = d.added_properties.iter().next().unwrap();
        assert_eq!(
            p.range_datatypes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "http://www.w3.org/2001/XMLSchema#int",
                "http://www.w3.org/2001/XMLSchema#string"
            ]
        );
        let pid = format!("g\\u001fhttp://example.org/p");
        assert_eq!(
            generate_cypher_ddl(&d, "g").unwrap(),
            vec![format!(
                "MERGE (n:OntologyProperty {{id: '{pid}'}}) ON CREATE SET n.iri = 'http://example.org/p', n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty', n.label = '', n.comment = '', n.rangeDatatypes = '[\"http://www.w3.org/2001/XMLSchema#int\",\"http://www.w3.org/2001/XMLSchema#string\"]', n.namespace = 'http://example.org/', n.graph = 'g'"
            )]
        );
    }

    #[test]
    fn kind_flip_and_datatype_list_change_are_exact_and_nondestructive() {
        let old = format!(
            "{}{}ex:p a owl:ObjectProperty ; rdfs:label \"L\" ; rdfs:range xsd:int , xsd:string .\n",
            prefix_head(),
            xsd_prefix()
        );
        let flip = format!(
            "{}{}ex:p a owl:DatatypeProperty ; rdfs:label \"L\" ; rdfs:range xsd:int , xsd:string .\n",
            prefix_head(),
            xsd_prefix()
        );
        let narrowed = format!(
            "{}{}ex:p a owl:DatatypeProperty ; rdfs:label \"L\" ; rdfs:range xsd:int .\n",
            prefix_head(),
            xsd_prefix()
        );

        // Object -> Datatype: kind changes, valid datatype metadata is preserved.
        let d = diff_ontologies(&old, &flip).unwrap();
        let p = d.changed_properties.iter().next().unwrap();
        assert_eq!(
            p.kind.as_deref(),
            Some("http://www.w3.org/2002/07/owl#DatatypeProperty")
        );
        assert_eq!(
            p.range_datatypes.len(),
            2,
            "datatypes must not be erased on a kind flip"
        );
        let pid = format!("g\\u001fhttp://example.org/p");
        let flip_ddl = generate_cypher_ddl(&d, "g").unwrap();
        assert_eq!(
            flip_ddl,
            vec![format!(
                "MERGE (n:OntologyProperty {{id: '{pid}'}}) ON CREATE SET n.iri = 'http://example.org/p', n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty', n.label = 'L', n.comment = '', n.rangeDatatypes = '[\"http://www.w3.org/2001/XMLSchema#int\",\"http://www.w3.org/2001/XMLSchema#string\"]', n.namespace = 'http://example.org/', n.graph = 'g' ON MATCH SET n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty', n.label = 'L', n.comment = '', n.rangeDatatypes = '[\"http://www.w3.org/2001/XMLSchema#int\",\"http://www.w3.org/2001/XMLSchema#string\"]'"
            )]
        );

        // Datatype -> Object (reverse) keeps the same exact metadata.
        let d = diff_ontologies(&flip, &old).unwrap();
        let p = d.changed_properties.iter().next().unwrap();
        assert_eq!(
            p.kind.as_deref(),
            Some("http://www.w3.org/2002/07/owl#ObjectProperty")
        );
        assert_eq!(p.range_datatypes.len(), 2);

        // Datatype-list-only change updates ON MATCH to the new post-image.
        let d = diff_ontologies(&flip, &narrowed).unwrap();
        let p = d.changed_properties.iter().next().unwrap();
        assert_eq!(
            p.range_datatypes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["http://www.w3.org/2001/XMLSchema#int"]
        );
        assert_eq!(
            generate_cypher_ddl(&d, "g").unwrap(),
            vec![format!(
                "MERGE (n:OntologyProperty {{id: '{pid}'}}) ON CREATE SET n.iri = 'http://example.org/p', n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty', n.label = 'L', n.comment = '', n.rangeDatatypes = '[\"http://www.w3.org/2001/XMLSchema#int\"]', n.namespace = 'http://example.org/', n.graph = 'g' ON MATCH SET n.kind = 'http://www.w3.org/2002/07/owl#DatatypeProperty', n.label = 'L', n.comment = '', n.rangeDatatypes = '[\"http://www.w3.org/2001/XMLSchema#int\"]'"
            )]
        );
    }
}
