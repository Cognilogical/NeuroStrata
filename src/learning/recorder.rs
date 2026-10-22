// TODO: Step 2 — implement LearningRecorder
// placeholder so crate compiles
use crate::learning::observation::{DecisionObservation, LearningOutcome, PayloadHash};
use crate::traits::MemoryPayload;
use std::path::PathBuf;

pub struct LearningRecorder { root: PathBuf }
impl LearningRecorder {
    pub async fn open(_root: PathBuf) -> anyhow::Result<Self> { todo!() }
    pub async fn record_observation(&self, _value: &DecisionObservation) -> anyhow::Result<()> { todo!() }
    pub async fn record_outcome(&self, _value: &LearningOutcome) -> anyhow::Result<()> { todo!() }
    pub async fn put_payload(&self, _value: &MemoryPayload) -> anyhow::Result<PayloadHash> { todo!() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jsonl_append_reopen_atomic() { panic!("TODO: JSONL append reopen") }
}