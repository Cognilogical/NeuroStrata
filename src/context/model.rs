//! Canonical DTOs for the context read-planning boundary.
//!
//! These types are the contract between the context service, the MCP
//! handlers, and the host adapters. Nothing here reads the store, ranks, or
//! renders: a plan is built once and rendered from the same plan, so the
//! adapter and the server can never disagree about what was planned.
//!
//! Two rules shape the types:
//!
//! * **A closed state vocabulary.** The freshness dimensions and the
//!   degradation state are enums, not free-form strings. The spec requires
//!   `not_applicable` to stay distinct from `unknown`, and requires an
//!   unsupported backend to be reported as *unsupported* rather than as an
//!   empty result. A string would let both claims collapse silently.
//! * **Honest defaults.** `Degradation::Supported` is never the default of a
//!   plan; a plan that was built without a graph-read capability must say so
//!   (`Unavailable`), and an item that carries no computed freshness omits
//!   the field instead of claiming one.

use serde::{Deserialize, Serialize};

/// The schema version stamped on every [`ContextPlan`].
///
/// A consumer that does not recognize this version must refuse the plan
/// rather than guess at its fields.
pub const CONTEXT_PLAN_SCHEMA_VERSION: &str = "context-plan/v1";

/// A closed vocabulary of state tokens.
///
/// Every dimension gets the same shape: a canonical token, `as_str`,
/// `Display`, a tolerant `parse`, and comparison against a string literal, so
/// a call site can write `freshness.source_currency == "changed"` without
/// giving up the type check on the way in.
macro_rules! state_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $( $variant:ident => $token:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $( $variant, )+
        }

        impl $name {
            /// Every state of this dimension, in declaration order.
            pub const ALL: &'static [$name] = &[ $( $name::$variant, )+ ];

            /// The canonical token for this state.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $( $name::$variant => $token, )+
                }
            }

            /// Parse a canonical token.
            ///
            /// The comparison ignores case and surrounding whitespace, so a
            /// value that arrived over the wire is still recognized. An
            /// unrecognized token resolves to `None` rather than to a
            /// default state: a typo must not read as `unknown`.
            pub fn parse(value: &str) -> Option<Self> {
                let value = value.trim();
                $(
                    if value.eq_ignore_ascii_case($token) {
                        return Some($name::$variant);
                    }
                )+
                None
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
                    .ok_or_else(|| format!("unknown {}: `{}`", stringify!($name), value))
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.as_str() == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }
    };
}

state_enum! {
    /// How current a memory is with respect to the source it governs.
    ///
    /// `NotApplicable` means the memory governs no path and therefore has no
    /// source to be stale against. That is a different claim from `Unknown`,
    /// which means the comparison could not be made.
    SourceCurrency {
        Unchanged => "unchanged",
        Changed => "changed",
        Missing => "missing",
        Unknown => "unknown",
        NotApplicable => "not_applicable",
    }
}

state_enum! {
    /// Whether a memory's meaning was checked against its evidence.
    ///
    /// A matching file hash refreshes the anchor; it never verifies meaning.
    SemanticValidation {
        Unreviewed => "unreviewed",
        Verified => "verified",
        Contradicted => "contradicted",
    }
}

state_enum! {
    /// How much of the governed scope the active index generation covers.
    Coverage {
        Complete => "complete",
        Partial => "partial",
        Unavailable => "unavailable",
    }
}

state_enum! {
    /// Where a recorded decision sits in its lifecycle.
    ///
    /// Independent of source currency: a decision does not go stale because
    /// the code it touched moved.
    DecisionLifecycle {
        Proposed => "proposed",
        Accepted => "accepted",
        Superseded => "superseded",
        Withdrawn => "withdrawn",
    }
}

state_enum! {
    /// How much of the requested context the plan could actually deliver.
    ///
    /// `Supported` means the full request was served. `Unsupported` means
    /// the capability does not exist here. `Unavailable` means it exists but
    /// could not be reached. `Partial` means some of it was served and the
    /// rest was cut. `Degraded` means it was served by a weaker path.
    ///
    /// An empty plan is never implicitly `Supported`: an empty result and a
    /// backend that does not exist must be told apart.
    Degradation {
        Supported => "supported",
        Unsupported => "unsupported",
        Unavailable => "unavailable",
        Partial => "partial",
        Degraded => "degraded",
    }
}

impl Default for SourceCurrency {
    fn default() -> Self {
        SourceCurrency::Unknown
    }
}

impl Default for SemanticValidation {
    fn default() -> Self {
        SemanticValidation::Unreviewed
    }
}

impl Default for Coverage {
    fn default() -> Self {
        Coverage::Unavailable
    }
}

impl Default for DecisionLifecycle {
    fn default() -> Self {
        DecisionLifecycle::Proposed
    }
}

impl Default for Degradation {
    fn default() -> Self {
        Degradation::Unavailable
    }
}

/// Which checkout, worktree, and revision a context request is about.
///
/// Two concurrent worktrees of one repository are different sources. Without
/// this identity a recall in one worktree can quote a rule that was written
/// against another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SourceIdentity {
    /// The repository the work belongs to.
    pub repo_id: String,
    /// The checkout (clone) the work belongs to.
    pub checkout_id: String,
    /// The worktree within that checkout.
    pub worktree_id: String,
    /// The revision the source was read at.
    pub revision: String,
    /// Whether the worktree had uncommitted changes.
    #[serde(default)]
    pub dirty: bool,
    /// The path of the governed file, relative to the project root.
    pub relative_path: String,
}

/// Whether content may leave the process, and why not when it may not.
///
/// The policy travels with the request because the decision is made twice --
/// once before rendering and once before host injection -- and both calls
/// must see the same answer. A denial is not the same event as "not found",
/// so the reason is part of the contract, not a debug field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EgressPolicy {
    /// Whether egress is permitted.
    pub allowed: bool,
    /// Why egress was refused, when it was.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One planning request: what the host observed, and under which rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextRequest {
    /// The host session the request belongs to.
    pub session_id: String,
    /// Bumped whenever the host's context is rebuilt (compaction, restart).
    ///
    /// Dedup keys on `(content revision, context_epoch)`: an epoch change
    /// rehydrates the injected set, it never suppresses recall.
    pub context_epoch: u64,
    /// The event that produced this request.
    pub event_id: String,
    /// What triggered it (`session_start`, `file_touch`, `topic_change`, ...).
    pub trigger: String,
    /// The source the request is about.
    pub source: SourceIdentity,
    /// The egress policy that applies.
    pub policy: EgressPolicy,
}

/// One memory the plan would deliver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextItem {
    /// The memory id.
    pub id: String,
    /// The memory type (`rule`, `fact`, `procedure`, ...).
    pub memory_type: String,
    /// The rendered content of the memory.
    pub content: String,
    /// The computed freshness of this item, when there is one.
    ///
    /// Absent means "not computed" -- never "fresh". Ranking needs it, and
    /// an item that has none must not be treated as verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
}

/// The four freshness dimensions of one memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Freshness {
    /// Currency of the governed source.
    pub source_currency: SourceCurrency,
    /// Whether the meaning was checked against evidence.
    pub semantic_validation: SemanticValidation,
    /// How much of the scope the active index covers.
    pub index_coverage: Coverage,
    /// Where the decision sits in its lifecycle.
    pub decision_lifecycle: DecisionLifecycle,
}

/// The plan: what to deliver, how much of it, and how honestly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextPlan {
    /// The schema version of this plan.
    pub schema_version: String,
    /// The event the plan answers.
    pub event_id: String,
    /// The index generation the plan was built against.
    pub index_generation: u64,
    /// The scope the plan was applied to (the namespace, in practice).
    pub applied_scope: String,
    /// The items to deliver.
    #[serde(default)]
    pub items: Vec<ContextItem>,
    /// Whether a budget cut the plan short.
    #[serde(default)]
    pub truncation: bool,
    /// How much of the request was actually delivered.
    pub degradation: Degradation,
}

impl ContextPlan {
    /// An empty plan for one event, stamped with the current schema version.
    ///
    /// The caller states the degradation explicitly: an empty plan whose
    /// degradation was left to a default would claim more than it knows.
    pub fn new(event_id: impl Into<String>, degradation: Degradation) -> Self {
        ContextPlan {
            schema_version: CONTEXT_PLAN_SCHEMA_VERSION.to_string(),
            event_id: event_id.into(),
            index_generation: 0,
            applied_scope: String::new(),
            items: Vec::new(),
            truncation: false,
            degradation,
        }
    }
}
