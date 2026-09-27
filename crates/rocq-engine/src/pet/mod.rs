//! Structured Petanque runtime.
//!
//! The runtime owns only process-lifetime PET state.  It never edits the
//! attached project and never serializes a proof trace. Initial load/recovery
//! opens the Dune-selected source directly; ordinary proof steps
//! continue the cached PET state and replay only after state loss.

use crate::{
    CanonicalTactic, DeclarationIdentity, DeclarationInfo, DeclarationKind, DeclarationSource,
    Error, ErrorKind, dune, validate_identity,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufReader, Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

// Design note: this bound must dominate both the 1 MiB public expression
// limit and the 4 MiB replay limit after JSON-RPC framing/escaping. Otherwise
// an input accepted by Engine validation fails later as a configuration fault.
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
// Design note: PET document-declaration and vernacular AST responses for
// ordinary large Rocq source files exceed 64 KiB. Keep a transport bound, but
// size it for parsed project metadata rather than one interactive goal.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8192;
const MAX_REPLAY_ACTIONS: usize = 4096;
const MAX_REPLAY_BYTES: usize = 4 * 1024 * 1024;

fn configured_pet_binary() -> PathBuf {
    std::env::var_os("ROCQ_PET_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("pet"))
}

/// One declaration record returned by PET's document-level API.  PET has
/// already traversed the checked document, so this wrapper only validates the
/// wire shape and attaches the Dune logical-library prefix.  In particular it
/// does not reconstruct scopes or issue one RPC per sentence.
#[derive(Clone, Debug, Deserialize)]
struct PetDocumentDeclaration {
    qualified_path: Vec<String>,
    kind: String,
    range: PetDocumentRange,
    statement: String,
}

#[derive(Clone, Debug, Deserialize)]
struct PetDocumentRange {
    start: usize,
    #[serde(rename = "end")]
    end_: usize,
}

/// PET AST node used only while locating a writeback insertion point.  It is
/// intentionally not part of document declaration indexing; that path now
/// consumes [`PetDocumentDeclaration`] records in one request.
#[derive(Clone, Debug)]
struct PetDeclarationNode {
    name: String,
    modules: Vec<String>,
}

/// Errors are deliberately transport/domain typed so callers can distinguish
/// runtime-capacity/recovery failure from a native tactic rejection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PetError {
    Invalid(String),
    /// PET parsed a declaration, but its semantic identity differs from the
    /// identity requested by the engine.  This is a user-facing declaration
    /// error, not a malformed PET request or runtime configuration failure.
    InvalidDeclaration(String),
    UnsafeProofCommand(String),
    Environment(String),
    ProcessFailure(String),
    Timeout,
    /// A PET child accepted a request but did not produce a response before
    /// the per-RPC safety watchdog expired. This is distinct from waiting for
    /// a free runtime lane (`Timeout`) so the public diagnostic remains
    /// actionable and stable.
    OperationTimeout,
    Protocol(String),
    OutputOverflow,
    Remote {
        code: i64,
        message: String,
    },
    Stale,
}

impl std::fmt::Display for PetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid PET request: {message}"),
            Self::InvalidDeclaration(message) => f.write_str(message),
            Self::UnsafeProofCommand(message) => write!(f, "unsafe proof command: {message}"),
            Self::Environment(message) | Self::ProcessFailure(message) => f.write_str(message),
            Self::Timeout => f.write_str("timed out waiting for PET runtime capacity or recovery"),
            Self::OperationTimeout => f.write_str("PET operation timed out"),
            Self::Protocol(message) => write!(f, "PET protocol failure: {message}"),
            Self::OutputOverflow => f.write_str("PET response exceeded the output limit"),
            Self::Remote { code, message } => write!(f, "PET rejected request ({code}): {message}"),
            Self::Stale => f.write_str("PET state belongs to an older disposable workspace"),
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

/// Source-level trust facts reported by PET's parsed vernacular AST.
/// `explicit_axioms` contains deliberate `Axiom(s)` declarations; `admitted`
/// contains proof declarations closed with `Admitted`. Neither collection is
/// inferred from source keywords or from the wrapper's edit index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetSourceTrust {
    pub(crate) explicit_axioms: Vec<(String, String)>,
    pub(crate) admitted: Vec<String>,
}

/// Kernel assumptions returned by PET after every printed name has been
/// resolved through Rocq `Locate` in the same semantic state. `name` is thus
/// a unique kernel-facing path, never a context-dependent short spelling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetAssumptionReport {
    pub(crate) assumptions: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PetGoals {
    pub(crate) focused: Vec<PetGoal>,
    pub(crate) unfocused: Vec<PetGoal>,
    pub(crate) shelved: Vec<PetGoal>,
    pub(crate) given_up: Vec<PetGoal>,
    pub(crate) proof_mode: bool,
}
impl PetGoals {
    pub(crate) fn all_clear(&self) -> bool {
        self.focused.is_empty()
            && self.unfocused.is_empty()
            && self.shelved.is_empty()
            && self.given_up.is_empty()
    }
}

#[derive(Clone)]
pub(crate) struct PetState {
    pub(crate) process: Arc<PetProcess>,
    pub(crate) instance_epoch: u64,
    pub(crate) st: u64,
    pub(crate) proof_finished: bool,
    pub(crate) goals: PetGoals,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FixedPetQuery {
    Assumptions(String),
    Dependencies(String),
    Search(String),
    About(String),
    Print(String),
    ExpressionType(String),
    Notation(String),
}

/// Direct PET view of a Dune-owned source. No source or workspace is copied.
#[derive(Clone, Debug)]
struct PetDocument {
    workspace_root: PathBuf,
    uri: String,
    key: [u8; 32],
    synthetic_header: Option<String>,
    /// Position at which PET must obtain the state immediately preceding the
    /// declaration header.  For a declaration that is already in the source
    /// this is the header start; for a synthetic declaration it is EOF.
    start_position: Value,
    /// Only a byte-zero start is semantically equivalent to PET's root state.
    /// Falling back at any later position would silently lose imports/modules.
    allow_root_fallback: bool,
}

pub(crate) struct PetProcess {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    serial: Mutex<()>,
    next_id: AtomicU64,
    instance_epoch: AtomicU64,
    alive: AtomicBool,
    document: Mutex<Option<PetDocument>>,
    /// Maximum time a single PET JSON-RPC exchange may remain without a
    /// response.  This is a process-safety bound, not a native build or proof
    /// completion deadline; a timed-out exchange kills only this disposable
    /// PET lane and the immutable trace remains replayable elsewhere.
    rpc_timeout: Duration,
}

impl PetProcess {
    fn spawn(
        workspace_root: PathBuf,
        rpc_timeout: Duration,
        pet_binary: &Path,
    ) -> Result<Arc<Self>, PetError> {
        let mut command = Command::new(pet_binary);
        command
            .arg("--http_headers=yes")
            .arg("--root")
            .arg(&workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Design note: the process group gives Drop/recovery one safe
            // ownership boundary. PET also observes EOF when its owning
            // engine disappears because stdin is an engine-owned pipe.
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|_| PetError::Environment("PET executable is unavailable".into()))?;
        // A successfully spawned child is an owned resource even when the
        // requested pipes are unexpectedly absent.  Tear it down before
        // returning the construction error; otherwise this early path leaks
        // a PET process (and, on Unix, its process group).
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                terminate_child(&mut child);
                return Err(PetError::ProcessFailure("PET stdin is unavailable".into()));
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                terminate_child(&mut child);
                return Err(PetError::ProcessFailure("PET stdout is unavailable".into()));
            }
        };
        let process = Arc::new(Self {
            child: Mutex::new(child),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            serial: Mutex::new(()),
            next_id: AtomicU64::new(1),
            instance_epoch: AtomicU64::new(0),
            alive: AtomicBool::new(true),
            document: Mutex::new(None),
            rpc_timeout,
        });
        Ok(process)
    }

    fn replay(
        self: &Arc<Self>,
        document: PetDocument,
        _root: &DeclarationSource,
        actions: &[CanonicalTactic],
    ) -> Result<PetState, PetError> {
        if actions.len() > MAX_REPLAY_ACTIONS
            || actions
                .iter()
                .map(|action| action.0.len())
                .try_fold(0usize, usize::checked_add)
                .is_none_or(|bytes| bytes > MAX_REPLAY_BYTES)
        {
            return Err(PetError::Invalid("replay trace is oversized".into()));
        }
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.alive.load(Ordering::Acquire) {
            return Err(PetError::ProcessFailure("PET process is not alive".into()));
        }
        self.configure_document_locked(document)?;
        let instance_epoch = self.instance_epoch.load(Ordering::Acquire);
        let (uri, synthetic_header, start_position, allow_root_fallback) = self.document_start()?;
        let header = synthetic_header
            .ok_or_else(|| PetError::Protocol("PET declaration replay has no header".into()))?;
        let initial = match self.rpc_locked(
            "petanque/get_state_at_pos",
            json!({"uri": uri, "position": start_position}),
        ) {
            Ok(state) => state,
            Err(_) if allow_root_fallback => {
                self.rpc_locked("petanque/get_root_state", json!({"uri": uri}))?
            }
            Err(error) => return Err(error),
        };
        let initial = parse_run_result(&initial)?;
        let header = if header.trim_end().ends_with('.') {
            header
        } else {
            format!("{header}.")
        };
        let start = self
            .rpc_locked("petanque/run", json!({"st": initial.st, "tac": header}))
            .map_err(declaration_start_error)?;
        let mut state = parse_run_result(&start)?;
        let mut goals = self.goals_locked(state.st, state.proof_finished)?;
        for action in actions {
            // PET's AST is the authority for command classification. This is
            // not a textual tactic parser: it rejects global vernacular and
            // control commands before they can mutate the disposable proof
            // document, while accepting plugin-defined proof commands that
            // PET identifies as proof-local.
            let parsed =
                self.rpc_locked("petanque/ast", json!({"st": state.st, "text": action.0}))?;
            if !pet_proof_command(&parsed) {
                return Err(PetError::UnsafeProofCommand(
                    "PET parsed a proof command as global vernacular or a control command".into(),
                ));
            }
            let response =
                self.rpc_locked("petanque/run", json!({"st": state.st, "tac": action.0}))?;
            state = parse_run_result(&response)?;
            goals = self.goals_locked(state.st, state.proof_finished)?;
            // Design note: `admit` is a valid tactic AST, but PET reports its
            // abandoned obligations in given_up. Reject the edge before the
            // forest stores it, rather than guessing from the command text.
            if !goals.given_up.is_empty() {
                return Err(PetError::UnsafeProofCommand(
                    "PET reports given-up goals after this command".into(),
                ));
            }
        }
        Ok(PetState {
            process: Arc::clone(self),
            instance_epoch,
            st: state.st,
            proof_finished: state.proof_finished,
            goals,
        })
    }

    /// Continue an already checked proof state in the same PET document.
    /// The caller must hold the lane lease; this method never creates a new
    /// document or replays the preceding tactic prefix.
    fn run_tactic(&self, state: &PetState, tactic: &CanonicalTactic) -> Result<PetState, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.alive.load(Ordering::Acquire)
            || state.instance_epoch != self.instance_epoch.load(Ordering::Acquire)
        {
            return Err(PetError::Stale);
        }
        let parsed = self.rpc_locked(
            "petanque/ast",
            json!({
                "st": state.st,
                "text": tactic.0,
            }),
        )?;
        if !pet_proof_command(&parsed) {
            return Err(PetError::UnsafeProofCommand(
                "PET parsed a proof command as global vernacular or a control command".into(),
            ));
        }
        let response = self.rpc_locked("petanque/run", json!({"st": state.st, "tac": tactic.0}))?;
        let next = parse_run_result(&response)?;
        let goals = self.goals_locked(next.st, next.proof_finished)?;
        if !goals.given_up.is_empty() {
            return Err(PetError::UnsafeProofCommand(
                "PET reports given-up goals after this command".into(),
            ));
        }
        Ok(PetState {
            process: Arc::clone(&state.process),
            instance_epoch: state.instance_epoch,
            st: next.st,
            proof_finished: next.proof_finished,
            goals,
        })
    }

    /// Reap an externally terminated child before a lane hands it out again.
    /// PET has no reliable out-of-band health notification, so the lane does
    /// this non-blocking probe when it is selected for a replay.
    fn exited(&self) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return true;
        }
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match child.try_wait() {
            Ok(Some(_status)) => {
                self.alive.store(false, Ordering::Release);
                self.document
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                true
            }
            Ok(None) | Err(_) => false,
        }
    }

    fn run_fixed(&self, state: &PetState, query: FixedPetQuery) -> Result<String, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.alive.load(Ordering::Acquire)
            || state.instance_epoch != self.instance_epoch.load(Ordering::Acquire)
        {
            return Err(PetError::Stale);
        }
        self.run_fixed_locked(state.st, query)
    }

    fn close_and_query_assumptions(
        &self,
        state: &PetState,
        terminator: &str,
        name: &str,
    ) -> Result<PetAssumptionReport, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.alive.load(Ordering::Acquire)
            || state.instance_epoch != self.instance_epoch.load(Ordering::Acquire)
        {
            return Err(PetError::Stale);
        }
        let response =
            self.rpc_locked("petanque/run", json!({"st": state.st, "tac": terminator}))?;
        let closed = parse_run_result(&response)?;
        self.assumptions_locked(closed.st, name)
    }

    /// Query the checked state at a position in the original project file.
    /// This does not synthesize a theorem or discard its existing proof body.
    fn query_source(
        &self,
        source: &Path,
        offset: usize,
        expected_digest: [u8; 32],
        expected_header: Option<(&str, DeclarationKind, usize)>,
        query: FixedPetQuery,
    ) -> Result<String, PetError> {
        self.with_source_state(source, offset, expected_digest, expected_header, |state| {
            self.run_fixed_locked(state, query)
        })
    }

    /// Return uniquely resolved assumptions at an unchanged source position.
    /// The same PET state supplies both `Print Assumptions` and `Locate`, so
    /// pretty-printer abbreviations cannot be confused across modules/files.
    fn source_assumptions(
        &self,
        source: &Path,
        offset: usize,
        expected_digest: [u8; 32],
        expected_header: Option<(&str, DeclarationKind, usize)>,
        name: &str,
    ) -> Result<PetAssumptionReport, PetError> {
        self.with_source_state(source, offset, expected_digest, expected_header, |state| {
            self.assumptions_locked(state, name)
        })
    }

    /// Run one operation in PET's checked state at `offset`, with digest and
    /// exact-header validation both before and after the semantic operation.
    fn with_source_state<T>(
        &self,
        source: &Path,
        offset: usize,
        expected_digest: [u8; 32],
        expected_header: Option<(&str, DeclarationKind, usize)>,
        operation: impl FnOnce(u64) -> Result<T, PetError>,
    ) -> Result<T, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = fs::read(source)
            .map_err(|_| PetError::Environment("query source is unavailable".into()))?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected_digest {
            return Err(PetError::Invalid(
                "query declaration interface changed".into(),
            ));
        }
        let source_text = std::str::from_utf8(&bytes)
            .map_err(|_| PetError::Environment("query source is not UTF-8".into()))?;
        if let Some((name, kind, header_end)) = expected_header {
            self.validate_header_locked(source, source_text, name, kind, header_end)?;
        }
        let position = source_position(source_text, offset)?;
        let response = self.rpc_locked(
            "petanque/get_state_at_pos",
            json!({"uri": file_uri(source), "position": position}),
        )?;
        let state = parse_run_result(&response)?;
        let result = operation(state.st)?;
        let latest = fs::read(source)
            .map_err(|_| PetError::Environment("query source is unavailable".into()))?;
        if <[u8; 32]>::from(Sha256::digest(&latest)) != expected_digest {
            return Err(PetError::Invalid(
                "query declaration interface changed".into(),
            ));
        }
        Ok(result)
    }

    /// Ask PET whether the declaration's terminal AST node is a proved
    /// declaration. The byte range is only an edit anchor; `Proved` versus
    /// `Admitted`/`Abort` comes exclusively from PET.
    fn source_proof_completed(
        &self,
        source: &Path,
        range_end: usize,
        expected_digest: [u8; 32],
    ) -> Result<bool, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = fs::read(source)
            .map_err(|_| PetError::Environment("query source is unavailable".into()))?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected_digest {
            return Err(PetError::Invalid(
                "query declaration interface changed".into(),
            ));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| PetError::Environment("query source is not UTF-8".into()))?;
        let terminal_byte = range_end.saturating_sub(1);
        let range = sentence_ranges(text)
            .map_err(|error| PetError::Invalid(error.to_string()))?
            .into_iter()
            .find(|range| range.contains(&terminal_byte))
            .ok_or_else(|| PetError::Invalid("declaration terminal sentence is absent".into()))?;
        let (ast, _) = self.source_ast_locked(source, text, &range)?;
        let expr = ast.pointer("/v/expr/1").and_then(Value::as_array);
        let closed = match expr.and_then(|value| value.first()).and_then(Value::as_str) {
            Some("VernacEndProof") => {
                expr.and_then(|value| value.get(1))
                    .and_then(|value| value.get(0))
                    .and_then(Value::as_str)
                    == Some("Proved")
            }
            Some("VernacDefinition") => {
                expr.and_then(|value| value.get(3))
                    .and_then(|value| value.get(0))
                    .and_then(Value::as_str)
                    == Some("DefineBody")
            }
            _ => false,
        };
        let latest = fs::read(source)
            .map_err(|_| PetError::Environment("query source is unavailable".into()))?;
        if latest != bytes {
            return Err(PetError::Invalid(
                "query declaration interface changed".into(),
            ));
        }
        Ok(closed)
    }

    /// Resolve the editable extent of one PET-indexed declaration. Sentence
    /// boundaries are byte candidates only; PET AST decides whether a node is
    /// the declaration's proof terminator.
    fn source_span(&self, source: &DeclarationSource) -> Result<DeclarationSource, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = fs::read(&source.anchor.source).map_err(|error| {
            PetError::Environment(format!(
                "source edit anchor is unavailable: {} ({error})",
                source.anchor.source.display()
            ))
        })?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != source.anchor.digest {
            return Err(PetError::Invalid("declaration interface changed".into()));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| PetError::Environment("source edit anchor is not UTF-8".into()))?;
        let ranges = sentence_ranges(text).map_err(|error| PetError::Invalid(error.to_string()))?;
        let header_byte = source.anchor.header.end.saturating_sub(1);
        let header_range = ranges
            .iter()
            .find(|range| range.contains(&header_byte))
            .ok_or_else(|| PetError::Invalid("declaration header sentence is absent".into()))?;
        let (header_ast, _) = self.source_ast_locked(&source.anchor.source, text, header_range)?;
        if header_ast.pointer("/v/expr/1/0").and_then(Value::as_str) == Some("VernacDefinition")
            && header_ast.pointer("/v/expr/1/3/0").and_then(Value::as_str) == Some("DefineBody")
        {
            let mut hydrated = source.clone();
            // Design note: a direct definition is a one-sentence declaration;
            // PET's document-declaration header range is therefore also its
            // complete range.
            hydrated.anchor.declaration = Some(hydrated.anchor.header.clone());
            return Ok(hydrated);
        }
        let mut end = text.len();
        for range in ranges {
            if range.start < source.anchor.header.end {
                continue;
            }
            let (ast, location_base) =
                self.source_ast_locked(&source.anchor.source, text, &range)?;
            if ast.pointer("/v/expr/1/0").and_then(Value::as_str) == Some("VernacEndProof") {
                end = ast
                    .pointer("/loc/ep")
                    .and_then(Value::as_u64)
                    .and_then(|offset| location_base.checked_add(offset as usize))
                    .unwrap_or(range.end);
                break;
            }
        }
        if end < source.anchor.header.end || end > bytes.len() {
            return Err(PetError::Protocol(
                "PET declaration edit range is invalid".into(),
            ));
        }
        let mut hydrated = source.clone();
        hydrated.anchor.declaration = Some(crate::types::PetRange {
            start: hydrated.anchor.header.start,
            end,
        });
        Ok(hydrated)
    }

    /// Locate the source insertion point for a new declaration in an exact
    /// nested module context. Module and section transitions come from PET's
    /// vernacular AST; byte framing supplies request positions only.
    fn insertion_offset(
        &self,
        source: &Path,
        expected_digest: [u8; 32],
        modules: &[String],
        declaration: &DeclarationInfo,
    ) -> Result<usize, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = fs::read(source)
            .map_err(|_| PetError::Environment("declaration target is unavailable".into()))?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected_digest {
            return Err(PetError::Invalid(
                "declaration target changed while locating its context".into(),
            ));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| PetError::Environment("declaration target is not UTF-8".into()))?;
        let name = declaration
            .identity
            .constant()
            .ok_or_else(|| PetError::InvalidDeclaration("declaration has no leaf name".into()))?;
        let ranges = sentence_ranges(text).map_err(|error| PetError::Invalid(error.to_string()))?;
        let mut scopes = Vec::<(String, bool)>::new();
        for range in ranges {
            let (ast, location_base) = self.source_ast_locked(source, text, &range)?;
            let Some(expr) = ast.pointer("/v/expr/1").and_then(Value::as_array) else {
                continue;
            };
            // The AST node's active module path is the only duplicate
            // identity check used here.  It catches source-backed collisions
            // without consulting the touched-attempt map or parsing names
            // from bytes, while keeping this pass single-traversal.
            let module_path = scopes
                .iter()
                .filter(|(_, is_module)| *is_module)
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            for node in declaration_nodes_from_ast(expr, module_path)? {
                if node.name == name && node.modules == modules {
                    return Err(PetError::InvalidDeclaration(
                        "declaration identity is already present".into(),
                    ));
                }
            }
            match expr.first().and_then(Value::as_str) {
                Some("VernacDefineModule") => {
                    let has_body = expr
                        .get(5)
                        .and_then(Value::as_array)
                        .is_none_or(|value| !value.is_empty());
                    if !has_body {
                        scopes.push((pet_identifier(expr.get(2))?, true));
                    }
                }
                Some("VernacDeclareModuleType") => {
                    // A module type is part of a declaration's printed path,
                    // but it is not an implementation context into which the
                    // wrapper may publish a theorem body.
                    scopes.push((pet_identifier(expr.get(1))?, false));
                }
                Some("VernacBeginSection") => {
                    scopes.push((pet_identifier(expr.get(1))?, false));
                }
                Some("VernacEndSegment") => {
                    let close_name = pet_identifier(expr.get(1))?;
                    let module_path = scopes
                        .iter()
                        .filter(|(_, is_module)| *is_module)
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>();
                    if module_path == modules
                        && scopes
                            .last()
                            .is_some_and(|(open, is_module)| *is_module && open == &close_name)
                    {
                        let offset = ast
                            .pointer("/loc/bp")
                            .and_then(Value::as_u64)
                            .and_then(|offset| location_base.checked_add(offset as usize))
                            .ok_or_else(|| {
                                PetError::Protocol(
                                    "PET module closing range has no byte start".into(),
                                )
                            })?;
                        if offset > bytes.len() {
                            return Err(PetError::Protocol(
                                "PET module insertion point is out of bounds".into(),
                            ));
                        }
                        self.validate_synthetic_header(
                            source,
                            text,
                            offset,
                            &declaration.statement,
                            declaration.kind,
                            name,
                        )?;
                        return Ok(offset);
                    }
                    let Some((open, _)) = scopes.pop() else {
                        return Err(PetError::Invalid(
                            "PET scope closes without an opener".into(),
                        ));
                    };
                    if open != close_name {
                        return Err(PetError::Invalid("PET scope closes out of order".into()));
                    }
                }
                _ => {}
            }
        }
        if modules.is_empty() {
            self.validate_synthetic_header(
                source,
                text,
                text.len(),
                &declaration.statement,
                declaration.kind,
                name,
            )?;
            return Ok(text.len());
        }
        Err(PetError::Invalid(
            "requested declaration module context does not exist".into(),
        ))
    }

    /// Validate a new declaration header with PET before creating its
    /// immutable trace root. The requested identity is compared with PET's
    /// vernacular AST; no local keyword/name parser is used.
    fn validate_synthetic_header(
        &self,
        source: &Path,
        text: &str,
        offset: usize,
        header: &str,
        kind: DeclarationKind,
        name: &str,
    ) -> Result<(), PetError> {
        let position = source_position(text, offset)?;
        let state = self
            .rpc_locked(
                "petanque/get_state_at_pos",
                json!({"uri": file_uri(source), "position": position}),
            )
            .or_else(|_| {
                self.rpc_locked("petanque/get_root_state", json!({"uri": file_uri(source)}))
            })?;
        let state = parse_run_result(&state)?;
        let header = if header.trim_end().ends_with('.') {
            header.to_owned()
        } else {
            format!("{header}.")
        };
        let parsed = self.rpc_locked("petanque/ast", json!({"st": state.st, "text": header}))?;
        let ast = parsed
            .get("st")
            .ok_or_else(|| PetError::Protocol("PET parsed declaration has no AST state".into()))?;
        if !ast_declaration_matches(ast, kind, name) {
            return Err(PetError::InvalidDeclaration(
                "declaration statement identity does not match request".into(),
            ));
        }
        Ok(())
    }

    /// Check the source-edit header anchor against PET's parsed vernacular AST.
    /// AST-at-position confirms the exact anchored node rather than trusting a
    /// name or locally decoded source header.
    fn validate_header_locked(
        &self,
        source: &Path,
        text: &str,
        name: &str,
        kind: DeclarationKind,
        header_end: usize,
    ) -> Result<(), PetError> {
        let header_byte = header_end.checked_sub(1).ok_or_else(|| {
            PetError::Invalid("declaration header has no final sentence byte".into())
        })?;
        let range = sentence_ranges(text)
            .map_err(|error| PetError::Invalid(error.to_string()))?
            .into_iter()
            .find(|range| range.contains(&header_byte))
            .ok_or_else(|| PetError::Invalid("declaration header sentence is absent".into()))?;
        let (ast, location_base) = self.source_ast_locked(source, text, &range)?;
        if !ast_header_matches(&ast, kind, name, header_end, location_base) {
            return Err(PetError::Invalid(
                "declaration interface changed: PET AST disagrees with source anchor".into(),
            ));
        }
        Ok(())
    }

    /// Return PET's vernacular AST for one locally framed source sentence.
    ///
    /// PET 0.2.5 sometimes returns `null` from `ast_at_pos` for the first
    /// checked node of a generated-project workspace. In that case the same
    /// PET instance parses the exact sentence in its source state. The second
    /// return value translates command-relative PET locations back to source
    /// bytes; names, kinds, and scope tags still come exclusively from PET.
    fn source_ast_locked(
        &self,
        source: &Path,
        text: &str,
        range: &Range<usize>,
    ) -> Result<(Value, usize), PetError> {
        let position = source_position(text, range.end.saturating_sub(1))?;
        let ast = self.rpc_locked(
            "petanque/ast_at_pos",
            json!({"uri": file_uri(source), "position": position}),
        )?;
        if ast.pointer("/v/expr/1").is_some() {
            return Ok((ast, 0));
        }
        let before = source_position(text, range.start)?;
        let state = self
            .rpc_locked(
                "petanque/get_state_at_pos",
                json!({"uri": file_uri(source), "position": before}),
            )
            .or_else(|_| {
                self.rpc_locked("petanque/get_root_state", json!({"uri": file_uri(source)}))
            })?;
        let state = parse_run_result(&state)?;
        let parsed = self.rpc_locked(
            "petanque/ast",
            json!({"st": state.st, "text": &text[range.clone()]}),
        )?;
        let ast = parsed
            .get("st")
            .cloned()
            .ok_or_else(|| PetError::Protocol("PET parsed AST has no vernacular node".into()))?;
        Ok((ast, range.start))
    }

    /// Index one Dune-selected source through PET's document-level declaration
    /// endpoint.  PET traverses the checked document once and returns an
    /// ordered list, preserving duplicate leaf names and complete module paths.
    /// The wrapper performs only wire validation, source-digest anchoring, and
    /// Dune-library prefixing; it never scans sentences or maintains scopes.
    fn document_declarations(
        &self,
        workspace: &Path,
        source: &Path,
        library: &crate::LogicalLibrary,
    ) -> Result<BTreeMap<DeclarationIdentity, DeclarationSource>, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = fs::read(source).map_err(|error| {
            PetError::Environment(format!(
                "declaration source is unavailable: {} ({error})",
                source.display()
            ))
        })?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| PetError::Environment("declaration source is not UTF-8".into()))?;
        let response = self.rpc_locked(
            "petanque/document_declarations",
            json!({"uri": file_uri(source)}),
        )?;
        let rows = response
            .as_array()
            .ok_or_else(|| PetError::Protocol("PET document declarations are not a list".into()))?
            .iter()
            .map(|row| {
                serde_json::from_value::<PetDocumentDeclaration>(row.clone()).map_err(|error| {
                    PetError::Protocol(format!(
                        "PET document declaration has invalid shape: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let library = library.clone();
        let source_digest = <[u8; 32]>::from(Sha256::digest(&bytes));
        let mut declarations = BTreeMap::new();

        let file =
            crate::types::FileId::from_path(workspace, source).map_err(PetError::Protocol)?;
        for row in rows {
            let kind = declaration_kind_from_pet(&row.kind).ok_or_else(|| {
                PetError::Protocol(format!(
                    "PET document declaration kind is unsupported: {}",
                    row.kind
                ))
            })?;
            let start = row.range.start;
            let end = row.range.end_;
            if start >= end
                || end > bytes.len()
                || !text.is_char_boundary(start)
                || !text.is_char_boundary(end)
            {
                return Err(PetError::Protocol(
                    "PET document declaration range is invalid".into(),
                ));
            }
            if row.qualified_path.is_empty()
                || row.qualified_path.iter().any(|part| part.is_empty())
            {
                return Err(PetError::Protocol(
                    "PET document declaration path is empty".into(),
                ));
            }
            if row.statement.is_empty() {
                return Err(PetError::Protocol(
                    "PET document declaration statement is empty".into(),
                ));
            }
            let mut qualified_path = library.0.clone();
            qualified_path.extend(row.qualified_path);
            let identity = DeclarationIdentity {
                file: file.clone(),
                qualified_path,
            };
            let info = DeclarationInfo {
                identity: identity.clone(),
                kind,
                statement: row.statement,
            };
            let source_declaration = DeclarationSource {
                info,
                library: library.clone(),
                anchor: crate::types::PetAnchor {
                    source: source.to_owned(),
                    digest: source_digest,
                    header: crate::types::PetRange { start, end },
                    declaration: None,
                },
            };
            if declarations.insert(identity, source_declaration).is_some() {
                return Err(PetError::Invalid(
                    "PET declaration identity is ambiguous".into(),
                ));
            }
        }
        let latest = fs::read(source)
            .map_err(|_| PetError::Environment("declaration source is unavailable".into()))?;
        if latest != bytes {
            return Err(PetError::Invalid("declaration interface changed".into()));
        }
        Ok(declarations)
    }

    /// Read explicit assumption declarations from PET's vernacular AST. The
    /// local sentence splitter supplies byte positions only; it never decides
    /// whether a sentence declares an axiom or variable.
    fn source_trust_file(&self, source: &Path) -> Result<PetSourceTrust, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = fs::read(source)
            .map_err(|_| PetError::Environment("assumption source is unavailable".into()))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| PetError::Environment("assumption source is not UTF-8".into()))?;
        let ranges = sentence_ranges(text)
            .map_err(|_| PetError::Invalid("assumption source has malformed sentences".into()))?;
        let mut scopes = Vec::<(String, bool, bool)>::new();
        let mut ignored = 0usize;
        let mut explicit_axioms = Vec::new();
        let mut admitted = Vec::new();
        // Design note: this path is only a source-context expression used to
        // ask PET `Locate`; it is never retained as declaration identity.
        let qualify = |scopes: &[(String, bool, bool)], leaf: &str| {
            scopes
                .iter()
                .filter(|(_, module, _)| *module)
                .map(|(name, _, _)| name.clone())
                .chain(std::iter::once(leaf.to_owned()))
                .collect::<Vec<_>>()
                .join(".")
        };
        let mut pending_proof = None::<String>;
        for range in ranges {
            let (ast, location_base) = self.source_ast_locked(source, text, &range)?;
            let Some(expr) = ast.pointer("/v/expr/1").and_then(Value::as_array) else {
                continue;
            };
            match expr.first().and_then(Value::as_str) {
                Some("VernacStartTheoremProof") => {
                    pending_proof = Some(pet_declaration_identifier(
                        expr.get(2).and_then(|value| value.pointer("/0/0/0")),
                    )?);
                }
                Some("VernacDefinition")
                    if expr
                        .get(3)
                        .and_then(|value| value.get(0))
                        .and_then(Value::as_str)
                        == Some("ProveBody") =>
                {
                    pending_proof = Some(pet_declaration_identifier(
                        expr.get(2).and_then(|value| value.get(0)),
                    )?);
                }
                Some("VernacEndProof") => {
                    let terminator = expr
                        .get(1)
                        .and_then(|value| value.get(0))
                        .and_then(Value::as_str);
                    if terminator == Some("Admitted")
                        && ignored == 0
                        && let Some(name) = pending_proof.take()
                    {
                        admitted.push(qualify(&scopes, &name));
                    } else {
                        pending_proof = None;
                    }
                }
                Some("VernacDefineModule") => {
                    let name = pet_identifier(expr.get(2))?;
                    let has_parameters = expr
                        .get(3)
                        .and_then(Value::as_array)
                        .is_none_or(|v| !v.is_empty());
                    let has_body = expr
                        .get(5)
                        .and_then(Value::as_array)
                        .is_none_or(|v| !v.is_empty());
                    if !has_body {
                        let ordinary = !has_parameters;
                        if !ordinary {
                            ignored += 1;
                        }
                        scopes.push((name, ordinary, !ordinary));
                    }
                }
                Some("VernacDeclareModuleType") => {
                    let name = pet_identifier(expr.get(1))?;
                    ignored += 1;
                    scopes.push((name, false, true));
                }
                Some("VernacBeginSection") => {
                    scopes.push((pet_identifier(expr.get(1))?, false, false));
                }
                Some("VernacEndSegment") => {
                    let name = pet_identifier(expr.get(1))?;
                    let Some((last, _, was_ignored)) = scopes.pop() else {
                        return Err(PetError::Invalid(
                            "PET scope closes without an opener".into(),
                        ));
                    };
                    if last != name {
                        return Err(PetError::Invalid("PET scope closes out of order".into()));
                    }
                    if was_ignored {
                        ignored -= 1;
                    }
                }
                Some("VernacAssumption") if ignored == 0 => {
                    if ast.pointer("/v/expr/1/1/0/0").and_then(Value::as_str) != Some("NoDischarge")
                    {
                        continue;
                    }
                    let groups = expr.get(3).and_then(Value::as_array).ok_or_else(|| {
                        PetError::Protocol("PET assumption groups are invalid".into())
                    })?;
                    for group in groups {
                        let declarations =
                            group.get(1).and_then(Value::as_array).ok_or_else(|| {
                                PetError::Protocol("PET assumption declaration is invalid".into())
                            })?;
                        let names =
                            declarations
                                .first()
                                .and_then(Value::as_array)
                                .ok_or_else(|| {
                                    PetError::Protocol("PET assumption names are invalid".into())
                                })?;
                        let loc = declarations
                            .get(1)
                            .and_then(|value| value.get("loc"))
                            .ok_or_else(|| {
                                PetError::Protocol("PET assumption type has no location".into())
                            })?;
                        let start = loc
                            .get("bp")
                            .and_then(Value::as_u64)
                            .and_then(|offset| location_base.checked_add(offset as usize))
                            .ok_or_else(|| {
                                PetError::Protocol("PET assumption type start is invalid".into())
                            })?;
                        let end = loc
                            .get("ep")
                            .and_then(Value::as_u64)
                            .and_then(|offset| location_base.checked_add(offset as usize))
                            .ok_or_else(|| {
                                PetError::Protocol("PET assumption type end is invalid".into())
                            })?;
                        let ty = text.get(start..end).ok_or_else(|| {
                            PetError::Protocol("PET assumption type location is invalid".into())
                        })?;
                        for name in names {
                            let leaf = pet_identifier(name.get(0))?;
                            explicit_axioms.push((
                                qualify(&scopes, &leaf),
                                ty.split_whitespace().collect::<Vec<_>>().join(" "),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        if !explicit_axioms.is_empty() || !admitted.is_empty() {
            let position = source_position(text, text.len())?;
            let state = parse_run_result(&self.rpc_locked(
                "petanque/get_state_at_pos",
                json!({"uri": file_uri(source), "position": position}),
            )?)?;
            for (name, ty) in &mut explicit_axioms {
                let report = self.text_command_locked(state.st, &format!("Check {name}."))?;
                *ty = normalize_type(&pet_checked_type(&report, name).ok_or_else(|| {
                    PetError::Protocol("PET Check did not return the assumption type".into())
                })?);
                *name = self.canonical_constant_locked(state.st, name)?;
            }
            for name in &mut admitted {
                *name = self.canonical_constant_locked(state.st, name)?;
            }
        }
        let latest = fs::read(source)
            .map_err(|_| PetError::Environment("assumption source is unavailable".into()))?;
        if latest != bytes {
            return Err(PetError::Invalid(
                "assumption source changed during PET query".into(),
            ));
        }
        Ok(PetSourceTrust {
            explicit_axioms,
            admitted,
        })
    }

    /// Execute one fixed read-only query in a PET state while `serial` is held.
    fn run_fixed_locked(&self, st: u64, query: FixedPetQuery) -> Result<String, PetError> {
        match query {
            FixedPetQuery::Assumptions(name) => {
                self.text_command_locked(st, &format!("Print Assumptions {name}."))
            }
            FixedPetQuery::Dependencies(name) => {
                self.text_command_locked(st, &format!("Print All Dependencies {name}."))
            }
            FixedPetQuery::Search(pattern) => {
                self.text_command_locked(st, &format!("Search {pattern}."))
            }
            FixedPetQuery::About(name) => self.text_command_locked(st, &format!("About {name}.")),
            FixedPetQuery::Print(name) => self.text_command_locked(st, &format!("Print {name}.")),
            FixedPetQuery::ExpressionType(expression) => {
                self.text_command_locked(st, &format!("Check ({expression})."))
            }
            FixedPetQuery::Notation(expression) => self.notation_locked(st, &expression),
        }
    }

    /// Ask PET for a theorem's transitive kernel assumptions, then resolve
    /// every context-shortened printed name to the unique constant selected
    /// by Rocq in this exact state.
    fn assumptions_locked(&self, st: u64, name: &str) -> Result<PetAssumptionReport, PetError> {
        let text = self.text_command_locked(st, &format!("Print Assumptions {name}."))?;
        let assumptions = parse_assumption_feedback(&text)?
            .into_iter()
            .map(|(printed, ty)| {
                self.canonical_constant_locked(st, &printed)
                    .map(|canonical| (canonical, ty))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PetAssumptionReport { assumptions })
    }

    /// Resolve one Rocq reference through PET/Rocq itself. Wrapper-composed
    /// module paths are valid only as query expressions, never as identity.
    fn canonical_constant_locked(&self, st: u64, name: &str) -> Result<String, PetError> {
        let report = self.text_command_locked(st, &format!("Locate {name}."))?;
        report
            .lines()
            .find_map(|line| line.trim().strip_prefix("Constant "))
            .and_then(|rest| rest.split_whitespace().next())
            .filter(|constant| crate::engine::validate_name(constant).is_ok())
            .map(str::to_owned)
            .ok_or_else(|| {
                PetError::Protocol(format!(
                    "PET Locate did not return a unique constant for {name}"
                ))
            })
    }

    fn text_command_locked(&self, st: u64, command: &str) -> Result<String, PetError> {
        let response = self.rpc_locked("petanque/run", json!({"st": st, "tac": command}))?;
        parse_feedback(&response)
    }

    fn notation_locked(&self, st: u64, expression: &str) -> Result<String, PetError> {
        let statement = format!("Lemma __rocq_engine_notation_probe : {expression}.");
        let response = self.rpc_locked(
            "petanque/list_notations_in_statement",
            json!({"st": st, "statement": statement}),
        )?;
        serde_json::to_string(&response)
            .map_err(|_| PetError::Protocol("PET notation response is invalid".into()))
    }

    fn configure_document_locked(&self, document: PetDocument) -> Result<(), PetError> {
        let response = self.rpc_locked(
            "petanque/setWorkspace",
            json!({"debug": false, "root": file_uri(&document.workspace_root)}),
        )?;
        if !response.is_null() {
            return Err(PetError::Protocol(
                "setWorkspace returned an invalid result".into(),
            ));
        }
        self.instance_epoch.fetch_add(1, Ordering::AcqRel);
        let mut current = self
            .document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = Some(document);
        Ok(())
    }

    fn document_start(&self) -> Result<(String, Option<String>, Value, bool), PetError> {
        self.document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|document| {
                (
                    document.uri.clone(),
                    document.synthetic_header.clone(),
                    document.start_position.clone(),
                    document.allow_root_fallback,
                )
            })
            .ok_or_else(|| PetError::Environment("PET document is not configured".into()))
    }

    fn goals_locked(&self, st: u64, proof_finished: bool) -> Result<PetGoals, PetError> {
        let response = self.rpc_locked("petanque/goals", json!({"st": st}))?;
        parse_goals(&response, !proof_finished)
    }

    fn rpc_locked(&self, method: &str, params: Value) -> Result<Value, PetError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|_| PetError::Invalid("PET request encoding failed".into()))?;
        if request.len() > MAX_REQUEST_BYTES {
            return Err(PetError::Invalid("PET request is oversized".into()));
        }
        // Design note: PET/Rocq checking is terminating, but its duration is
        // project-dependent. The wrapper blocks for the protocol response and
        // reports PET EOF/protocol errors instead of inventing a proof deadline.
        // Process termination remains explicit via detach/shutdown.
        let mut input = self
            .stdin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        write_frame(&mut input, &request)?;
        drop(input);
        // `ChildStdout` is a blocking pipe, so a malformed/non-terminating
        // Rocq request cannot be bounded by polling the caller thread alone.
        // One response reader owns the stdout lock for this serialized PET
        // exchange; on timeout we kill the process group, making the lane
        // unusable and allowing the runtime to evict it.  The reader is left
        // to observe EOF after the kill rather than being detached with a
        // live child or a reusable stream.
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let stdout = Arc::clone(&self.stdout);
        std::thread::spawn(move || {
            let mut output = stdout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = read_response(&mut output, id);
            let _ = sender.send(result);
        });
        match receiver.recv_timeout(self.rpc_timeout) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.terminate();
                Err(PetError::OperationTimeout)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.alive.store(false, Ordering::Release);
                Err(PetError::ProcessFailure(
                    "PET response reader exited".into(),
                ))
            }
        }
    }

    fn terminate(&self) {
        if self.alive.swap(false, Ordering::AcqRel) {
            let mut child = self
                .child
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            terminate_child(&mut child);
        }
        // Killing PET invalidates every opaque state associated with its
        // current direct source document.
        self.document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

/// Kill and reap a just-spawned or live PET child, including its Unix process
/// group.  This helper is intentionally synchronous: callers must not expose
/// a failed spawn/termination as success while descendants remain running.
fn terminate_child(child: &mut Child) {
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

impl Drop for PetProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

mod runtime;
pub(crate) use runtime::PetRuntime;

fn is_fatal(error: &PetError) -> bool {
    matches!(error, PetError::Timeout | PetError::OperationTimeout) || state_was_lost(error)
}

/// Classify a declaration-opening collision at the PET boundary.  PET's
/// structured RPC error identifies this as a remote semantic rejection; the
/// surrounding operation is specifically the synthetic declaration header, so
/// exposing it as a declaration error is more precise than a failed tactic.
fn declaration_start_error(error: PetError) -> PetError {
    match error {
        PetError::Remote { message, .. } if message.contains(" already exists") => {
            PetError::InvalidDeclaration("declaration identity is already present".into())
        }
        other => other,
    }
}

/// Errors proving that an opaque state can no longer be trusted or resumed in
/// its current PET owner. Only `PetRuntime` decides whether replay is safe.
fn state_was_lost(error: &PetError) -> bool {
    matches!(
        error,
        PetError::Protocol(_)
            | PetError::OutputOverflow
            | PetError::ProcessFailure(_)
            | PetError::Stale
    )
}

mod results;
use results::{parse_feedback, parse_goals, parse_run_result};

fn root_affinity(root: &DeclarationSource) -> String {
    format!(
        "{}::{}",
        root.info.identity.file.0,
        root.info.identity.qualified_name()
    )
}

/// Distinguish tactic prefixes without hashing the project inputs again on
/// every step. This key owns only the lane's most recent replay cache entry.
fn replay_key(document_key: [u8; 32], actions: &[CanonicalTactic]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(document_key);
    for action in actions {
        digest.update((action.0.len() as u64).to_le_bytes());
        digest.update(action.0.as_bytes());
    }
    digest.finalize().into()
}

fn write_frame(writer: &mut ChildStdin, body: &[u8]) -> Result<(), PetError> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())
        .map_err(|_| PetError::ProcessFailure("PET stdin write failed".into()))?;
    writer
        .write_all(body)
        .and_then(|_| writer.flush())
        .map_err(|_| PetError::ProcessFailure("PET stdin write failed".into()))
}

fn read_response(reader: &mut BufReader<ChildStdout>, request_id: u64) -> Result<Value, PetError> {
    let mut header = Vec::new();
    loop {
        let mut line = Vec::new();
        read_line_bounded(reader, &mut line)?;
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
        let name = trim_ascii_space(&line[..colon]);
        if !name.eq_ignore_ascii_case(b"Content-Length") {
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
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| PetError::ProcessFailure("PET stdout closed".into()))?;
    // Design note: Rocq vernacular ASTs are deeply recursive. The default
    // serde_json depth of 128 rejects valid PET replies from real theorems;
    // framing still enforces MAX_RESPONSE_BYTES before parsing.
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
                "PET response has an invalid JSON-RPC result/error pair".into(),
            ));
        }
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(|| PetError::Protocol("PET JSON-RPC error has no code".into()))?;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| PetError::Protocol("PET JSON-RPC error has no message".into()))?
            .to_owned();
        return Err(PetError::Remote { code, message });
    }
    object
        .get("result")
        .cloned()
        .ok_or_else(|| PetError::Protocol("PET response has no result".into()))
}

fn trim_ascii_space(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !matches!(*byte, b' ' | b'\t'))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !matches!(*byte, b' ' | b'\t'))
        .map_or(start, |position| position + 1);
    &value[start..end]
}

fn read_line_bounded(
    reader: &mut BufReader<ChildStdout>,
    line: &mut Vec<u8>,
) -> Result<(), PetError> {
    loop {
        let mut byte = [0u8; 1];
        reader
            .read_exact(&mut byte)
            .map_err(|_| PetError::ProcessFailure("PET stdout closed".into()))?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(());
        }
        if line.len() > MAX_HEADER_BYTES {
            return Err(PetError::OutputOverflow);
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

/// Convert a UTF-8 byte anchor from the current source snapshot to PET's LSP
/// zero-based line/UTF-16-character position. Invalid/stale anchors fail.
fn source_position(source: &str, offset: usize) -> Result<Value, PetError> {
    let prefix = source
        .get(..offset)
        .ok_or_else(|| PetError::Invalid("query source position is no longer valid".into()))?;
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let character = prefix
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .encode_utf16()
        .count();
    Ok(json!({"line": line, "character": character}))
}

fn declaration_kind_from_pet(detail: &str) -> Option<DeclarationKind> {
    match detail {
        "Theorem" => Some(DeclarationKind::Theorem),
        "Lemma" => Some(DeclarationKind::Lemma),
        "Fact" => Some(DeclarationKind::Fact),
        "Remark" => Some(DeclarationKind::Remark),
        "Corollary" => Some(DeclarationKind::Corollary),
        "Proposition" => Some(DeclarationKind::Proposition),
        "Definition" => Some(DeclarationKind::Definition),
        _ => None,
    }
}

/// Decode only declaration headers that the public engine can represent from
/// one PET AST node.  This is deliberately structural: no keyword search or
/// identifier extraction from source text is used here.
fn declaration_nodes_from_ast(
    expr: &[Value],
    modules: Vec<String>,
) -> Result<Vec<PetDeclarationNode>, PetError> {
    let tag = expr.first().and_then(Value::as_str);
    let names = match tag {
        Some("VernacStartTheoremProof") => {
            let _kind = expr
                .get(1)
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
                .and_then(declaration_kind_from_pet)
                .ok_or_else(|| PetError::Protocol("PET theorem kind is invalid".into()))?;
            let declarations = expr
                .get(2)
                .and_then(Value::as_array)
                .ok_or_else(|| PetError::Protocol("PET theorem declarations are invalid".into()))?;
            let mut names = Vec::with_capacity(declarations.len());
            for declaration in declarations {
                let name_node = declaration
                    .get(0)
                    .and_then(Value::as_array)
                    .and_then(|values| values.first())
                    .ok_or_else(|| {
                        PetError::Protocol("PET theorem declaration name is invalid".into())
                    })?;
                names.push(pet_declaration_identifier(Some(name_node))?);
            }
            names
        }
        Some("VernacDefinition") => {
            // PET serializes the definition binder as
            // `[located Name, universe_decl]`; the located node, not the
            // enclosing pair, owns the `Name (Id ...)` payload.
            let name = pet_declaration_identifier(expr.get(2).and_then(|value| value.get(0)))?;
            vec![name]
        }
        _ => return Ok(Vec::new()),
    };
    Ok(names
        .into_iter()
        .map(|name| PetDeclarationNode {
            name,
            modules: modules.clone(),
        })
        .collect())
}

/// Match PET's parsed declaration node, including its byte end, against an
/// edit anchor. The name/kind paths are PET's vernacular AST representation;
/// an unknown shape fails closed rather than falling back to source parsing.
fn ast_header_matches(
    ast: &Value,
    kind: DeclarationKind,
    name: &str,
    end: usize,
    location_base: usize,
) -> bool {
    if ast
        .pointer("/loc/ep")
        .and_then(Value::as_u64)
        .and_then(|offset| location_base.checked_add(offset as usize))
        != Some(end)
    {
        return false;
    }
    ast_declaration_matches(ast, kind, name)
}

/// Compare a PET AST declaration node with the requested kind and leaf name.
/// This helper intentionally knows only PET's structured AST paths.
fn ast_declaration_matches(ast: &Value, kind: DeclarationKind, name: &str) -> bool {
    let tag = ast.pointer("/v/expr/1/0").and_then(Value::as_str);
    let (actual_kind, actual_name) = match tag {
        Some("VernacStartTheoremProof") => (
            ast.pointer("/v/expr/1/1/0").and_then(Value::as_str),
            ast.pointer("/v/expr/1/2/0/0/0/v/1").and_then(Value::as_str),
        ),
        Some("VernacDefinition") => (
            ast.pointer("/v/expr/1/1/1/0").and_then(Value::as_str),
            ast.pointer("/v/expr/1/2/0/v/1/1").and_then(Value::as_str),
        ),
        _ => return false,
    };
    actual_kind == Some(declaration_kind_name(kind)) && actual_name == Some(name)
}

/// PET's detail spelling for the supported declaration kinds.
fn declaration_kind_name(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Theorem => "Theorem",
        DeclarationKind::Lemma => "Lemma",
        DeclarationKind::Fact => "Fact",
        DeclarationKind::Remark => "Remark",
        DeclarationKind::Corollary => "Corollary",
        DeclarationKind::Proposition => "Proposition",
        DeclarationKind::Definition => "Definition",
    }
}

/// Extract an identifier from PET's located `Id` AST node.
fn pet_identifier(node: Option<&Value>) -> Result<String, PetError> {
    let value = node
        .and_then(|node| node.pointer("/v/1"))
        .and_then(Value::as_str)
        .ok_or_else(|| PetError::Protocol("PET identifier is invalid".into()))?;
    Ok(value.to_owned())
}

/// Extract a declaration name from PET's theorem `Id` or definition `Name`
/// located node. These are distinct Rocq AST shapes for the same semantic
/// declaration-name role.
fn pet_declaration_identifier(node: Option<&Value>) -> Result<String, PetError> {
    let value = node
        .and_then(|node| {
            node.pointer("/v/1")
                .and_then(Value::as_str)
                .or_else(|| node.pointer("/v/1/1").and_then(Value::as_str))
        })
        .ok_or_else(|| PetError::Protocol("PET declaration identifier is invalid".into()))?;
    Ok(value.to_owned())
}

/// Extract the normalized kernel-facing type from PET's `Check` feedback.
/// The identifier line must match exactly; similarly named constants cannot
/// be substituted by a pretty-printer or recovered state.
fn pet_checked_type(report: &str, name: &str) -> Option<String> {
    let lines = report.lines().collect::<Vec<_>>();
    let marker = lines.iter().position(|line| line.trim() == name)?;
    let mut ty = String::new();
    for line in lines.into_iter().skip(marker + 1) {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(':') {
            ty.push_str(rest.trim());
        } else if !ty.is_empty() {
            ty.push(' ');
            ty.push_str(line);
        }
    }
    (!ty.is_empty()).then_some(ty)
}

/// Decode Rocq's `Print Assumptions` feedback while retaining its printed
/// names only long enough for `canonical_constant_locked` to resolve them.
fn parse_assumption_feedback(report: &str) -> Result<Vec<(String, String)>, PetError> {
    if report
        .lines()
        .any(|line| line.trim() == "Closed under the global context")
    {
        return Ok(Vec::new());
    }
    let mut saw_header = false;
    let mut assumptions = Vec::<(String, String)>::new();
    for raw in report.lines() {
        let continuation = raw.starts_with(char::is_whitespace);
        let line = raw.trim();
        if line == "Axioms:" {
            saw_header = true;
            continue;
        }
        if !saw_header || line.is_empty() {
            continue;
        }
        if !continuation && let Some((name, ty)) = line.split_once(':') {
            let name = name.trim();
            if crate::engine::validate_name(name).is_ok() {
                assumptions.push((name.to_owned(), normalize_type(ty)));
            }
        } else if let Some((_, ty)) = assumptions.last_mut() {
            if !ty.is_empty() {
                ty.push(' ');
            }
            ty.push_str(&normalize_type(line));
        }
    }
    if saw_header {
        Ok(assumptions)
    } else {
        Err(PetError::Protocol(
            "PET assumption query returned no recognizable result".into(),
        ))
    }
}

fn normalize_type(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

/// Accept only proof-local commands according to PET's parsed Rocq AST.
/// This wrapper applies no textual tactic classification of its own.
fn pet_proof_command(parsed: &Value) -> bool {
    if !parsed
        .pointer("/st/v/control")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        return false;
    }
    match (
        parsed.pointer("/st/v/expr/0").and_then(Value::as_str),
        parsed.pointer("/st/v/expr/1/0").and_then(Value::as_str),
    ) {
        (Some("VernacSynterp"), Some("VernacExtend")) => {
            parsed
                .pointer("/st/v/expr/1/1/ext_plugin")
                .and_then(Value::as_str)
                == Some("rocq-runtime.plugins.ltac")
                && matches!(
                    parsed
                        .pointer("/st/v/expr/1/1/ext_entry")
                        .and_then(Value::as_str),
                    Some("VernacSolve" | "Unshelve")
                )
        }
        (Some("VernacSynPure"), Some(tag)) => matches!(
            tag,
            "VernacFocus"
                | "VernacUnfocus"
                | "VernacSubproof"
                | "VernacEndSubproof"
                | "VernacBullet"
        ),
        _ => false,
    }
}

/// Resolve the direct Dune/PET document for one proof attempt.
///
/// Existing declarations are opened by PET from their original source path.
/// A not-yet-written declaration starts from PET's state at end-of-file and
/// submits its header through `petanque/run`. No project file is copied or
/// modified. The input digest only detects changes that invalidate live PET
/// state; Dune remains the authority for the input inventory.
fn pet_document(
    project: &Path,
    root: &DeclarationSource,
    timeout: Duration,
) -> crate::Result<PetDocument> {
    validate_pet_root(root)?;
    let layout = dune::Layout::load(project, &[], timeout)?;
    let target = layout.target(&root.library)?;
    let target = fs::canonicalize(&target).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune-selected PET target is unavailable",
        )
    })?;
    let anchored = fs::canonicalize(&root.anchor.source).map_err(|_| {
        Error::new(
            ErrorKind::DeclarationChanged,
            "declaration source changed while preparing PET",
        )
    })?;
    if target != anchored {
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "Dune-selected declaration source changed",
        ));
    }
    let bytes = fs::read(&target).map_err(|_| {
        Error::new(
            ErrorKind::DeclarationChanged,
            "declaration source disappeared",
        )
    })?;
    if <[u8; 32]>::from(Sha256::digest(&bytes)) != root.anchor.digest {
        return Err(Error::new(
            ErrorKind::DeclarationChanged,
            "declaration source changed while preparing PET",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new(ErrorKind::InvalidConfiguration, "source is not UTF-8"))?;
    let pet_workspace = layout.prepare_pet_workspace(&target)?;
    let workspace_root = dune::dune_workspace_root(project, timeout)?;
    let mut digest = Sha256::new();
    digest.update(target.to_string_lossy().as_bytes());
    for input in layout.inputs() {
        if !input.starts_with(&workspace_root) {
            continue;
        }
        let input_bytes = fs::read(&input).map_err(|_| {
            Error::new(ErrorKind::InvalidConfiguration, "Dune input is unavailable")
        })?;
        digest.update(input.to_string_lossy().as_bytes());
        digest.update((input_bytes.len() as u64).to_le_bytes());
        digest.update(&input_bytes);
    }
    // PET's `start` endpoint accepts only the leaf key and therefore cannot
    // select one of two same-named declarations in nested modules.  Always
    // replay from the state immediately before this exact PET range and run
    // the header itself.  For a newly declared proof the zero-sized anchor
    // selects EOF and the normalized synthetic header is used.
    let (synthetic_header, start_offset) = if root.anchor.header.start < root.anchor.header.end {
        let header = text
            .get(root.anchor.header.start..root.anchor.header.end)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::DeclarationChanged,
                    "declaration header range is no longer valid",
                )
            })?
            .to_owned();
        (Some(header), root.anchor.header.start)
    } else {
        if root.anchor.header.start > text.len() {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "declaration insertion point is no longer valid",
            ));
        }
        (Some(root.info.statement.clone()), root.anchor.header.start)
    };
    Ok(PetDocument {
        workspace_root: pet_workspace,
        uri: file_uri(&target),
        key: digest.finalize().into(),
        synthetic_header,
        start_position: source_position(text, start_offset)
            .map_err(|error| Error::new(ErrorKind::InvalidConfiguration, error.to_string()))?,
        allow_root_fallback: start_offset == 0,
    })
}

fn validate_pet_root(root: &DeclarationSource) -> crate::Result<()> {
    validate_identity(&root.info.identity)?;
    if root.info.statement.is_empty() || root.info.statement.len() > 1024 * 1024 {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "PET declaration header is empty or oversized",
        ));
    }
    Ok(())
}

mod source;
pub(crate) use source::{canonical_tactic, sentence_ranges};

#[cfg(test)]
mod tests;
