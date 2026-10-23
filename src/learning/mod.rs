pub mod observation;
pub mod recorder;
pub mod blob_store;
pub mod labels;
pub mod split;
pub mod dataset;

pub use observation::{
    CandidateSource, DecisionObservation, LearningContext, LearningOutcome, LearningOutcomeKind,
    ObservedCandidate, PayloadHash,
};
pub use recorder::LearningRecorder;
pub use blob_store::BlobStore;
pub use labels::{derive_labels, DerivedLabel, LabelRule, LabelTier};
pub use split::{split_observations, DatasetSplits, Split};
pub use dataset::{build_dedup_dataset, DatasetBuildReport, DatasetManifest, DistancePolicy};
