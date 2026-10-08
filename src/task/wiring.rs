//! The wiring panel — the closed registry of gates NeuroStrata prescribes.
//!
//! Design: `docs/design-wiring-panel.md` (K3, 2026-10-08). Constraint in
//! force: **git-first, never git-only**. Gates are the concept; git hooks are
//! the default instance. `runs_in` is four values on purpose — the actual
//! chokepoint of a non-software project (a CMS publish step, a render
//! submission) is free text in `gate_point`, never an enum.
//!
//! Rule: never hand a project a reminder where a machine will do.

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
    /// No machine exists; the prompt is the mechanism. Last resort. No
    /// shipped wire uses it today — that is the point of the category.
    #[allow(dead_code)]
    AgentReminder,
}

impl RunsIn {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunsIn::Core => "core",
            RunsIn::GitPrePush => "git-pre-push",
            RunsIn::ProjectPipeline => "project-pipeline",
            RunsIn::AgentReminder => "agent-reminder",
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
        blocks: true,
        reason: "no state-mutating action before a Goal exists and is claimed",
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
