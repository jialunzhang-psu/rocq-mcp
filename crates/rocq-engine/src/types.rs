//! Values crossing the Dune, PET, writeback, and MCP boundaries.
//!
//! Proof topology deliberately does not live here. The MCP crate owns its
//! request-boundary checkpoint graph; these values describe only one Dune
//! declaration and one ephemeral PET result.

use crate::pet::PetStateId;
use serde_json::Value;
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
    /// A caller-selected deadline expired while PET was evaluating one proof
    /// fragment. The checkpoint graph is unchanged and the PET epoch is gone.
    ProofStepTimeout,
    /// The MCP peer cancelled an admitted request. Any in-flight PET epoch is
    /// terminated; replayable proof topology remains owned by MCP.
    RequestCancelled,
    /// The PET child or its protocol transport was lost; this is not a
    /// correctness or execution deadline.
    PetLost,
    /// PET rejected a semantic query after validating the request shape.
    QueryFailed,
    /// PET reported an internal/system failure that is not a project
    /// configuration error and did not necessarily lose the transport.
    PetFailure,
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
    /// Optional source-relative diagnostic supplied by PET.  This is present
    /// only for an operation whose Rocq command has a precise location; all
    /// ordinary validation errors keep it absent.
    pub diagnostic: Option<ProofDiagnostic>,
    /// Whether `message` is Rocq's semantic diagnostic rather than a
    /// wrapper/infrastructure explanation.  MCP must preserve this text
    /// verbatim instead of appending generic recovery prose.
    pub semantic: bool,
}

impl Error {
    /// Construct a wrapper-owned failure.  Callers forwarding a Rocq
    /// rejection must additionally call [`Self::semantic`] so MCP leaves its
    /// diagnostic untouched.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            diagnostic: None,
            semantic: false,
        }
    }

    /// Attach PET's half-open UTF-8 byte range for the rejected fragment.
    pub fn with_diagnostic(mut self, diagnostic: ProofDiagnostic) -> Self {
        self.diagnostic = Some(diagnostic);
        self
    }

    /// Mark this message as an authoritative Rocq semantic diagnostic.  MCP
    /// will not append recovery prose to a marked error.
    pub fn semantic(mut self) -> Self {
        self.semantic = true;
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// A byte range in the caller's proof fragment.  PET computes this from
/// Rocq's parser/exception location; the wrapper never splits or reparses the
/// fragment to guess where execution failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofDiagnostic {
    pub byte_start: usize,
    pub byte_end: usize,
}

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

/// One proof-stack frame as returned by PET.  The two lists must remain
/// distinct: Rocq uses them to validate bullets and focus transitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalStackFrame {
    pub left: Vec<Vec<Value>>,
    pub right: Vec<Vec<Value>>,
}

/// Lossless *identity* projection of PET's goal response.  Full hypotheses
/// and types remain in PET and are fetched only for a bounded `query(goals)`
/// rendering; checkpoints retain this small metadata rather than duplicating
/// every pretty-printed context.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GoalFocus {
    pub focused: Vec<Vec<Value>>,
    pub stack: Vec<GoalStackFrame>,
    pub shelved: Vec<Vec<Value>>,
    pub given_up: Vec<Vec<Value>>,
    pub next_bullet: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoalScope {
    Focused,
    Unfocused,
    Shelved,
    GivenUp,
    All,
}

impl GoalFocus {
    pub fn unfocused_count(&self) -> usize {
        self.stack
            .iter()
            .map(|frame| frame.left.len() + frame.right.len())
            .sum()
    }

    pub fn total_count(&self) -> usize {
        self.focused.len() + self.unfocused_count() + self.shelved.len() + self.given_up.len()
    }

    pub fn focus_depth(&self) -> usize {
        self.stack.len()
    }
}

/// Materialized PET goal view returned to MCP. Completion is never inferred
/// from this text; it comes from PET's `proof_finished` result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofState {
    pub theorem: DeclarationInfo,
    pub lifecycle: ProofLifecycle,
    /// Focused or caller-selected PET rendering. MCP bounds this before a
    /// state is retained in a checkpoint; semantic goal data stays in PET.
    pub goals: String,
    /// PET-owned focus/identity metadata. Full goal contexts are deliberately
    /// not copied into every checkpoint; query(goals) asks PET for them again.
    pub goal_focus: GoalFocus,
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
    Open(Box<OpenedProof>),
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
}
