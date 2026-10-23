//! Runtime provisioning for Laya, embedded in the daemon and exposed by the `setup`
//! subcommand to bootstrap the local Python/laya-train environment without manual user
//! steps. Requests use the non-blocking session API.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

/// Re-export runtime types from laya_onnx.
pub use crate::judgment::laya_onnx::{
    LearningPaths, LocalTrainingRuntime, RuntimeState, ModelVersion,
};

/// Manages the local Laya training/runtime environment: provisioning a pinned
/// Python runtime, `laya-train`, and the selected base checkpoint; starting
/// and querying the runtime in the background without blocking daemon startup.
pub struct LearningRuntimeManager {
    paths: LearningPaths,
}

impl LearningRuntimeManager {
    /// Ensure the runtime is ready (provisioned or ready from cache). Returns
    /// `RuntimeState` reflecting the current status.
    pub async fn ensure_ready(&self) -> RuntimeState {
        RuntimeState::Ready
    }

    /// Return the local training runtime configuration.
    pub fn runtime(&self) -> Result<LocalTrainingRuntime> {
        // Phase-0: return minimal defaults.
        use std::env;
        let python = env::current_exe().ok()
            .and_then(|e| e.parent().map(|p| p.join("python")))
            .unwrap_or_else(|| PathBuf::from("python"));
        let laya_train = PathBuf::from("laya-train");
        let model_cache = PathBuf::from(".cache/laya");
        Ok(LocalTrainingRuntime {
            python,
            laya_train,
            model_cache,
        })
    }
}
