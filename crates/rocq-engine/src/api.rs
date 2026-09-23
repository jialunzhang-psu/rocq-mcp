//! Stable public API contracts for the Rocq engine.

use super::*;

/// Disposable runtime configuration.  Source/build state remains project-owned.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub state_parent: PathBuf,
    pub trace_memory_bytes: usize,
    pub operation_timeout: Duration,
    pub close_timeout: Duration,
    pub runtime_cache_bytes: usize,
    pub max_pet_processes: usize,
}
impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            state_parent: std::env::temp_dir().join("rocq-engine"),
            trace_memory_bytes: 8 * 1024 * 1024,
            operation_timeout: Duration::from_secs(20),
            close_timeout: Duration::from_secs(20),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProofLifecycle {
    Open,
    Completed,
    Pending,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeclarationInfo {
    pub identity: DeclarationIdentity,
    pub context: Vec<LexicalScope>,
    pub kind: DeclarationKind,
    pub statement: String,
    pub status: ProofLifecycle,
}

/// A proof record recovered by logical declaration identity, never by UUID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveredProof {
    Solved {
        candidate: SolvedCandidate,
    },
    Rejected {
        candidate: SolvedCandidate,
        phase: RejectionPhase,
        diagnostics: Vec<Diagnostic>,
    },
    Closed,
}

/// Public catalog.  `sources` is an internal semantic index and is omitted
/// from serialization so callers cannot observe paths or byte anchors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectCatalog {
    pub root: PathBuf,
    pub declarations: Vec<DeclarationInfo>,
    #[serde(skip)]
    pub(crate) sources: BTreeMap<DeclarationIdentity, SourceDeclaration>,
}

/// State returned by open/inspect.  Goal fields are populated only by PET;
/// the semantic tranche leaves them empty rather than guessing native output.
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
    pub accepted_commands: usize,
    pub recovery: Option<RecoveredProof>,
}
impl fmt::Debug for ProofState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProofState")
            .field("theorem", &self.theorem)
            .field("lifecycle", &self.lifecycle)
            .field("accepted_commands", &self.accepted_commands)
            .finish()
    }
}

#[derive(Debug)]
pub struct StepResult {
    pub state: ProofState,
    pub error: Option<Error>,
}
#[derive(Debug)]
pub struct CandidateResult {
    pub state: Option<ProofState>,
    pub solved: bool,
    pub error: Option<Error>,
}

/// Native query requests. Target names are logical declaration suffixes;
/// search names are substrings. Expressions are validated as one Rocq sentence
/// before PET receives them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Query {
    Goals,
    Search {
        name: Option<String>,
        statement: Option<String>,
        status: Option<ProofLifecycle>,
        offset: usize,
        limit: usize,
    },
    Statement {
        name: String,
    },
    Proof {
        name: String,
    },
    Definition {
        name: String,
    },
    Assumptions {
        name: String,
    },
    Dependencies {
        name: String,
    },
    ExpressionType {
        expression: String,
    },
    Notation {
        expression: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueryResult {
    State(Box<ProofState>),
    Text(String),
}
