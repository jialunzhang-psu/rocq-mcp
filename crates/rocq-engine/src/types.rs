//! Values crossing the Dune, PET, writeback, and MCP boundaries.
//!
//! Proof topology deliberately does not live here. The MCP crate owns its
//! request-boundary checkpoint graph; these values describe only one Dune
//! declaration and one ephemeral PET result.

use crate::pet::PetStateId;
use std::{fmt, path::PathBuf, time::Duration};

/// Operator policy for external commands.
///
/// `command_timeout = None` is the correctness-preserving default: valid Dune
/// and Rocq work is allowed to finish. A deployment may opt into a deadline.
#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    pub command_timeout: Option<Duration>,
}

/// Stable error classes projected one-for-one by the MCP adapter.
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

/// A user-observable engine failure.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProofLifecycle {
    Open,
    Completed,
}

/// Workspace-relative identity selected by Dune.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
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
            .map(|part| part.as_os_str().to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>()
            .join("/");
        if value.is_empty() || value == ".." || value.starts_with("../") {
            return Err("source path is not workspace-relative".into());
        }
        Ok(Self(value))
    }
}

/// Canonical Dune logical compilation-unit path.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LogicalLibrary(pub Vec<String>);

/// One load-path entry copied from a Dune-selected Rocq compiler action.
/// `implicit` preserves Dune's `-R` (`true`) versus `-Q` (`false`) choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetLoadPath {
    pub(crate) physical: PathBuf,
    pub(crate) logical: LogicalLibrary,
    pub(crate) implicit: bool,
}

/// Complete Dune-owned PET workspace configuration for one source unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetWorkspace {
    pub(crate) root: PathBuf,
    pub(crate) load_paths: Vec<PetLoadPath>,
}

/// A declaration address. `qualified_path` is the Dune compilation-unit path
/// followed by PET's canonical path inside that source document.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeclarationIdentity {
    pub file: FileId,
    pub qualified_path: Vec<String>,
}

impl DeclarationIdentity {
    pub fn qualified_name(&self) -> String {
        self.qualified_path.join(".")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    pub(crate) fn terminator(self) -> &'static str {
        if self == Self::Definition {
            "Defined."
        } else {
            "Qed."
        }
    }

    pub(crate) fn keyword(self) -> &'static str {
        match self {
            Self::Theorem => "Theorem",
            Self::Lemma => "Lemma",
            Self::Fact => "Fact",
            Self::Remark => "Remark",
            Self::Corollary => "Corollary",
            Self::Proposition => "Proposition",
            Self::Definition => "Definition",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationInfo {
    pub identity: DeclarationIdentity,
    pub kind: DeclarationKind,
    pub statement: String,
}

/// Materialized PET goal view returned to MCP. Completion is never inferred
/// from this text; it comes from PET's `proof_finished` result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofState {
    pub theorem: DeclarationInfo,
    pub lifecycle: ProofLifecycle,
    pub focused_goals: usize,
    pub unfocused_goals: usize,
    pub shelved_goals: usize,
    pub given_up_goals: usize,
    pub goals: String,
}

/// PET-provided byte range in the exact source snapshot. The end is exclusive.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Compare-and-swap anchor for one declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceAnchor {
    pub(crate) source: PathBuf,
    pub(crate) digest: [u8; 32],
    pub(crate) header: PetRange,
    pub(crate) declaration: PetRange,
    /// Whether exactly one PET declaration owns `declaration`. Shared or
    /// overlapping command ranges may be inspected but must never be replaced
    /// as though they represented one standalone declaration.
    pub(crate) replaceable: bool,
}

/// Immutable source-side information required to replay and publish one active
/// proof. It is intentionally not a project catalogue entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationTarget {
    pub(crate) info: DeclarationInfo,
    pub(crate) library: LogicalLibrary,
    pub(crate) anchor: SourceAnchor,
    pub(crate) new_header: Option<String>,
}

impl DeclarationTarget {
    pub fn identity(&self) -> &DeclarationIdentity {
        &self.info.identity
    }
    pub(crate) fn is_new(&self) -> bool {
        self.new_header.is_some()
    }
}

/// Root state of a newly selected unpublished proof.
#[derive(Clone, Debug)]
pub struct OpenedProof {
    pub target: DeclarationTarget,
    pub state: PetStateId,
    pub view: ProofState,
    pub finished: bool,
}

/// Selecting a source declaration either opens an interactive proof or marks
/// its PET-finished source form for build/trust validation.
#[derive(Clone, Debug)]
pub enum OpenResult {
    Open(OpenedProof),
    /// PET reports a proved terminal declaration. The MCP project coordinator
    /// must perform the Dune-build/PET-refresh epoch transition before
    /// exposing it as `Completed`.
    Published(DeclarationTarget),
}

/// Result of one atomic PET proof fragment.
#[derive(Clone, Debug)]
pub struct ProofStep {
    pub state: PetStateId,
    pub view: ProofState,
    pub finished: bool,
}

/// Direct PET query command. These variants are formatting requests, not a
/// wrapper-side semantic query implementation.
#[derive(Clone, Debug)]
pub enum PetQuery {
    Search(String),
    About(String),
    Print(String),
    Assumptions(String),
    Dependencies(String),
    ExpressionType(String),
    Notation(String),
    Locate(String),
}
