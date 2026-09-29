//! Thin, serialized client for one long-lived PET process.
//!
//! PET owns Rocq parsing, document declarations, proof execution, goals, and
//! semantic queries. This module owns only JSON-RPC framing and child-process
//! lifecycle; it contains no proof graph, source scanner, or replay cache.

use crate::types::{GoalFocus, GoalStackFrame, PetWorkspace, ProofDiagnostic};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread::JoinHandle,
};

const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8192;
const REQUIRED_CAPABILITIES: &[&str] = &[
    "document_declarations_v2",
    "dune_workspace_v1",
    "insertion_point_v1",
    "atomic_run_v1",
    "release_states_v1",
    "refresh_workspace_v1",
    "structured_assumptions_v1",
    "typed_errors_v1",
    "diagnostic_ranges_v1",
];

fn configured_pet_binary() -> PathBuf {
    std::env::var_os("ROCQ_PET_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("pet"))
}

/// Opaque identifier in the currently live PET process. It has no wire form
/// outside the engine/MCP implementation and is invalidated on process loss or
/// workspace refresh.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PetStateId(u64);

impl PetStateId {
    fn new(value: u64) -> Self {
        Self(value)
    }
    fn get(self) -> u64 {
        self.0
    }
}

/// Stable classification of a PET JSON-RPC error code.  The numeric code and
/// original message remain on [`PetError::Remote`]; this enum is the only
/// value used for operation-level semantics, so localized diagnostics never
/// become a hidden protocol parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PetRemoteKind {
    /// JSON-RPC method mismatch: the configured PET does not implement the
    /// required capability surface.
    MethodNotFound,
    Interrupted,
    Parsing,
    Coq,
    Anomaly,
    System,
    TheoremNotFound,
    NoNodeAtPoint,
    ReferenceNotFound,
    Unknown(i64),
}

impl PetRemoteKind {
    fn from_code(code: i64) -> Self {
        match code {
            -32601 => Self::MethodNotFound,
            -32001 => Self::Interrupted,
            -32002 => Self::Parsing,
            -32003 => Self::Coq,
            -32004 => Self::Anomaly,
            -32005 => Self::System,
            -32006 => Self::TheoremNotFound,
            -32007 => Self::NoNodeAtPoint,
            -32008 => Self::ReferenceNotFound,
            code => Self::Unknown(code),
        }
    }

    /// Whether this remote class is a Rocq/user-semantic rejection whose
    /// diagnostic must cross the MCP boundary unchanged.
    pub(crate) fn is_semantic(self) -> bool {
        matches!(
            self,
            Self::Interrupted
                | Self::Parsing
                | Self::Coq
                | Self::TheoremNotFound
                | Self::NoNodeAtPoint
                | Self::ReferenceNotFound
        )
    }
}

/// PET client failures are separated into local validation, project setup,
/// semantic remote rejection, and transport loss.  The MCP coordinator uses
/// transport loss to clear every checkpoint for the affected project before a
/// replacement process is admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PetError {
    Invalid(String),
    Environment(String),
    Cancelled,
    TimedOut {
        timeout_ms: u64,
    },
    ProcessLost(String),
    Protocol(String),
    OutputOverflow,
    Remote {
        code: i64,
        kind: PetRemoteKind,
        message: String,
        diagnostic: Option<ProofDiagnostic>,
    },
}

impl PetError {
    /// Whether the PET transport/process is no longer trustworthy.  The MCP
    /// owner uses this to discard all state handles before a replacement
    /// process is admitted.
    pub fn is_transport_loss(&self) -> bool {
        matches!(
            self,
            Self::ProcessLost(_) | Self::Protocol(_) | Self::OutputOverflow
        )
    }

    /// Whether the current PET epoch must be discarded. Intentional request
    /// cancellation and tactic deadlines terminate PET just like unexpected
    /// transport loss, but remain distinct user-facing error classes.
    fn invalidates_process(&self) -> bool {
        self.is_transport_loss() || matches!(self, Self::Cancelled | Self::TimedOut { .. })
    }

    /// Whether PET reported an internal/system failure while the JSON-RPC
    /// transport itself remained usable.  Callers must not label this as a
    /// user project configuration error.
    pub fn is_internal_failure(&self) -> bool {
        matches!(
            self,
            Self::Remote {
                kind: PetRemoteKind::Anomaly | PetRemoteKind::System | PetRemoteKind::Unknown(_),
                ..
            }
        )
    }

    /// Whether this error is a Rocq/user-semantic rejection.  This flag is
    /// used by every outer operation, including publication refresh, so a
    /// semantic diagnostic is never accidentally wrapped in infrastructure
    /// advice merely because it crossed a different lifecycle boundary.
    pub fn is_semantic(&self) -> bool {
        matches!(self, Self::Remote { kind, .. } if kind.is_semantic())
    }

    pub(crate) fn lost(&self) -> bool {
        self.is_transport_loss()
    }

    /// Return the backend-neutral message safe to expose through MCP.
    ///
    /// The typed variant is the public classification authority. Numeric
    /// JSON-RPC codes and the PET implementation name remain available in
    /// `Display` for internal diagnostics, but are not actionable for an MCP
    /// caller. Semantic rejections preserve Rocq's original message verbatim.
    pub fn public_message(&self) -> String {
        match self {
            Self::Invalid(message) | Self::Environment(message) | Self::ProcessLost(message) => {
                neutralize_transport_detail(message)
            }
            Self::Cancelled => "request cancelled".into(),
            Self::TimedOut { timeout_ms } => {
                format!("proof fragment exceeded {timeout_ms} ms")
            }
            Self::Protocol(_) => "received an invalid response".into(),
            Self::OutputOverflow => "response exceeded the output limit".into(),
            Self::Remote {
                kind: PetRemoteKind::MethodNotFound,
                ..
            } => "a required operation is not supported".into(),
            Self::Remote {
                kind: PetRemoteKind::Anomaly,
                ..
            } => "internal proof execution anomaly".into(),
            Self::Remote {
                kind: PetRemoteKind::System,
                ..
            } => "proof execution system failure".into(),
            Self::Remote {
                kind: PetRemoteKind::Unknown(_),
                ..
            } => "unrecognized proof execution failure".into(),
            Self::Remote { message, .. } => message.clone(),
        }
    }
}

/// Keep useful operating-system details (for example `Broken pipe` and a
/// signal number) without making the MCP response depend on the selected
/// prover's product name or JSON-RPC code vocabulary. Semantic remote
/// diagnostics do not pass through this function and remain byte-for-byte
/// unchanged.
fn neutralize_transport_detail(message: &str) -> String {
    // Design note: only non-semantic local transport text is normalized;
    // remote Rocq diagnostics take a separate, lossless path above.
    let mut value = message.replace("petanque", "prover protocol");
    value = value.replace("PET", "prover");
    let chars = value.chars().collect::<Vec<_>>();
    let mut redacted = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '-'
            && index + 6 <= chars.len()
            && chars[index + 1..index + 6]
                .iter()
                .all(|character| character.is_ascii_digit())
        {
            redacted.push_str("(internal error code)");
            index += 6;
        } else {
            redacted.push(chars[index]);
            index += 1;
        }
    }
    redacted
}

impl std::fmt::Display for PetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid PET request: {message}"),
            Self::Environment(message) => write!(f, "PET environment failure: {message}"),
            Self::ProcessLost(message) => write!(f, "PET process lost: {message}"),
            Self::Cancelled => f.write_str("PET request was cancelled"),
            Self::TimedOut { timeout_ms } => {
                write!(f, "PET proof fragment exceeded {timeout_ms} ms")
            }
            Self::Protocol(message) => write!(f, "PET protocol failure: {message}"),
            Self::OutputOverflow => f.write_str("PET response exceeded the output limit"),
            Self::Remote { code, message, .. } => {
                write!(f, "PET rejected request ({code}): {message}")
            }
        }
    }
}

impl std::error::Error for PetError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetHypothesis {
    pub(crate) names: Vec<String>,
    pub(crate) definition: Option<String>,
    pub(crate) ty: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetGoal {
    pub(crate) evar: Vec<Value>,
    pub(crate) name: Option<String>,
    pub(crate) hypotheses: Vec<PetHypothesis>,
    pub(crate) ty: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetGoalStackFrame {
    pub(crate) left: Vec<PetGoal>,
    pub(crate) right: Vec<PetGoal>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetGoals {
    pub(crate) focused: Vec<PetGoal>,
    pub(crate) stack: Vec<PetGoalStackFrame>,
    pub(crate) shelved: Vec<PetGoal>,
    pub(crate) given_up: Vec<PetGoal>,
    pub(crate) bullet: Option<String>,
    pub(crate) proof_mode: bool,
}

impl PetGoals {
    fn outside_proof() -> Self {
        Self {
            focused: Vec::new(),
            stack: Vec::new(),
            shelved: Vec::new(),
            given_up: Vec::new(),
            bullet: None,
            proof_mode: false,
        }
    }

    pub(crate) fn focus(&self) -> GoalFocus {
        let ids = |goals: &[PetGoal]| {
            goals
                .iter()
                .map(|goal| goal.evar.clone())
                .collect::<Vec<_>>()
        };
        GoalFocus {
            focused: ids(&self.focused),
            stack: self
                .stack
                .iter()
                .map(|frame| GoalStackFrame {
                    left: ids(&frame.left),
                    right: ids(&frame.right),
                })
                .collect(),
            shelved: ids(&self.shelved),
            given_up: ids(&self.given_up),
            next_bullet: self.bullet.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PetExecution {
    pub(crate) state: PetStateId,
    pub(crate) proof_finished: bool,
    pub(crate) goals: PetGoals,
}

/// One record returned by PET's whole-document declaration endpoint. `kind`
/// also admits `Axiom` internally so writeback can audit trust without a Rust
/// source scanner; public discovery filters unsupported proof kinds.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PetDeclaration {
    pub(crate) qualified_path: Vec<String>,
    pub(crate) kind: String,
    pub(crate) range: PetRange,
    pub(crate) declaration_range: PetRange,
    pub(crate) proof_finished: bool,
    pub(crate) statement: String,
}

/// Semantic class assigned by Rocq's global-context dependency analysis.
/// Only `Axiom` can correspond to an ordinary project declaration; all other
/// values represent trust conditions that the engine rejects explicitly.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PetAssumptionKind {
    Axiom,
    Positive,
    Guarded,
    TypeInType,
    Uip,
    SectionVariable,
    Opaque,
    Transparent,
}

/// One structured dependency identity returned directly by PET. The path is
/// absolute in Rocq's name table and never reconstructed from printed output.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct PetAssumption {
    pub(crate) kind: PetAssumptionKind,
    pub(crate) qualified_path: Vec<String>,
}

/// Environment-wide trust switches that are not attached to one declaration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct PetTheoryAssumptions {
    pub(crate) rewrite_rules: bool,
    pub(crate) impredicative_set: bool,
    pub(crate) type_in_type: bool,
}

/// Complete structured trust report for one declaration at one immutable PET
/// state. Empty assumptions and false theory flags mean globally closed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct PetAssumptionReport {
    pub(crate) assumptions: Vec<PetAssumption>,
    pub(crate) theory: PetTheoryAssumptions,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct PetRange {
    pub(crate) start: usize,
    #[serde(rename = "end")]
    pub(crate) end: usize,
}

#[derive(Default)]
struct ActorState {
    process: Option<PetProcess>,
    workspace: Option<PetWorkspace>,
}

/// Sole serialized owner of the PET subprocess for one active Dune project.
pub struct PetActor {
    state: Mutex<ActorState>,
    binary: PathBuf,
}

impl Default for PetActor {
    fn default() -> Self {
        Self::new()
    }
}

impl PetActor {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(ActorState::default()),
            binary: configured_pet_binary(),
        }
    }

    /// Return all declarations from exactly one checked document in one PET
    /// call. No source parsing or name reconstruction occurs in Rust.
    pub(crate) fn document_declarations(
        &self,
        workspace: &PetWorkspace,
        source: &Path,
    ) -> Result<Vec<PetDeclaration>, PetError> {
        let value = self.call_in_workspace(
            workspace,
            "petanque/document_declarations",
            json!({"uri": file_uri(source)}),
        )?;
        match serde_json::from_value(value) {
            Ok(declarations) => Ok(declarations),
            Err(error) => {
                let error =
                    PetError::Protocol(format!("invalid PET declaration response: {error}"));
                self.poison_if_invalidated(&error);
                Err(error)
            }
        }
    }

    /// Ask PET for the source insertion point inside one exact nested module.
    pub(crate) fn insertion_point(
        &self,
        workspace: &PetWorkspace,
        source: &Path,
        modules: &[String],
    ) -> Result<usize, PetError> {
        let value = self.call_in_workspace(
            workspace,
            "petanque/insertion_point",
            json!({"uri": file_uri(source), "modules": modules}),
        )?;
        value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                let error = PetError::Protocol("PET insertion point is invalid".into());
                self.poison_if_invalidated(&error);
                error
            })
    }

    /// Export the immutable Rocq state at one PET-provided byte anchor.
    pub(crate) fn state_at(
        &self,
        workspace: &PetWorkspace,
        source: &Path,
        offset: usize,
    ) -> Result<PetExecution, PetError> {
        let text = fs::read_to_string(source)
            .map_err(|_| PetError::Environment("source is unavailable".into()))?;
        let position = source_position(&text, offset)?;
        let value = self.call_in_workspace(
            workspace,
            "petanque/get_state_at_pos",
            json!({"uri": file_uri(source), "position": position}),
        )?;
        self.materialize_run(value)
    }

    /// Execute one complete caller fragment atomically. PET parses and runs all
    /// sentences itself; an error exports no partial-prefix state.
    pub(crate) fn run(&self, state: PetStateId, fragment: &str) -> Result<PetExecution, PetError> {
        self.run_with_timeout(state, fragment, None)
    }

    /// Execute one complete caller fragment with an optional caller-selected
    /// deadline. A deadline terminates the complete PET epoch because the
    /// synchronous PET shell cannot accept an interrupt while running Rocq.
    pub(crate) fn run_with_timeout(
        &self,
        state: PetStateId,
        fragment: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<PetExecution, PetError> {
        if fragment.len() > MAX_REQUEST_BYTES / 2 {
            return Err(PetError::Invalid("proof fragment is oversized".into()));
        }
        let value = self.call_live_with_timeout(
            "petanque/run",
            json!({"st": state.get(), "tac": fragment}),
            timeout,
        );
        if let Err(PetError::Remote {
            diagnostic: Some(diagnostic),
            ..
        }) = &value
            && (diagnostic.byte_end > fragment.len()
                || !fragment.is_char_boundary(diagnostic.byte_start)
                || !fragment.is_char_boundary(diagnostic.byte_end))
        {
            let error =
                PetError::Protocol("PET diagnostic range is outside the submitted fragment".into());
            self.poison_if_invalidated(&error);
            return Err(error);
        }
        let value = value?;
        self.materialize_run(value)
    }

    /// Read structured goals without allocating another PET state ID.
    pub(crate) fn goals(&self, state: PetStateId) -> Result<PetGoals, PetError> {
        let value = self.call_live("petanque/goals", json!({"st": state.get()}))?;
        let result = decode_goals(&value);
        if let Err(error) = &result {
            self.poison_if_invalidated(error);
        }
        result
    }

    /// Return Rocq's structured global-context dependencies for one absolute
    /// declaration path. Invalid response shape or empty name components are
    /// protocol loss and discard the PET process; this call has no side
    /// effects and allocates no exported state.
    pub(crate) fn assumptions(
        &self,
        state: PetStateId,
        qualified_path: &[String],
    ) -> Result<PetAssumptionReport, PetError> {
        let value = self.call_live(
            "petanque/assumptions",
            json!({"st": state.get(), "qualified_path": qualified_path}),
        )?;
        let result = serde_json::from_value::<PetAssumptionReport>(value)
            .map_err(|error| {
                PetError::Protocol(format!("invalid PET assumption response: {error}"))
            })
            .and_then(|report| {
                let invalid_path = report.assumptions.iter().any(|assumption| {
                    assumption.qualified_path.is_empty()
                        || assumption
                            .qualified_path
                            .iter()
                            .any(|component| component.is_empty())
                });
                if invalid_path {
                    Err(PetError::Protocol(
                        "PET assumption response contains an invalid path".into(),
                    ))
                } else {
                    Ok(report)
                }
            });
        if let Err(error) = &result {
            self.poison_if_invalidated(error);
        }
        result
    }

    /// Run one Rocq query in an exact immutable context. The temporary state
    /// exported by PET's `run` response is released before returning.
    pub(crate) fn query(
        &self,
        state: PetStateId,
        query: &crate::PetQuery,
    ) -> Result<String, PetError> {
        if let crate::PetQuery::Notation(expression) = query {
            let statement = format!("Lemma __rocq_mcp_notation_probe : {expression}.");
            let value = self.call_live(
                "petanque/list_notations_in_statement",
                json!({"st": state.get(), "statement": statement}),
            )?;
            return serde_json::to_string(&value).map_err(|_| {
                let error = PetError::Protocol("PET notation response is invalid".into());
                self.poison_if_invalidated(&error);
                error
            });
        }
        let command = match query {
            crate::PetQuery::Search(pattern) => format!("Search {pattern}."),
            crate::PetQuery::About(name) => format!("About {name}."),
            crate::PetQuery::Print(name) => format!("Print {name}."),
            crate::PetQuery::Assumptions(name) => format!("Print Assumptions {name}."),
            crate::PetQuery::Dependencies(name) => format!("Print All Dependencies {name}."),
            crate::PetQuery::ExpressionType(expression) => format!("Check ({expression})."),
            crate::PetQuery::Notation(_) => unreachable!(),
        };
        let value = self.call_live("petanque/run", json!({"st": state.get(), "tac": command}))?;
        let run = match results::parse_run_result(&value) {
            Ok(run) => run,
            Err(error) => {
                self.poison_if_invalidated(&error);
                return Err(error);
            }
        };
        let feedback = results::parse_feedback(&value);
        if let Err(error) = &feedback {
            self.poison_if_invalidated(error);
        }
        let release = self.release_states(&[PetStateId::new(run.st)]);
        match (feedback, release) {
            (Ok(text), Ok(())) => Ok(text),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    /// Remove exactly the supplied exported IDs. Parent/child semantics do not
    /// exist at this layer.
    pub fn release_states(&self, states: &[PetStateId]) -> Result<(), PetError> {
        if states.is_empty() {
            return Ok(());
        }
        let values = states.iter().map(|state| state.get()).collect::<Vec<_>>();
        let value = self.call_live("petanque/release_states", json!({"states": values}))?;
        let response: ReleaseResponse = match serde_json::from_value(value) {
            Ok(response) => response,
            Err(_) => {
                let error = PetError::Protocol("PET release response is invalid".into());
                self.poison_if_invalidated(&error);
                return Err(error);
            }
        };
        // Missing IDs are intentionally idempotent success. Validate only that
        // PET accounted for every request occurrence.
        if response.released.len() + response.missing.len() != states.len() {
            let error = PetError::Protocol("PET release response has the wrong cardinality".into());
            self.poison_if_invalidated(&error);
            return Err(error);
        }
        Ok(())
    }

    /// Return PET's optional exported-state cardinality for lifecycle tests.
    ///
    /// This deliberately exposes neither state identities nor state-management
    /// controls and is not mapped to any MCP tool. It is not a required
    /// production capability; pinned-PET tests use it to prove that
    /// temporary and rejected operations leave no exported state behind.
    #[doc(hidden)]
    pub fn diagnostic_state_count(&self) -> Result<usize, PetError> {
        let value = self.call_live("petanque/state_count", json!({}))?;
        value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                let error = PetError::Protocol("PET state count is invalid".into());
                self.poison_if_invalidated(&error);
                error
            })
    }

    /// Force the next PET-backed operation to start a fresh child.  This is a
    /// lifecycle primitive for transport-recovery tests and process owners;
    /// it exports neither the PID nor any state ID through MCP.
    pub fn restart(&self) {
        let mut state = self.lock_state();
        state.process.take();
        state.workspace.take();
    }

    /// Clear exported IDs and every PET/Fleche filesystem-derived cache while
    /// retaining the normal-path child process. If native refresh fails, this
    /// method replaces PET before returning success.
    pub(crate) fn refresh_workspace(&self, workspace: &PetWorkspace) -> Result<(), PetError> {
        let mut state = self.lock_state();
        self.ensure_process(&mut state, workspace)?;
        let refreshed = state
            .process
            .as_mut()
            .expect("ensure_process installs PET")
            .rpc("petanque/refresh_workspace", json!({}));
        match refreshed {
            Ok(Value::Null) => Ok(()),
            Ok(_) => {
                self.replace_locked(&mut state, workspace)?;
                Ok(())
            }
            Err(_) => {
                self.replace_locked(&mut state, workspace)?;
                Ok(())
            }
        }
    }

    fn materialize_run(&self, value: Value) -> Result<PetExecution, PetError> {
        let run = match results::parse_run_result(&value) {
            Ok(run) => run,
            Err(error) => {
                self.poison_if_invalidated(&error);
                return Err(error);
            }
        };
        let state = PetStateId::new(run.st);
        let goals = match self.goals(state) {
            Ok(goals) => goals,
            Err(error) => {
                let _ = self.release_states(&[state]);
                return Err(error);
            }
        };
        Ok(PetExecution {
            state,
            proof_finished: run.proof_finished,
            goals,
        })
    }

    fn call_in_workspace(
        &self,
        workspace: &PetWorkspace,
        method: &str,
        params: Value,
    ) -> Result<Value, PetError> {
        let mut state = self.lock_state();
        self.ensure_process(&mut state, workspace)?;
        let result = state
            .process
            .as_mut()
            .expect("PET exists")
            .rpc(method, params);
        if result.as_ref().is_err_and(PetError::invalidates_process) {
            state.process.take();
            state.workspace.take();
        }
        result
    }

    fn call_live(&self, method: &str, params: Value) -> Result<Value, PetError> {
        self.call_live_with_timeout(method, params, None)
    }

    fn call_live_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Option<std::time::Duration>,
    ) -> Result<Value, PetError> {
        let mut state = self.lock_state();
        let Some(process) = state.process.as_mut() else {
            return Err(PetError::ProcessLost("process is not running".into()));
        };
        let result = process.rpc_with_timeout(method, params, timeout);
        if result.as_ref().is_err_and(PetError::invalidates_process) {
            state.process.take();
            state.workspace.take();
        }
        result
    }

    fn ensure_process(
        &self,
        state: &mut ActorState,
        workspace: &PetWorkspace,
    ) -> Result<(), PetError> {
        if state.process.is_none() {
            let mut process = PetProcess::spawn(&workspace.root, &self.binary)?;
            handshake(&mut process)?;
            state.process = Some(process);
            state.workspace = None;
        }
        if state.workspace.as_ref() != Some(workspace) {
            let load_paths = workspace
                .load_paths
                .iter()
                .map(|mapping| {
                    json!({
                        "physical": mapping.physical,
                        "logical": mapping.logical.0.join("."),
                        "implicit": mapping.implicit,
                    })
                })
                .collect::<Vec<_>>();
            let result = state.process.as_mut().expect("PET exists").rpc(
                "petanque/setWorkspace",
                json!({
                    "debug": false,
                    "root": file_uri(&workspace.root),
                    "load_paths": load_paths,
                }),
            );
            if result.as_ref().is_err_and(PetError::invalidates_process) {
                state.process.take();
                state.workspace.take();
            }
            let value = result?;
            if !value.is_null() {
                let error = PetError::Protocol("setWorkspace returned a non-null result".into());
                state.process.take();
                state.workspace.take();
                return Err(error);
            }
            state.workspace = Some(workspace.clone());
        }
        Ok(())
    }

    fn replace_locked(
        &self,
        state: &mut ActorState,
        workspace: &PetWorkspace,
    ) -> Result<(), PetError> {
        state.process.take();
        state.workspace.take();
        self.ensure_process(state, workspace)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ActorState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A syntactically valid but schema-invalid response is still a transport
    /// failure: the child may have violated the protocol contract, and its
    /// state table cannot be trusted for replay.  Drop it before the next
    /// operation can reuse the process.  MCP serializes project operations, so
    /// this cannot race a replacement admitted for the same actor.
    fn poison_if_invalidated(&self, error: &PetError) {
        if error.invalidates_process() {
            let mut state = self.lock_state();
            state.process.take();
            state.workspace.take();
        }
    }
}

#[derive(Deserialize)]
struct ReleaseResponse {
    released: Vec<u64>,
    missing: Vec<u64>,
}

struct PetProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<StderrTail>>,
    stderr_reader: Option<JoinHandle<()>>,
    lifeline: Option<PetLifeline>,
    next_id: u64,
}

/// Keeps the Linux thread that forked PET alive for exactly the child's
/// lifetime.  Linux binds `PR_SET_PDEATHSIG` to the creating *thread*, not to
/// the surrounding process; callers may themselves be short-lived worker
/// threads.
struct PetLifeline {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for PetLifeline {
    fn drop(&mut self) {
        // Closing the channel is also a stop signal, but an explicit value
        // documents the normal shutdown path.  Join before returning so a
        // completed PET cannot leave a launcher thread behind.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Bounded diagnostics from the PET child.  PET's stdout is the JSON-RPC
/// channel, so stderr is the only safe place to collect an OCaml backtrace or
/// fatal-signal report.  The reader runs independently; otherwise a verbose
/// PET failure could fill the stderr pipe and make the transport failure look
/// like a stdin deadlock.
const MAX_STDERR_BYTES: usize = 16 * 1024;

#[derive(Default)]
struct StderrTail {
    bytes: VecDeque<u8>,
}

impl StderrTail {
    fn append(&mut self, bytes: &[u8]) {
        let keep = MAX_STDERR_BYTES.min(bytes.len());
        if bytes.len() > keep {
            self.bytes.clear();
        }
        for byte in &bytes[bytes.len() - keep..] {
            if self.bytes.len() == MAX_STDERR_BYTES {
                self.bytes.pop_front();
            }
            self.bytes.push_back(*byte);
        }
    }

    fn text(&self) -> String {
        let mut bytes = Vec::with_capacity(self.bytes.len());
        let (first, second) = self.bytes.as_slices();
        bytes.extend_from_slice(first);
        bytes.extend_from_slice(second);
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }
}

fn drain_stderr(mut stderr: ChildStderr, capture: Arc<Mutex<StderrTail>>) {
    let mut buffer = [0_u8; 4096];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(length) => {
                let mut tail = capture
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                tail.append(&buffer[..length]);
            }
        }
    }
}

impl PetProcess {
    fn spawn(workspace: &Path, binary: &Path) -> Result<Self, PetError> {
        let mut command = Command::new(binary);
        command
            .arg("--http_headers=yes")
            .arg("--root")
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_pet_command(&mut command);
        let (mut child, lifeline) = spawn_pet_child(command).map_err(|error| {
            PetError::Environment(format!("executable is unavailable: {error}"))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            terminate_child(&mut child);
            PetError::ProcessLost("input channel is unavailable".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            terminate_child(&mut child);
            PetError::ProcessLost("output channel is unavailable".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            terminate_child(&mut child);
            PetError::ProcessLost("diagnostic channel is unavailable".into())
        })?;
        let stderr_capture = Arc::new(Mutex::new(StderrTail::default()));
        let capture = Arc::clone(&stderr_capture);
        let stderr_reader = std::thread::Builder::new()
            .name("rocq-pet-stderr".into())
            .spawn(move || drain_stderr(stderr, capture))
            .map_err(|error| {
                terminate_child(&mut child);
                PetError::Environment(format!("diagnostic reader is unavailable: {error}"))
            })?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr: stderr_capture,
            stderr_reader: Some(stderr_reader),
            lifeline,
            next_id: 1,
        })
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, PetError> {
        self.rpc_with_timeout(method, params, None)
    }

    fn rpc_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Option<std::time::Duration>,
    ) -> Result<Value, PetError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| PetError::Protocol("PET request id space exhausted".into()))?;
        let request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }))
        .map_err(|_| PetError::Invalid("request encoding failed".into()))?;
        if request.len() > MAX_REQUEST_BYTES {
            return Err(PetError::Invalid("request is oversized".into()));
        }
        let deadline = timeout.map(ResponseDeadline::new).transpose()?;
        if cancellation::request_cancelled() {
            terminate_child(&mut self.child);
            return Err(PetError::Cancelled);
        }
        write_frame(&mut self.stdin, &request)
            .map_err(|error| self.transport_error("stdin write", error))?;
        let result = read_response(&mut self.stdout, id, deadline);
        if result
            .as_ref()
            .is_err_and(|error| matches!(error, PetError::Cancelled | PetError::TimedOut { .. }))
        {
            // Design note: PET's stdio loop cannot read a cancellation message
            // while Rocq is executing. Killing this epoch is the only prompt,
            // correctness-preserving interruption: exported IDs are discarded
            // and MCP retains proof text for lazy replay.
            terminate_child(&mut self.child);
        }
        result.map_err(|error| self.annotate_transport(error))
    }

    fn annotate_transport(&mut self, error: PetError) -> PetError {
        if !error.is_transport_loss() {
            return error;
        }
        let detail = self.process_diagnostic();
        match error {
            PetError::ProcessLost(message) => PetError::ProcessLost(format!("{message}; {detail}")),
            PetError::Protocol(message) => PetError::Protocol(format!("{message}; {detail}")),
            PetError::OutputOverflow => {
                PetError::Protocol(format!("response exceeded the output limit; {detail}"))
            }
            other => other,
        }
    }

    fn transport_error(&mut self, operation: &str, error: io::Error) -> PetError {
        PetError::ProcessLost(format!(
            "connection {operation} failed: {error}; {}",
            self.process_diagnostic()
        ))
    }

    fn process_diagnostic(&mut self) -> String {
        // Pipe EOF/EPIPE can become visible a few scheduler ticks before
        // `waitpid`. A short diagnostics-only grace captures the real exit
        // code without imposing a correctness timeout on PET execution.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(25);
        let observed = loop {
            match self.child.try_wait() {
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                result => break result,
            }
        };
        let (status, exited) = match observed {
            Ok(Some(status)) => (format!("child exited with {status}"), true),
            Ok(None) => ("child is still running".to_owned(), false),
            Err(error) => (format!("child status unavailable: {error}"), false),
        };
        // Once the process has exited, joining the drain is nonblocking and
        // guarantees that a fatal OCaml diagnostic is not lost to a race
        // between `waitpid` and the stderr reader.
        if exited && let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        let stderr = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .text();
        if stderr.is_empty() {
            status
        } else {
            format!("{status}; stderr: {stderr}")
        }
    }
}

impl Drop for PetProcess {
    fn drop(&mut self) {
        terminate_child(&mut self.child);
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        // Exit the stable launcher only after PET has been killed and reaped;
        // its parent-death signal is then harmless rather than the mechanism
        // used for normal shutdown.
        self.lifeline.take();
    }
}

/// Apply child-only process controls. PET owns a separate process group so
/// normal shutdown can terminate descendants, while Linux additionally kills
/// PET if the complete MCP process disappears.
fn configure_pet_command(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            let server_pid = unsafe { nix::libc::getpid() };
            // Design note: `PR_SET_PDEATHSIG` observes the thread that calls
            // `fork`, so `spawn_pet_child` performs this spawn on a stable
            // lifeline thread rather than on Tokio's ephemeral blocking
            // worker. Checking `getppid` closes the process-death race between
            // `fork` and this pre-exec hook.
            unsafe {
                command.pre_exec(move || {
                    if nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL, 0, 0, 0)
                        == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if nix::libc::getppid() != server_pid {
                        // `pre_exec` may call only async-signal-safe code; use
                        // a raw errno rather than allocating a custom error.
                        return Err(io::Error::from_raw_os_error(nix::libc::ECHILD));
                    }
                    Ok(())
                });
            }
        }
    }
}

/// Spawn PET and return both its process handle and the stable creator-thread
/// owner required by Linux parent-death semantics. On non-Linux platforms no
/// parent-death signal is installed, so a launcher thread would add no value.
fn spawn_pet_child(mut command: Command) -> io::Result<(Child, Option<PetLifeline>)> {
    #[cfg(target_os = "linux")]
    {
        let (child_tx, child_rx) = mpsc::sync_channel(0);
        let (stop_tx, stop_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("rocq-pet-lifeline".into())
            .spawn(move || match command.spawn() {
                Ok(child) => match child_tx.send(Ok(child)) {
                    Ok(()) => {
                        // Both an explicit message and sender disconnection
                        // end the lifeline. In either case thread termination
                        // activates PET's parent-death signal.
                        let _ = stop_rx.recv();
                    }
                    Err(mpsc::SendError(Ok(mut child))) => {
                        // The caller vanished before taking ownership.
                        terminate_child(&mut child);
                    }
                    Err(mpsc::SendError(Err(_))) => {}
                },
                Err(error) => {
                    let _ = child_tx.send(Err(error));
                }
            })?;
        let child = match child_rx.recv() {
            Ok(Ok(child)) => child,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error);
            }
            Err(_) => {
                let _ = thread.join();
                return Err(io::Error::other("launcher exited before spawn"));
            }
        };
        let lifeline = PetLifeline {
            stop: Some(stop_tx),
            thread: Some(thread),
        };
        Ok((child, Some(lifeline)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        command.spawn().map(|child| (child, None))
    }
}

fn handshake(process: &mut PetProcess) -> Result<(), PetError> {
    let value = process.rpc("petanque/capabilities", json!({}))?;
    let capabilities: Vec<String> = serde_json::from_value(value)
        .map_err(|_| PetError::Protocol("PET capabilities response is invalid".into()))?;
    let missing = REQUIRED_CAPABILITIES
        .iter()
        .filter(|required| !capabilities.iter().any(|value| value == **required))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(PetError::Environment(format!(
            "required capabilities are missing: {}",
            missing.join(", ")
        )))
    }
}

fn decode_goals(value: &Value) -> Result<PetGoals, PetError> {
    if value.is_null() {
        Ok(PetGoals::outside_proof())
    } else {
        results::parse_goals(value, true)
    }
}

fn write_frame(writer: &mut ChildStdin, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

#[derive(Clone, Copy)]
struct ResponseDeadline {
    at: std::time::Instant,
    timeout_ms: u64,
}

impl ResponseDeadline {
    fn new(timeout: std::time::Duration) -> Result<Self, PetError> {
        if timeout.is_zero() {
            return Err(PetError::Invalid("timeout must be positive".into()));
        }
        let timeout_ms = u64::try_from(timeout.as_millis())
            .map_err(|_| PetError::Invalid("timeout is too large".into()))?;
        if timeout_ms == 0 {
            return Err(PetError::Invalid(
                "timeout must be at least one millisecond".into(),
            ));
        }
        let at = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| PetError::Invalid("timeout is too large".into()))?;
        Ok(Self { at, timeout_ms })
    }

    fn error(self) -> PetError {
        PetError::TimedOut {
            timeout_ms: self.timeout_ms,
        }
    }
}

fn read_response(
    reader: &mut BufReader<ChildStdout>,
    request_id: u64,
    deadline: Option<ResponseDeadline>,
) -> Result<Value, PetError> {
    let mut header = Vec::new();
    loop {
        let mut line = Vec::new();
        read_line_bounded(reader, &mut line, deadline)?;
        if line == b"\n" || line == b"\r\n" {
            if header.is_empty() {
                continue;
            }
            break;
        }
        header.extend_from_slice(&line);
        if header.len() > MAX_HEADER_BYTES {
            return Err(PetError::OutputOverflow);
        }
    }
    let mut content_length = None;
    for line in header.split(|byte| *byte == b'\n' || *byte == b'\r') {
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        if !trim_ascii_space(&line[..colon]).eq_ignore_ascii_case(b"Content-Length") {
            continue;
        }
        let value = std::str::from_utf8(trim_ascii_space(&line[colon + 1..]))
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| PetError::Protocol("PET Content-Length is invalid".into()))?;
        if content_length
            .replace(value)
            .is_some_and(|old| old != value)
        {
            return Err(PetError::Protocol(
                "PET response has conflicting Content-Length headers".into(),
            ));
        }
    }
    let length = content_length
        .ok_or_else(|| PetError::Protocol("PET response has no Content-Length".into()))?;
    if length > MAX_RESPONSE_BYTES {
        return Err(PetError::OutputOverflow);
    }
    let mut body = vec![0; length];
    read_exact_interruptible(
        reader,
        &mut body,
        deadline,
        &format!("output body read failed after Content-Length {length}"),
    )?;
    let mut deserializer = serde_json::Deserializer::from_slice(&body);
    deserializer.disable_recursion_limit();
    let value = Value::deserialize(&mut deserializer)
        .map_err(|error| PetError::Protocol(format!("PET response JSON is invalid: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET response is not an object".into()))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(PetError::Protocol(
            "PET response has an invalid JSON-RPC version".into(),
        ));
    }
    if object.get("id").and_then(Value::as_u64) != Some(request_id) {
        return Err(PetError::Protocol(
            "PET response id does not match request".into(),
        ));
    }
    if let Some(error) = object.get("error") {
        if !error.is_object() || object.contains_key("result") {
            return Err(PetError::Protocol(
                "PET response has an invalid result/error pair".into(),
            ));
        }
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(|| PetError::Protocol("PET error has no code".into()))?;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| PetError::Protocol("PET error has no message".into()))?
            .to_owned();
        if message.trim().is_empty() {
            return Err(PetError::Protocol("PET error message is empty".into()));
        }
        let diagnostic = error
            .get("data")
            .map(parse_pet_diagnostic)
            .transpose()?
            .flatten();
        return Err(PetError::Remote {
            code,
            kind: PetRemoteKind::from_code(code),
            message,
            diagnostic,
        });
    }
    object
        .get("result")
        .cloned()
        .ok_or_else(|| PetError::Protocol("PET response has no result".into()))
}

/// Decode the optional range that PET attaches to a rejected proof fragment.
/// The JSON shell currently transports this through its standard feedback
/// envelope; accepting both a list and a single object keeps the decoder
/// forward-compatible without interpreting human-readable diagnostics.
fn parse_pet_diagnostic(value: &Value) -> Result<Option<ProofDiagnostic>, PetError> {
    let entries = value
        .as_array()
        .map(|entries| entries.as_slice())
        .unwrap_or_else(|| std::slice::from_ref(value));
    for entry in entries {
        let Some(range) = entry.get("range") else {
            continue;
        };
        if range.is_null() {
            continue;
        }
        let start = range
            .get("start")
            .and_then(|point| point.get("offset"))
            .and_then(Value::as_u64)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or_else(|| PetError::Protocol("PET diagnostic start offset is invalid".into()))?;
        let end = range
            .get("end")
            .or_else(|| range.get("end_"))
            .and_then(|point| point.get("offset"))
            .and_then(Value::as_u64)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or_else(|| PetError::Protocol("PET diagnostic end offset is invalid".into()))?;
        if start > end {
            return Err(PetError::Protocol(
                "PET diagnostic range is reversed".into(),
            ));
        }
        return Ok(Some(ProofDiagnostic {
            byte_start: start,
            byte_end: end,
        }));
    }
    Ok(None)
}

fn trim_ascii_space(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !matches!(*byte, b' ' | b'\t'))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !matches!(*byte, b' ' | b'\t'))
        .map_or(start, |at| at + 1);
    &value[start..end]
}

fn read_line_bounded(
    reader: &mut BufReader<ChildStdout>,
    line: &mut Vec<u8>,
    deadline: Option<ResponseDeadline>,
) -> Result<(), PetError> {
    loop {
        let mut byte = [0];
        read_exact_interruptible(reader, &mut byte, deadline, "output header read failed")?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(());
        }
        if line.len() > MAX_HEADER_BYTES {
            return Err(PetError::OutputOverflow);
        }
    }
}

/// Fill exactly one framed-response component without allowing a blocking
/// stdout read to hide request cancellation or a caller-selected deadline.
/// EOF and I/O errors are classified as transport loss with operation context.
fn read_exact_interruptible(
    reader: &mut BufReader<ChildStdout>,
    output: &mut [u8],
    deadline: Option<ResponseDeadline>,
    operation: &str,
) -> Result<(), PetError> {
    let mut offset = 0;
    while offset < output.len() {
        if reader.buffer().is_empty() {
            wait_for_pet_output(reader, deadline)?;
        }
        match reader.read(&mut output[offset..]) {
            Ok(0) => {
                return Err(PetError::ProcessLost(format!(
                    "{operation}: failed to fill whole buffer"
                )));
            }
            Ok(length) => offset += length,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(PetError::ProcessLost(format!("{operation}: {error}")));
            }
        }
    }
    Ok(())
}

/// Wait until PET stdout can be read, checking the request signal and optional
/// response deadline at least every 25 ms. A returned cancellation/deadline
/// error is consumed by `rpc_with_timeout`, which kills and reaps the epoch.
fn wait_for_pet_output(
    reader: &BufReader<ChildStdout>,
    deadline: Option<ResponseDeadline>,
) -> Result<(), PetError> {
    if !cancellation::request_scope_active() && deadline.is_none() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        loop {
            if cancellation::request_cancelled() {
                return Err(PetError::Cancelled);
            }
            let wait_ms = if let Some(deadline) = deadline {
                let now = std::time::Instant::now();
                if now >= deadline.at {
                    return Err(deadline.error());
                }
                i32::try_from((deadline.at - now).as_millis().clamp(1, 25)).unwrap_or(25)
            } else {
                25
            };
            let mut descriptor = nix::libc::pollfd {
                fd: reader.get_ref().as_raw_fd(),
                events: nix::libc::POLLIN | nix::libc::POLLHUP | nix::libc::POLLERR,
                revents: 0,
            };
            // SAFETY: `descriptor` is a valid stack allocation, the count is
            // exactly one, and `poll` does not retain the pointer after return.
            let result = unsafe { nix::libc::poll(&mut descriptor, 1, wait_ms) };
            if result > 0 {
                return Ok(());
            }
            if result == 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(PetError::ProcessLost(format!(
                    "output readiness wait failed: {error}"
                )));
            }
        }
    }

    #[cfg(not(unix))]
    {
        if cancellation::request_cancelled() {
            Err(PetError::Cancelled)
        } else if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline.at) {
            Err(deadline.unwrap().error())
        } else {
            Ok(())
        }
    }
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.' | b'~') {
            uri.push(*byte as char);
        } else {
            uri.push('%');
            uri.push(hex((*byte >> 4) & 0xf));
            uri.push(hex(*byte & 0xf));
        }
    }
    uri
}

fn hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

/// Convert a UTF-8 byte anchor into PET's zero-based UTF-16 LSP position.
fn source_position(source: &str, offset: usize) -> Result<Value, PetError> {
    let prefix = source
        .get(..offset)
        .ok_or_else(|| PetError::Invalid("source position is no longer valid".into()))?;
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .encode_utf16()
        .count();
    Ok(json!({"line": line, "character": character}))
}

fn terminate_child(child: &mut Child) {
    // `process_diagnostic` may already have reaped an exited child. Avoid a
    // process-group signal with a stale numeric PID, which could otherwise be
    // reused between diagnosis and Drop.
    if child.try_wait().is_ok_and(|status| status.is_some()) {
        return;
    }
    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{Signal, killpg},
            unistd::Pid,
        };
        let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

mod cancellation;
mod results;

pub use cancellation::{
    PetRequestCancellation, commit_request, request_cancelled, with_request_cancellation,
};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write a minimal framed JSON-RPC PET double. `after_handshake` runs
    /// immediately after the capabilities response; `on_request` owns every
    /// later request and must either reply or terminate the process.
    fn fake_pet(directory: &tempfile::TempDir, after_handshake: &str, on_request: &str) -> PathBuf {
        fn python_block(source: &str) -> String {
            let source = if source.is_empty() { "pass" } else { source };
            source
                .lines()
                .map(|line| format!("        {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        }

        let script = directory.path().join("fake-pet.py");
        let capabilities = serde_json::to_string(REQUIRED_CAPABILITIES).unwrap();
        let source = format!(
            r#"#!/usr/bin/env python3
import json
import os
import signal
import sys

CAPABILITIES = {capabilities}

def read_request():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line in (b"\n", b"\r\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        if name.lower() == "content-length":
            length = int(value.strip())
    return json.loads(sys.stdin.buffer.read(length))

def reply(request, result):
    payload = {{"jsonrpc":"2.0", "id":request["id"], "result":result}}
    body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(("Content-Length: %d\r\n\r\n" % len(body)).encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    request = read_request()
    if request["method"] == "petanque/capabilities":
        reply(request, CAPABILITIES)
{after_handshake}
    else:
{on_request}
"#,
            after_handshake = python_block(after_handshake),
            on_request = python_block(on_request),
        );
        fs::write(&script, source).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn wait_until_exited(child: &mut Child) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "fake PET did not exit"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pet_outlives_the_ephemeral_thread_that_requested_spawn() {
        let directory = tempfile::tempdir().unwrap();
        let script = fake_pet(&directory, "", r#"reply(request, {"alive": True})"#);
        let workspace = directory.path().to_owned();
        let (process_tx, process_rx) = mpsc::sync_channel(0);
        let worker = std::thread::spawn(move || {
            let mut process = PetProcess::spawn(&workspace, &script).unwrap();
            handshake(&mut process).unwrap();
            process_tx.send(process).unwrap();
        });
        let mut process = process_rx.recv().unwrap();
        worker.join().unwrap();

        // Regression: before the stable launcher existed, Linux treated the
        // now-exited worker as PET's pdeath parent and sent SIGKILL here.
        assert_eq!(
            process.rpc("test/alive", json!({})).unwrap(),
            json!({"alive": true})
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stdin_epipe_reports_signal_and_bounded_stderr_tail() {
        let directory = tempfile::tempdir().unwrap();
        let after_handshake = format!(
            "sys.stderr.write('BEGIN-MUST-BE-DROPPED\\n' + 'x' * {} + '\\nEND-OF-STDERR\\n')\n\
             sys.stderr.flush()\n\
             os.kill(os.getpid(), signal.SIGKILL)",
            MAX_STDERR_BYTES * 2
        );
        let script = fake_pet(&directory, &after_handshake, "reply(request, None)");
        let mut process = PetProcess::spawn(directory.path(), &script).unwrap();
        handshake(&mut process).unwrap();
        wait_until_exited(&mut process.child);

        let error = process.rpc("test/after-exit", json!({})).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Broken pipe (os error 32)"), "{message}");
        assert!(message.contains("signal: 9 (SIGKILL)"), "{message}");
        assert!(message.contains("END-OF-STDERR"), "{message}");
        assert!(!message.contains("BEGIN-MUST-BE-DROPPED"), "{message}");
        assert!(message.len() <= MAX_STDERR_BYTES + 256, "{message}");
    }

    #[test]
    fn stdout_eof_reports_exit_status_and_stderr() {
        let directory = tempfile::tempdir().unwrap();
        let script = fake_pet(
            &directory,
            "",
            "sys.stderr.write('EOF-DIAGNOSTIC\\n')\n\
             sys.stderr.flush()\n\
             sys.exit(23)",
        );
        let mut process = PetProcess::spawn(directory.path(), &script).unwrap();
        handshake(&mut process).unwrap();

        let error = process.rpc("test/eof", json!({})).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("output header read failed"), "{message}");
        assert!(message.contains("failed to fill whole buffer"), "{message}");
        assert!(message.contains("exit status: 23"), "{message}");
        assert!(message.contains("EOF-DIAGNOSTIC"), "{message}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn admitted_request_cancellation_terminates_a_hung_pet_epoch() {
        let directory = tempfile::tempdir().unwrap();
        let script = fake_pet(
            &directory,
            "",
            "sys.stderr.write('HUNG-RPC-STARTED\\n')\n\
             sys.stderr.flush()\n\
             __import__('time').sleep(60)",
        );
        let mut process = PetProcess::spawn(directory.path(), &script).unwrap();
        handshake(&mut process).unwrap();
        let process_group = process.child.id() as i32;
        let cancellation = PetRequestCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (result_tx, result_rx) = mpsc::sync_channel(0);
        let worker = std::thread::spawn(move || {
            let error = with_request_cancellation(&worker_cancellation, || {
                process.rpc("test/hang", json!({})).unwrap_err()
            });
            result_tx.send((process, error)).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        cancellation.cancel();
        let (mut process, error) = result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap_or_else(|_| {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-process_group),
                    nix::sys::signal::Signal::SIGKILL,
                );
                panic!("cancelled PET request did not stop")
            });
        worker.join().unwrap();
        assert_eq!(error, PetError::Cancelled);
        assert!(process.child.try_wait().unwrap().is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proof_fragment_deadline_terminates_a_hung_pet_epoch() {
        let directory = tempfile::tempdir().unwrap();
        let script = fake_pet(&directory, "", "__import__('time').sleep(60)");
        let mut process = PetProcess::spawn(directory.path(), &script).unwrap();
        handshake(&mut process).unwrap();
        let started = std::time::Instant::now();
        let error = process
            .rpc_with_timeout(
                "test/hang",
                json!({}),
                Some(std::time::Duration::from_millis(40)),
            )
            .unwrap_err();
        assert_eq!(error, PetError::TimedOut { timeout_ms: 40 });
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(process.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn stable_remote_codes_are_decoded_without_message_inspection() {
        assert_eq!(
            PetRemoteKind::from_code(-32008),
            PetRemoteKind::ReferenceNotFound
        );
        assert_eq!(PetRemoteKind::from_code(-32003), PetRemoteKind::Coq);
        assert_eq!(
            PetRemoteKind::from_code(-32601),
            PetRemoteKind::MethodNotFound
        );
        assert_eq!(
            PetRemoteKind::from_code(-32999),
            PetRemoteKind::Unknown(-32999)
        );
    }

    #[test]
    fn public_messages_separate_rocq_diagnostics_from_transport_details() {
        let semantic = PetError::Remote {
            code: -32008,
            kind: PetRemoteKind::ReferenceNotFound,
            message: "Reference_not_found: pt_generated".into(),
            diagnostic: None,
        };
        assert_eq!(
            semantic.public_message(),
            "Reference_not_found: pt_generated"
        );
        assert!(!semantic.public_message().contains("-32008"));
        assert!(!semantic.public_message().contains("PET"));

        let unsupported = PetError::Remote {
            code: -32601,
            kind: PetRemoteKind::MethodNotFound,
            message: "method petanque/legacy not found".into(),
            diagnostic: None,
        };
        assert_eq!(
            unsupported.public_message(),
            "a required operation is not supported"
        );
        assert!(!unsupported.public_message().contains("petanque"));
        assert!(!unsupported.public_message().contains("-32601"));

        for (kind, expected) in [
            (PetRemoteKind::Anomaly, "internal proof execution anomaly"),
            (PetRemoteKind::System, "proof execution system failure"),
            (
                PetRemoteKind::Unknown(-32999),
                "unrecognized proof execution failure",
            ),
        ] {
            let error = PetError::Remote {
                code: -32999,
                kind,
                message: "implementation detail: PET crashed".into(),
                diagnostic: None,
            };
            assert_eq!(error.public_message(), expected);
            assert!(!error.public_message().contains("PET"));
            assert!(!error.public_message().contains("-32999"));
        }

        for error in [
            PetError::Protocol("bad JSON-RPC response".into()),
            PetError::OutputOverflow,
        ] {
            let message = error.public_message();
            assert!(!message.contains("PET"), "{message}");
            assert!(!message.contains("JSON-RPC"), "{message}");
        }
        let transport =
            PetError::ProcessLost("PET petanque connection failed: Broken pipe (-32008)".into());
        let message = transport.public_message();
        assert!(message.contains("Broken pipe"));
        assert!(!message.contains("PET"));
        assert!(!message.contains("petanque"));
        assert!(!message.contains("-32008"));

        // The implementation-facing formatter remains useful for logs; only
        // the MCP projection is intentionally backend-neutral.
        assert!(semantic.to_string().contains("-32008"));
    }

    #[test]
    fn remote_error_data_preserves_pet_byte_range() {
        let data = json!([{
            "range": {
                "start": {"line": 0, "character": 12, "offset": 12},
                "end": {"line": 0, "character": 20, "offset": 20}
            },
            "level": 1,
            "text": "bad tactic"
        }]);
        assert_eq!(
            parse_pet_diagnostic(&data).unwrap(),
            Some(ProofDiagnostic {
                byte_start: 12,
                byte_end: 20,
            })
        );
        for malformed in [
            json!({"range":{"start":{"offset":20},"end":{"offset":12}}}),
            json!({"range":{"start":{},"end":{"offset":12}}}),
        ] {
            assert!(matches!(
                parse_pet_diagnostic(&malformed),
                Err(PetError::Protocol(_))
            ));
        }
    }

    #[test]
    fn malformed_typed_response_discards_the_pet_process() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("fake-pet.py");
        let source = r#"#!/usr/bin/env python3
import json
import sys

while True:
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line in (b"\n", b"\r\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        if name.lower() == "content-length":
            length = int(value.strip())
    request = json.loads(sys.stdin.buffer.read(length))
    method = request["method"]
    if method == "petanque/capabilities":
        result = [
            "document_declarations_v2", "dune_workspace_v1",
            "insertion_point_v1", "atomic_run_v1", "release_states_v1",
            "refresh_workspace_v1", "structured_assumptions_v1", "typed_errors_v1",
            "diagnostic_ranges_v1"]
    elif method == "petanque/setWorkspace":
        result = None
    elif method == "petanque/document_declarations":
        result = {"not": "the declared response schema"}
    else:
        result = None
    payload = {"jsonrpc":"2.0", "id":request["id"], "result":result}
    body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(("Content-Length: %d\r\n\r\n" % len(body)).encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
"#;
        fs::write(&script, source).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let actor = PetActor {
            state: Mutex::new(ActorState::default()),
            binary: script,
        };
        let workspace = PetWorkspace {
            root: directory.path().to_owned(),
            load_paths: Vec::new(),
        };
        let error = actor
            .document_declarations(&workspace, &directory.path().join("A.v"))
            .unwrap_err();
        assert!(matches!(error, PetError::Protocol(_)));
        let state = actor.lock_state();
        assert!(state.process.is_none());
        assert!(state.workspace.is_none());
    }

    #[test]
    fn refresh_rpc_failure_replaces_the_pet_process() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("spawns");
        let script = directory.path().join("fake-pet.py");
        let marker_literal = serde_json::to_string(&marker.to_string_lossy()).unwrap();
        let source = format!(
            r#"#!/usr/bin/env python3
import json
import sys

with open({marker_literal}, "a", encoding="utf-8") as marker:
    marker.write("spawn\n")

while True:
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line in (b"\n", b"\r\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        if name.lower() == "content-length":
            length = int(value.strip())
    request = json.loads(sys.stdin.buffer.read(length))
    method = request["method"]
    if method == "petanque/capabilities":
        payload = {{"jsonrpc":"2.0", "id":request["id"], "result":[
            "document_declarations_v2", "dune_workspace_v1",
            "insertion_point_v1", "atomic_run_v1", "release_states_v1",
            "refresh_workspace_v1", "state_count_v1",
            "structured_assumptions_v1", "typed_errors_v1", "diagnostic_ranges_v1"]}}
    elif method == "petanque/refresh_workspace":
        payload = {{"jsonrpc":"2.0", "id":request["id"],
                   "error":{{"code":-32000, "message":"forced refresh failure"}}}}
    elif method == "petanque/state_count":
        payload = {{"jsonrpc":"2.0", "id":request["id"], "result":0}}
    else:
        payload = {{"jsonrpc":"2.0", "id":request["id"], "result":None}}
    body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(("Content-Length: %d\r\n\r\n" % len(body)).encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
"#
        );
        fs::write(&script, source).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let actor = PetActor {
            state: Mutex::new(ActorState::default()),
            binary: script,
        };
        let workspace = PetWorkspace {
            root: directory.path().to_owned(),
            load_paths: Vec::new(),
        };

        actor.refresh_workspace(&workspace).unwrap();
        assert_eq!(actor.diagnostic_state_count().unwrap(), 0);
        assert_eq!(fs::read_to_string(marker).unwrap().lines().count(), 2);
    }
}
