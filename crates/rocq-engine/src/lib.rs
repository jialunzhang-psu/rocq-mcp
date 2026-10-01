//! Minimal Dune/PET/writeback facade used by the Rocq MCP adapter.

mod dune;
mod engine;
pub mod pet;
mod request;
mod types;
mod writeback;

pub use engine::{DuneProject, Engine, validate_fragments, validate_identity};
pub use pet::{PetActor, PetStateId};
pub use request::{
    RequestCancellation, commit_request, request_cancelled, request_timed_out,
    with_request_cancellation,
};
pub use types::{
    ArtifactFingerprint, DeclarationIdentity, DeclarationInfo, DeclarationKind, DeclarationTarget,
    EngineConfig, Error, ErrorKind, FileId, GoalDetail, GoalFocus, GoalHypothesis, GoalScope,
    GoalStackDetail, GoalStackFrame, LogicalLibrary, OpenResult, OpenedProof, PetQuery,
    ProofDiagnostic, ProofFailure, ProofLifecycle, ProofState, ProofStep, ProofTraceStep,
    StructuredGoals, SymbolLocation, TracedProofStep,
};

pub(crate) type Result<T> = std::result::Result<T, Error>;
