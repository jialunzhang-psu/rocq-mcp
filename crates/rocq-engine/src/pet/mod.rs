//! Thin, serialized client for one long-lived PET process.
//!
//! PET owns Rocq parsing, document declarations, proof execution, goals, and
//! semantic queries. This module owns only JSON-RPC framing and child-process
//! lifecycle; it contains no proof graph, source scanner, or replay cache.

use crate::types::PetWorkspace;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Mutex,
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

/// Failures distinguish semantic PET rejection from transport loss. The MCP
/// coordinator uses `ProcessLost` to clear every checkpoint for this project
/// before starting a replacement process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PetError {
    Invalid(String),
    Environment(String),
    ProcessLost(String),
    Protocol(String),
    OutputOverflow,
    Remote { code: i64, message: String },
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

    pub(crate) fn lost(&self) -> bool {
        self.is_transport_loss()
    }
}

impl std::fmt::Display for PetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid PET request: {message}"),
            Self::Environment(message) | Self::ProcessLost(message) => f.write_str(message),
            Self::Protocol(message) => write!(f, "PET protocol failure: {message}"),
            Self::OutputOverflow => f.write_str("PET response exceeded the output limit"),
            Self::Remote { code, message } => write!(f, "PET rejected request ({code}): {message}"),
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
pub(crate) struct PetGoals {
    pub(crate) focused: Vec<PetGoal>,
    pub(crate) unfocused: Vec<PetGoal>,
    pub(crate) shelved: Vec<PetGoal>,
    pub(crate) given_up: Vec<PetGoal>,
    pub(crate) proof_mode: bool,
}

impl PetGoals {
    fn outside_proof() -> Self {
        Self {
            focused: Vec::new(),
            unfocused: Vec::new(),
            shelved: Vec::new(),
            given_up: Vec::new(),
            proof_mode: false,
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
                self.poison_if_lost(&error);
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
                self.poison_if_lost(&error);
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
            .map_err(|_| PetError::Environment("PET source is unavailable".into()))?;
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
        if fragment.len() > MAX_REQUEST_BYTES / 2 {
            return Err(PetError::Invalid("proof fragment is oversized".into()));
        }
        let value = self.call_live("petanque/run", json!({"st": state.get(), "tac": fragment}))?;
        self.materialize_run(value)
    }

    /// Read structured goals without allocating another PET state ID.
    pub(crate) fn goals(&self, state: PetStateId) -> Result<PetGoals, PetError> {
        let value = self.call_live("petanque/goals", json!({"st": state.get()}))?;
        let result = decode_goals(&value);
        if let Err(error) = &result {
            self.poison_if_lost(error);
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
                self.poison_if_lost(&error);
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
            crate::PetQuery::Locate(name) => format!("Locate {name}."),
            crate::PetQuery::Notation(_) => unreachable!(),
        };
        let value = self.call_live("petanque/run", json!({"st": state.get(), "tac": command}))?;
        let run = match results::parse_run_result(&value) {
            Ok(run) => run,
            Err(error) => {
                self.poison_if_lost(&error);
                return Err(error);
            }
        };
        let feedback = results::parse_feedback(&value);
        if let Err(error) = &feedback {
            self.poison_if_lost(error);
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
                self.poison_if_lost(&error);
                return Err(error);
            }
        };
        // Missing IDs are intentionally idempotent success. Validate only that
        // PET accounted for every request occurrence.
        if response.released.len() + response.missing.len() != states.len() {
            let error = PetError::Protocol("PET release response has the wrong cardinality".into());
            self.poison_if_lost(&error);
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
                self.poison_if_lost(&error);
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
                self.poison_if_lost(&error);
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
        if result.as_ref().is_err_and(PetError::lost) {
            state.process.take();
            state.workspace.take();
        }
        result
    }

    fn call_live(&self, method: &str, params: Value) -> Result<Value, PetError> {
        let mut state = self.lock_state();
        let Some(process) = state.process.as_mut() else {
            return Err(PetError::ProcessLost("PET process is not running".into()));
        };
        let result = process.rpc(method, params);
        if result.as_ref().is_err_and(PetError::lost) {
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
            if result.as_ref().is_err_and(PetError::lost) {
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
    fn poison_if_lost(&self, error: &PetError) {
        if error.is_transport_loss() {
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
    next_id: u64,
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
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
            #[cfg(target_os = "linux")]
            {
                // Design note: a forced MCP/server crash must not orphan a
                // PET process holding a large Rocq environment.  Install the
                // kernel parent-death signal before exec.  PET is also placed
                // in its own process group so normal shutdown can reap all of
                // its descendants.
                unsafe {
                    command.pre_exec(move || {
                        let result = nix::libc::prctl(
                            nix::libc::PR_SET_PDEATHSIG,
                            nix::libc::SIGKILL,
                            0,
                            0,
                            0,
                        );
                        if result == -1 {
                            Err(std::io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    });
                }
            }
        }
        let mut child = command
            .spawn()
            .map_err(|_| PetError::Environment("PET executable is unavailable".into()))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            terminate_child(&mut child);
            PetError::ProcessLost("PET stdin is unavailable".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            terminate_child(&mut child);
            PetError::ProcessLost("PET stdout is unavailable".into())
        })?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, PetError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| PetError::Protocol("PET request id space exhausted".into()))?;
        let request = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }))
        .map_err(|_| PetError::Invalid("PET request encoding failed".into()))?;
        if request.len() > MAX_REQUEST_BYTES {
            return Err(PetError::Invalid("PET request is oversized".into()));
        }
        write_frame(&mut self.stdin, &request)?;
        read_response(&mut self.stdout, id)
    }
}

impl Drop for PetProcess {
    fn drop(&mut self) {
        terminate_child(&mut self.child);
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
            "PET is missing required capabilities: {}",
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

fn write_frame(writer: &mut ChildStdin, body: &[u8]) -> Result<(), PetError> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())
        .map_err(|_| PetError::ProcessLost("PET stdin write failed".into()))?;
    writer
        .write_all(body)
        .and_then(|_| writer.flush())
        .map_err(|_| PetError::ProcessLost("PET stdin write failed".into()))
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
    reader
        .read_exact(&mut body)
        .map_err(|_| PetError::ProcessLost("PET stdout closed".into()))?;
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
        .map_or(start, |at| at + 1);
    &value[start..end]
}

fn read_line_bounded(
    reader: &mut BufReader<ChildStdout>,
    line: &mut Vec<u8>,
) -> Result<(), PetError> {
    loop {
        let mut byte = [0];
        reader
            .read_exact(&mut byte)
            .map_err(|_| PetError::ProcessLost("PET stdout closed".into()))?;
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

mod results;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
            "refresh_workspace_v1"]
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
            "refresh_workspace_v1", "state_count_v1"]}}
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
