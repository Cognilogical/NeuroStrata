//! SHACL and ShEx validation with [rudof](https://rudof-project.github.io/).
//!
//! The engines run unchanged over rudof's in-memory graph. [`Report`] flattens rudof's
//! validation report into plain strings so callers need not depend on rudof's types.

use crate::error::{Error, Result};
use oxrdfio::{RdfParser, RdfFormat};
use oxrdf::{NamedOrBlankNode, Term};
use std::collections::BTreeMap;
use std::fmt;

// The rudof SHACL engine types; underlying rudof::ir::IRSchema etc.
pub use rudof_rdf::rdf_core::RDFFormat;
use rudof_rdf::rdf_impl::{OxigraphInMemory, ReaderMode};
use rudof_iri::IriS;
use shacl::ir::IRSchema;
use shacl::rdf::ShaclParser;
use shacl::validator::engine::{Engine, NativeEngine, SparqlEngine};
use shacl::validator::processor::ShaclProcessor;
use shacl::validator::report::ValidationReport;
use shacl::validator::ShaclConfig;
pub use shacl::validator::ShaclValidationMode as Mode;
use shex_ast::compact::ShapeMapParser;
use shex_ast::ir::map_state::MapState;
use shex_ast::ir::schema_ir::SchemaIR;
use shex_ast::ir::semantic_actions_registry::SemanticActionsRegistry;
use shex_ast::{ResolveMethod, ShExParser};
use shex_validation::{Validator, ValidatorConfig};

/// One SHACL validation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// `Violation`, `Warning` or `Info`.
    pub severity: String,
    pub focus: String,
    pub path: Option<String>,
    pub value: Option<String>,
    /// The constraint component, e.g. `sh:MinCountConstraintComponent`.
    pub component: String,
    /// The shape that produced the result.
    pub source: Option<String>,
    pub message: Option<String>,
}

/// The outcome of a SHACL validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub conforms: bool,
    pub results: Vec<Finding>,
}

impl Report {
    /// Results of severity `Violation`.
    pub fn violations(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.severity == "Violation")
            .count()
    }

    fn from_rudof(r: &ValidationReport) -> Self {
        let mut results: Vec<Finding> = r
            .results()
            .iter()
            .map(|v| {
                let msg = v
                    .message()
                    .iter()
                    .map(|(_, m)| m.clone())
                    .collect::<Vec<_>>();
                Finding {
                    severity: v.severity().to_string(),
                    focus: v.focus_node().to_string(),
                    path: v.path().map(ToString::to_string),
                    value: v.value().map(ToString::to_string),
                    component: v.constraint_component().to_string(),
                    source: v.source().map(ToString::to_string),
                    message: (!msg.is_empty()).then(|| msg.join(" / ")),
                }
            })
            .collect();
        results.sort_by(|a, b| {
            (&a.focus, &a.path, &a.component).cmp(&(&b.focus, &b.path, &b.component))
        });
        Self {
            conforms: r.conforms(),
            results,
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.conforms {
            return writeln!(f, "conforms: yes");
        }
        writeln!(f, "conforms: no ({} results)", self.results.len())?;
        for r in &self.results {
            write!(f, "- {} {}", r.severity, r.focus)?;
            if let Some(p) = &r.path {
                write!(f, " {p}")?;
            }
            if let Some(v) = &r.value {
                write!(f, " = {v}")?;
            }
            writeln!(f)?;
            writeln!(f, "    {}", short(&r.component))?;
            if let Some(m) = &r.message {
                writeln!(f, "    {m}")?;
            }
        }
        Ok(())
    }
}

/// `sh:MinCountConstraintComponent` for the full IRI.
fn short(iri: &str) -> String {
    iri.trim_matches(|c| c == '<' || c == '>')
        .replace("http://www.w3.org/ns/shacl#", "sh:")
}

/// Parse RDF content into an Oxigraph in-memory graph.
fn graph(data: &str, format: &RDFFormat) -> Result<OxigraphInMemory> {
    OxigraphInMemory::from_str(data, format, None, &ReaderMode::Strict)
        .map_err(|e| Error::Validation(format!("data graph: {e}")))
}

/// Parses a SHACL shapes graph and compiles it.
pub fn shacl_schema(shapes: &str, format: &RDFFormat) -> Result<IRSchema> {
    let graph = OxigraphInMemory::from_str(shapes, format, None, &ReaderMode::Strict)
        .map_err(|e| Error::Shapes(e.to_string()))?;
    let ast = ShaclParser::new(graph)
        .parse()
        .map_err(|e| Error::Shapes(e.to_string()))?;
    IRSchema::try_from(ast).map_err(|e| Error::Shapes(e.to_string()))
}

struct Processor {
    data: OxigraphInMemory,
}

impl ShaclProcessor<OxigraphInMemory> for Processor {
    fn store(&self) -> &OxigraphInMemory {
        &self.data
    }

    fn runner(mode: &Mode, config: &ShaclConfig) -> Box<dyn Engine<OxigraphInMemory>> {
        match mode {
            Mode::Native => Box::new(NativeEngine::new(config.recursion_semantics())),
            Mode::Sparql => Box::new(SparqlEngine::new(config.recursion_semantics())),
        }
    }
}

/// Validates N-Triples data against N-Triples (or Turtle) shapes.
pub fn shacl(data: &str, shapes: &str, shapes_format: &RDFFormat, mode: &Mode) -> Result<Report> {
    let schema = shacl_schema(shapes, shapes_format)?;
    let report = Processor {
        data: graph(data, &RDFFormat::NTriples)?,
    }
    .validate(&schema, mode, &ShaclConfig::default())
    .map_err(|e| Error::Validation(e.to_string()))?;
    Ok(Report::from_rudof(&report))
}

/// Parses and compiles a ShEx schema (ShExC).
pub fn shex_schema(schema: &str, base: &str) -> Result<SchemaIR> {
    let base = IriS::new(base).map_err(|e| Error::Shapes(e.to_string()))?;
    let ast = ShExParser::parse(schema, Some(base.clone()), &base)
        .map_err(|e| Error::Shapes(e.to_string()))?;
    let config = ValidatorConfig::default();
    let mut map_state = MapState::default();
    let registry = SemanticActionsRegistry::default();
    registry.set_map_state(&mut map_state);
    let mut compiler = shex_ast::ir::ast2ir::AST2IR::new(&ResolveMethod::default(), map_state);
    let mut compiled = SchemaIR::new(registry);
    compiler
        .compile(
            &ast,
            &base,
            &Some(base.clone()),
            &mut compiled,
            config.external_resolvers(),
        )
        .map_err(|e| Error::Shapes(e.to_string()))?;
    Ok(compiled)
}

/// One node/shape pair of a ShEx result shape map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShexResult {
    pub node: String,
    pub shape: String,
    pub conformant: bool,
    pub reason: String,
}

/// Validates N-Triples data against a ShExC schema for the nodes of a shape map
/// (compact syntax, e.g. `<http://ex/a>@<http://ex/S>`).
pub fn shex(data: &str, shexc: &str, base: &str, shape_map: &str) -> Result<Vec<ShexResult>> {
    let schema = shex_schema(shexc, base)?;
    let data = graph(data, &RDFFormat::NTriples)?;
    let nodes_pm = Some(data.prefixmap().clone());
    let shapes_pm = Some(schema.prefixmap());
    let map = ShapeMapParser::parse(shape_map, &nodes_pm, &None, &shapes_pm, &None)
        .map_err(|e| Error::Shapes(e.to_string()))?;
    let validator = Validator::new(&schema, &ValidatorConfig::default())
        .map_err(|e| Error::Validation(e.to_string()))?;
    let result = validator
        .validate_shapemap(&map, &data, &schema, &nodes_pm)
        .map_err(|e| Error::Validation(e.to_string()))?;
    let mut out: Vec<ShexResult> = result
        .iter()
        .map(|(node, shape, status)| ShexResult {
            node: node.to_string(),
            shape: shape.to_string(),
            conformant: status.is_conformant(),
            reason: status.reason(),
        })
        .collect();
    out.sort_by(|a, b| (&a.node, &a.shape).cmp(&(&b.node, &b.shape)));
    Ok(out)
}

/// A SHACL shape distilled from a shapes graph: its IRI, optional target class, and the
/// property constraints it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedShaclShape {
    pub shape_iri: String,
    pub target_class: Option<String>,
    pub property_constraints: Vec<ExtractedShaclProperty>,
    pub min_counts: BTreeMap<String, i64>,
    pub max_counts: BTreeMap<String, i64>,
    pub datatypes: BTreeMap<String, String>,
    pub node_kinds: BTreeMap<String, String>,
    pub in_values: BTreeMap<String, Vec<String>>,
    pub messages: BTreeMap<String, String>,
}

/// One property constraint extracted from a SHACL NodeShape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedShaclProperty {
    pub path: String,
    pub min_count: Option<i64>,
    pub max_count: Option<i64>,
    pub datatype: Option<String>,
    pub node_kind: Option<String>,
    pub in_values: Vec<String>,
    pub message: Option<String>,
}

/// Validate data as Turtle against shapes as Turtle.
pub fn shacl_turtle(data_ttl: &str, shapes_ttl: &str, mode: &Mode) -> Result<Report> {
    let schema = shacl_schema(shapes_ttl, &RDFFormat::Turtle)?;
    let report = Processor {
        data: graph(data_ttl, &RDFFormat::Turtle)?,
    }
    .validate(&schema, mode, &ShaclConfig::default())
    .map_err(|e| Error::Validation(e.to_string()))?;
    Ok(Report::from_rudof(&report))
}

/// One RDF triple from a parsed Turtle document.
#[derive(Debug, Clone)]
struct Triple {
    subject: String,
    predicate: String,
    object: String,
}

/// Extract SHACL NodeShape definitions from a Turtle shapes graph.
///
/// Parses Turtle via `oxrdfio::RdfParser`, resolves `rdf:first`/`rdf:rest` list
/// chains regardless of triple emission order, and extracts `sh:NodeShape`
/// targetClass, property constraints (`sh:path`, `sh:minCount`, `sh:maxCount`,
/// `sh:datatype`, `sh:nodeKind`, `sh:in`, `sh:message`).
pub fn extract_shacl_shapes(ttl: &str) -> Result<Vec<ExtractedShaclShape>> {
    /// Parse Turtle triples from the given Turtle string.
    fn parse_turtle(ttl: &str) -> Result<Vec<Triple>> {
        let parser = RdfParser::from_format(RdfFormat::Turtle);
        let mut triples = Vec::new();
        for q_result in parser.for_slice(ttl.as_bytes()) {
            let q = q_result.map_err(|e| Error::Shapes(e.to_string()))?;
            let s = match q.subject {
                NamedOrBlankNode::NamedNode(n) => n.as_str().to_owned(),
                NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
            };
            let p = q.predicate.as_str().to_owned();
            let o = match &q.object {
                Term::NamedNode(n) => n.as_str().to_owned(),
                Term::Literal(l) => l.value().to_owned(),
                _ => q.object.to_string(),
            };
            triples.push(Triple { subject: s, predicate: p, object: o });
        }
        Ok(triples)
    }

    /// Group triples by subject IRI.
    fn by_subject(triples: &[Triple]) -> BTreeMap<String, Vec<(String, String)>> {
        let mut by_subj: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for t in triples {
            by_subj
                .entry(t.subject.clone())
                .or_default()
                .push((t.predicate.clone(), t.object.clone()));
        }
        by_subj
    }

    /// Resolve an RDF list (rdf:first / rdf:rest chain) starting from `head`.
    fn resolve_rdf_list(
        by_subj: &BTreeMap<String, Vec<(String, String)>>,
        head: &str,
        first_iri: &str,
        rest_iri: &str,
    ) -> Vec<String> {
        let mut values = Vec::new();
        let mut cur = head.to_string();
        let mut hops = 0usize;
        while !cur.is_empty() && hops < 10_000 {
            let firsts: Option<Vec<String>> = by_subj.get(&cur).map(|v| {
                v.iter()
                    .filter(|(p, _)| p == first_iri)
                    .map(|(_, o)| o.clone())
                    .collect()
            });
            if let Some(firsts) = firsts {
                if let Some(f) = firsts.first() {
                    values.push(f.clone());
                } else {
                    break;
                }
            } else {
                break;
            }
            let rests: Option<Vec<String>> = by_subj.get(&cur).map(|v| {
                v.iter()
                    .filter(|(p, _)| p == rest_iri)
                    .map(|(_, o)| o.clone())
                    .collect()
            });
            if let Some(rests) = rests {
                if let Some(r) = rests.first() {
                    if !r.is_empty() {
                        cur = r.clone();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
            hops += 1;
        }
        values
    }

    /// Parse `sh:in` values, handling both inline quoted literals/IRI tokens and
    /// rdf:first/rdf:rest list chains anchored at a blank node.
    fn parse_shacl_in_values(input: &str) -> Vec<String> {
        let mut out = Vec::new();
        let chars: Vec<char> = input.chars().collect();
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

    let triples = parse_turtle(ttl)?;
    let by_subj = by_subject(&triples);

    // IRIs used throughout
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let sh_node_shape = "http://www.w3.org/ns/shacl#NodeShape";
    let sh_target_class = "http://www.w3.org/ns/shacl#targetClass";
    let sh_property = "http://www.w3.org/ns/shacl#property";
    let sh_path = "http://www.w3.org/ns/shacl#path";
    let sh_min_count = "http://www.w3.org/ns/shacl#minCount";
    let sh_max_count = "http://www.w3.org/ns/shacl#maxCount";
    let sh_datatype = "http://www.w3.org/ns/shacl#datatype";
    let sh_node_kind = "http://www.w3.org/ns/shacl#nodeKind";
    let sh_in = "http://www.w3.org/ns/shacl#in";
    let sh_message = "http://www.w3.org/ns/shacl#message";

    // Find sh:NodeShape subjects and build extracted shapes.
    let mut shapes = Vec::new();

    for (subj, subj_triples) in &by_subj {
        // A shape subject is typed as `a sh:NodeShape` (rdf:type object = sh:NodeShape).
        let is_node_shape = subj_triples
            .iter()
            .any(|(p, o)| p == rdf_type && o == sh_node_shape);
        if !is_node_shape {
            continue;
        }

        // Extract targetClass
        let target_class = subj_triples
            .iter()
            .filter(|(p, _)| p == sh_target_class)
            .map(|(_, o)| o.clone())
            .next();

        // Collect property constraints from sh:property blank-node constraints
        let mut property_constraints: Vec<ExtractedShaclProperty> = Vec::new();
        let mut seen_prop_nodes: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();

        for (p, o) in subj_triples {
            if *p != sh_property {
                continue;
            }
            // sh:property may reference either a blank node or a named property
            // shape (IRI); both are keyed in by_subj, so resolve either the same way.
            let o_str: String = o.clone();
            if seen_prop_nodes.contains(&o_str) {
                continue;
            }
            seen_prop_nodes.insert(o_str.clone());
            let mut constraint = ExtractedShaclProperty {
                path: String::new(),
                min_count: None,
                max_count: None,
                datatype: None,
                node_kind: None,
                in_values: Vec::new(),
                message: None,
            };

            let prop_constraints_triples: Vec<(String, String)> =
                by_subj.get(&o_str).cloned().unwrap_or_default();
            for (constr_path, constr_val) in &prop_constraints_triples {
                let p = constr_path.as_str();
                if p == sh_path {
                    constraint.path = constr_val.clone();
                } else if p == sh_min_count {
                    if let Ok(v) = constr_val.parse::<i64>() {
                        constraint.min_count = Some(v);
                    }
                } else if p == sh_max_count {
                    if let Ok(v) = constr_val.parse::<i64>() {
                        constraint.max_count = Some(v);
                    }
                } else if p == sh_datatype {
                    constraint.datatype = Some(constr_val.clone());
                } else if p == sh_node_kind {
                    constraint.node_kind = Some(constr_val.clone());
                } else if p == sh_in {
                    if constr_val.starts_with("_:") {
                        let resolved = resolve_rdf_list(
                            &by_subj,
                            &constr_val,
                            "http://www.w3.org/1999/02/22-rdf-syntax-ns#first",
                            "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest",
                        );
                        constraint.in_values = resolved;
                    } else {
                        constraint.in_values = parse_shacl_in_values(&constr_val);
                    }
                } else if p == sh_message {
                    constraint.message = Some(constr_val.clone());
                }
            }

            property_constraints.push(constraint);
        }

        shapes.push(ExtractedShaclShape {
            shape_iri: subj.clone(),
            target_class,
            property_constraints,
            min_counts: BTreeMap::new(),
            max_counts: BTreeMap::new(),
            datatypes: BTreeMap::new(),
            node_kinds: BTreeMap::new(),
            in_values: BTreeMap::new(),
            messages: BTreeMap::new(),
        });
    }

    Ok(shapes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EX: &str = "http://example.org/";
    const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
    const SH: &str = "http://www.w3.org/ns/shacl#";

    /// A named-node IRI used as `sh:property` must contribute its constraints.
    /// Regression: named property shapes were skipped, dropping minCount/datatype.
    #[test]
    fn named_node_property_shape_constraints_are_extracted() {
        let ttl = format!(
            "@prefix sh: <{SH}> .\n\
             @prefix ex: <{EX}> .\n\
             @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
             ex:PersonShape a sh:NodeShape ;\n\
                 sh:targetClass ex:Person ;\n\
                 sh:property ex:nameProperty .\n\
             ex:nameProperty a sh:PropertyShape ;\n\
                 sh:path ex:name ;\n\
                 sh:minCount 1 ;\n\
                 sh:datatype xsd:string ."
        );

        let shapes = extract_shacl_shapes(&ttl).expect("parse shapes");
        assert_eq!(shapes.len(), 1, "one NodeShape");
        let shape = &shapes[0];
        assert_eq!(shape.shape_iri, format!("{EX}PersonShape"));
        assert_eq!(shape.property_constraints.len(), 1, "named property kept");
        let prop = &shape.property_constraints[0];
        assert_eq!(prop.path, format!("{EX}name"));
        assert_eq!(prop.min_count, Some(1));
        assert_eq!(prop.datatype.as_deref(), Some(XSD_STRING));
    }

    /// Blank-node property shapes must keep working alongside named nodes.
    #[test]
    fn blank_node_property_shape_constraints_still_extracted() {
        let ttl = format!(
            "@prefix sh: <{SH}> .\n\
             @prefix ex: <{EX}> .\n\
             @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n\
             ex:PersonShape a sh:NodeShape ;\n\
                 sh:targetClass ex:Person ;\n\
                 sh:property [ sh:path ex:age ; sh:minCount 2 ; sh:datatype xsd:integer ] ."
        );

        let shapes = extract_shacl_shapes(&ttl).expect("parse shapes");
        assert_eq!(shapes.len(), 1, "one NodeShape");
        let prop = &shapes[0].property_constraints[0];
        assert_eq!(prop.path, format!("{EX}age"));
        assert_eq!(prop.min_count, Some(2));
    }
}
