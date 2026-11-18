use crate::traits::{EvidenceEdge, EvidenceKind, MemoryPayload, SearchEvidence, SearchResult};
use crate::rdf::ontology::ClosureResult;
use crate::rdf::ontology::OntologyIndex;
use crate::rdf::ontology::QueryBudget;
use serde_json::json;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// Configuration for deterministic reasoning.
#[derive(Debug, Clone, Copy)]
pub struct ReasoningConfig {
    pub governs_max_hops: u8,
    pub closure_budget: QueryBudget,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            governs_max_hops: 2,
            closure_budget: QueryBudget {
                max_ms: 50,
                max_nodes: 256,
            },
        }
    }
}

/// ReasoningAdapter — deterministic query-time RDFS/OWL 2 RL reasoning.
/// Bounded BFS via QueryBudget, no tableau reasoner.
pub struct ReasoningAdapter {
    pub config: ReasoningConfig,
    /// Payload lookup keyed by memory id, populated by the call site from the
    /// store so `execute` can hydrate inferred `SearchResult` rows with real
    /// `MemoryPayload`s instead of empty stubs.
    payloads: HashMap<String, MemoryPayload>,
}

impl ReasoningAdapter {
    pub fn new() -> Self {
        Self { config: ReasoningConfig::default(), payloads: HashMap::new() }
    }

    pub fn new_with(config: ReasoningConfig) -> Self {
        Self { config, payloads: HashMap::new() }
    }

    /// Builder: register a payload lookup map (id -> MemoryPayload).
    pub fn with_payloads(mut self, payloads: HashMap<String, MemoryPayload>) -> Self {
        self.payloads = payloads;
        self
    }

    /// Hydrate a payload for `id`, falling back to the empty stub.
    fn payload_for(&self, id: &str) -> MemoryPayload {
        self.payloads.get(id).cloned().unwrap_or_else(Self::empty_payload)
    }

    fn empty_payload() -> MemoryPayload {
        MemoryPayload {
            content: String::new(),
            user_id: String::new(),
            memory_type: "fact".to_string(),
            agent_name: None,
            location: String::new(),
            location_lines: String::new(),
            metadata: json!({}),
        }
    }

    /// Expand domain via subClassOf* closure (RDFS §5.3 query-time expansion).
    pub fn expand_domain(&self, ontology: &OntologyIndex, domain: &str) -> Vec<String> {
        if self.config.governs_max_hops == 0
            || self.config.closure_budget.max_nodes == 0
        {
            return vec![domain.to_string()];
        }
        ontology.expand_domain(domain)
    }

    /// Expand identifiers via exactMatch / altLabels equivalence closure.
    pub fn expand_identifiers(&self, ontology: &OntologyIndex, query: &str) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        out.insert(query.to_string());
        for e in ontology.equivalents_of(query) {
            out.insert(e);
        }
        for a in ontology.alt_labels_for(query) {
            out.insert(a);
        }
        for a in ontology.aliases_for(query) {
            out.insert(a);
        }
        let mut v: Vec<String> = out.into_iter().collect();
        v.sort();
        v
    }

    /// Expand relation filters via subPropertyOf* closure.
    pub fn expand_relation(&self, ontology: &OntologyIndex, relation: &str) -> Vec<String> {
        ontology.expand_relation(relation)
    }

    /// Bounded transitive GOVERNS closure (default 2 hops, §5.5).
    pub fn closure_governs(&self, ontology: &OntologyIndex, root: &str, edges: &[String]) -> ClosureResult {
        // GOVERNS closure expands over subPropertyOf*: an edge carrying any
        // sub-relation of GOVERNS (e.g. a strict-governs variant) participates.
        let governed_rels: Vec<String> = self
            .expand_relation(ontology, "GOVERNS")
            .into_iter()
            .map(|r| r.to_lowercase())
            .collect();
        let mut result = ClosureResult::default();
        let max_hops = self.config.governs_max_hops as usize;
        let budget = self.config.closure_budget;
        let start = std::time::Instant::now();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut frontier: VecDeque<String> = VecDeque::new();
        let mut node_count = 0usize;

        // Hop 0: direct outgoing edges from root
        for edge in edges {
            let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
            if parts.len() < 2 || parts[0] != root {
                continue;
            }
            // 2-part edges carry no explicit relation; 3-part "src::tgt::rel" must be a
            // GOVERNS sub-relation (lowercase compare) to participate.
            let relates = parts.len() == 2
                || governed_rels.iter().any(|r| *r == parts[2].to_lowercase());
            if !relates {
                continue;
            }
            let target = &parts[1];
            if !visited.contains(target) {
                visited.insert(target.clone());
                result.results.push(target.clone());
                result.evidence.push(SearchEvidence {
                    kind: EvidenceKind::Governs2hop,
                    path: vec![EvidenceEdge {
                        source: root.to_string(),
                        relation: "governs".to_string(),
                        target: target.clone(),
                    }],
                    explanation: format!("GOVERNS {}::{}", root, target),
                    derivation: vec!["reasoning".to_string()],
                });
                // Only unvisited targets seed the frontier; re-pushing an
                // already-visited node wastes a BFS slot (AN-R3).
                frontier.push_back(target.clone());
            }
        }

        // BFS for further bounded transitive hops
        let mut current_hops = 1usize;
        while current_hops < max_hops
            && node_count < budget.max_nodes
            && (budget.max_ms == 0 || start.elapsed().as_millis() as u64 <= budget.max_ms)
        {
            let frontier_len = frontier.len();
            for _ in 0..frontier_len {
                let node = match frontier.pop_front() {
                    Some(n) => n,
                    None => break,
                };
                node_count += 1;
                for edge in edges {
                    let parts: Vec<String> = edge.split("::").map(|s| s.to_string()).collect();
                    if parts.len() < 2 || parts[0] != node {
                        continue;
                    }
                    let relates = parts.len() == 2
                        || governed_rels.iter().any(|r| *r == parts[2].to_lowercase());
                    if !relates {
                        continue;
                    }
                    let target = &parts[1];
                    if !visited.contains(target) {
                        visited.insert(target.clone());
                        result.results.push(target.clone());
                        frontier.push_back(target.clone());
                        result.evidence.push(SearchEvidence {
                            kind: EvidenceKind::Governs2hop,
                            path: vec![EvidenceEdge {
                                source: node.clone(),
                                relation: "governs".to_string(),
                                target: target.clone(),
                            }],
                            explanation: format!("GOVERNS {}::{}", node, target),
                            derivation: vec!["reasoning".to_string()],
                        });
                    }
                }
            }
            current_hops += 1;
            if node_count >= budget.max_nodes {
                break;
            }
        }

        result
    }

    /// Execute reasoning-backed search expansion over domain, query, and edges.
    pub fn execute(
        &self,
        ontology: &OntologyIndex,
        domain: &str,
        query: &str,
        edges: &[String],
    ) -> Vec<SearchResult> {
        // Domain expansion via subClassOf*
        let expanded_domains = self.expand_domain(ontology, domain);
        let expanded_query = self.expand_identifiers(ontology, query);

        // Bounded GOVERNS closure over expanded domains; closure evidence is
        // attached to the SearchResult rows it discovered.
        let mut all_results: Vec<String> = Vec::new();
        let mut closure_evidence: HashMap<String, Vec<SearchEvidence>> = HashMap::new();
        for d in &expanded_domains {
            let closure = self.closure_governs(ontology, d, edges);
            for ev in closure.evidence {
                if let Some(target) = ev.path.first().map(|e| e.target.clone()) {
                    closure_evidence.entry(target).or_default().push(ev);
                }
            }
            for r in closure.results {
                all_results.push(r);
            }
        }

        // Build SearchResults
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<SearchResult> = Vec::new();
        for e in &expanded_query {
            if seen.insert(e.clone()) {
                out.push(SearchResult {
                    id: e.clone(),
                    score: 1.0,
                    payload: self.payload_for(e),
                    evidence: None,
                });
            }
        }
        for r in &all_results {
            if seen.insert(r.clone()) {
                out.push(SearchResult {
                    id: r.clone(),
                    score: 0.8,
                    payload: self.payload_for(r),
                    evidence: closure_evidence.get(r).cloned(),
                });
            }
        }
        // If nothing expanded, fall back to baseline
        if out.is_empty() {
            out.push(SearchResult {
                id: query.to_string(),
                score: 1.0,
                payload: self.payload_for(query),
                evidence: None,
            });
        }
        out
    }

    /// Production search-side domain expansion (§5.3): subclass-closure BFS,
    /// class/superclass resolution, equivalence, and identifier expansion.
    pub fn expand_domains_for_search(
        &self,
        index: &OntologyIndex,
        seed_domains: &[String],
    ) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for d in seed_domains {
            if d.is_empty() || !out.insert(d.clone()) {
                continue;
            }
            for r in index.closure_subclasses(d, &self.config.closure_budget).results {
                out.insert(r);
            }
            for p in index.expand_class(d) {
                out.insert(p);
            }
            for p in index.subclasses_of(d) {
                out.insert(p);
            }
            for p in index.superclasses_of(d) {
                out.insert(p);
            }
            for p in index.super_classes(d) {
                out.insert(p);
            }
            if let Some(e) = index.equivalent_of(d) {
                out.insert(e);
            }
            for e in index.equivalents_of(d) {
                out.insert(e);
            }
            for a in index.aliases_for(d) {
                out.insert(a);
            }
            for a in index.alt_labels_for(d) {
                out.insert(a);
            }
        }
        out
    }

    /// Production search-side relation expansion (§5.4): subPropertyOf* chain,
    /// direct subproperties, and inverses.
    pub fn expand_relations_for_search(
        &self,
        index: &OntologyIndex,
        rel: &str,
    ) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        out.insert(rel.to_string());
        for r in index.expand_relation(rel) {
            out.insert(r);
        }
        for r in index.subproperties_of(rel) {
            out.insert(r);
        }
        for r in index.inverse_of(rel) {
            out.insert(r);
        }
        // Transitive properties keep the seed relation across chains.
        if index.is_transitive(rel) {
            out.insert(rel.to_string());
        }
        out
    }

    /// SHACL validation of a payload against the index's loaded shapes.
    /// Returns no violations when no shapes are loaded.
    pub fn validate_payload(
        &self,
        index: &OntologyIndex,
        payload: &crate::traits::MemoryPayload,
    ) -> Vec<crate::rdf::shacl::ShaclViolation> {
        if index.shacl_shapes_loaded() > 0 {
            index.shacl_validate(payload)
        } else {
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// OWL 2 RL materialization with proof-carrying derivation certificates.
//
// `reasonable 0.5.0-a1` (DataFrog) performs the fixed-point closure but does not
// expose which rule produced which triple. NeuroStrata therefore deterministically
// reconstructs, for every inferred triple, the exact rule instance that
// establishes it, over the base + materialized triple sets. Any inferred triple
// whose derivation cannot be reconstructed fails the whole call closed — no
// uncertified output is ever returned.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Reasoning profile recorded on every materialization certificate so an
/// operator can tell which rule set produced an entailment (spec §2.2a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningProfile {
    /// Caller label for hierarchy-oriented queries. It does NOT restrict the
    /// closure: `reason_full()` always materializes full OWL 2 RL; the label is
    /// recorded on the certificate for operator visibility only (I10).
    Rdfs,
    /// Full supported DataFrog OWL 2 RL materialization + certified derivations.
    OwlRl,
}

impl Default for ReasoningProfile {
    fn default() -> Self {
        Self::OwlRl
    }
}

impl ReasoningProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rdfs => "rdfs",
            Self::OwlRl => "owl-rl",
        }
    }
}

/// Identifies the W3C OWL 2 RL rule that fired to produce an inferred triple.
/// Serde tags match `crate::formal_kernel::KernelOwl2RlRuleId::as_str` exactly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Owl2RlRuleId {
    /// prp-dom: (?p rdfs:domain ?c) ^ (?x ?p ?y) => (?x rdf:type ?c)
    #[serde(rename = "prp-dom")]
    PrpDom,
    /// prp-rng: (?p rdfs:range ?c) ^ (?x ?p ?y) => (?y rdf:type ?c)
    #[serde(rename = "prp-rng")]
    PrpRng,
    /// prp-trp: (?p rdf:type owl:TransitiveProperty) ^ (?x ?p ?y) ^ (?y ?p ?z) => (?x ?p ?z)
    #[serde(rename = "prp-trp")]
    PrpTrp,
    /// prp-symp: (?p rdf:type owl:SymmetricProperty) ^ (?x ?p ?y) => (?y ?p ?x)
    #[serde(rename = "prp-symp")]
    PrpSymp,
    /// prp-inv: (?p1 owl:inverseOf ?p2) ^ (?x ?p1 ?y) => (?y ?p2 ?x)
    #[serde(rename = "prp-inv")]
    PrpInv,
    /// prp-spo1: (?p1 rdfs:subPropertyOf ?p2) ^ (?x ?p1 ?y) => (?x ?p2 ?y)
    #[serde(rename = "prp-spo1")]
    PrpSpo1,
    /// prp-spo2: (?p owl:propertyChainAxiom ?chain) ^ chain => (?x ?p ?z)
    #[serde(rename = "prp-spo2")]
    PrpSpo2,
    /// prp-eqp: (?p1 owl:equivalentProperty ?p2) ^ (?x ?p1 ?y) => (?x ?p2 ?y)
    #[serde(rename = "prp-eqp")]
    PrpEqp,
    /// cax-sco: (?c1 rdfs:subClassOf ?c2) ^ (?x rdf:type ?c1) => (?x rdf:type ?c2)
    #[serde(rename = "cax-sco")]
    CaxSco,
    /// cax-eqc: (?c1 owl:equivalentClass ?c2) ^ (?x rdf:type ?c1) => (?x rdf:type ?c2)
    #[serde(rename = "cax-eqc")]
    CaxEqc,
    /// cls-hv: owl:hasValue/owl:onProperty restriction inference
    #[serde(rename = "cls-hv")]
    ClsHv,
    /// cls-svf: owl:someValuesFrom/owl:onProperty restriction inference
    #[serde(rename = "cls-svf")]
    ClsSvf,
    /// cls-avf: owl:allValuesFrom/owl:onProperty restriction inference
    #[serde(rename = "cls-avf")]
    ClsAvf,
    /// cax-dw: (?c1 owl:disjointWith ?c2) ^ membership in both => inconsistency
    #[serde(rename = "cax-dw")]
    CaxDwViolation,
    /// owl-universe: adapter extension for the zero-premise OWL 2 universe axioms
    /// the engine injects (T(owl:Thing rdf:type owl:Thing), T(owl:Thing rdf:type
    /// owl:Class), T(owl:Nothing rdf:type owl:Thing), T(owl:Nothing rdf:type
    /// owl:Class)) and the universal T(?x rdf:type owl:Thing) for every node that
    /// appears as the subject of some triple. Spec §2.4 item 5 requires every
    /// returned triple to be certified; these carry deterministic zero-premise
    /// proofs rather than failing the whole call closed.
    #[serde(rename = "owl-universe")]
    OwlUniverse,
    /// scm-eqc: adapter extension; reasonable derives T(?c1 rdfs:subClassOf ?c2)
    /// and the reverse from owl:equivalentClass. One premise.
    #[serde(rename = "scm-eqc")]
    ScmEqc,
    /// rdfs11-subClassOf: adapter extension (RDFS closure); transitive
    /// subClassOf chains. Two premises; ?c1 == ?c3 allowed (SCO cycles).
    #[serde(rename = "rdfs11-subClassOf")]
    RdfsScoTrans,
    /// eq-sym: adapter extension; owl:sameAs is symmetric. One premise.
    #[serde(rename = "eq-sym")]
    EqSym,
    /// eq-trans: adapter extension; owl:sameAs is transitive. Two premises.
    #[serde(rename = "eq-trans")]
    EqTrans,
    /// eq-rep-s: adapter extension; replace triple subjects via owl:sameAs.
    #[serde(rename = "eq-rep-s")]
    EqRepS,
    /// eq-rep-p: adapter extension; replace triple predicates via owl:sameAs.
    #[serde(rename = "eq-rep-p")]
    EqRepP,
    /// eq-rep-o: adapter extension; replace triple objects via owl:sameAs.
    #[serde(rename = "eq-rep-o")]
    EqRepO,
    /// cls-int1: adapter extension; instance of every intersectionOf part
    /// is an instance of the intersection class. List-walk premises.
    #[serde(rename = "cls-int1")]
    ClsInt1,
    /// cls-int2: adapter extension; instance of an intersectionOf class is an
    /// instance of every listed part. List-walk premises.
    #[serde(rename = "cls-int2")]
    ClsInt2,
    /// cls-uni: adapter extension; instance of a unionOf part is an instance
    /// of the union class. List-walk premises.
    #[serde(rename = "cls-uni")]
    ClsUni,
    /// rdfs-container-membership: reasonable injects the axiomatic consequences
    /// of using an `rdf:_n` predicate (rdf:Property / ContainerMembershipProperty
    /// typing plus subPropertyOf rdfs:member). One premise: the usage triple.
    #[serde(rename = "rdfs-container-membership")]
    RdfsContainer,
}

impl Owl2RlRuleId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PrpDom => "prp-dom",
            Self::PrpRng => "prp-rng",
            Self::PrpTrp => "prp-trp",
            Self::PrpSymp => "prp-symp",
            Self::PrpInv => "prp-inv",
            Self::PrpSpo1 => "prp-spo1",
            Self::PrpSpo2 => "prp-spo2",
            Self::PrpEqp => "prp-eqp",
            Self::CaxSco => "cax-sco",
            Self::CaxEqc => "cax-eqc",
            Self::ClsHv => "cls-hv",
            Self::ClsSvf => "cls-svf",
            Self::ClsAvf => "cls-avf",
            Self::CaxDwViolation => "cax-dw",
            Self::OwlUniverse => "owl-universe",
            Self::ScmEqc => "scm-eqc",
            Self::RdfsScoTrans => "rdfs11-subClassOf",
            Self::EqSym => "eq-sym",
            Self::EqTrans => "eq-trans",
            Self::EqRepS => "eq-rep-s",
            Self::EqRepP => "eq-rep-p",
            Self::EqRepO => "eq-rep-o",
            Self::ClsInt1 => "cls-int1",
            Self::ClsInt2 => "cls-int2",
            Self::ClsUni => "cls-uni",
            Self::RdfsContainer => "rdfs-container-membership",
        }
    }

    /// Minimum premises the deterministic audit requires for this rule.
    /// Variable-arity rules (prp-spo2, cls-int*) may carry more.
    pub fn min_premise_count(&self) -> usize {
        match self {
            Self::OwlUniverse => 0,
            Self::ScmEqc | Self::EqSym => 1,
            Self::PrpTrp | Self::PrpSpo2 | Self::ClsHv | Self::ClsSvf => 3,
            Self::CaxDwViolation => 3,
            Self::ClsAvf | Self::ClsInt1 | Self::ClsInt2 | Self::ClsUni => 4,
            Self::RdfsContainer => 1,
            _ => 2,
        }
    }

    /// Numeric tag matching `formal_kernel::kernel_rule_tag`.
    pub fn to_kernel_tag(&self) -> u8 {
        match self {
            Self::PrpDom => 0,
            Self::PrpRng => 1,
            Self::PrpTrp => 2,
            Self::PrpSymp => 3,
            Self::PrpInv => 4,
            Self::PrpSpo1 => 5,
            Self::PrpSpo2 => 6,
            Self::PrpEqp => 7,
            Self::CaxSco => 8,
            Self::CaxEqc => 9,
            Self::ClsHv => 10,
            Self::ClsSvf => 11,
            Self::ClsAvf => 12,
            Self::CaxDwViolation => 13,
            Self::OwlUniverse => 14,
            Self::ScmEqc => 15,
            Self::RdfsScoTrans => 16,
            Self::EqSym => 17,
            Self::EqTrans => 18,
            Self::EqRepS => 19,
            Self::EqRepP => 20,
            Self::EqRepO => 21,
            Self::ClsInt1 => 22,
            Self::ClsInt2 => 23,
            Self::ClsUni => 24,
            Self::RdfsContainer => 25,
        }
    }
}

/// A single derivation step in a proof-carrying inference DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivationStep {
    pub rule: Owl2RlRuleId,
    /// Canonical N-Triple strings for the premise triples that triggered `rule`.
    pub premises: Vec<String>,
    /// Canonical N-Triple string for the entailed conclusion triple.
    pub conclusion: String,
}

/// Complete proof certificate for a materialized closure pass, suitable for
/// verification by the Lean 4 formal kernel (`src/formal_kernel.rs`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DerivationCertificate {
    pub namespace: String,
    pub profile: ReasoningProfile,
    pub base_triple_count: usize,
    pub inferred_triple_count: usize,
    pub steps: Vec<DerivationStep>,
    pub disjointness_violations: Vec<DerivationStep>,
    /// Deterministic SHA-256 digest over canonicalized profile + steps.
    pub certificate_sha256: String,
}

/// Fail-closed outcome for `materialize_owl2_rl_with_proofs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningMaterializeError {
    /// Inferred closure exceeded the caller-provided bound.
    InferredLimitExceeded { max: usize, actual: usize },
    /// An inferred triple's derivation could not be reconstructed by audit.
    /// Carries the offending triple and the reason; the whole call fails closed.
    ProofUnavailable { triple: String, reason: String },
    /// The kernel chain/hash self-check rejected the certificate.
    KernelVerificationFailed { reason: String },
}

impl std::fmt::Display for ReasoningMaterializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InferredLimitExceeded { max, actual } => write!(
                f,
                "inferred triple count {actual} exceeds max_inferred_triples {max}"
            ),
            Self::ProofUnavailable { triple, reason } => write!(
                f,
                "proof unavailable for inferred triple {triple}: {reason}"
            ),
            Self::KernelVerificationFailed { reason } => {
                write!(f, "formal kernel verification failed: {reason}")
            }
        }
    }
}

impl std::error::Error for ReasoningMaterializeError {}

/// Inferred triples plus the certificate that proves them. Returned only when
/// every single inferred triple carries a reconstructed, kernel-checked proof.
#[derive(Debug, Clone)]
pub struct MaterializationCertificate {
    pub inferred_triples: Vec<oxrdf::Triple>,
    pub certificate: DerivationCertificate,
    /// Named graph IRI that isolates inferred entailments from explicit asserts.
    pub inference_graph: String,
    /// N-Quads placing every certified inferred triple in `inference_graph`.
    pub inferred_nquads: Vec<String>,
    /// RDF-star provenance annotations emitted from verified steps only:
    /// `<< s p o >> ns:derivedByRule "rule" .` and
    /// `<< s p o >> ns:certificateHash "sha256" .`
    pub rdf_star_annotations: Vec<String>,
}

impl MaterializationCertificate {
    /// Operator-facing summary. The `profile` label never restricts the
    /// closure: materialization is always full OWL 2 RL (I10).
    pub fn materialization_summary(&self) -> String {
        format!(
            "profile={} materialization=owl-rl profile_restricts_closure=false inferred={} verified=true",
            self.certificate.profile.as_str(),
            self.certificate.inferred_triple_count
        )
    }
}

fn escape_nt(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            _ => out.push(c),
        }
    }
    out
}

fn nt_literal(l: &oxrdf::Literal) -> String {
    let value = escape_nt(l.value());
    match (l.language(), l.direction()) {
        (Some(lang), Some(dir)) => {
            let d = match dir {
                oxrdf::BaseDirection::Ltr => "ltr",
                oxrdf::BaseDirection::Rtl => "rtl",
            };
            format!("\"{value}\"@{lang}--{d}")
        }
        (Some(lang), None) => format!("\"{value}\"@{lang}"),
        (None, _) => {
            let dt = l.datatype().as_str();
            if dt == "http://www.w3.org/2001/XMLSchema#string" {
                format!("\"{value}\"")
            } else {
                format!("\"{value}\"^^<{}>", escape_nt(dt))
            }
        }
    }
}

fn nt_term(term: &oxrdf::Term) -> String {
    match term {
        oxrdf::Term::NamedNode(n) => format!("<{}>", escape_nt(n.as_str())),
        oxrdf::Term::BlankNode(b) => format!("_:{}", b.as_str()),
        oxrdf::Term::Literal(l) => nt_literal(l),
        oxrdf::Term::Triple(t) => format!(
            "<<({} {} {})>>",
            nt_subject(&t.subject),
            format!("<{}>", escape_nt(t.predicate.as_str())),
            nt_term(&t.object)
        ),
    }
}

fn nt_subject(subject: &oxrdf::NamedOrBlankNode) -> String {
    match subject {
        oxrdf::NamedOrBlankNode::NamedNode(n) => format!("<{}>", escape_nt(n.as_str())),
        oxrdf::NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
    }
}

/// Canonical, single-line N-Triples rendering used for digests and premises.
pub fn nt_triple(triple: &oxrdf::Triple) -> String {
    format!(
        "{} <{}> {} .",
        nt_subject(&triple.subject),
        escape_nt(triple.predicate.as_str()),
        nt_term(&triple.object)
    )
}

/// Canonical material hashed into `certificate_sha256`. Covers the profile and
/// every step (derivations and disjointness violations), in DAG order.
fn canonical_certificate_material(
    profile: ReasoningProfile,
    steps: &[DerivationStep],
    violations: &[DerivationStep],
) -> String {
    let mut out = String::new();
    out.push_str("profile:");
    out.push_str(profile.as_str());
    out.push('\n');
    let mut push_step = |tag: &str, st: &DerivationStep| {
        out.push_str(tag);
        out.push('|');
        out.push_str(st.rule.as_str());
        for p in &st.premises {
            out.push('|');
            out.push_str(p);
        }
        out.push('|');
        out.push_str(&st.conclusion);
        out.push('\n');
    };
    for st in steps {
        push_step("step", st);
    }
    for st in violations {
        push_step("violation", st);
    }
    out
}

/// Named graph isolating inferred entailments from explicit ABox/TBox
/// assertions (spec §2.4.6).
pub fn inference_graph_iri(namespace: &str) -> String {
    format!("https://neurostrata.io/ns/graph/inferred#{namespace}")
}

/// SHA-256 of the canonical certificate material; hex-encoded.
pub fn certificate_digest(material: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(material.as_bytes());
    hex::encode(hasher.finalize())
}

fn iri(iri_str: &str) -> oxrdf::NamedNode {
    // Only called with vetted absolute-IRI constants; unchecked construction
    // mirrors the crate's internal conventions.
    oxrdf::NamedNode::new_unchecked(iri_str.to_string())
}

fn named_term(iri_str: &str) -> oxrdf::Term {
    oxrdf::Term::NamedNode(iri(iri_str))
}

const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_PROPERTY: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property";
const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDFS_SUBPROP_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
const RDFS_MEMBER: &str = "http://www.w3.org/2000/01/rdf-schema#member";
const RDFS_CONTAINER_MEMBERSHIP_PROPERTY: &str =
    "http://www.w3.org/2000/01/rdf-schema#ContainerMembershipProperty";
const RDFS_DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
const RDFS_RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
const OWL_TRANSITIVE: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";
const OWL_SYMMETRIC: &str = "http://www.w3.org/2002/07/owl#SymmetricProperty";
const OWL_INVERSE_OF: &str = "http://www.w3.org/2002/07/owl#inverseOf";
const OWL_EQUIV_CLASS: &str = "http://www.w3.org/2002/07/owl#equivalentClass";
const OWL_EQUIV_PROP: &str = "http://www.w3.org/2002/07/owl#equivalentProperty";
const OWL_CHAIN_AXIOM: &str = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
const OWL_ON_PROPERTY: &str = "http://www.w3.org/2002/07/owl#onProperty";
const OWL_HAS_VALUE: &str = "http://www.w3.org/2002/07/owl#hasValue";
const OWL_SOME_VALUES: &str = "http://www.w3.org/2002/07/owl#someValuesFrom";
const OWL_ALL_VALUES: &str = "http://www.w3.org/2002/07/owl#allValuesFrom";
const OWL_DISJOINT_WITH: &str = "http://www.w3.org/2002/07/owl#disjointWith";
const OWL_NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const OWL_CLASS: &str = "http://www.w3.org/2002/07/owl#Class";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_INTERSECTION_OF: &str = "http://www.w3.org/2002/07/owl#intersectionOf";
const OWL_UNION_OF: &str = "http://www.w3.org/2002/07/owl#unionOf";

/// Deterministic audit index over `base ∪ inferred`, keyed by predicate IRI.
struct AuditGraph {
    all: Vec<oxrdf::Triple>,
    by_pred: HashMap<String, Vec<usize>>,
    /// Canonical N-Triples of the authoritative base assertions. Used to rank
    /// derivations by grounding: a proof from base facts is preferred over one
    /// that depends on other inferred triples, which breaks equality/subclass
    /// cycles while keeping the reconstruction deterministic.
    base_nts: HashSet<String>,
}

impl AuditGraph {
    fn new(all: Vec<oxrdf::Triple>) -> Self {
        Self::new_with_base(all.clone(), all)
    }

    fn new_with_base(all: Vec<oxrdf::Triple>, base: Vec<oxrdf::Triple>) -> Self {
        let mut by_pred: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, t) in all.iter().enumerate() {
            by_pred.entry(t.predicate.as_str().to_string()).or_default().push(i);
        }
        let base_nts = base.iter().map(nt_triple).collect();
        Self { all, by_pred, base_nts }
    }

    fn is_base(&self, nt: &str) -> bool {
        self.base_nts.contains(nt)
    }

    fn triples_with_pred(&self, pred: &str) -> impl Iterator<Item = &oxrdf::Triple> + '_ {
        self.by_pred
            .get(pred)
            .into_iter()
            .flat_map(|idxs| idxs.iter().map(|&i| &self.all[i]))
    }
}

/// Keep the lexicographically smallest (deterministic) premise tuple.
fn pick_minimal(candidates: Vec<Vec<oxrdf::Triple>>) -> Option<Vec<oxrdf::Triple>> {
    candidates
        .into_iter()
        .min_by(|a, b| {
            a.iter()
                .map(nt_triple)
                .collect::<Vec<_>>()
                .cmp(&b.iter().map(nt_triple).collect::<Vec<_>>())
        })
}

fn mk_step(
    rule: Owl2RlRuleId,
    premises: Vec<oxrdf::Triple>,
    conclusion: &oxrdf::Triple,
) -> DerivationStep {
    DerivationStep {
        rule,
        premises: premises.iter().map(nt_triple).collect(),
        conclusion: nt_triple(conclusion),
    }
}

/// Pick a minimal premise set that never depends on its own conclusion.
/// A self-referential candidate (e.g. reflexive subClassOf/sameAs, where
/// another sound derivation exists) forms a dependency cycle in the
/// topological order even though a well-founded proof is available. Circular
/// reasoning is unsound, so such candidates are discarded before selection.
fn pick_minimal_step(
    rule: Owl2RlRuleId,
    candidates: Vec<Vec<oxrdf::Triple>>,
    conclusion: &oxrdf::Triple,
) -> Option<DerivationStep> {
    let concl_nt = nt_triple(conclusion);
    let well_founded: Vec<_> = candidates
        .into_iter()
        .filter(|p| !p.iter().any(|t| nt_triple(t) == concl_nt))
        .collect();
    pick_minimal(well_founded).map(|p| mk_step(rule, p, conclusion))
}

fn nb_to_term(s: &oxrdf::NamedOrBlankNode) -> oxrdf::Term {
    match s {
        oxrdf::NamedOrBlankNode::NamedNode(n) => oxrdf::Term::NamedNode(n.clone()),
        oxrdf::NamedOrBlankNode::BlankNode(b) => oxrdf::Term::BlankNode(b.clone()),
    }
}

fn nb_iri<'a>(s: &'a oxrdf::NamedOrBlankNode) -> &'a str {
    match s {
        oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str(),
        _ => "",
    }
}

fn term_named<'a>(t: &'a oxrdf::Term) -> Option<&'a oxrdf::NamedNode> {
    match t {
        oxrdf::Term::NamedNode(n) => Some(n),
        _ => None,
    }
}

fn term_to_nb(t: &oxrdf::Term) -> Option<oxrdf::NamedOrBlankNode> {
    match t {
        oxrdf::Term::NamedNode(n) => Some(oxrdf::NamedOrBlankNode::NamedNode(n.clone())),
        oxrdf::Term::BlankNode(b) => Some(oxrdf::NamedOrBlankNode::BlankNode(b.clone())),
        _ => None,
    }
}

/// The subject term of a member/type triple expressed as `NamedOrBlankNode`.
fn member_nb<'a>(t: &'a oxrdf::Triple) -> &'a oxrdf::NamedOrBlankNode {
    &t.subject
}

fn audit_cax_sco(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(RDFS_SUBCLASS_OF) {
        if ax.object != t.object {
            continue;
        }
        let c1 = nb_to_term(&ax.subject);
        for m in g.triples_with_pred(RDF_TYPE) {
            if member_nb(m) == &t.subject && m.object == c1 {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::CaxSco, cand, t)
}

fn audit_cax_eqc(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_EQUIV_CLASS) {
        // Orientation 1: (c1, equivClass, t.object-class) ; member in c1.
        if ax.object == t.object {
            let c1 = nb_to_term(&ax.subject);
            for m in g.triples_with_pred(RDF_TYPE) {
                if member_nb(m) == &t.subject && m.object == c1 {
                    cand.push(vec![ax.clone(), m.clone()]);
                }
            }
        }
        // Orientation 2: t.object-class is the axiom subject (symmetric).
        if let Some(t_obj_nb) = term_to_nb(&t.object) {
            if ax.subject == t_obj_nb {
                let c1 = ax.object.clone();
                for m in g.triples_with_pred(RDF_TYPE) {
                    if member_nb(m) == &t.subject && m.object == c1 {
                        cand.push(vec![ax.clone(), m.clone()]);
                    }
                }
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::CaxEqc, cand, t)
}

fn audit_prp_dom(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(RDFS_DOMAIN) {
        if ax.object != t.object {
            continue;
        }
        let p = nb_iri(&ax.subject);
        for m in g.triples_with_pred(p) {
            if member_nb(m) == &t.subject {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::PrpDom, cand, t)
}

fn audit_prp_rng(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(RDFS_RANGE) {
        if ax.object != t.object {
            continue;
        }
        let p = nb_iri(&ax.subject);
        for m in g.triples_with_pred(p) {
            if m.object == nb_to_term(&t.subject) {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::PrpRng, cand, t)
}

fn audit_prp_spo1(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(RDFS_SUBPROP_OF) {
        if ax.object != oxrdf::Term::NamedNode(t.predicate.clone()) {
            continue;
        }
        let p1 = nb_iri(&ax.subject);
        for m in g.triples_with_pred(p1) {
            if member_nb(m) == &t.subject && m.object == t.object {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::PrpSpo1, cand, t)
}

fn audit_prp_eqp(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_EQUIV_PROP) {
        // Direction A: (A, eqp, t.pred) ; member predicate A.
        if ax.object == oxrdf::Term::NamedNode(t.predicate.clone()) {
            let p_other = nb_iri(&ax.subject);
            for m in g.triples_with_pred(p_other) {
                if member_nb(m) == &t.subject && m.object == t.object {
                    cand.push(vec![ax.clone(), m.clone()]);
                }
            }
        }
        // Direction B: (t.pred, eqp, B) ; member predicate B.
        if ax.subject == oxrdf::NamedOrBlankNode::NamedNode(t.predicate.clone()) {
            if let Some(p_other) = term_named(&ax.object) {
                for m in g.triples_with_pred(p_other.as_str()) {
                    if member_nb(m) == &t.subject && m.object == t.object {
                        cand.push(vec![ax.clone(), m.clone()]);
                    }
                }
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::PrpEqp, cand, t)
}

fn audit_prp_inv(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let t_obj_nb = match term_to_nb(&t.object) {
        Some(nb) => nb,
        None => return None,
    };
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_INVERSE_OF) {
        // t=(y, P, x). Case 1: ax=(q, inverseOf, P); m=(x, q, y).
        if ax.object == oxrdf::Term::NamedNode(t.predicate.clone()) {
            let q = nb_iri(&ax.subject);
            for m in g.triples_with_pred(q) {
                if member_nb(m) == &t_obj_nb && m.object == nb_to_term(&t.subject) {
                    cand.push(vec![ax.clone(), m.clone()]);
                }
            }
        }
        // Case 2: ax=(P, inverseOf, q); m=(x, q, y).
        if ax.subject == oxrdf::NamedOrBlankNode::NamedNode(t.predicate.clone()) {
            if let Some(q) = term_named(&ax.object) {
                for m in g.triples_with_pred(q.as_str()) {
                    if member_nb(m) == &t_obj_nb && m.object == nb_to_term(&t.subject) {
                        cand.push(vec![ax.clone(), m.clone()]);
                    }
                }
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::PrpInv, cand, t)
}

fn audit_prp_symp(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let p = t.predicate.as_str();
    let mut cand = Vec::new();
    let sym: Vec<oxrdf::Triple> = g
        .triples_with_pred(RDF_TYPE)
        .filter(|m| m.object == named_term(OWL_SYMMETRIC))
        .filter(|m| nb_iri(&m.subject) == p)
        .cloned()
        .collect();
    let t_obj_nb = term_to_nb(&t.object)?;
    for ax in sym {
        for m in g.triples_with_pred(p) {
            if member_nb(m) == &t_obj_nb && m.object == nb_to_term(&t.subject) {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::PrpSymp, cand, t)
}

fn audit_prp_trp(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let p = t.predicate.as_str();
    let trans: Vec<oxrdf::Triple> = g
        .triples_with_pred(RDF_TYPE)
        .filter(|m| m.object == named_term(OWL_TRANSITIVE))
        .filter(|m| nb_iri(&m.subject) == p)
        .cloned()
        .collect();
    if trans.is_empty() {
        return None;
    }
    let mut cand = Vec::new();
    for ax in &trans {
        for m1 in g.triples_with_pred(p) {
            if member_nb(m1) != &t.subject {
                continue;
            }
            let mid = match term_to_nb(&m1.object) {
                Some(nb) => nb,
                None => continue,
            };
            for m2 in g.triples_with_pred(p) {
                if member_nb(m2) == &mid && m2.object == t.object {
                    cand.push(vec![ax.clone(), m1.clone(), m2.clone()]);
                }
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::PrpTrp, cand, t)
}

fn audit_prp_spo2(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_CHAIN_AXIOM) {
        if nb_iri(&ax.subject) != t.predicate.as_str() {
            continue;
        }
        // Walk the rdf:List chain from the axiom object.
        let mut chain: Vec<String> = Vec::new();
        let mut cells: Vec<oxrdf::Triple> = Vec::new();
        let mut cur = ax.object.clone();
        // Finite fuel derived from graph size; a revisit means a cycle, which
        // is not a valid finite rdf:List.
        let fuel = g.all.len() + 1;
        let mut visited: HashSet<oxrdf::Term> = HashSet::new();
        for _ in 0..fuel {
            if cur == named_term(RDF_NIL) {
                break;
            }
            if !visited.insert(cur.clone()) {
                return None;
            }
            let cur_nb = term_to_nb(&cur)?;
            let mut first = None;
            let mut rest = None;
            for c in g.triples_with_pred(RDF_FIRST) {
                if c.subject == cur_nb {
                    first = Some(c.clone());
                }
            }
            for c in g.triples_with_pred(RDF_REST) {
                if c.subject == cur_nb {
                    rest = Some(c.clone());
                }
            }
            let (first, rest) = match (first, rest) {
                (Some(f), Some(r)) => (f, r),
                _ => return None, // malformed list – not a usable chain axiom
            };
            match term_named(&first.object) {
                Some(pi) => chain.push(pi.as_str().to_string()),
                None => return None,
            }
            cells.push(first);
            cells.push(rest.clone());
            cur = rest.object;
        }
        if chain.is_empty() {
            continue;
        }
        // Chain the member triples from t.subject to t.object.
        let mut members: Vec<oxrdf::Triple> = Vec::with_capacity(chain.len());
        let mut cur_nb = t.subject.clone();
        let mut ok = true;
        for (i, pi) in chain.iter().enumerate() {
            let last = i + 1 == chain.len();
            let mut best: Option<oxrdf::Triple> = None;
            for m in g.triples_with_pred(pi) {
                if member_nb(m) != &cur_nb {
                    continue;
                }
                if last && m.object != t.object {
                    continue;
                }
                match &best {
                    Some(b) => {
                        if nt_triple(m) < nt_triple(b) {
                            best = Some(m.clone());
                        }
                    }
                    None => best = Some(m.clone()),
                }
            }
            match best {
                Some(m) => {
                    cur_nb = match term_to_nb(&m.object) {
                        Some(nb) => nb,
                        None => {
                            ok = false;
                            break;
                        }
                    };
                    members.push(m);
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let mut premises = vec![ax.clone()];
        premises.extend(cells);
        premises.extend(members);
        cand.push(premises);
    }
    pick_minimal_step(Owl2RlRuleId::PrpSpo2, cand, t)
}

fn audit_cls_hv(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let p = t.predicate.as_str();
    let mut cand = Vec::new();
    for ax1 in g.triples_with_pred(OWL_ON_PROPERTY) {
        if ax1.object != oxrdf::Term::NamedNode(t.predicate.clone()) {
            continue;
        }
        let c_nb = ax1.subject.clone();
        for ax2 in g.triples_with_pred(OWL_HAS_VALUE) {
            if ax2.subject != c_nb || ax2.object != t.object {
                continue;
            }
            for m in g.triples_with_pred(RDF_TYPE) {
                if member_nb(m) == &t.subject && m.object == nb_to_term(&c_nb) {
                    cand.push(vec![ax1.clone(), ax2.clone(), m.clone()]);
                }
            }
        }
    }
    let _ = p;
    // cls-hv2: (x onProperty p) ^ (x hasValue y) ^ (u p y) => (u rdf:type x)
    if t.predicate.as_str() == RDF_TYPE {
        for ax2 in g.triples_with_pred(OWL_HAS_VALUE) {
            if nb_to_term(&ax2.subject) != t.object {
                continue;
            }
            let y = ax2.object.clone();
            for ax1 in g.triples_with_pred(OWL_ON_PROPERTY) {
                if ax1.subject != ax2.subject {
                    continue;
                }
                let p1 = match term_named(&ax1.object) {
                    Some(n) => n.as_str().to_string(),
                    None => continue,
                };
                for m in g.triples_with_pred(&p1) {
                    if member_nb(m) == &t.subject && m.object == y {
                        cand.push(vec![ax1.clone(), ax2.clone(), m.clone()]);
                    }
                }
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::ClsHv, cand, t)
}

fn audit_cls_svf(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    // t = (x, rdf:type, C) : C onProperty P, C someValuesFrom C2, (x P y), (y a C2).
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax1 in g.triples_with_pred(OWL_ON_PROPERTY) {
        if term_named(&ax1.object).is_none() {
            continue;
        }
        if nb_to_term(&ax1.subject) != t.object {
            continue;
        }
        let p = term_named(&ax1.object).unwrap().as_str().to_string();
        for ax2 in g.triples_with_pred(OWL_SOME_VALUES) {
            if ax2.subject != ax1.subject {
                continue;
            }
            let c2 = ax2.object.clone();
            for m1 in g.triples_with_pred(&p) {
                if member_nb(m1) != &t.subject {
                    continue;
                }
                let filler = match term_to_nb(&m1.object) {
                    Some(nb) => nb,
                    None => continue,
                };
                for m2 in g.triples_with_pred(RDF_TYPE) {
                    if member_nb(m2) == &filler && m2.object == c2 {
                        cand.push(vec![ax1.clone(), ax2.clone(), m1.clone(), m2.clone()]);
                    }
                }
            }
        }
    }
    // cls-svf2: (x someValuesFrom owl:Thing) ^ (x onProperty p) ^ (u p v) => (u rdf:type x)
    for ax2 in g.triples_with_pred(OWL_SOME_VALUES) {
        if nb_to_term(&ax2.subject) != t.object {
            continue;
        }
        if ax2.object != named_term(OWL_THING) {
            continue;
        }
        for ax1 in g.triples_with_pred(OWL_ON_PROPERTY) {
            if ax1.subject != ax2.subject {
                continue;
            }
            let p1 = match term_named(&ax1.object) {
                Some(n) => n.as_str().to_string(),
                None => continue,
            };
            for m in g.triples_with_pred(&p1) {
                if member_nb(m) == &t.subject {
                    cand.push(vec![ax1.clone(), ax2.clone(), m.clone()]);
                }
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::ClsSvf, cand, t)
}

fn audit_cls_avf(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    // t = (y, rdf:type, C2) : C onProperty P, C allValuesFrom C2, (x a C), (x P y).
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax2 in g.triples_with_pred(OWL_ALL_VALUES) {
        if ax2.object != t.object {
            continue;
        }
        for ax1 in g.triples_with_pred(OWL_ON_PROPERTY) {
            if ax1.subject != ax2.subject {
                continue;
            }
            let p = term_named(&ax1.object)?.as_str().to_string();
            let c = ax1.subject.clone();
            for m1 in g.triples_with_pred(&p) {
                if m1.object != nb_to_term(&t.subject) {
                    continue;
                }
                for m2 in g.triples_with_pred(RDF_TYPE) {
                    if member_nb(m2) == &m1.subject && m2.object == nb_to_term(&c) {
                        cand.push(vec![ax1.clone(), ax2.clone(), m2.clone(), m1.clone()]);
                    }
                }
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::ClsAvf, cand, t)
}

/// Walk an rdf:List from `head`; returns (member URIs, cell triples).
/// Fuel derives from graph size, so any finite valid list is supported;
/// revisiting a cell means a cycle and the list is rejected.
fn walk_list(
    g: &AuditGraph,
    head: &oxrdf::Term,
) -> Option<(Vec<oxrdf::NamedNode>, Vec<oxrdf::Triple>)> {
    let mut members = Vec::new();
    let mut cells = Vec::new();
    let mut cur = head.clone();
    let fuel = g.all.len() + 1;
    let mut visited: HashSet<oxrdf::Term> = HashSet::new();
    for _ in 0..fuel {
        if cur == named_term(RDF_NIL) {
            return Some((members, cells));
        }
        if !visited.insert(cur.clone()) {
            return None;
        }
        let cur_nb = term_to_nb(&cur)?;
        let first = g
            .triples_with_pred(RDF_FIRST)
            .find(|c| c.subject == cur_nb)
            .cloned();
        let rest = g
            .triples_with_pred(RDF_REST)
            .find(|c| c.subject == cur_nb)
            .cloned();
        let (first, rest) = match (first, rest) {
            (Some(f), Some(r)) => (f, r),
            _ => return None,
        };
        match term_named(&first.object) {
            Some(pi) => members.push(pi.clone()),
            None => return None,
        }
        cells.push(first);
        cells.push(rest.clone());
        cur = rest.object;
    }
    None
}

/// scm-eqc: EQC(c1,c2) -> SCO(c1,c2) and SCO(c2,c1). One premise.
fn audit_scm_eqc(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDFS_SUBCLASS_OF {
        return None;
    }
    let t_subject_term = nb_to_term(&t.subject);
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_EQUIV_CLASS) {
        // EQC(a,b) -> SCO(a,b): t = (a SCO b)
        if nb_to_term(&ax.subject) == t_subject_term && ax.object == t.object {
            cand.push(vec![ax.clone()]);
            continue;
        }
        // EQC(b,a) -> SCO(a,b): t = (a SCO b) with premise EQC(b,a)
        if nb_to_term(&ax.subject) == t.object && ax.object == t_subject_term {
            cand.push(vec![ax.clone()]);
        }
    }
    pick_minimal_step(Owl2RlRuleId::ScmEqc, cand, t)
}

/// rdfs11-subClassOf: SCO(A,B) ^ SCO(B,C) => SCO(A,C) (A==C allowed).
fn audit_rdfs11(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDFS_SUBCLASS_OF {
        return None;
    }
    let mut cand = Vec::new();
    for ax1 in g.triples_with_pred(RDFS_SUBCLASS_OF) {
        if ax1.subject != t.subject {
            continue;
        }
        let mid = match term_to_nb(&ax1.object) {
            Some(nb) => nb,
            None => continue,
        };
        for ax2 in g.triples_with_pred(RDFS_SUBCLASS_OF) {
            if ax2.subject == mid && ax2.object == t.object {
                cand.push(vec![ax1.clone(), ax2.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::RdfsScoTrans, cand, t)
}

/// eq-sym: sameAs(x,y) => sameAs(y,x). One premise.
fn audit_eq_sym(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != OWL_SAME_AS {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_SAME_AS) {
        // ax=(a, SA, b) -> t=(b, SA, a)
        let ax_obj_nb = term_to_nb(&ax.object);
        if ax_obj_nb.as_ref() == Some(&t.subject) && nb_to_term(&ax.subject) == t.object.clone() {
            cand.push(vec![ax.clone()]);
        }
    }
    pick_minimal_step(Owl2RlRuleId::EqSym, cand, t)
}

/// eq-trans: sameAs(x,y) ^ sameAs(y,z) => sameAs(x,z). Two premises.
fn audit_eq_trans(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != OWL_SAME_AS {
        return None;
    }
    let mut cand = Vec::new();
    for ax1 in g.triples_with_pred(OWL_SAME_AS) {
        if nb_to_term(&ax1.subject) != nb_to_term(&t.subject) {
            continue;
        }
        let mid = match term_to_nb(&ax1.object) {
            Some(nb) => nb,
            None => continue,
        };
        for ax2 in g.triples_with_pred(OWL_SAME_AS) {
            if ax2.subject == mid && ax2.object == t.object {
                cand.push(vec![ax1.clone(), ax2.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::EqTrans, cand, t)
}

/// eq-rep-s: sameAs(s1,s2) ^ T(s1,p,o) => T(s2,p,o) (sameAs symmetric).
fn audit_eq_rep_s(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let t_subj = nb_to_term(&t.subject);
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_SAME_AS) {
        let other = if ax.object == t_subj {
            nb_to_term(&ax.subject)
        } else if nb_to_term(&ax.subject) == t_subj {
            ax.object.clone()
        } else {
            continue;
        };
        let other_nb = match term_to_nb(&other) {
            Some(nb) => nb,
            None => continue,
        };
        for m in g.triples_with_pred(t.predicate.as_str()) {
            if m.subject == other_nb && m.object == t.object {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::EqRepS, cand, t)
}

/// eq-rep-p: sameAs(p1,p2) ^ T(s,p1,o) => T(s,p2,o).
fn audit_eq_rep_p(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let t_pred_term = oxrdf::Term::NamedNode(t.predicate.clone());
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_SAME_AS) {
        let other = if ax.object == t_pred_term {
            nb_to_term(&ax.subject)
        } else if nb_to_term(&ax.subject) == t_pred_term {
            ax.object.clone()
        } else {
            continue;
        };
        let p1 = match term_named(&other) {
            Some(p1) => p1.as_str().to_string(),
            None => continue,
        };
        for m in g.triples_with_pred(&p1) {
            if m.subject == t.subject && m.object == t.object {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::EqRepP, cand, t)
}

/// eq-rep-o: sameAs(o1,o2) ^ T(s,p,o1) => T(s,p,o2).
fn audit_eq_rep_o(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_SAME_AS) {
        let other = if ax.object == t.object {
            nb_to_term(&ax.subject)
        } else if nb_to_term(&ax.subject) == t.object {
            ax.object.clone()
        } else {
            continue;
        };
        for m in g.triples_with_pred(t.predicate.as_str()) {
            if m.subject == t.subject && m.object == other {
                cand.push(vec![ax.clone(), m.clone()]);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::EqRepO, cand, t)
}

/// cls-int1: T(x, each part of I) ^ T(I owl:intersectionOf L) => T(x, I).
fn audit_cls_int1(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_INTERSECTION_OF) {
        if nb_to_term(&ax.subject) != t.object {
            continue;
        }
        let (members, cells) = walk_list(g, &ax.object)?;
        if members.is_empty() {
            continue;
        }
        let mut premises = vec![ax.clone()];
        premises.extend(cells);
        let mut ok = true;
        for m_class in &members {
            match g
                .triples_with_pred(RDF_TYPE)
                .find(|m| m.subject == t.subject && m.object == named_term(m_class.as_str()))
                .cloned()
            {
                Some(m) => premises.push(m),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            cand.push(premises);
        }
    }
    pick_minimal_step(Owl2RlRuleId::ClsInt1, cand, t)
}

/// cls-int2: T(x, I) ^ T(I owl:intersectionOf L) => T(x, each part of L).
fn audit_cls_int2(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_INTERSECTION_OF) {
        let target = ax.subject.clone();
        let (members, cells) = match walk_list(g, &ax.object) {
            Some(v) => v,
            None => continue,
        };
        if !members
            .iter()
            .any(|m| named_term(m.as_str()) == t.object)
        {
            continue;
        }
        // premises: axiom + cells + membership triple T(t.subject, I)
        let membership = g
            .triples_with_pred(RDF_TYPE)
            .find(|m| m.subject == t.subject && m.object == nb_to_term(&target));
        match membership {
            Some(m) => {
                let mut premises = vec![ax.clone()];
                premises.extend(cells);
                premises.push(m.clone());
                cand.push(premises);
            }
            None => continue,
        }
    }
    pick_minimal_step(Owl2RlRuleId::ClsInt2, cand, t)
}

/// cls-uni: T(x, ci in L) ^ T(U owl:unionOf L) => T(x, U).
fn audit_cls_uni(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let mut cand = Vec::new();
    for ax in g.triples_with_pred(OWL_UNION_OF) {
        if nb_to_term(&ax.subject) != t.object {
            continue;
        }
        let (members, cells) = match walk_list(g, &ax.object) {
            Some(v) => v,
            None => continue,
        };
        for m_class in &members {
            let member = g
                .triples_with_pred(RDF_TYPE)
                .find(|m| m.subject == t.subject && m.object == named_term(m_class.as_str()));
            if let Some(m) = member {
                let mut premises = vec![ax.clone()];
                premises.extend(cells.iter().cloned());
                premises.push(m.clone());
                cand.push(premises);
            }
        }
    }
    pick_minimal_step(Owl2RlRuleId::ClsUni, cand, t)
}

/// Zero-premise universe axioms emitted by the engine (owl:Thing / owl:Class /
/// True for `rdf:_1`, `rdf:_2`, ... container-membership predicate IRIs.
fn is_container_membership_iri(s: &str) -> bool {
    match s.strip_prefix(RDF_NS) {
        Some(rest) => {
            !rest.is_empty() && rest.starts_with('_') && rest[1..].bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// rdfs12: using `rdf:_n` as a predicate yields three axiomatic consequences
/// (rdf:Property typing, ContainerMembershipProperty typing, and
/// rdfs:subPropertyOf rdfs:member). The single premise is a graph triple that
/// actually uses `rdf:_n` as its predicate.
fn audit_rdfs_container(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let cm = nb_iri(&t.subject);
    if !is_container_membership_iri(cm) {
        return None;
    }
    let p = t.predicate.as_str();
    let shape_ok = (p == RDF_TYPE
        && (t.object == named_term(RDF_PROPERTY)
            || t.object == named_term(RDFS_CONTAINER_MEMBERSHIP_PROPERTY)))
        || (p == RDFS_SUBPROP_OF && t.object == named_term(RDFS_MEMBER));
    if !shape_ok {
        return None;
    }
    let usage = g.triples_with_pred(cm).next()?.clone();
    Some(mk_step(Owl2RlRuleId::RdfsContainer, vec![usage], t))
}

/// owl:Nothing reflexivity plus universal T(?x rdf:type owl:Thing) for subject
/// nodes). Deterministic and zero-premise; certified as axiom steps.
fn audit_owl_universe(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    if t.predicate.as_str() != RDF_TYPE {
        return None;
    }
    let thing = named_term(OWL_THING);
    let class = named_term(OWL_CLASS);
    let axiom = (nb_to_term(&t.subject) == thing && t.object == thing)
        || (nb_to_term(&t.subject) == thing && t.object == class)
        || (nb_to_term(&t.subject) == named_term(OWL_NOTHING) && t.object == thing)
        || (nb_to_term(&t.subject) == named_term(OWL_NOTHING) && t.object == class);
    // Universal T(?x rdf:type owl:Thing) is certified only when ?x occurs as a
    // subject in the authoritative graph; no free membership for object-only nodes.
    let universal = t.object == thing && g.all.iter().any(|src| src.subject == t.subject);
    if axiom || universal {
        return Some(mk_step(Owl2RlRuleId::OwlUniverse, Vec::new(), t));
    }
    None
}

/// Attempt to reconstruct one auditable rule instance for `t`.
///
/// Every applicable rule contributes a candidate. Candidates are ranked by
/// grounding (fewest non-base premises) then by fixed rule priority, with the
/// zero-premise owl-universe axiom ranked last so a real derivation is never
/// pre-empted. This prefers base-fact proofs over mutually-dependent inferred
/// chains, which would otherwise form cycles (e.g. eq-sym deriving
/// x sameAs z from z sameAs x while eq-trans has a base-grounded proof).
fn audit_step(g: &AuditGraph, t: &oxrdf::Triple) -> Option<DerivationStep> {
    let mut steps: Vec<(usize, DerivationStep)> = Vec::new();
    let mut push = |priority: usize, st: Option<DerivationStep>| {
        if let Some(s) = st {
            steps.push((priority, s));
        }
    };
    // Fixed priority order keeps reconstruction deterministic on ties.
    push(0, audit_cax_sco(g, t));
    push(1, audit_cax_eqc(g, t));
    push(2, audit_prp_dom(g, t));
    push(3, audit_prp_rng(g, t));
    push(4, audit_prp_spo1(g, t));
    push(5, audit_prp_eqp(g, t));
    push(6, audit_prp_inv(g, t));
    push(7, audit_prp_symp(g, t));
    push(8, audit_prp_trp(g, t));
    push(9, audit_prp_spo2(g, t));
    push(10, audit_cls_hv(g, t));
    push(11, audit_cls_svf(g, t));
    push(12, audit_cls_avf(g, t));
    push(13, audit_scm_eqc(g, t));
    push(14, audit_rdfs11(g, t));
    push(15, audit_eq_sym(g, t));
    push(16, audit_eq_trans(g, t));
    push(17, audit_eq_rep_s(g, t));
    push(18, audit_eq_rep_p(g, t));
    push(19, audit_eq_rep_o(g, t));
    push(20, audit_cls_int1(g, t));
    push(21, audit_cls_int2(g, t));
    push(22, audit_cls_uni(g, t));
    push(23, audit_rdfs_container(g, t));
    // Universe is axiomatically last: a genuine rule derivation wins on ties.
    push(24, audit_owl_universe(g, t));

    steps
        .into_iter()
        .min_by(|(pa, a), (pb, b)| {
            let na = a.premises.iter().filter(|p| !g.is_base(p)).count();
            let nb = b.premises.iter().filter(|p| !g.is_base(p)).count();
            na.cmp(&nb)
                .then(pa.cmp(pb))
                .then(a.conclusion.cmp(&b.conclusion))
        })
        .map(|(_, s)| s)
}

/// Detect owl:disjointWith inconsistencies (same individual typed by both
/// classes). Emitted as separate violation steps, never as inferred triples.
fn audit_disjointness_violations(g: &AuditGraph) -> Vec<DerivationStep> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for ax in g.triples_with_pred(OWL_DISJOINT_WITH) {
        let c2 = match term_to_nb(&ax.object) {
            Some(nb) => nb,
            None => continue,
        };
        let c1 = ax.subject.clone();
        let c1_t = nb_to_term(&c1);
        let c2_t = nb_to_term(&c2);
        let m1: Vec<oxrdf::Triple> = g
            .triples_with_pred(RDF_TYPE)
            .filter(|m| m.object == c1_t.clone())
            .cloned()
            .collect();
        for ma in &m1 {
            let x = &ma.subject;
            for mb in g.triples_with_pred(RDF_TYPE) {
                if mb.subject != *x || mb.object != c2_t {
                    continue;
                }
                let (lo, hi) = if nt_term(&c1_t) <= nt_term(&c2_t) {
                    (c1_t.clone(), c2_t.clone())
                } else {
                    (c2_t.clone(), c1_t.clone())
                };
                let key = format!("{}|{}|{}", nt_subject(x), nt_term(&lo), nt_term(&hi));
                if !seen.insert(key) {
                    continue;
                }
                let conclusion =
                    oxrdf::Triple::new(x.clone(), iri(RDF_TYPE), named_term(OWL_NOTHING));
                out.push(mk_step(
                    Owl2RlRuleId::CaxDwViolation,
                    vec![ax.clone(), ma.clone(), mb.clone()],
                    &conclusion,
                ));
            }
        }
    }
    out.sort_by(|a, b| {
        a.conclusion
            .cmp(&b.conclusion)
            .then(a.premises.cmp(&b.premises))
    });
    out
}

/// Topologically order derivation steps (Kahn, lexicographic tie-break) so DAG
/// order matches dependency order and the kernel continuity check is meaningful.
fn topo_order_steps(
    steps: Vec<DerivationStep>,
    base_nt: &HashSet<String>,
) -> Result<Vec<DerivationStep>, ReasoningMaterializeError> {
    let mut available: HashSet<String> = base_nt.clone();
    let mut remaining = steps;
    let mut out = Vec::with_capacity(remaining.len());
    loop {
        if remaining.is_empty() {
            return Ok(out);
        }
        let pick = remaining
            .iter()
            .enumerate()
            .filter(|(_, st)| st.premises.iter().all(|p| available.contains(p)))
            .min_by(|a, b| a.1.conclusion.cmp(&b.1.conclusion))
            .map(|(i, _)| i);
        match pick {
            Some(i) => {
                let st = remaining.remove(i);
                available.insert(st.conclusion.clone());
                out.push(st);
            }
            None => {
                return Err(ReasoningMaterializeError::ProofUnavailable {
                    triple: remaining[0].conclusion.clone(),
                    reason: "unresolvable premise dependency cycle".to_string(),
                })
            }
        }
    }
}

/// Independently verify a persisted materialization certificate.
///
/// Unlike the in-line construction check, this entry point never trusts
/// caller-supplied booleans: it recomputes the canonical digest over the
/// persisted profile + steps + violations, derives proof coverage from the
/// actual inferred triple set, independently re-derives every step's rule
/// instance (and the disjointness-violation set) over `base ∪ inferred`,
/// rebuilds kernel witnesses with genuine premise availability, and passes only
/// computed booleans to the formal kernel. A forged conclusion whose digest was
/// recomputed, a mismatched rule, tampered steps, a tampered digest, a missing
/// proof, or a broken chain are all rejected (I2).
pub fn verify_persisted_certificate(
    certificate: &DerivationCertificate,
    inferred_triples: &[oxrdf::Triple],
    base_triples: &[oxrdf::Triple],
) -> Result<(), ReasoningMaterializeError> {
    let material = canonical_certificate_material(
        certificate.profile,
        &certificate.steps,
        &certificate.disjointness_violations,
    );
    let recomputed = certificate_digest(&material);
    let digest_matches = recomputed == certificate.certificate_sha256;
    if !digest_matches {
        return Err(ReasoningMaterializeError::KernelVerificationFailed {
            reason: format!(
                "certificate digest mismatch: stored {} recomputed {}",
                certificate.certificate_sha256, recomputed
            ),
        });
    }

    // Real coverage: one certified step per inferred triple, with matching
    // canonical conclusions. No blanket `true`.
    let all_inferred_have_proof = certificate.steps.len() == inferred_triples.len()
        && inferred_triples.iter().all(|t| {
            let nt = nt_triple(t);
            certificate.steps.iter().any(|st| st.conclusion == nt)
        });

    // Independent semantic re-derivation. The kernel verifier only sees rule
    // tags, premise counts and availability booleans; those are all attacker
    // forgeable when the digest is unkeyed. Here, with the real triples in hand,
    // every certified step is reconstructed from its rule instance over
    // base ∪ inferred using the SAME deterministic audit that produced it. A
    // fabricated conclusion, a mismatched rule tag, or a premise set that does
    // not actually fire the rule cannot be reconstructed and fails closed.
    let mut audit_triples = base_triples.to_vec();
    audit_triples.extend_from_slice(inferred_triples);
    let graph = AuditGraph::new_with_base(audit_triples, base_triples.to_vec());
    for st in &certificate.steps {
        let conclusion = match inferred_triples
            .iter()
            .find(|t| nt_triple(t) == st.conclusion)
        {
            Some(t) => t,
            None => {
                return Err(ReasoningMaterializeError::KernelVerificationFailed {
                    reason: format!(
                        "step conclusion {} is not an inferred triple",
                        st.conclusion
                    ),
                })
            }
        };
        let reconstructed = audit_step(&graph, conclusion);
        let matches = reconstructed.as_ref().is_some_and(|r| {
            r.rule == st.rule
                && r.premises.iter().cloned().collect::<BTreeSet<_>>()
                    == st.premises.iter().cloned().collect::<BTreeSet<_>>()
        });
        if !matches {
            return Err(ReasoningMaterializeError::KernelVerificationFailed {
                reason: format!(
                    "step for {} is not derivable by {} from its premises",
                    st.conclusion,
                    st.rule.as_str()
                ),
            });
        }
    }
    // Disjointness violations carry the same proof obligation: re-derive the
    // authoritative violation set and require an exact match.
    let expected_violations: HashSet<_> = audit_disjointness_violations(&graph)
        .into_iter()
        .map(|v| (v.rule, v.premises, v.conclusion))
        .collect();
    let certified_violations: HashSet<_> = certificate
        .disjointness_violations
        .iter()
        .map(|v| (v.rule.clone(), v.premises.clone(), v.conclusion.clone()))
        .collect();
    if certified_violations != expected_violations {
        return Err(ReasoningMaterializeError::KernelVerificationFailed {
            reason: "disjointness violations do not match an independent re-derivation".to_string(),
        });
    }

    let profile_tag = match certificate.profile {
        ReasoningProfile::Rdfs => 0,
        ReasoningProfile::OwlRl => 1,
    };
    let base_nt: HashSet<String> = base_triples.iter().map(nt_triple).collect();
    let mut available = base_nt;
    let mut kernel_steps = Vec::with_capacity(certificate.steps.len());
    for st in &certificate.steps {
        if st.premises.len() > u8::MAX as usize {
            return Err(ReasoningMaterializeError::KernelVerificationFailed {
                reason: format!(
                    "step for {} has {} premises (kernel cap {})",
                    st.conclusion,
                    st.premises.len(),
                    u8::MAX
                ),
            });
        }
        let premises_available = st.premises.iter().all(|p| available.contains(p));
        kernel_steps.push(crate::formal_kernel::KernelCertificateStep {
            rule_tag: st.rule.to_kernel_tag(),
            premise_count: st.premises.len() as u8,
            premises_available,
        });
        available.insert(st.conclusion.clone());
    }

    let verdict = crate::formal_kernel::verify_derivation_certificate(
        profile_tag,
        digest_matches,
        &kernel_steps,
        all_inferred_have_proof,
    );
    if verdict != crate::formal_kernel::KernelCertificateVerdict::Valid {
        return Err(ReasoningMaterializeError::KernelVerificationFailed {
            reason: format!("certificate verdict: {}", verdict.as_str()),
        });
    }
    Ok(())
}

/// Base triples are fed to the reasoner in bounded batches so the
/// `max_inferred_triples` cap is enforced before an oversized closure is ever
/// materialized. After each batch the incremental output is inspected by
/// reference (no `to_vec`) for a monotonic lower bound on the final inferred
/// count; exceeding the cap aborts immediately.
const REASON_BATCH: usize = 16;

impl ReasoningAdapter {
    /// Materialize the DataFrog OWL 2 RL closure over `base_triples` and return
    /// every inferred triple together with a proof-carrying certificate.
    ///
    /// Fail-closed contract: each inferred triple must be reconstructible by
    /// deterministic audit over `base ∪ inferred`; if any single triple cannot be
    /// certified, the whole call fails and no uncertified output is returned.
    /// `max_inferred_triples` bounds the closure (default callers should pass
    /// 50_000). `profile` is recorded on the certificate for operator visibility.
    pub fn materialize_owl2_rl_with_proofs(
        &self,
        base_triples: &[oxrdf::Triple],
        namespace: &str,
        max_inferred_triples: usize,
        profile: ReasoningProfile,
    ) -> Result<MaterializationCertificate, ReasoningMaterializeError> {
        let base_nt: HashSet<String> = base_triples.iter().map(nt_triple).collect();

        // Bound CPU/allocation: load in small batches and check the cap after
        // each incremental materialization. closure(S) ⊆ closure(base) for any
        // subset S, so a non-base count above the cap here is a monotonic lower
        // bound on the final inferred count and rejection is sound.
        let mut reasoner = reasonable::reasoner::Reasoner::new();
        for batch in base_triples.chunks(REASON_BATCH) {
            reasoner.load_triples(batch.to_vec());
            reasoner.reason();
            let inferred_so_far = reasoner
                .view_output()
                .iter()
                .filter(|t| !base_nt.contains(&nt_triple(t)))
                .count();
            if inferred_so_far > max_inferred_triples {
                return Err(ReasoningMaterializeError::InferredLimitExceeded {
                    max: max_inferred_triples,
                    actual: inferred_so_far,
                });
            }
        }

        // Canonical full materialization regardless of incremental internals.
        // Reachable only with inferred_so_far ≤ cap, so this pass is bounded.
        // Enforce the cap once more on the borrowed view BEFORE copying the
        // closure or allocating any proof/nquad vectors (I2, I6).
        reasoner.reason_full();
        let inferred_len = reasoner
            .view_output()
            .iter()
            .filter(|t| !base_nt.contains(&nt_triple(t)))
            .count();
        if inferred_len > max_inferred_triples {
            return Err(ReasoningMaterializeError::InferredLimitExceeded {
                max: max_inferred_triples,
                actual: inferred_len,
            });
        }

        // Deterministic inferred set: sorted by canonical N-Triple rendering.
        let full = reasoner.view_output().to_vec();
        let mut inferred: Vec<oxrdf::Triple> = full
            .into_iter()
            .filter(|t| !base_nt.contains(&nt_triple(t)))
            .collect();
        inferred.sort_by(|a, b| nt_triple(a).cmp(&nt_triple(b)));
        inferred.dedup();

        let mut audit_graph_triples = base_triples.to_vec();
        audit_graph_triples.extend(inferred.iter().cloned());
        let graph = AuditGraph::new_with_base(audit_graph_triples, base_triples.to_vec());

        // Deterministic reconstruction audit for every inferred triple.
        let mut audited: Vec<DerivationStep> = Vec::with_capacity(inferred.len());
        for t in &inferred {
            match audit_step(&graph, t) {
                Some(st) => audited.push(st),
                None => {
                    return Err(ReasoningMaterializeError::ProofUnavailable {
                        triple: nt_triple(t),
                        reason: "no supported OWL 2 RL rule reconstructs this entailment"
                            .to_string(),
                    })
                }
            }
        }
        let violations = audit_disjointness_violations(&graph);
        let steps = topo_order_steps(audited, &base_nt)?;

        // Build the digest from canonical material, then verify the assembled
        // certificate through the independent entry point. No constant kernel
        // booleans and no u64 self-comparison of the same digest (I2).
        let material = canonical_certificate_material(profile, &steps, &violations);
        let certificate_sha256 = certificate_digest(&material);
        let certificate = DerivationCertificate {
            namespace: namespace.to_string(),
            profile,
            base_triple_count: base_triples.len(),
            inferred_triple_count: inferred.len(),
            steps,
            disjointness_violations: violations,
            certificate_sha256,
        };
        verify_persisted_certificate(&certificate, &inferred, base_triples)?;

        // Named-graph isolation and RDF-star provenance are emitted only after
        // the certificate has been independently verified (I6).
        let inference_graph = inference_graph_iri(namespace);
        let inferred_nquads: Vec<String> = certificate
            .steps
            .iter()
            .map(|st| {
                let core = st.conclusion.strip_suffix(" .").unwrap_or(&st.conclusion);
                format!("{core} <{inference_graph}> .")
            })
            .collect();
        let mut rdf_star_annotations = Vec::with_capacity(certificate.steps.len() * 2);
        for st in &certificate.steps {
            let core = st.conclusion.strip_suffix(" .").unwrap_or(&st.conclusion);
            rdf_star_annotations.push(format!(
                "<< {core} >> <https://neurostrata.io/ns/derivedByRule> \"{}\" .",
                st.rule.as_str()
            ));
            rdf_star_annotations.push(format!(
                "<< {core} >> <https://neurostrata.io/ns/certificateHash> \"{}\" .",
                certificate.certificate_sha256
            ));
        }

        Ok(MaterializationCertificate {
            inferred_triples: inferred,
            certificate,
            inference_graph,
            inferred_nquads,
            rdf_star_annotations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_adapter_expands_and_executes_within_budget() {
        let idx = OntologyIndex::from_registry_metadata(&json!({
            "domain_registry": {
                "domains": [
                    {"slug": "auth", "aliases": ["authn"]},
                    {"slug": "tls", "parent": "auth", "aliases": ["ssl"]}
                ]
            }
        }));
        let adapter = ReasoningAdapter::new_with(ReasoningConfig::default());
        assert!(adapter.expand_domain(&idx, "auth").contains(&"tls".to_string()));
        assert!(adapter.expand_identifiers(&idx, "ssl").contains(&"tls".to_string()));
        assert!(!adapter.expand_relation(&idx, "GOVERNS").is_empty());
        let results = adapter.execute(&idx, "auth", "ssl", &["tls::src/tls.rs".to_string()]);
        assert!(results.iter().any(|r| r.id == "src/tls.rs"));
    }

    #[test]
    fn reasoning_adapter_hydrates_registered_payloads() {
        let idx = OntologyIndex::from_registry_metadata(&json!({
            "domain_registry": {
                "domains": [
                    {"slug": "auth", "aliases": ["authn"]},
                    {"slug": "tls", "parent": "auth", "aliases": ["ssl"]}
                ]
            }
        }));
        let mut payloads: HashMap<String, MemoryPayload> = HashMap::new();
        payloads.insert(
            "src/tls.rs".to_string(),
            MemoryPayload {
                content: "TLS terminates at src/tls.rs".to_string(),
                user_id: "agent:bosun".to_string(),
                memory_type: "rule".to_string(),
                agent_name: Some("bosun".to_string()),
                location: "src/tls.rs".to_string(),
                location_lines: "1-10".to_string(),
                metadata: json!({"domain": "tls"}),
            },
        );
        let adapter = ReasoningAdapter::new_with(ReasoningConfig::default())
            .with_payloads(payloads);

        let results = adapter.execute(&idx, "auth", "ssl", &["tls::src/tls.rs".to_string()]);
        let hit = results
            .iter()
            .find(|r| r.id == "src/tls.rs")
            .expect("governs closure result must be present");
        assert_eq!(hit.payload.content, "TLS terminates at src/tls.rs");
        assert_eq!(hit.payload.memory_type, "rule");
        assert_eq!(hit.payload.user_id, "agent:bosun");

        // Unregistered ids still fall back to the empty stub.
        let miss = results
            .iter()
            .find(|r| r.id != "src/tls.rs")
            .expect("expanded query result must be present");
        assert_eq!(miss.payload.content, "");
        assert_eq!(miss.payload.memory_type, "fact");
    }

    fn ex_tb(s: &str, p: &str, o: &str) -> oxrdf::Triple {
        oxrdf::Triple::new(iri(s), iri(p), named_term(o))
    }

    const EX_SCO: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    const EX_INV: &str = "http://www.w3.org/2002/07/owl#inverseOf";
    const EX_CHAIN: &str = "http://www.w3.org/2002/07/owl#propertyChainAxiom";
    const EX_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
    const EX_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
    const EX_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
    const EX_TRP: &str = "http://www.w3.org/2002/07/owl#TransitiveProperty";
    const EX_DW: &str = "http://www.w3.org/2002/07/owl#disjointWith";

    fn all_inferred_are_certified(out: &MaterializationCertificate) {
        let certified: HashSet<&str> = out
            .certificate
            .steps
            .iter()
            .map(|s| s.conclusion.as_str())
            .collect();
        for t in &out.inferred_triples {
            assert!(
                certified.contains(nt_triple(t).as_str()),
                "inferred triple {} must carry a certified derivation",
                nt_triple(t)
            );
        }
        assert_eq!(
            out.certificate.steps.len(),
            out.inferred_triples.len(),
            "exactly one derivation step per inferred triple"
        );
    }

    #[test]
    fn materialize_owl2_rl_certifies_subclass_entailment() {
        let base = vec![
            ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
        ];
        let adapter = ReasoningAdapter::new();
        let out = adapter
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("materialization must certify");
        let want = "<http://ex.org/x> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/B> .";
        let step = out
            .certificate
            .steps
            .iter()
            .find(|s| s.conclusion == want)
            .expect("cax-sco derivation must be present");
        assert_eq!(step.rule, Owl2RlRuleId::CaxSco);
        assert_eq!(step.premises.len(), 2);
        assert_eq!(out.certificate.base_triple_count, 2);
        assert_eq!(out.certificate.profile, ReasoningProfile::OwlRl);
        assert_eq!(out.certificate.certificate_sha256.len(), 64);
        all_inferred_are_certified(&out);
    }

    #[test]
    fn materialize_owl2_rl_transitive_property() {
        let base = vec![
            ex_tb("http://ex.org/p", RDF_TYPE, EX_TRP),
            ex_tb("http://ex.org/x", "http://ex.org/p", "http://ex.org/y"),
            ex_tb("http://ex.org/y", "http://ex.org/p", "http://ex.org/z"),
        ];
        let adapter = ReasoningAdapter::new();
        let out = adapter
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("materialization must certify");
        let want = "<http://ex.org/x> <http://ex.org/p> <http://ex.org/z> .";
        let step = out
            .certificate
            .steps
            .iter()
            .find(|s| s.conclusion == want)
            .expect("prp-trp derivation must be present");
        assert_eq!(step.rule, Owl2RlRuleId::PrpTrp);
        assert_eq!(step.premises.len(), 3);
        all_inferred_are_certified(&out);
    }

    #[test]
    fn materialize_owl2_rl_inverse_property() {
        let base = vec![
            ex_tb("http://ex.org/p", EX_INV, "http://ex.org/q"),
            ex_tb("http://ex.org/x", "http://ex.org/p", "http://ex.org/y"),
        ];
        let adapter = ReasoningAdapter::new();
        let out = adapter
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("materialization must certify");
        let want = "<http://ex.org/y> <http://ex.org/q> <http://ex.org/x> .";
        let step = out
            .certificate
            .steps
            .iter()
            .find(|s| s.conclusion == want)
            .expect("prp-inv derivation must be present");
        assert_eq!(step.rule, Owl2RlRuleId::PrpInv);
        all_inferred_are_certified(&out);
    }

    #[test]
    fn materialize_owl2_rl_property_chain() {
        let base = vec![
            ex_tb("http://ex.org/p", EX_CHAIN, "http://ex.org/l1"),
            ex_tb("http://ex.org/l1", EX_FIRST, "http://ex.org/p1"),
            ex_tb("http://ex.org/l1", EX_REST, "http://ex.org/l2"),
            ex_tb("http://ex.org/l2", EX_FIRST, "http://ex.org/p2"),
            ex_tb("http://ex.org/l2", EX_REST, EX_NIL),
            ex_tb("http://ex.org/s", "http://ex.org/p1", "http://ex.org/m"),
            ex_tb("http://ex.org/m", "http://ex.org/p2", "http://ex.org/o"),
        ];
        let adapter = ReasoningAdapter::new();
        let out = adapter
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("materialization must certify");
        let want = "<http://ex.org/s> <http://ex.org/p> <http://ex.org/o> .";
        let step = out
            .certificate
            .steps
            .iter()
            .find(|s| s.conclusion == want)
            .expect("prp-spo2 derivation must be present");
        assert_eq!(step.rule, Owl2RlRuleId::PrpSpo2);
        assert!(step.premises.len() >= 3);
        all_inferred_are_certified(&out);
    }

    #[test]
    fn materialize_owl2_rl_disjoint_violation_reported() {
        let base = vec![
            ex_tb("http://ex.org/C1", EX_DW, "http://ex.org/C2"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/C1"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/C2"),
        ];
        let adapter = ReasoningAdapter::new();
        let out = adapter
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("disjoint classes are an inconsistency report, not a rejection");
        assert_eq!(out.certificate.disjointness_violations.len(), 1);
        let v = &out.certificate.disjointness_violations[0];
        assert_eq!(v.rule, Owl2RlRuleId::CaxDwViolation);
        assert_eq!(v.premises.len(), 3);
        all_inferred_are_certified(&out);
    }

    #[test]
    fn materialize_owl2_rl_enforces_inferred_limit() {
        let base = vec![
            ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
        ];
        let adapter = ReasoningAdapter::new();
        let err = adapter
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 0, ReasoningProfile::OwlRl)
            .expect_err("limit 0 must fail closed");
        match err {
            ReasoningMaterializeError::InferredLimitExceeded { max, .. } => assert_eq!(max, 0),
            other => panic!("expected InferredLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn materialize_owl2_rl_digest_is_deterministic() {
        let base = vec![
            ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
        ];
        let adapter = ReasoningAdapter::new();
        let run = || {
            adapter
                .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
                .expect("materialization must certify")
                .certificate
                .certificate_sha256
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn materialize_owl2_rl_profile_is_recorded() {
        let base = vec![ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A")];
        let adapter = ReasoningAdapter::new();
        let out = adapter
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::Rdfs)
            .expect("materialization must certify");
        assert_eq!(out.certificate.profile, ReasoningProfile::Rdfs);
        assert_eq!(out.certificate.profile.as_str(), "rdfs");
    }

    // ---- independent-review remediation (C1-C4, I1-I10) ----

    #[test]
    fn audit_universe_is_last_and_specific_rule_wins() {
        // (A subClassOf owl:Thing), (x type A) entail (x type owl:Thing) via
        // cax-sco; the zero-premise universe axiom must not pre-empt it.
        let all = vec![
            ex_tb("http://ex.org/A", EX_SCO, OWL_THING),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
            ex_tb("http://ex.org/x", RDF_TYPE, OWL_THING),
        ];
        let g = AuditGraph::new(all);
        let t = &g.all[2];
        let st = audit_step(&g, t).expect("entailment must be auditable");
        assert_eq!(st.rule, Owl2RlRuleId::CaxSco);
    }

    #[test]
    fn universe_has_no_free_membership_for_object_only_node() {
        let g = AuditGraph::new(vec![ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B")]);
        // B occurs only as object: no universal T(B type owl:Thing) proof.
        let object_only = ex_tb("http://ex.org/B", RDF_TYPE, OWL_THING);
        assert!(audit_owl_universe(&g, &object_only).is_none());
        // A occurs as subject: universal membership is certified.
        let as_subject = ex_tb("http://ex.org/A", RDF_TYPE, OWL_THING);
        assert!(audit_owl_universe(&g, &as_subject).is_some());
    }

    #[test]
    fn walk_list_handles_more_than_16_cells_and_detects_cycles() {
        let mut all = Vec::new();
        let n = 20usize;
        for i in 0..n {
            let cell = format!("http://ex.org/l{i}");
            let next = if i + 1 == n {
                RDF_NIL.to_string()
            } else {
                format!("http://ex.org/l{}", i + 1)
            };
            all.push(ex_tb(&cell, EX_FIRST, &format!("http://ex.org/m{i}")));
            all.push(ex_tb(&cell, EX_REST, &next));
        }
        let g = AuditGraph::new(all);
        let (members, cells) = walk_list(&g, &named_term("http://ex.org/l0"))
            .expect("20-cell list must be walked");
        assert_eq!(members.len(), n);
        assert_eq!(cells.len(), 2 * n);

        let cyclic = AuditGraph::new(vec![
            ex_tb("http://ex.org/c0", EX_FIRST, "http://ex.org/m0"),
            ex_tb("http://ex.org/c0", EX_REST, "http://ex.org/c0"),
        ]);
        assert!(walk_list(&cyclic, &named_term("http://ex.org/c0")).is_none());
    }

    #[test]
    fn rdfs12_container_axioms_are_certified() {
        let cm = "http://www.w3.org/1999/02/22-rdf-syntax-ns#_1";
        let base = vec![ex_tb("http://ex.org/x", cm, "http://ex.org/y")];
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("container closure must certify");
        all_inferred_are_certified(&out);
        let property = out.inferred_triples.iter().any(|t| {
            nb_iri(&t.subject) == cm && t.predicate.as_str() == RDF_TYPE && t.object == named_term(RDF_PROPERTY)
        });
        assert!(property, "rdf:_n rdf:type rdf:Property must be inferred");
        assert!(out
            .certificate
            .steps
            .iter()
            .any(|st| st.rule == Owl2RlRuleId::RdfsContainer));
    }

    #[test]
    fn cls_com_is_diagnostic_only_no_emitted_triples() {
        let base = vec![
            ex_tb("http://ex.org/A", "http://www.w3.org/2002/07/owl#complementOf", "http://ex.org/B"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
        ];
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("materialization must certify");
        let contradiction = ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/B");
        assert!(!out.inferred_triples.contains(&contradiction));
        assert!(out.certificate.disjointness_violations.is_empty());
    }

    #[test]
    fn named_graph_and_rdf_star_emitted_after_verification() {
        let base = vec![ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B")];
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "team/alpha", 50_000, ReasoningProfile::OwlRl)
            .expect("materialization must certify");
        let graph = inference_graph_iri("team/alpha");
        assert_eq!(out.inference_graph, graph);
        assert_eq!(out.inferred_nquads.len(), out.inferred_triples.len());
        for q in &out.inferred_nquads {
            assert!(q.ends_with(&format!("<{graph}> .")));
        }
        assert_eq!(out.rdf_star_annotations.len(), out.certificate.steps.len() * 2);
        assert!(out.rdf_star_annotations.iter().any(|a| a.contains("derivedByRule")));
        assert!(out.rdf_star_annotations.iter().any(|a| a.contains("certificateHash")));
        assert!(out.rdf_star_annotations.iter().any(|a| a.contains(&out.certificate.certificate_sha256)));
    }

    #[test]
    fn certificate_over_64_steps_is_fully_verified() {
        let p = "http://ex.org/p";
        let mut base = vec![ex_tb(p, RDF_TYPE, EX_TRP)];
        for i in 0..13 {
            base.push(ex_tb(&format!("http://ex.org/n{i}"), p, &format!("http://ex.org/n{}", i + 1)));
        }
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("large closure must certify");
        assert!(out.certificate.steps.len() > 64);
        all_inferred_are_certified(&out);
    }

    #[test]
    fn rdfs_profile_still_materializes_full_owl_rl() {
        let p = "http://ex.org/q";
        let base = vec![
            ex_tb(p, RDF_TYPE, EX_TRP),
            ex_tb("http://ex.org/a", p, "http://ex.org/b"),
            ex_tb("http://ex.org/b", p, "http://ex.org/c"),
            ex_tb("http://ex.org/c", p, "http://ex.org/d"),
        ];
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::Rdfs)
            .expect("rdfs-labeled call must still materialize full owl-rl");
        assert!(out.inferred_triples.contains(&ex_tb("http://ex.org/a", p, "http://ex.org/d")));
        let summary = out.materialization_summary();
        assert!(summary.contains("materialization=owl-rl"));
        assert!(summary.contains("profile_restricts_closure=false"));
    }

    // ---- I2: independent persisted-certificate verification (RED-first) ----

    fn persisted_fixture() -> (ReasoningProfile, Vec<oxrdf::Triple>, MaterializationCertificate) {
        let profile = ReasoningProfile::OwlRl;
        let base = vec![
            ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
        ];
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, profile)
            .expect("valid certificate required for tamper fixtures");
        (profile, base, out)
    }

    #[test]
    fn verify_persisted_certificate_accepts_valid_certificate() {
        let (_profile, base, out) = persisted_fixture();
        verify_persisted_certificate(&out.certificate, &out.inferred_triples, &base)
            .expect("valid persisted certificate must verify");
    }

    #[test]
    fn verify_persisted_certificate_rejects_tampered_step() {
        let (_profile, base, mut out) = persisted_fixture();
        out.certificate.steps[0].rule = Owl2RlRuleId::PrpDom;
        assert!(verify_persisted_certificate(&out.certificate, &out.inferred_triples, &base).is_err());
    }

    #[test]
    fn verify_persisted_certificate_rejects_tampered_digest() {
        let (_profile, base, mut out) = persisted_fixture();
        out.certificate.certificate_sha256 = "00".repeat(32);
        assert!(verify_persisted_certificate(&out.certificate, &out.inferred_triples, &base).is_err());
    }

    #[test]
    fn verify_persisted_certificate_rejects_missing_proof() {
        let (profile, base, mut out) = persisted_fixture();
        out.certificate.steps.pop();
        out.certificate.certificate_sha256 = certificate_digest(&canonical_certificate_material(
            profile,
            &out.certificate.steps,
            &out.certificate.disjointness_violations,
        ));
        assert!(verify_persisted_certificate(&out.certificate, &out.inferred_triples, &base).is_err());
    }

    #[test]
    fn verify_persisted_certificate_rejects_reordered_step() {
        // Transitive chain where a later step consumes an earlier inferred
        // conclusion; reversing the DAG must break premise availability.
        let profile = ReasoningProfile::OwlRl;
        let p = "http://ex.org/tp";
        let base = vec![
            ex_tb(p, RDF_TYPE, EX_TRP),
            ex_tb("http://ex.org/a", p, "http://ex.org/b"),
            ex_tb("http://ex.org/b", p, "http://ex.org/c"),
            ex_tb("http://ex.org/c", p, "http://ex.org/d"),
        ];
        let mut out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, profile)
            .expect("valid certificate required");
        assert!(out.certificate.steps.len() >= 3);
        out.certificate.steps.reverse();
        out.certificate.certificate_sha256 = certificate_digest(&canonical_certificate_material(
            profile,
            &out.certificate.steps,
            &out.certificate.disjointness_violations,
        ));
        assert!(verify_persisted_certificate(&out.certificate, &out.inferred_triples, &base).is_err());
    }

    #[test]
    fn verify_persisted_certificate_rejects_fabricated_conclusion_with_valid_digest() {
        // P1: an attacker swaps one step's conclusion for an arbitrary triple
        // and recomputes the UNKEYED SHA-256 over the canonical material. Rule
        // tag / premise count / availability / coverage / digest all still pass
        // on the current verifier, so only an independent re-derivation catches
        // it.
        let (profile, base, mut out) = persisted_fixture();
        let idx = out
            .certificate
            .steps
            .iter()
            .position(|s| s.rule == Owl2RlRuleId::CaxSco)
            .expect("fixture must contain a cax-sco step");
        let fake = ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/FAKE");
        let fake_nt = nt_triple(&fake);
        let old_nt = out.certificate.steps[idx].conclusion.clone();
        out.certificate.steps[idx].conclusion = fake_nt;
        let iidx = out
            .inferred_triples
            .iter()
            .position(|t| nt_triple(t) == old_nt)
            .expect("inferred triple for cax-sco step must exist");
        out.inferred_triples[iidx] = fake;
        out.certificate.certificate_sha256 = certificate_digest(&canonical_certificate_material(
            profile,
            &out.certificate.steps,
            &out.certificate.disjointness_violations,
        ));
        assert!(
            verify_persisted_certificate(&out.certificate, &out.inferred_triples, &base).is_err(),
            "fabricated conclusion with a recomputed unkeyed digest must be rejected"
        );
    }

    #[test]
    fn verify_persisted_certificate_rejects_wrong_rule_with_valid_digest() {
        // P1: genuine premise set + genuine conclusion, but the rule tag is a
        // different (valid) OWL-RL rule whose premise pattern does not match.
        let (profile, base, mut out) = persisted_fixture();
        let idx = out
            .certificate
            .steps
            .iter()
            .position(|s| s.rule == Owl2RlRuleId::CaxSco)
            .expect("fixture must contain a cax-sco step");
        out.certificate.steps[idx].rule = Owl2RlRuleId::PrpDom;
        out.certificate.certificate_sha256 = certificate_digest(&canonical_certificate_material(
            profile,
            &out.certificate.steps,
            &out.certificate.disjointness_violations,
        ));
        assert!(
            verify_persisted_certificate(&out.certificate, &out.inferred_triples, &base).is_err(),
            "a step whose rule does not entail its conclusion must be rejected"
        );
    }

    #[test]
    fn inferred_limit_aborts_before_full_closure() {
        // P2: with a large cap the transitive-property fixture yields a large
        // inferred closure. A tiny cap must abort materialization early instead
        // of first building/copying that entire closure; the reported `actual`
        // must therefore be far below the full closure size.
        let p = "http://ex.org/tp";
        let mut base = vec![ex_tb(p, RDF_TYPE, EX_TRP)];
        for i in 0..40 {
            base.push(ex_tb(
                &format!("http://ex.org/n{i}"),
                p,
                &format!("http://ex.org/n{}", i + 1),
            ));
        }
        let full = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("full closure must certify");
        let full_count = full.certificate.inferred_triple_count;
        assert!(
            full_count > 500,
            "fixture must produce a large closure, got {full_count}"
        );

        match ReasoningAdapter::new().materialize_owl2_rl_with_proofs(
            &base,
            "test/ns",
            8,
            ReasoningProfile::OwlRl,
        ) {
            Err(ReasoningMaterializeError::InferredLimitExceeded { max, actual }) => {
                assert_eq!(max, 8);
                assert!(
                    actual < full_count,
                    "tiny cap must abort before building the full closure ({actual} vs {full_count})"
                );
            }
            other => panic!("expected InferredLimitExceeded, got {other:?}"),
        }
    }

    // ---- I4: exact reasonable 0.5.0-a1 supported-behavior matrix ----
    // These RDFS/OWL rules emit NO characteristic entailment in this engine;
    // only the universe axioms + universal owl:Thing typing are produced, and
    // those are certified by audit_owl_universe. Tests pin the real behavior
    // so no caller mistakes a no-op for a silently filtered normal path.
    fn assert_no_characteristic_entailment(
        name: &str,
        base: Vec<oxrdf::Triple>,
        forbidden: Vec<oxrdf::Triple>,
    ) {
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .unwrap_or_else(|e| panic!("{name}: materialization must certify: {e}"));
        all_inferred_are_certified(&out);
        for f in forbidden {
            assert!(
                !out.inferred_triples.contains(&f),
                "{name}: unexpected entailment {}",
                nt_triple(&f)
            );
        }
    }

    #[test]
    fn i4_cls_com_is_diagnostic_only() {
        assert_no_characteristic_entailment(
            "cls-com",
            vec![
                ex_tb("http://ex.org/A", "http://www.w3.org/2002/07/owl#complementOf", "http://ex.org/B"),
                ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
            ],
            vec![ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/B")],
        );
    }

    #[test]
    fn i4_rdfs1_arbitrary_predicate_emits_no_property_typing() {
        assert_no_characteristic_entailment(
            "rdfs1",
            vec![ex_tb("http://ex.org/x", "http://ex.org/p", "http://ex.org/y")],
            vec![ex_tb("http://ex.org/p", RDF_TYPE, RDF_PROPERTY)],
        );
    }

    #[test]
    fn i4_rdfs4a4b_emit_no_resource_typing() {
        let resource = "http://www.w3.org/2000/01/rdf-schema#Resource";
        assert_no_characteristic_entailment(
            "rdfs4a/rdfs4b",
            vec![ex_tb("http://ex.org/x", "http://ex.org/p", "http://ex.org/y")],
            vec![
                ex_tb("http://ex.org/x", RDF_TYPE, resource),
                ex_tb("http://ex.org/y", RDF_TYPE, resource),
            ],
        );
    }

    #[test]
    fn i4_rdfs5_emits_no_subproperty_transitivity() {
        assert_no_characteristic_entailment(
            "rdfs5",
            vec![
                ex_tb("http://ex.org/p1", RDFS_SUBPROP_OF, "http://ex.org/p2"),
                ex_tb("http://ex.org/p2", RDFS_SUBPROP_OF, "http://ex.org/p3"),
            ],
            vec![ex_tb("http://ex.org/p1", RDFS_SUBPROP_OF, "http://ex.org/p3")],
        );
    }

    #[test]
    fn i4_rdfs6_emits_no_property_typing() {
        assert_no_characteristic_entailment(
            "rdfs6",
            vec![ex_tb("http://ex.org/p1", RDFS_SUBPROP_OF, "http://ex.org/p2")],
            vec![ex_tb("http://ex.org/p1", RDF_TYPE, RDF_PROPERTY)],
        );
    }

    #[test]
    fn i4_rdfs8_10_emit_no_class_typing() {
        let class = "http://www.w3.org/2000/01/rdf-schema#Class";
        assert_no_characteristic_entailment(
            "rdfs8/rdfs10",
            vec![ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B")],
            vec![
                ex_tb("http://ex.org/A", RDF_TYPE, class),
                ex_tb("http://ex.org/B", RDF_TYPE, class),
            ],
        );
    }

    // ---- I5: per-extension end-to-end certification ----
    fn assert_certified_rule(base: &[oxrdf::Triple], triple: &oxrdf::Triple, rule: Owl2RlRuleId) {
        let nt = nt_triple(triple);
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .unwrap_or_else(|e| panic!("materialization must certify: {e}"));
        all_inferred_are_certified(&out);
        let found = out
            .certificate
            .steps
            .iter()
            .find(|st| st.conclusion == nt)
            .unwrap_or_else(|| panic!("missing inferred triple {nt}"));
        assert_eq!(found.rule, rule, "wrong rule for {nt}");
    }

    fn list_triples(list: &str, members: &[&str]) -> Vec<oxrdf::Triple> {
        let mut out = Vec::new();
        for (i, m) in members.iter().enumerate() {
            let cell = format!("{list}_c{i}");
            let next = if i + 1 == members.len() {
                RDF_NIL.to_string()
            } else {
                format!("{list}_c{}", i + 1)
            };
            out.push(ex_tb(&cell, EX_FIRST, m));
            out.push(ex_tb(&cell, EX_REST, &next));
        }
        out
    }

    #[test]
    fn i5_scm_eqc_certifies_bidirectional_subclass() {
        let base = vec![ex_tb("http://ex.org/A", OWL_EQUIV_CLASS, "http://ex.org/B")];
        assert_certified_rule(&base, &ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B"), Owl2RlRuleId::ScmEqc);
        assert_certified_rule(&base, &ex_tb("http://ex.org/B", EX_SCO, "http://ex.org/A"), Owl2RlRuleId::ScmEqc);
    }

    #[test]
    fn i5_eq_sym_certifies_reversed_same_as() {
        let base = vec![ex_tb("http://ex.org/x", OWL_SAME_AS, "http://ex.org/y")];
        assert_certified_rule(&base, &ex_tb("http://ex.org/y", OWL_SAME_AS, "http://ex.org/x"), Owl2RlRuleId::EqSym);
    }

    #[test]
    fn i5_eq_trans_certifies_composed_same_as() {
        let base = vec![
            ex_tb("http://ex.org/x", OWL_SAME_AS, "http://ex.org/y"),
            ex_tb("http://ex.org/y", OWL_SAME_AS, "http://ex.org/z"),
        ];
        assert_certified_rule(&base, &ex_tb("http://ex.org/x", OWL_SAME_AS, "http://ex.org/z"), Owl2RlRuleId::EqTrans);
    }

    #[test]
    fn i5_eq_rep_s_certifies_subject_substitution() {
        let base = vec![
            ex_tb("http://ex.org/x", OWL_SAME_AS, "http://ex.org/y"),
            ex_tb("http://ex.org/x", "http://ex.org/p", "http://ex.org/o"),
        ];
        assert_certified_rule(&base, &ex_tb("http://ex.org/y", "http://ex.org/p", "http://ex.org/o"), Owl2RlRuleId::EqRepS);
    }

    #[test]
    fn i5_eq_rep_p_certifies_predicate_substitution() {
        let base = vec![
            ex_tb("http://ex.org/p1", OWL_SAME_AS, "http://ex.org/p2"),
            ex_tb("http://ex.org/s", "http://ex.org/p1", "http://ex.org/o"),
        ];
        assert_certified_rule(&base, &ex_tb("http://ex.org/s", "http://ex.org/p2", "http://ex.org/o"), Owl2RlRuleId::EqRepP);
    }

    #[test]
    fn i5_eq_rep_o_certifies_object_substitution() {
        let base = vec![
            ex_tb("http://ex.org/o1", OWL_SAME_AS, "http://ex.org/o2"),
            ex_tb("http://ex.org/s", "http://ex.org/p", "http://ex.org/o1"),
        ];
        assert_certified_rule(&base, &ex_tb("http://ex.org/s", "http://ex.org/p", "http://ex.org/o2"), Owl2RlRuleId::EqRepO);
    }

    #[test]
    fn i5_rdfs_sco_trans_certifies_transitive_subclass() {
        let base = vec![
            ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/B"),
            ex_tb("http://ex.org/B", EX_SCO, "http://ex.org/C"),
            ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"),
        ];
        assert_certified_rule(&base, &ex_tb("http://ex.org/A", EX_SCO, "http://ex.org/C"), Owl2RlRuleId::RdfsScoTrans);
    }

    #[test]
    fn i5_cls_int1_certifies_intersection_membership() {
        let mut base = list_triples("http://ex.org/L", &["http://ex.org/A", "http://ex.org/B"]);
        base.push(ex_tb("http://ex.org/I", OWL_INTERSECTION_OF, "http://ex.org/L_c0"));
        base.push(ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"));
        base.push(ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/B"));
        assert_certified_rule(&base, &ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/I"), Owl2RlRuleId::ClsInt1);
    }

    #[test]
    fn i5_cls_int2_certifies_intersection_decomposition() {
        let mut base = list_triples("http://ex.org/L", &["http://ex.org/A", "http://ex.org/B"]);
        base.push(ex_tb("http://ex.org/I", OWL_INTERSECTION_OF, "http://ex.org/L_c0"));
        base.push(ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/I"));
        assert_certified_rule(&base, &ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"), Owl2RlRuleId::ClsInt2);
        assert_certified_rule(&base, &ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/B"), Owl2RlRuleId::ClsInt2);
    }

    #[test]
    fn i5_cls_uni_certifies_union_membership() {
        let mut base = list_triples("http://ex.org/L", &["http://ex.org/A", "http://ex.org/B"]);
        base.push(ex_tb("http://ex.org/U", OWL_UNION_OF, "http://ex.org/L_c0"));
        base.push(ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"));
        assert_certified_rule(&base, &ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/U"), Owl2RlRuleId::ClsUni);
    }

    #[test]
    fn i5_owl_universe_certifies_universal_membership() {
        let base = vec![ex_tb("http://ex.org/x", "http://ex.org/p", "http://ex.org/y")];
        assert_certified_rule(&base, &ex_tb("http://ex.org/x", RDF_TYPE, OWL_THING), Owl2RlRuleId::OwlUniverse);
    }

    #[test]
    fn i5_cls_int1_without_all_members_entails_nothing() {
        let mut base = list_triples("http://ex.org/L", &["http://ex.org/A", "http://ex.org/B"]);
        base.push(ex_tb("http://ex.org/I", OWL_INTERSECTION_OF, "http://ex.org/L_c0"));
        base.push(ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/A"));
        let out = ReasoningAdapter::new()
            .materialize_owl2_rl_with_proofs(&base, "test/ns", 50_000, ReasoningProfile::OwlRl)
            .expect("partial intersection still certifies its emitted closure");
        assert!(!out.inferred_triples.contains(&ex_tb("http://ex.org/x", RDF_TYPE, "http://ex.org/I")));
    }
}
