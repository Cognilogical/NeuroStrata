use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidateRequest {
    pub action_type: String, // e.g., "bash", "write_file"
    pub payload: String,     // The command or file content
    pub cwd: String,         // Current working directory
    pub orchestrator_id: String,
    pub trace_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "bucket")]
pub enum ValidateVerdict {
    DeterministicReject {
        reasons: Vec<String>,
        constraints: Vec<String>,
    },
    SandboxReject {
        reasons: Vec<String>,
        logs: String,
        constraints: Vec<String>,
    },
    SandboxPassHighFidelity {
        notes: Vec<String>,
    },
    SandboxPassLowFidelity {
        notes: Vec<String>,
    },
    ApprovedFailOpen {
        warning: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidateResponse {
    pub trace_id: String,
    pub verdict: ValidateVerdict,
    pub rule_ids_triggered: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BehavioralRule {
    pub id: String,
    pub version: u32,
    pub rule_class: String,
    pub trigger_pattern: String,
    pub constraint_text: String,
    pub hit_count: u32,
    pub status: String, // active, deprecated
}
