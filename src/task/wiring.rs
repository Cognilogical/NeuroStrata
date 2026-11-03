//! The wiring panel — the closed registry of gates NeuroStrata prescribes.
//!
//! Design: `docs/design-wiring-panel.md` (K3, 2026-10-08). Constraint in
//! force: **git-first, never git-only**. Gates are the concept; git hooks are
//! the default instance. `Wire.runs_in` is the three-value `RunsIn` enum —
//! a non-software project's actual chokepoint (a CMS publish step, a render
//! submission) is the generic `ProjectPipeline` variant, never a new enum
//! value.
//!
//! Rule: never hand a project a reminder where a machine will do.
//!
//! Registry size: 13 automatic wires and 7 reminder wires. The counts are
//! stated here so a stale "four wires" claim in prose cannot outlive the
//! registry it describes.

/// Where a wire runs. Closed enum; "project-ci" is just the software
/// project's `project-pipeline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunsIn {
    /// NeuroStrata enforces it itself (tool, error path, lock). No project action.
    Core,
    /// Default instance, emitted only when git is detected.
    GitPrePush,
    /// The project's own chokepoint: CI job, CMS publish step, render
    /// submission, deploy script. Generic on purpose.
    ProjectPipeline,
}

impl RunsIn {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunsIn::Core => "core",
            RunsIn::GitPrePush => "git-pre-push",
            RunsIn::ProjectPipeline => "project-pipeline",
        }
    }
}

pub struct Wire {
    pub id: &'static str,
    pub runs_in: RunsIn,
    /// true = hard fail, false = warn. Automatic wires only.
    pub blocks: bool,
    pub reason: &'static str,
}

pub struct Reminder {
    pub id: &'static str,
    /// Closed enum: mcp-call-failure | memory-added-or-superseded |
    /// memory-corrected | session-start | before-edit | during-work
    /// (`VALID_FIRES_ON` is the machine-readable form of this list; new
    /// triggers must join both).
    pub fires_on: &'static str,
    pub text: &'static str,
}

pub const AUTOMATIC_WIRES: &[Wire] = &[
    Wire {
        id: "task-close-lock",
        runs_in: RunsIn::Core,
        blocks: true,
        reason: "done requires an EXTRACTED_FROM edge; enforced in task_complete",
    },
    Wire {
        id: "claim-exclusivity",
        runs_in: RunsIn::Core,
        blocks: true,
        reason: "one live session per Goal; duplicate work fails at claim time",
    },
    Wire {
        id: "stale-claim-expiry",
        runs_in: RunsIn::Core,
        blocks: true,
        reason: "claims older than 60 minutes are released automatically",
    },
    Wire {
        id: "zero-action-start",
        runs_in: RunsIn::Core,
        blocks: false,
        reason: "no state-mutating action before a Goal exists and is claimed (advisory; no interception point for arbitrary actions)",
    },
    Wire {
        id: "single-daemon-lock",
        runs_in: RunsIn::Core,
        blocks: true,
        reason: "a second daemon is refused at the store lock; status is the preflight",
    },
    Wire {
        id: "rule-honesty",
        runs_in: RunsIn::Core,
        blocks: true,
        reason: "a rule claiming ENFORCED must name a real guard; checked in task_validate",
    },
    Wire {
        id: "task-gate",
        runs_in: RunsIn::GitPrePush,
        blocks: true,
        reason: "pre-push runs neurostrata-mcp task gate <namespace> --strict",
    },
    Wire {
        id: "memory-to-repo-drift",
        runs_in: RunsIn::ProjectPipeline,
        blocks: true,
        reason: "paths in memories rot silently as files move",
    },
    Wire {
        id: "repo-to-memory-drift",
        runs_in: RunsIn::ProjectPipeline,
        blocks: true,
        reason: "owner rulings written only in project docs bypass the layer agents query first",
    },
    Wire {
        id: "directive-preservation",
        runs_in: RunsIn::ProjectPipeline,
        blocks: true,
        reason: "recorded owner rulings must survive verbatim; a silent reword is how a constraint becomes advice",
    },
    Wire {
        id: "export-freshness",
        runs_in: RunsIn::ProjectPipeline,
        blocks: false,
        reason: "a stale backup is still a backup; warn, never block",
    },
    Wire {
        id: "supersede-hygiene",
        runs_in: RunsIn::ProjectPipeline,
        blocks: true,
        reason: "no superseded id may survive as a live node in the committed export",
    },
    Wire {
        id: "gate-self-test",
        runs_in: RunsIn::ProjectPipeline,
        blocks: true,
        reason: "a gate that passes its own test is not a gate: plant a violation, expect fail, prove byte-identical restore",
    },
];

pub const REMINDER_WIRES: &[Reminder] = &[
    Reminder {
        id: "status-before-daemon",
        fires_on: "mcp-call-failure",
        text: "run `neurostrata-mcp status` — do NOT start a daemon",
    },
    Reminder {
        id: "export-after-change",
        fires_on: "memory-added-or-superseded",
        text: "refresh the committed export so the backup matches the graph",
    },
    Reminder {
        id: "supersede-never-delete",
        fires_on: "memory-corrected",
        text: "correct with neurostrata_supersede_memory; a correction that erases what it corrected loses history",
    },
    Reminder {
        id: "episodic-log",
        fires_on: "during-work",
        text: "append to the Episodic Buffer (neurostrata_append_log) as work happens",
    },
    Reminder {
        id: "lane-declaration",
        fires_on: "session-start",
        text: "declare the lane this session works in; sprawl burns the context window",
    },
    Reminder {
        id: "task-first",
        fires_on: "before-edit",
        text: "claim or create a Goal before editing; edits with no task close with no memory extracted",
    },
    Reminder {
        id: "task-capture-on-intent",
        fires_on: "during-work",
        text: "when speech or a log entry contains a task-shaped statement (TODO:, 'later we need to', 'we should also', ...), capture it with neurostrata_task_create instead of letting it evaporate",
    },
    Reminder {
        id: "graph-first-lookup",
        fires_on: "during-work",
        text: "before blind grep/glob or file edits, query neurostrata_memories_for_file, neurostrata_related, neurostrata_search_memory (include_related: true), or neurostrata_sparql_query for GOVERNS/DEPENDS_ON/calls/imports edges",
    },
    Reminder {
        id: "subagent-context-propagation",
        fires_on: "before-edit",
        text: "subagents start with zero memory context: paste the subagent_prompt_injection block from neurostrata_task_claim into EVERY subagent prompt you spawn",
    },
];

/// Every id a rule may name in its `guard` field (A6). A dangling guard is
/// one that appears in no registry list.
pub fn known_wire_ids() -> Vec<&'static str> {
    AUTOMATIC_WIRES
        .iter()
        .map(|w| w.id)
        .chain(REMINDER_WIRES.iter().map(|r| r.id))
        .collect()
}

/// Whether `name` is an automatic wire id at all (as opposed to a reminder).
pub fn is_automatic_wire(name: &str) -> bool {
    AUTOMATIC_WIRES.iter().any(|w| w.id == name)
}

/// Whether `name` is a reminder wire id whose trigger is a member of the
/// closed `fires_on` enum. An ENFORCED rule citing one of these names a
/// reminder where a machine belongs.
pub fn is_reminder_wire(name: &str) -> bool {
    REMINDER_WIRES
        .iter()
        .any(|r| r.id == name && is_valid_fires_on(r.fires_on))
}

/// Whether the automatic wire `name` hard-fails. An ENFORCED rule needs a
/// machine that blocks: citing an advisory wire (`blocks == false`) as the
/// sole guard is an enforcement claim no machine carries.
pub fn is_blocking_automatic_wire(name: &str) -> bool {
    AUTOMATIC_WIRES.iter().any(|w| w.id == name && w.blocks)
}

/// The closed set of `fires_on` triggers a `Reminder` may name — the
/// machine-readable form of the closed enum documented on the struct.
pub const VALID_FIRES_ON: &[&str] = &[
    "mcp-call-failure",
    "memory-added-or-superseded",
    "memory-corrected",
    "session-start",
    "before-edit",
    "during-work",
];

/// Compute the delta of newly added automatic wires, reminder wires, and
/// `fires_on` lifecycle triggers relative to the lists previously recorded
/// on a project's namespace registry row.
pub fn compute_lifecycle_delta(
    configured_wires: &[String],
    configured_fires_on: &[String],
) -> (Vec<&'static Wire>, Vec<&'static Reminder>, Vec<&'static str>) {
    let new_auto: Vec<&'static Wire> = AUTOMATIC_WIRES
        .iter()
        .filter(|w| !configured_wires.iter().any(|c| c == w.id))
        .collect();
    let new_rem: Vec<&'static Reminder> = REMINDER_WIRES
        .iter()
        .filter(|r| !configured_wires.iter().any(|c| c == r.id))
        .collect();
    let new_fires: Vec<&'static str> = VALID_FIRES_ON
        .iter()
        .copied()
        .filter(|t| !configured_fires_on.iter().any(|c| c == *t))
        .collect();
    (new_auto, new_rem, new_fires)
}

/// Whether `s` is a legitimate `Reminder.fires_on` trigger.
pub fn is_valid_fires_on(s: &str) -> bool {
    VALID_FIRES_ON.contains(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every reminder wire must declare a trigger the closed enum knows:
    /// a reminder that fires on nothing is dead text, and one that fires on
    /// an unknown trigger is outside the contract.
    #[test]
    fn wiring_reminder_wires_declare_a_valid_fires_on_trigger() {
        for r in REMINDER_WIRES {
            assert!(
                is_valid_fires_on(r.fires_on),
                "reminder wire '{}' declares unknown fires_on '{}'",
                r.id,
                r.fires_on
            );
        }
    }

    /// The delta identifies newly added automatic wires, reminder wires, and
    /// lifecycle triggers relative to previously configured lists.
    #[test]
    fn compute_lifecycle_delta_identifies_new_wires_and_triggers() {
        let (new_auto, new_rem, new_fires) = compute_lifecycle_delta(&[], &[]);
        assert!(!new_auto.is_empty());
        assert!(!new_rem.is_empty());
        assert!(!new_fires.is_empty());
        let (new_auto2, new_rem2, new_fires2) = compute_lifecycle_delta(
            &known_wire_ids().into_iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &VALID_FIRES_ON.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        assert!(new_auto2.is_empty());
        assert!(new_rem2.is_empty());
        assert!(new_fires2.is_empty());
    }

    /// The closed enum is exhaustive over the registry: every trigger
    /// actually used by a reminder is a member of VALID_FIRES_ON, and the
    /// known enum values validate.
    #[test]
    fn wiring_fires_on_members_and_non_members() {
        assert!(is_valid_fires_on("mcp-call-failure"));
        assert!(is_valid_fires_on("memory-added-or-superseded"));
        assert!(is_valid_fires_on("memory-corrected"));
        assert!(is_valid_fires_on("session-start"));
        assert!(is_valid_fires_on("before-edit"));
        assert!(is_valid_fires_on("during-work"));
        assert!(!is_valid_fires_on("git-pre-push"));
        assert!(!is_valid_fires_on(""));
    }
}
