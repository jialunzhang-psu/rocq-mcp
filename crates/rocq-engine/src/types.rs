//! Core engine data definitions.
//!
//! This module contains identity and live runtime state only. Dune discovers
//! projects, PET owns Rocq semantics, `trace-forest` owns proof topology, and
//! `writeback` owns the synchronous source transaction.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};
use trace_forest::{CursorId, TraceForest};

/// Disposable runtime configuration.  Source/build state remains project-owned.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub state_parent: PathBuf,
    pub trace_memory_bytes: usize,
    pub operation_timeout: Duration,
    /// Explicit native close deadline. `None` lets Dune/Rocq finish naturally.
    pub close_timeout: Option<Duration>,
    pub runtime_cache_bytes: usize,
    pub max_pet_processes: usize,
}
impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            state_parent: std::env::temp_dir().join("rocq-engine"),
            trace_memory_bytes: 8 * 1024 * 1024,
            operation_timeout: Duration::from_secs(20),
            // Design note: dependency-chain build time is project-dependent;
            // no arbitrary default may reject a valid proof.
            close_timeout: None,
            runtime_cache_bytes: 32 * 1024 * 1024,
            max_pet_processes: 4,
        }
    }
}

/// Stable public error class. Every variant is meaningful to an MCP user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidRequest,
    InvalidConfiguration,
    InvalidDeclaration,
    NotFound,
    Ambiguous,
    DeclarationChanged,
    ProofStepFailed,
    ProofTimeout,
    ProjectTimeout,
    BuildTimeout,
    AxiomDependencyOutOfScope,
    UnfinishedDependency,
}

/// A failure that is part of the public engine/MCP/user contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}
impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

/// Opaque in-process trace capability.  It has no formatting or wire form.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct AttemptId(pub(crate) CursorId);

impl Ord for AttemptId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_uuid().cmp(&other.0.as_uuid())
    }
}
impl PartialOrd for AttemptId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProofLifecycle {
    Open,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeclarationInfo {
    pub identity: DeclarationIdentity,
    pub kind: DeclarationKind,
    pub statement: String,
}

/// State returned by proof operations. `attempt` is present only when the
/// represented goals correspond to a selectable, live TraceForest cursor;
/// completed, hypothetical, and concurrently retired states have none. Goal
/// fields are populated only by PET rather than inferred from source text.
#[derive(Clone, Eq, PartialEq)]
pub struct ProofState {
    pub attempt: Option<AttemptId>,
    pub theorem: DeclarationInfo,
    pub lifecycle: ProofLifecycle,
    pub focused_goals: usize,
    pub unfocused_goals: usize,
    pub shelved_goals: usize,
    pub given_up_goals: usize,
    pub goals: String,
}
impl fmt::Debug for ProofState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProofState")
            .field("theorem", &self.theorem)
            .field("lifecycle", &self.lifecycle)
            .finish()
    }
}

/// Result of an ordered, mutating proof check.
///
/// `selected` is the zero-based index of the first completely accepted input
/// fragment. `rejected` contains exactly the preceding fragment failures. If
/// every fragment is rejected, `selected` is `None`, `state` is the unchanged
/// base state, and `rejected` contains one error per input. `error` is reserved
/// for close/writeback after a fragment has already been selected.
#[derive(Debug)]
pub struct CheckResult {
    pub selected: Option<usize>,
    pub state: ProofState,
    pub rejected: Vec<Error>,
    pub error: Option<Error>,
}

/// Result of evaluating one non-mutating proof fragment from a shared base.
///
/// A fully PET-accepted fragment returns a hypothetical `state`; a rejected
/// fragment returns its PET error and no partial-prefix state. `solved` reports
/// PET's terminal proof status without publishing or changing the trace.
#[derive(Debug)]
pub struct AttemptResult {
    pub state: Option<ProofState>,
    pub solved: bool,
    pub error: Option<Error>,
}

/// Canonical Dune logical compilation-unit name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalLibrary(pub Vec<String>);

/// Workspace-relative source identity. Absolute paths and `_build` paths are
/// never exposed as declaration identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileId(pub String);

impl FileId {
    pub(crate) fn from_path(
        workspace: &std::path::Path,
        source: &std::path::Path,
    ) -> Result<Self, String> {
        let relative = source
            .strip_prefix(workspace)
            .map_err(|_| "source escapes Dune workspace".to_owned())?;
        let value = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>()
            .join("/");
        if value.is_empty() || value.starts_with("../") || value == ".." {
            return Err("source path is not workspace-relative".into());
        }
        Ok(Self(value))
    }
}

/// A declaration address returned by PET for one source document. The complete
/// qualified path is authoritative; module/constant splits are derived views.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationIdentity {
    pub file: FileId,
    pub qualified_path: Vec<String>,
}
impl DeclarationIdentity {
    pub fn qualified_name(&self) -> String {
        self.qualified_path.join(".")
    }
    pub fn constant(&self) -> Option<&str> {
        self.qualified_path.last().map(String::as_str)
    }
}

/// Rocq declaration category. It determines the source proof terminator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeclarationKind {
    Theorem,
    Lemma,
    Fact,
    Remark,
    Corollary,
    Proposition,
    Definition,
}
impl DeclarationKind {
    pub fn terminator(self) -> &'static str {
        if self == Self::Definition {
            "Defined."
        } else {
            "Qed."
        }
    }
}

/// Source snapshot for one declaration. Dune supplies the source path and PET
/// supplies ranges for indexed declarations; a newly declared in-memory proof
/// uses a zero-width header range at PET's AST-derived insertion point until
/// writeback. This is the single metadata representation shared by project
/// state, trace roots, PET, and writeback.
/// Its ranges are valid only for `anchor.digest`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeclarationSource {
    pub(crate) info: DeclarationInfo,
    pub(crate) library: LogicalLibrary,
    pub(crate) anchor: PetAnchor,
}

/// One proof command accepted by PET and stored as a trace-forest action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct CanonicalTactic(pub(crate) String);

pub(crate) fn format_name(identity: &DeclarationIdentity) -> String {
    identity.qualified_name()
}

/// PET byte range after conversion from PET positions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PetRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// PET-derived declaration location, valid only for its source digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct PetAnchor {
    pub(crate) source: PathBuf,
    pub(crate) digest: [u8; 32],
    pub(crate) header: PetRange,
    pub(crate) declaration: Option<PetRange>,
}

/// One declaration and every interactive attempt that belongs to it.
#[derive(Clone, Debug)]
pub(crate) struct DeclarationInstance {
    pub(crate) source: DeclarationSource,
    pub(crate) attempts: BTreeMap<AttemptId, ProofAttempt>,
}

/// Opaque reference to a PET-owned proof state for a trace prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PetProofState {
    pub(crate) instance_epoch: u64,
    pub(crate) state: u64,
    pub(crate) prefix_len: usize,
}

/// One trace branch of one declaration.
#[derive(Clone, Debug)]
pub(crate) struct ProofAttempt {
    pub(crate) pet_state: Option<PetProofState>,
}

/// The single engine-owned state object for an attached project.
pub(crate) struct ProjectState {
    pub(crate) root: PathBuf,
    pub(crate) gate: Arc<RwLock<()>>,
    pub(crate) traces: Arc<TraceForest<DeclarationSource, CanonicalTactic>>,
    /// Only declarations that this runtime has actually loaded or declared.
    pub(crate) touched: BTreeMap<DeclarationIdentity, DeclarationInstance>,
}
