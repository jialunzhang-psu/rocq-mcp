//! Minimal Dune/PET/writeback facade used by the Rocq MCP adapter.

mod dune;
mod engine;
pub mod pet;
mod types;
mod writeback;

pub use engine::{DuneProject, Engine, validate_fragments, validate_identity};
pub use pet::{PetActor, PetStateId};
pub use types::{
    DeclarationIdentity, DeclarationInfo, DeclarationKind, DeclarationTarget, EngineConfig, Error,
    ErrorKind, FileId, LogicalLibrary, OpenResult, OpenedProof, PetQuery, ProofLifecycle,
    ProofState, ProofStep,
};

pub(crate) type Result<T> = std::result::Result<T, Error>;
