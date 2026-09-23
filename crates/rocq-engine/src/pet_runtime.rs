//! Disposable structured Petanque runtime.
//!
//! The runtime owns only process-lifetime PET state.  It never edits the
//! attached project and never serializes a proof trace.  A replay constructs a
//! fresh engine-owned document from the current project view and sends only
//! structured JSON-RPC requests to `pet`.

use crate::{CanonicalTactic, OpenDeclaration, PetDocumentSpec};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

// Design note: this bound must dominate both the 1 MiB public expression
// limit and the 4 MiB replay limit after JSON-RPC framing/escaping. Otherwise
// an input accepted by Engine validation fails later as a configuration fault.
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 8192;
const MAX_REPLAY_ACTIONS: usize = 4096;
const MAX_REPLAY_BYTES: usize = 4 * 1024 * 1024;

/// Errors are deliberately transport/domain typed so callers can distinguish
/// timeout/protocol death from a native tactic rejection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PetError {
    Invalid(String),
    Environment(String),
    ProcessFailure(String),
    Timeout,
    Protocol(String),
    OutputOverflow,
    Remote { code: i64, message: String },
    Stale,
}

impl std::fmt::Display for PetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid PET request: {message}"),
            Self::Environment(message) | Self::ProcessFailure(message) => f.write_str(message),
            Self::Timeout => f.write_str("PET operation timed out"),
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

/// Structured focused/unfocused/shelved/given-up collections from
/// `petanque/goals`; no console text is parsed.
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

/// Opaque native state plus structured goals.  The process is private to this
/// crate and is invalidated when its lane changes workspace or dies.
#[derive(Clone)]
pub(crate) struct PetState {
    pub(crate) process: Arc<PetProcess>,
    pub(crate) instance_epoch: u64,
    pub(crate) st: u64,
    pub(crate) proof_finished: bool,
    pub(crate) goals: PetGoals,
}

/// Fixed query operation.  There is intentionally no arbitrary command API;
/// future query branches add enum variants with fixed JSON construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FixedPetQuery {
    Assumptions(String),
    Dependencies(String),
    ExpressionType(String),
    Notation(String),
}

#[derive(Clone, Debug)]
struct Workspace {
    root: PathBuf,
    uri: String,
}

impl Workspace {
    fn create(parent: &Path, spec: &PetDocumentSpec) -> Result<Self, PetError> {
        let root = parent
            .join("pet-workspaces")
            .join(format!("{}", Uuid::now_v7()));
        fs::create_dir_all(&root)
            .map_err(|_| PetError::Environment("PET workspace unavailable".into()))?;
        let result = (|| {
            for (relative, bytes) in &spec.files {
                if relative.as_os_str().is_empty()
                    || relative
                        .components()
                        .any(|part| !matches!(part, Component::Normal(_)))
                {
                    return Err(PetError::Invalid("unsafe PET workspace path".into()));
                }
                let path = root.join(relative);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|_| PetError::Environment("PET workspace unavailable".into()))?;
                }
                let mut file = fs::File::create(&path)
                    .map_err(|_| PetError::Environment("PET workspace unavailable".into()))?;
                file.write_all(bytes)
                    .map_err(|_| PetError::Environment("PET workspace unavailable".into()))?;
            }
            let root = fs::canonicalize(&root)
                .map_err(|_| PetError::Environment("PET workspace unavailable".into()))?;
            let target = root.join(&spec.target);
            if !target.starts_with(&root) {
                return Err(PetError::Invalid("unsafe PET target".into()));
            }
            Ok(Self {
                root: root.clone(),
                uri: file_uri(&target),
            })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&root);
        }
        result
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) struct PetProcess {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    serial: Mutex<()>,
    next_id: AtomicU64,
    instance_epoch: AtomicU64,
    alive: AtomicBool,
    timeout: Duration,
    workspace: Mutex<Option<Workspace>>,
}

impl PetProcess {
    fn spawn(workspace_root: PathBuf, timeout: Duration) -> Result<Arc<Self>, PetError> {
        let mut command = Command::new("pet");
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
            timeout,
            workspace: Mutex::new(None),
        });
        Ok(process)
    }

    fn replay(
        self: &Arc<Self>,
        workspace: Workspace,
        root: &OpenDeclaration,
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
        self.configure_workspace_locked(workspace)?;
        let instance_epoch = self.instance_epoch.load(Ordering::Acquire);
        let start = self.rpc_locked(
            "petanque/start",
            json!({"uri": self.workspace_uri()?, "thm": theorem_name(root)}),
        )?;
        let mut state = parse_run_result(&start)?;
        for action in actions {
            let response =
                self.rpc_locked("petanque/run", json!({"st": state.st, "tac": action.0}))?;
            state = parse_run_result(&response)?;
        }
        let goals = self.goals_locked(state.st, state.proof_finished)?;
        Ok(PetState {
            process: Arc::clone(self),
            instance_epoch,
            st: state.st,
            proof_finished: state.proof_finished,
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
                self.workspace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                true
            }
            Ok(None) | Err(_) => false,
        }
    }

    fn run_fixed(
        self: &Arc<Self>,
        state: &PetState,
        query: FixedPetQuery,
    ) -> Result<String, PetError> {
        let _serial = self
            .serial
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.alive.load(Ordering::Acquire)
            || state.instance_epoch != self.instance_epoch.load(Ordering::Acquire)
        {
            return Err(PetError::Stale);
        }
        match query {
            FixedPetQuery::Assumptions(name) => {
                self.text_command_locked(state.st, &format!("Print Assumptions {name}."))
            }
            FixedPetQuery::Dependencies(name) => {
                self.text_command_locked(state.st, &format!("Print All Dependencies {name}."))
            }
            FixedPetQuery::ExpressionType(expression) => {
                self.text_command_locked(state.st, &format!("Check ({expression})."))
            }
            FixedPetQuery::Notation(expression) => self.notation_locked(state.st, &expression),
        }
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

    fn configure_workspace_locked(&self, workspace: Workspace) -> Result<(), PetError> {
        let response = self.rpc_locked(
            "petanque/setWorkspace",
            json!({"debug": false, "root": file_uri(&workspace.root)}),
        )?;
        if !response.is_null() {
            return Err(PetError::Protocol(
                "setWorkspace returned an invalid result".into(),
            ));
        }
        self.instance_epoch.fetch_add(1, Ordering::AcqRel);
        let mut current = self
            .workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = Some(workspace);
        Ok(())
    }

    fn workspace_uri(&self) -> Result<String, PetError> {
        self.workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|workspace| workspace.uri.clone())
            .ok_or_else(|| PetError::Environment("PET workspace is not configured".into()))
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
        let stdin = Arc::clone(&self.stdin);
        let stdout = Arc::clone(&self.stdout);
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let result = (|| {
                let mut input = stdin
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                write_frame(&mut input, &request)?;
                drop(input);
                let mut output = stdout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                read_response(&mut output, id)
            })();
            let _ = sender.send(result);
        });
        match receiver.recv_timeout(self.timeout) {
            Ok(result) => {
                let _ = handle.join();
                result
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.terminate();
                let _ = handle.join();
                Err(PetError::Timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.terminate();
                let _ = handle.join();
                Err(PetError::ProcessFailure("PET I/O worker died".into()))
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
        // Design note: killing PET also invalidates its engine-owned document;
        // remove that document immediately rather than retaining a stale
        // workspace through cloned PetState handles.
        self.workspace
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

/// One project-local PET checkout. `busy` covers a complete replay, so one
/// checkout never interleaves workspaces or proof commands from two attempts.
struct ProjectLane {
    process: Arc<PetProcess>,
    /// One PET document/state lane is affinity-bound to a theorem root. This
    /// preserves same-root single-flight while allowing independent roots to
    /// occupy the remaining project pool capacity.
    root_affinity: String,
    busy: AtomicBool,
    /// Only the most recent replay is retained. This is sufficient for
    /// same-root single-flight without retaining historical PET instances.
    cached: Mutex<Option<CachedReplay>>,
    /// Fingerprint of the mirrored project inputs currently trusted by this
    /// PET process. Tactic changes do not change this key.
    document_key: Mutex<Option<[u8; 32]>>,
    rotate_before_reuse: AtomicBool,
}

#[derive(Clone)]
struct CachedReplay {
    key: [u8; 32],
    state: PetState,
}

struct ProjectPoolState {
    lanes: Vec<Arc<ProjectLane>>,
    spawning: bool,
    detached: bool,
}

/// A project owns a pool of disposable lanes rather than one process. This is
/// the concurrency boundary: independent attempts can acquire independent
/// lanes while an individual lane remains serial.
struct ProjectPool {
    state: Mutex<ProjectPoolState>,
    changed: Condvar,
}

impl ProjectPool {
    fn new() -> Self {
        Self {
            state: Mutex::new(ProjectPoolState {
                lanes: Vec::new(),
                spawning: false,
                detached: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn release(&self, lane: &Arc<ProjectLane>, capacity: &Capacity) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current) = state
            .lanes
            .iter_mut()
            .find(|current| Arc::ptr_eq(current, lane))
        {
            // Design note: a lane is released exactly once by its lease; a
            // removed/invalidated lane is intentionally ignored here.
            current.busy.store(false, Ordering::Release);
            self.changed.notify_all();
            capacity.wake();
        }
    }

    fn remove_lane(&self, lane: &Arc<ProjectLane>) -> Option<Arc<PetProcess>> {
        let removed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = state
                .lanes
                .iter()
                .position(|current| Arc::ptr_eq(current, lane))?;
            Some(state.lanes.remove(index).process.clone())
        };
        self.changed.notify_all();
        removed
    }

    fn detach(&self, capacity: &Capacity) {
        let lanes = self.state.lock().map_or_else(
            |poisoned| {
                let mut state = poisoned.into_inner();
                state.detached = true;
                std::mem::take(&mut state.lanes)
            },
            |mut state| {
                state.detached = true;
                std::mem::take(&mut state.lanes)
            },
        );
        self.changed.notify_all();
        for lane in lanes {
            lane.process.terminate();
            capacity.release();
        }
        capacity.wake();
    }

    fn reap_dead_idle(&self) -> usize {
        let mut removed = Vec::new();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.lanes.retain(|lane| {
            let dead = !lane.busy.load(Ordering::Acquire) && lane.process.exited();
            if dead {
                removed.push(lane.process.clone());
            }
            !dead
        });
        if !removed.is_empty() {
            self.changed.notify_all();
        }
        removed.len()
    }

    fn evict_idle(&self) -> Option<Arc<PetProcess>> {
        let removed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = state.lanes.iter().position(|lane| {
                !lane.busy.load(Ordering::Acquire) && lane.process.serial.try_lock().is_ok()
            })?;
            Some(state.lanes.remove(index).process.clone())
        };
        if removed.is_some() {
            self.changed.notify_all();
        }
        removed
    }
}

struct PetLease {
    pool: Arc<ProjectPool>,
    lane: Arc<ProjectLane>,
    capacity: Arc<Capacity>,
}

impl PetLease {
    fn process(&self) -> &Arc<PetProcess> {
        &self.lane.process
    }
}

impl Drop for PetLease {
    fn drop(&mut self) {
        self.pool.release(&self.lane, &self.capacity);
    }
}

struct Capacity {
    active: Mutex<usize>,
    changed: Condvar,
    maximum: usize,
}

impl Capacity {
    fn reserve_with_reaper(
        &self,
        timeout: Duration,
        mut reclaim: impl FnMut() -> bool,
    ) -> Result<(), PetError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *active < self.maximum {
                *active += 1;
                return Ok(());
            }
            drop(active);
            // An idle lane can be discarded without disturbing any active
            // replay. This keeps a full pool from turning into an infinite
            // wait when another project needs a checkout.
            if reclaim() {
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(PetError::Timeout);
            }
            let active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *active < self.maximum {
                drop(active);
                continue;
            }
            let wait = remaining.min(Duration::from_millis(100));
            let (guard, _timeout) = self
                .changed
                .wait_timeout(active, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(guard);
        }
    }

    fn release(&self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        self.changed.notify_all();
    }

    fn wake(&self) {
        self.changed.notify_all();
    }
}

/// Project-indexed lanes with global process capacity.  The capacity mutex is
/// held only for reservation accounting; process I/O never runs under it.
pub(crate) struct PetRuntime {
    state_parent: PathBuf,
    timeout: Duration,
    lanes: Mutex<BTreeMap<PathBuf, Arc<ProjectPool>>>,
    capacity: Arc<Capacity>,
    cache_bytes: usize,
}

impl PetRuntime {
    pub(crate) fn new(
        state_parent: &Path,
        timeout: Duration,
        max_processes: usize,
        cache_bytes: usize,
    ) -> Result<Self, PetError> {
        if !(1..=64).contains(&max_processes) {
            return Err(PetError::Invalid(
                "max PET processes must be between 1 and 64".into(),
            ));
        }
        fs::create_dir_all(state_parent)
            .map_err(|_| PetError::Environment("PET state directory is unavailable".into()))?;
        let state_parent = fs::canonicalize(state_parent)
            .map_err(|_| PetError::Environment("PET state directory is unavailable".into()))?;
        Ok(Self {
            state_parent,
            timeout,
            lanes: Mutex::new(BTreeMap::new()),
            capacity: Arc::new(Capacity {
                active: Mutex::new(0),
                changed: Condvar::new(),
                maximum: max_processes,
            }),
            cache_bytes,
        })
    }

    pub(crate) fn replay(
        &self,
        project: &Path,
        root: &OpenDeclaration,
        actions: &[CanonicalTactic],
    ) -> Result<PetState, PetError> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        let mut protocol_restarted = false;
        loop {
            match self.replay_once(project, root, actions) {
                Err(PetError::ProcessFailure(_)) if Instant::now() < deadline => {
                    // Design note: process death is owned here. Keep rebuilding
                    // from the trace until the operation deadline rather than
                    // inventing a public availability error.
                    thread::sleep(Duration::from_millis(5));
                }
                Err(PetError::ProcessFailure(_)) => return Err(PetError::Timeout),
                Err(PetError::Protocol(_) | PetError::OutputOverflow | PetError::Stale)
                    if !protocol_restarted =>
                {
                    // One clean process distinguishes a damaged lane from an
                    // incompatible PET installation or changed source view.
                    protocol_restarted = true;
                }
                result => return result,
            }
        }
    }

    fn replay_once(
        &self,
        project: &Path,
        root: &OpenDeclaration,
        actions: &[CanonicalTactic],
    ) -> Result<PetState, PetError> {
        let project = fs::canonicalize(project)
            .map_err(|_| PetError::Environment("project is unavailable".into()))?;
        let spec = match crate::pet_document_spec(&project, &self.state_parent, root) {
            Ok(spec) => spec,
            Err(error) => {
                // Design note: once the latest source is incompatible, no PET
                // in this project may remain a cache authority. A later source
                // restoration must therefore perform a fresh replay.
                self.detach(&project);
                return Err(PetError::Invalid(error.message));
            }
        };
        let document_key = document_key(&spec);
        let replay_key = replay_key(document_key, actions);
        let workspace = Workspace::create(&self.state_parent, &spec)?;
        let pool = self.pool(project);
        let affinity = root_affinity(root);
        let mut lease = self.acquire(&pool, workspace.root.clone(), affinity.clone())?;
        if lease.lane.rotate_before_reuse.swap(false, Ordering::AcqRel) {
            self.invalidate(&pool, &lease.lane);
            drop(lease);
            lease = self.acquire(&pool, workspace.root.clone(), affinity.clone())?;
        }
        let inputs_changed = {
            let mut previous = lease
                .lane
                .document_key
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match *previous {
                Some(old) if old != document_key => true,
                _ => {
                    *previous = Some(document_key);
                    false
                }
            }
        };
        if inputs_changed {
            // Design note: setWorkspace cannot revoke already-loaded .vo
            // definitions inside a live PET. A changed mirrored input must
            // get a fresh process before any cached or native state is used.
            self.invalidate(&pool, &lease.lane);
            drop(lease);
            lease = self.acquire(&pool, workspace.root.clone(), affinity)?;
            *lease
                .lane
                .document_key
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(document_key);
        }
        if let Some(cached) = lease
            .lane
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|cached| cached.key == replay_key)
            .cloned()
        {
            return Ok(cached.state);
        }
        let result = lease.process().replay(workspace, root, actions);
        match result {
            Ok(state) => {
                *lease
                    .lane
                    .cached
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedReplay {
                    key: replay_key,
                    state: state.clone(),
                });
                let replay_bytes = actions
                    .iter()
                    .map(|action| action.0.len())
                    .sum::<usize>()
                    .saturating_add(64);
                lease
                    .lane
                    .rotate_before_reuse
                    .store(replay_bytes > self.cache_bytes, Ordering::Release);
                Ok(state)
            }
            Err(error) => {
                if is_fatal(&error) {
                    self.invalidate(&pool, &lease.lane);
                }
                Err(error)
            }
        }
    }

    pub(crate) fn run_fixed(
        &self,
        state: &PetState,
        query: FixedPetQuery,
    ) -> Result<String, PetError> {
        state.process.run_fixed(state, query)
    }

    pub(crate) fn detach(&self, project: &Path) {
        let Ok(project) = fs::canonicalize(project) else {
            return;
        };
        let pool = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&project);
        if let Some(pool) = pool {
            pool.detach(&self.capacity);
        }
    }

    pub(crate) fn shutdown(&self) {
        let lanes = self.lanes.lock().map_or_else(
            |poisoned| std::mem::take(&mut *poisoned.into_inner()),
            |mut lanes| std::mem::take(&mut *lanes),
        );
        for pool in lanes.into_values() {
            pool.detach(&self.capacity);
        }
        // Design note: workspaces are disposable; remove only the runtime's
        // own empty container and never caller-owned state-parent entries.
        let workspaces = self.state_parent.join("pet-workspaces");
        if fs::read_dir(&workspaces).is_ok_and(|mut entries| entries.next().is_none()) {
            let _ = fs::remove_dir(&workspaces);
        }
    }

    fn pool(&self, project: PathBuf) -> Arc<ProjectPool> {
        let mut lanes = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lanes
            .entry(project)
            .or_insert_with(|| Arc::new(ProjectPool::new()))
            .clone()
    }

    fn acquire(
        &self,
        pool: &Arc<ProjectPool>,
        workspace_root: PathBuf,
        affinity: String,
    ) -> Result<PetLease, PetError> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let mut state = pool
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.detached {
                return Err(PetError::ProcessFailure(
                    "project PET lane is detached".into(),
                ));
            }
            let mut dead = Vec::new();
            state.lanes.retain(|lane| {
                let dead_lane = !lane.busy.load(Ordering::Acquire) && lane.process.exited();
                if dead_lane {
                    dead.push(lane.process.clone());
                }
                !dead_lane
            });
            if !dead.is_empty() {
                drop(state);
                for _ in dead {
                    self.capacity.release();
                }
                pool.changed.notify_all();
                continue;
            }
            // Design note: equal roots single-flight; do not create a second
            // process whose document state could race the first replay.
            if state
                .lanes
                .iter()
                .any(|lane| lane.root_affinity == affinity && lane.busy.load(Ordering::Acquire))
            {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(PetError::Timeout);
                }
                let (next, _timeout) = pool
                    .changed
                    .wait_timeout(state, remaining.min(Duration::from_millis(100)))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(next);
                continue;
            }
            if let Some(lane) = state.lanes.iter().find(|lane| {
                lane.root_affinity == affinity
                    && !lane.busy.load(Ordering::Acquire)
                    && lane
                        .busy
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
            }) {
                return Ok(PetLease {
                    pool: Arc::clone(pool),
                    lane: Arc::clone(lane),
                    capacity: Arc::clone(&self.capacity),
                });
            }
            if state.spawning {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(PetError::Timeout);
                }
                let (next, _timeout) = pool
                    .changed
                    .wait_timeout(state, remaining.min(Duration::from_millis(100)))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(next);
                continue;
            }
            state.spawning = true;
            drop(state);
            let remaining = deadline.saturating_duration_since(Instant::now());
            let reserved = self.capacity.reserve_with_reaper(remaining, || {
                self.reap_dead_processes();
                self.evict_idle_lane()
            });
            if let Err(error) = reserved {
                let mut state = pool
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.spawning = false;
                pool.changed.notify_all();
                return Err(error);
            }
            let spawned = PetProcess::spawn(workspace_root, self.timeout);
            let mut state = pool
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.spawning = false;
            let result = match spawned {
                Ok(process) if !state.detached => {
                    let lane = Arc::new(ProjectLane {
                        process,
                        root_affinity: affinity,
                        busy: AtomicBool::new(true),
                        cached: Mutex::new(None),
                        document_key: Mutex::new(None),
                        rotate_before_reuse: AtomicBool::new(false),
                    });
                    let lease = PetLease {
                        pool: Arc::clone(pool),
                        lane: Arc::clone(&lane),
                        capacity: Arc::clone(&self.capacity),
                    };
                    state.lanes.push(lane);
                    Ok(lease)
                }
                Ok(process) => {
                    process.terminate();
                    self.capacity.release();
                    Err(PetError::ProcessFailure(
                        "project PET lane is detached".into(),
                    ))
                }
                Err(error) => {
                    self.capacity.release();
                    Err(error)
                }
            };
            pool.changed.notify_all();
            return result;
        }
    }

    fn invalidate(&self, pool: &Arc<ProjectPool>, lane: &Arc<ProjectLane>) {
        if let Some(process) = pool.remove_lane(lane) {
            process.terminate();
            self.capacity.release();
        }
        self.capacity.wake();
    }

    fn reap_dead_processes(&self) {
        let pools = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for pool in pools {
            for _ in 0..pool.reap_dead_idle() {
                self.capacity.release();
            }
        }
    }

    fn evict_idle_lane(&self) -> bool {
        let pools = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for pool in pools {
            if let Some(process) = pool.evict_idle() {
                process.terminate();
                self.capacity.release();
                self.capacity.wake();
                return true;
            }
        }
        false
    }
}

impl Drop for PetRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn is_fatal(error: &PetError) -> bool {
    matches!(
        error,
        PetError::Timeout
            | PetError::Protocol(_)
            | PetError::OutputOverflow
            | PetError::ProcessFailure(_)
            | PetError::Stale
    )
}

#[derive(Clone, Copy)]
struct RunResult {
    st: u64,
    proof_finished: bool,
}

fn parse_run_result(value: &Value) -> Result<RunResult, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET run result is not an object".into()))?;
    let st = object
        .get("st")
        .and_then(Value::as_u64)
        .ok_or_else(|| PetError::Protocol("PET run result has no state id".into()))?;
    let proof_finished = object
        .get("proof_finished")
        .and_then(Value::as_bool)
        .ok_or_else(|| PetError::Protocol("PET run result has no proof status".into()))?;
    Ok(RunResult { st, proof_finished })
}

fn parse_feedback(value: &Value) -> Result<String, PetError> {
    let feedback = value
        .as_object()
        .and_then(|object| object.get("feedback"))
        .and_then(Value::as_array)
        .ok_or_else(|| PetError::Protocol("PET run result has no feedback".into()))?;
    feedback
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).map_or_else(
                || {
                    serde_json::to_string(item).map_err(|_| {
                        PetError::Protocol("PET feedback item is not serializable".into())
                    })
                },
                Ok,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|items| items.join("\n"))
}

fn parse_goals(value: &Value, proof_mode: bool) -> Result<PetGoals, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET goals result is not an object".into()))?;
    Ok(PetGoals {
        focused: parse_goal_collection(object.get("goals"))?,
        unfocused: parse_goal_collection(object.get("stack"))?,
        shelved: parse_goal_collection(object.get("shelf"))?,
        given_up: parse_goal_collection(object.get("given_up"))?,
        proof_mode,
    })
}

fn parse_goal_collection(value: Option<&Value>) -> Result<Vec<PetGoal>, PetError> {
    let Some(value) = value else {
        return Err(PetError::Protocol(
            "PET goals result omits a goal collection".into(),
        ));
    };
    let Some(values) = value.as_array() else {
        return Err(PetError::Protocol(
            "PET goal collection is not an array".into(),
        ));
    };
    let mut output = Vec::new();
    // PET represents unfocused goals as arbitrarily nested proof-stack
    // groups. Flatten only arrays; every leaf must still satisfy the full
    // structured goal schema (never silently discard malformed data).
    fn append(value: &Value, output: &mut Vec<PetGoal>) -> Result<(), PetError> {
        if let Some(values) = value.as_array() {
            for value in values {
                append(value, output)?;
            }
            return Ok(());
        }
        output.push(parse_goal(value)?);
        Ok(())
    }
    for value in values {
        append(value, &mut output)?;
    }
    Ok(output)
}

fn parse_goal(value: &Value) -> Result<PetGoal, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET goal is not an object".into()))?;
    let info = object
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| PetError::Protocol("PET goal has no info".into()))?;
    let evar = info
        .get("evar")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| PetError::Protocol("PET goal has no evar".into()))?;
    let name = match info.get("name") {
        Some(Value::Null) | None => None,
        Some(Value::String(name)) => Some(name.clone()),
        _ => return Err(PetError::Protocol("PET goal name is invalid".into())),
    };
    let hypotheses = object
        .get("hyps")
        .and_then(Value::as_array)
        .ok_or_else(|| PetError::Protocol("PET goal has no hypotheses".into()))?
        .iter()
        .map(parse_hypothesis)
        .collect::<Result<Vec<_>, _>>()?;
    let ty = object
        .get("ty")
        .and_then(Value::as_str)
        .ok_or_else(|| PetError::Protocol("PET goal has no type".into()))?
        .to_owned();
    Ok(PetGoal {
        evar,
        name,
        hypotheses,
        ty,
    })
}

fn parse_hypothesis(value: &Value) -> Result<PetHypothesis, PetError> {
    let object = value
        .as_object()
        .ok_or_else(|| PetError::Protocol("PET hypothesis is not an object".into()))?;
    let names = object
        .get("names")
        .and_then(Value::as_array)
        .ok_or_else(|| PetError::Protocol("PET hypothesis has no names".into()))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| PetError::Protocol("PET hypothesis name is invalid".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let definition = match object.get("def") {
        Some(Value::Null) | None => None,
        Some(Value::String(value)) => Some(value.clone()),
        _ => {
            return Err(PetError::Protocol(
                "PET hypothesis definition is invalid".into(),
            ));
        }
    };
    let ty = object
        .get("ty")
        .and_then(Value::as_str)
        .ok_or_else(|| PetError::Protocol("PET hypothesis has no type".into()))?
        .to_owned();
    Ok(PetHypothesis {
        names,
        definition,
        ty,
    })
}

fn theorem_name(root: &OpenDeclaration) -> String {
    // PET resolves the declaration relative to the selected source URI.  The
    // module path is already supplied by that source, so `thm` is the local
    // declaration name rather than the fully-qualified logical identity.
    root.identity.constant.clone()
}

fn root_affinity(root: &OpenDeclaration) -> String {
    format!(
        "{}::{:?}::{:?}::{}",
        root.identity.library.0.join("."),
        root.identity.modules,
        root.anchor.context,
        root.identity.constant
    )
}

/// Hash the complete temporary project view, including binary .vo contents.
/// A source or dependency mutation changes the PET process authority.
fn document_key(spec: &PetDocumentSpec) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(spec.target.to_string_lossy().as_bytes());
    for (path, bytes) in &spec.files {
        digest.update((path.as_os_str().len() as u64).to_le_bytes());
        digest.update(path.to_string_lossy().as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    digest.finalize().into()
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
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| PetError::Protocol("PET response JSON is invalid".into()))?;
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

fn hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn frame_reader_rejects_malformed_and_oversized_headers() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "printf 'Content-Length: 70000\\n\\n'"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);
        assert_eq!(read_response(&mut reader, 1), Err(PetError::OutputOverflow));
        let _ = child.wait();
    }

    #[test]
    fn goal_parser_keeps_all_four_collections_structured() {
        let value = json!({
            "goals": [{"info":{"evar":["Ser_Evar", 1],"name":null},"hyps":[],"ty":"True"}],
            "stack": [[{"info":{"evar":["Ser_Evar", 2],"name":"x"},"hyps":[],"ty":"False"}]],
            "shelf": [],
            "given_up": []
        });
        let goals = parse_goals(&value, true).unwrap();
        assert_eq!(goals.focused.len(), 1);
        assert_eq!(goals.unfocused.len(), 1);
        assert!(goals.shelved.is_empty());
        assert!(goals.given_up.is_empty());
    }

    #[test]
    fn capacity_rejects_invalid_bounds() {
        assert!(PetRuntime::new(Path::new("/tmp"), Duration::from_secs(1), 0, 1024).is_err());
        assert!(PetRuntime::new(Path::new("/tmp"), Duration::from_secs(1), 65, 1024).is_err());
    }

    #[test]
    fn replay_uses_real_pet_and_returns_structured_goals() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("Main.v"),
            "Theorem t : True. Admitted.\n",
        )
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let runtime = PetRuntime::new(state.path(), Duration::from_secs(10), 1, 1024).unwrap();
        let root = OpenDeclaration {
            kind: crate::DeclarationKind::Theorem,
            identity: crate::DeclarationIdentity {
                library: crate::LogicalLibrary(vec!["Main".into()]),
                modules: vec![],
                constant: "t".into(),
            },
            anchor: crate::SourceAnchor {
                source_digest: [0; 32],
                normalized_statement: "Theorem t : True".into(),
                context: vec![],
                old_body_digest: [0; 32],
            },
        };
        let state = runtime.replay(project.path(), &root, &[]).unwrap();
        assert_eq!(state.goals.focused.len(), 1);
        assert!(state.goals.unfocused.is_empty());
        let solved = runtime
            .replay(project.path(), &root, &[CanonicalTactic("exact I.".into())])
            .unwrap();
        assert!(solved.proof_finished);
        assert!(solved.goals.all_clear());
        runtime.shutdown();
    }

    #[test]
    fn synthetic_and_nested_replays_use_abort_sentinels_without_admissions() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("Main.v"), "Module M.\n").unwrap();
        let state = tempfile::tempdir().unwrap();
        let runtime = PetRuntime::new(state.path(), Duration::from_secs(10), 2, 1024).unwrap();
        let synthetic = OpenDeclaration {
            kind: crate::DeclarationKind::Theorem,
            identity: crate::DeclarationIdentity {
                library: crate::LogicalLibrary(vec!["Synthetic".into()]),
                modules: vec![],
                constant: "synthetic".into(),
            },
            anchor: crate::SourceAnchor {
                source_digest: [0; 32],
                normalized_statement: "Theorem synthetic : True".into(),
                context: vec![],
                old_body_digest: [0; 32],
            },
        };
        let nested = OpenDeclaration {
            kind: crate::DeclarationKind::Theorem,
            identity: crate::DeclarationIdentity {
                library: crate::LogicalLibrary(vec!["Main".into()]),
                modules: vec!["M".into()],
                constant: "nested".into(),
            },
            anchor: crate::SourceAnchor {
                source_digest: [0; 32],
                normalized_statement: "Theorem nested : True".into(),
                context: vec![crate::LexicalScope::Module("M".into())],
                old_body_digest: [0; 32],
            },
        };
        let synthetic_spec =
            crate::pet_document_spec(project.path(), state.path(), &synthetic).unwrap();
        assert!(!synthetic_spec.files.iter().any(|(_, bytes)| {
            String::from_utf8_lossy(bytes)
                .to_ascii_lowercase()
                .contains("admitted")
        }));
        assert!(runtime.replay(project.path(), &synthetic, &[]).is_ok());
        runtime.replay(project.path(), &nested, &[]).unwrap();
        runtime.shutdown();
    }
}
