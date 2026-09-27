//! Independent public-API tests.  Every proof semantic assertion runs installed
//! Rocq on a disposable project; no simulated prover is used here.

use rocq_engine::{
    DeclarationIdentity, DeclarationKind, Engine, EngineConfig, ErrorKind, ProofLifecycle,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};
use tempfile::TempDir;

struct Lab {
    project: TempDir,
    _state: TempDir,
    engine: Engine,
}

fn initialize_dune_project(root: &Path) {
    fs::write(
        root.join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    fs::write(
        root.join("dune"),
        "(rocq.theory (name Main) (generate_project_file))\n",
    )
    .unwrap();
}

impl Lab {
    fn new(source: &str) -> Self {
        Self::with_close_timeout(source, Some(Duration::from_secs(10)))
    }

    fn with_close_timeout(source: &str, close_timeout: Option<Duration>) -> Self {
        let project = tempfile::tempdir().unwrap();
        initialize_dune_project(project.path());
        fs::write(project.path().join("Main.v"), source).unwrap();
        let state = tempfile::tempdir().unwrap();
        let engine = Engine::new(EngineConfig {
            state_parent: state.path().join("state"),
            trace_memory_bytes: 1024,
            operation_timeout: Duration::from_secs(10),
            close_timeout,
            runtime_cache_bytes: 1024,
            max_pet_processes: 4,
        })
        .unwrap();
        Self {
            project,
            _state: state,
            engine,
        }
    }
    fn path(&self) -> &Path {
        self.project.path()
    }
    fn source(&self) -> String {
        fs::read_to_string(self.path().join("Main.v")).unwrap()
    }
}

fn open_theorem(
    engine: &rocq_engine::Engine,
    project: &Path,
    name: &str,
) -> Result<rocq_engine::ProofState, rocq_engine::Error> {
    let declarations = main_declarations(engine, project)?;
    let matches = declarations
        .iter()
        .filter(|item| item.identity.constant() == Some(name))
        .collect::<Vec<_>>();
    let declaration = match matches.as_slice() {
        [item] => *item,
        [] => {
            return Err(rocq_engine::Error::new(
                ErrorKind::NotFound,
                "test declaration not found",
            ));
        }
        _ => {
            return Err(rocq_engine::Error::new(
                ErrorKind::Ambiguous,
                "test declaration is ambiguous",
            ));
        }
    };
    engine.open(project, declaration.identity.clone())
}

fn main_declarations(
    engine: &rocq_engine::Engine,
    project: &Path,
) -> Result<Vec<rocq_engine::DeclarationInfo>, rocq_engine::Error> {
    let mut declarations = Vec::new();
    for file in engine.list_files(project)? {
        declarations.extend(engine.list_decls(project, &file)?);
    }
    Ok(declarations)
}

fn attempt(state: &rocq_engine::ProofState) -> rocq_engine::AttemptId {
    state
        .attempt
        .expect("an open proof returns an opaque attempt")
}

fn declaration_id(engine: &Engine, project: &Path, constant: &str) -> DeclarationIdentity {
    main_declarations(engine, project)
        .unwrap()
        .into_iter()
        .find(|declaration| declaration.identity.constant() == Some(constant))
        .map(|declaration| declaration.identity)
        .unwrap_or_else(|| panic!("declaration {constant} not found"))
}
fn err_kind<T>(value: Result<T, rocq_engine::Error>) -> ErrorKind {
    match value {
        Ok(_) => panic!("expected public error"),
        Err(error) => error.kind,
    }
}
fn check_one(
    engine: &Engine,
    attempt: rocq_engine::AttemptId,
    fragment: &str,
) -> Result<rocq_engine::CheckResult, rocq_engine::Error> {
    engine.check(attempt, &[fragment.to_owned()])
}
fn rejected_check_kind(result: Result<rocq_engine::CheckResult, rocq_engine::Error>) -> ErrorKind {
    match result {
        Err(error) => error.kind,
        Ok(result) => {
            result
                .rejected
                .into_iter()
                .next()
                .or(result.error)
                .expect("rejected fragment must report an error")
                .kind
        }
    }
}
fn compile(project: &Path) {
    let output = Command::new("dune")
        .args(["build", "Main.vo"])
        .current_dir(project)
        .output()
        .expect("installed rocq");
    assert!(
        output.status.success(),
        "independent Dune build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn true_theorems() -> &'static str {
    "Theorem t : True. Admitted.\nTheorem u : True. Admitted.\n"
}

fn engine_with_pet_capacity(state_parent: &Path, max_pet_processes: usize) -> Engine {
    Engine::new(EngineConfig {
        state_parent: state_parent.to_owned(),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1024,
        max_pet_processes,
    })
    .unwrap()
}

/// Write a transparent PET protocol proxy. By default it records proof-header
/// `run` requests and their original source bytes; isolated protocol tests can
/// set `ROCQ_ENGINE_LOG_ALL_PET` to record every request method instead.
fn pet_recording_proxy(directory: &Path) -> PathBuf {
    let script = directory.join("pet_proxy.py");
    fs::write(&script, r#"import json, os, subprocess, sys, threading, urllib.parse
p = subprocess.Popen([os.environ["ROCQ_ENGINE_REAL_PET"]], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
def copy_out():
    while True:
        data = os.read(p.stdout.fileno(), 8192)
        if not data: return
        sys.stdout.buffer.write(data); sys.stdout.buffer.flush()
threading.Thread(target=copy_out, daemon=True).start()
source, sink = sys.stdin.buffer, p.stdin
current_path = None
while True:
    header = b""
    while True:
        line = source.readline()
        if not line: sink.close(); p.wait(); sys.exit(p.returncode or 0)
        header += line
        if line in (b"\n", b"\r\n"): break
    size = next(int(x.split(b":",1)[1]) for x in header.splitlines() if x.lower().startswith(b"content-length:"))
    body = source.read(size)
    try:
        message = json.loads(body)
        method = message.get("method")
        params = message.get("params", {})
        if "uri" in params:
            current_path = urllib.parse.unquote(urllib.parse.urlparse(params["uri"]).path)
        tac = params.get("tac", "")
        log_all = os.environ.get("ROCQ_ENGINE_LOG_ALL_PET") is not None
        proof_header = method == "petanque/run" and (tac.startswith("Theorem ") or tac.startswith("Lemma ") or tac.startswith("Definition "))
        if log_all or proof_header:
            item = {"pid": os.getpid(), "method": method, "params": params}
            if current_path:
                item.update({"path": current_path, "content": open(current_path).read()})
            with open(os.environ["ROCQ_ENGINE_PET_LOG"], "a") as log:
                log.write(json.dumps(item) + "\n")
    except Exception as error:
        sys.exit("proxy error: " + repr(error))
    sink.write(header + body); sink.flush()
"#).unwrap();
    let pet = directory.join("pet");
    fs::write(
        &pet,
        format!("#!/bin/sh\nif [ \"$1\" = --version ]; then exec \"$ROCQ_ENGINE_REAL_PET\" \"$@\"; fi\nexec python3 '{}'\n", script.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pet, fs::Permissions::from_mode(0o755)).unwrap();
    }
    pet
}

/// Resolve the PET selected by the parent test process. Isolated proxy tests
/// must delegate to this executable and override `ROCQ_PET_BIN` in the child;
/// relying only on PATH would bypass the proxy whenever the parent explicitly
/// selected the pinned PET launcher.
fn real_pet_executable() -> PathBuf {
    std::env::var_os("ROCQ_PET_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let output = Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap();
            assert!(output.status.success(), "PET executable is unavailable");
            PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        })
}

#[test]
fn list_decls_uses_one_document_request_and_no_legacy_ast_requests() {
    const CHILD: &str = "ROCQ_ENGINE_LIST_DECLS_PROTOCOL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let log = wrapper.path().join("pet-requests.jsonl");
        let proxy = pet_recording_proxy(wrapper.path());
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "list_decls_uses_one_document_request_and_no_legacy_ast_requests",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", &real)
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env("ROCQ_ENGINE_LOG_ALL_PET", "1")
            .env("ROCQ_PET_BIN", &proxy)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated list_decls protocol test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Module A.\nTheorem same : True. Admitted.\nEnd A.\n\
         Module B.\nTheorem same : False. Admitted.\nEnd B.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 1);
    let file = engine
        .list_files(project.path())
        .unwrap()
        .into_iter()
        .find(|file| file.0 == "Main.v")
        .unwrap();
    let declarations = engine.list_decls(project.path(), &file).unwrap();
    assert_eq!(
        declarations
            .iter()
            .filter(|declaration| declaration.identity.constant() == Some("same"))
            .count(),
        2,
        "one document response must preserve duplicate leaves"
    );

    let log = PathBuf::from(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap());
    let methods = fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter_map(|entry| entry["method"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == "petanque/document_declarations")
            .count(),
        1,
        "list_decls must issue one document declaration request: {methods:?}"
    );
    for legacy in ["petanque/toc", "petanque/ast_at_pos", "petanque/ast"] {
        assert!(
            !methods.iter().any(|method| method == legacy),
            "list_decls must not use legacy {legacy}: {methods:?}"
        );
    }
}

/// Returns running proxy PIDs. The proxy is the PET process-group leader, so
/// this observes the actual native-process capacity rather than only start logs.
#[cfg(unix)]
fn live_recorded_pet_pids(log: &Path) -> BTreeSet<u32> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|entry| entry["pid"].as_u64().map(|pid| pid as u32))
        .filter(|pid| Path::new(&format!("/proc/{pid}")).exists())
        .collect()
}

#[cfg(unix)]
#[test]
fn pet_enforces_configured_capacity_and_single_flight_spawning() {
    const CHILD: &str = "ROCQ_ENGINE_CAPACITY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let log = wrapper.path().join("pet-starts.jsonl");
        let proxy = pet_recording_proxy(wrapper.path());
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pet_enforces_configured_capacity_and_single_flight_spawning",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", &real)
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env("ROCQ_PET_BIN", &proxy)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated PET-capacity test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    // A global one-process bound evicts project A before B starts; a replay of
    // A lazily recreates it without ever leaving two process-group leaders live.
    let state = tempfile::tempdir().unwrap();
    let one = engine_with_pet_capacity(state.path(), 1);
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    initialize_dune_project(a.path());
    initialize_dune_project(b.path());
    fs::write(a.path().join("Main.v"), "Theorem a : True. Admitted.\n").unwrap();
    fs::write(b.path().join("Main.v"), "Theorem b : True. Admitted.\n").unwrap();
    let a_attempt = attempt(&open_theorem(&one, a.path(), "a").unwrap());
    assert_eq!(
        live_recorded_pet_pids(&PathBuf::from(
            std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()
        ))
        .len(),
        1
    );
    let _b_attempt = attempt(&open_theorem(&one, b.path(), "b").unwrap());
    assert_eq!(
        live_recorded_pet_pids(&PathBuf::from(
            std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()
        ))
        .len(),
        1
    );
    assert!(
        one.inspect(a_attempt).is_ok(),
        "evicted selected traces replay lazily"
    );
    assert_eq!(
        live_recorded_pet_pids(&PathBuf::from(
            std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()
        ))
        .len(),
        1
    );
    drop(one);

    // Two independent projects retain two real PET process groups simultaneously.
    let state = tempfile::tempdir().unwrap();
    let two = engine_with_pet_capacity(state.path(), 2);
    let c = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    initialize_dune_project(c.path());
    initialize_dune_project(d.path());
    fs::write(c.path().join("Main.v"), "Theorem c : True. Admitted.\n").unwrap();
    fs::write(d.path().join("Main.v"), "Theorem d : True. Admitted.\n").unwrap();
    let _ = open_theorem(&two, c.path(), "c").unwrap();
    let _ = open_theorem(&two, d.path(), "d").unwrap();
    assert_eq!(
        live_recorded_pet_pids(&PathBuf::from(
            std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()
        ))
        .len(),
        2
    );
    drop(two);

    // More than four roots in one project must reach the configured capacity.
    // Eight sequential roots both fill all five lanes and exercise idle-lane
    // eviction without turning this resource invariant into a stress test.
    let state = tempfile::tempdir().unwrap();
    let five = engine_with_pet_capacity(state.path(), 5);
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    let source = (0..8)
        .map(|index| format!("Theorem t{index} : True. Admitted.\n"))
        .collect::<String>();
    fs::write(project.path().join("Main.v"), source).unwrap();
    for index in 0..8 {
        let _ = open_theorem(&five, project.path(), &format!("t{index}")).unwrap();
    }
    assert_eq!(
        live_recorded_pet_pids(&PathBuf::from(
            std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()
        ))
        .len(),
        5
    );
    drop(five);

    // Concurrent reconstruction of the same evicted root is serialised at the
    // lane: both callers succeed but only one process is spawned for that root.
    let state = tempfile::tempdir().unwrap();
    let single = Arc::new(engine_with_pet_capacity(state.path(), 1));
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem once : True. Admitted.\n",
    )
    .unwrap();
    let once = attempt(&open_theorem(&single, project.path(), "once").unwrap());
    let evictor = tempfile::tempdir().unwrap();
    initialize_dune_project(evictor.path());
    fs::write(
        evictor.path().join("Main.v"),
        "Theorem evictor : True. Admitted.\n",
    )
    .unwrap();
    let _ = open_theorem(&single, evictor.path(), "evictor").unwrap();
    let before = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap_or_default()
        .lines()
        .count();
    let gate = Arc::new(Barrier::new(3));
    let mut joins = Vec::new();
    for _ in 0..2 {
        let engine = Arc::clone(&single);
        let gate = Arc::clone(&gate);
        joins.push(thread::spawn(move || {
            gate.wait();
            engine.inspect(once).unwrap()
        }));
    }
    gate.wait();
    for join in joins {
        assert!(join.join().unwrap().attempt.is_some());
    }
    let after = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        after - before,
        1,
        "same-root reconstruction must be single-flight"
    );

    // Invalidation is scoped to its project: after A loses its current view,
    // B's cached PET state remains live and does not receive a replacement.
    let state = tempfile::tempdir().unwrap();
    let scoped = engine_with_pet_capacity(state.path(), 2);
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    initialize_dune_project(a.path());
    initialize_dune_project(b.path());
    fs::write(a.path().join("Main.v"), "Theorem a : True. Admitted.\n").unwrap();
    fs::write(b.path().join("Main.v"), "Theorem b : True. Admitted.\n").unwrap();
    let a_attempt = attempt(&open_theorem(&scoped, a.path(), "a").unwrap());
    let b_attempt = attempt(&open_theorem(&scoped, b.path(), "b").unwrap());
    let before_invalidation = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    // A repeated inspection of the same document/prefix is served by the
    // PET replay cache.  The engine retains the prefix, but never invents a
    // second completion result locally.
    assert!(scoped.inspect(b_attempt).is_ok());
    let after_first_inspect = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    assert!(scoped.inspect(b_attempt).is_ok());
    let after_second_inspect = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        after_second_inspect, after_first_inspect,
        "repeated inspection must not re-run PET for an unchanged prefix"
    );
    fs::write(a.path().join("Main.v"), "Theorem a : False. Admitted.\n").unwrap();
    assert_eq!(
        err_kind(scoped.inspect(a_attempt)),
        ErrorKind::DeclarationChanged
    );
    assert!(scoped.inspect(b_attempt).is_ok());
    let after_b = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        after_b, before_invalidation,
        "A invalidation must not restart B PET"
    );
    fs::write(a.path().join("Main.v"), "Theorem a : True. Admitted.\n").unwrap();
    assert!(scoped.inspect(a_attempt).is_ok());
    let after_restore = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    assert_eq!(after_restore, before_invalidation + 1);
}

#[test]
fn branching_attempts_do_not_conflict_or_advance_each_other() {
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let left = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let right = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let left = check_one(&lab.engine, left, "intro P.").unwrap();
    assert!(left.error.is_none());
    assert_eq!(
        lab.engine.inspect(right).unwrap().attempt,
        Some(right),
        "one logical interaction must not advance another cursor"
    );
    let right = check_one(&lab.engine, right, "intro Q.").unwrap();
    assert!(right.error.is_none());
    assert_ne!(left.state.attempt, right.state.attempt);
}

#[test]
fn retry_is_idempotent_and_rejected_multi_sentence_fragment_is_atomic() {
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let first = check_one(&lab.engine, root, "intro P.").unwrap();
    assert!(first.error.is_none());
    let retry = check_one(&lab.engine, root, " intro P. ").unwrap();
    assert!(retry.error.is_none());
    assert_eq!(
        retry.state.attempt, first.state.attempt,
        "equal canonical parent/action must share a child"
    );
    let partial = check_one(&lab.engine, root, "intro P. this_is_not_a_tactic.").unwrap();
    assert_eq!(
        partial.state.attempt,
        Some(root),
        "a rejected fragment must not commit its PET-accepted prefix"
    );
    assert_eq!(
        partial.rejected.first().expect("bad suffix reported").kind,
        ErrorKind::ProofStepFailed
    );
    let retained = attempt(&partial.state);
    assert_eq!(
        lab.engine.inspect(retained).unwrap().attempt,
        Some(retained),
        "the unchanged base must remain replayable"
    );
}

#[test]
fn checkout_selects_a_saved_attempt_without_pruning_the_old_suffix() {
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let after_intro = attempt(&check_one(&lab.engine, root, "intro P.").unwrap().state);
    let old_suffix = check_one(&lab.engine, after_intro, "intro HP.").unwrap();
    assert!(old_suffix.error.is_none());

    let checked_out = lab
        .engine
        .checkout(old_suffix.state.attempt.unwrap(), root)
        .unwrap();
    let selected_root = attempt(&checked_out);
    assert_eq!(selected_root, root);

    // The old immutable suffix remains valid after moving the selected cursor.
    assert_eq!(
        lab.engine
            .inspect(attempt(&old_suffix.state))
            .unwrap()
            .attempt,
        old_suffix.state.attempt,
    );
    let alternate = check_one(&lab.engine, selected_root, "intros P HP.").unwrap();
    assert!(alternate.error.is_none());
    assert_ne!(alternate.state.attempt, old_suffix.state.attempt);
}

#[test]
fn checkout_rejects_an_attempt_from_another_proof_or_project_without_changing_current() {
    let lab =
        Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\nTheorem u : True. Admitted.\n");
    let t = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let current = attempt(&check_one(&lab.engine, t, "intro P.").unwrap().state);
    let u = attempt(&open_theorem(&lab.engine, lab.path(), "u").unwrap());
    assert_eq!(
        err_kind(lab.engine.checkout(current, u)),
        ErrorKind::InvalidRequest
    );

    let other = tempfile::tempdir().unwrap();
    initialize_dune_project(other.path());
    fs::write(other.path().join("Main.v"), "Theorem v : True. Admitted.\n").unwrap();
    let v = attempt(&open_theorem(&lab.engine, other.path(), "v").unwrap());
    assert_eq!(
        err_kind(lab.engine.checkout(current, v)),
        ErrorKind::InvalidRequest
    );
    assert_eq!(lab.engine.inspect(current).unwrap().attempt, Some(current));
}

#[test]
fn checkout_replays_a_saved_attempt_after_pet_eviction() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n\
         Theorem u : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 1);
    let root = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let after_intro = attempt(&check_one(&engine, root, "intro P.").unwrap().state);
    let suffix = check_one(&engine, after_intro, "intro HP.").unwrap();
    assert!(suffix.error.is_none());

    // With one process slot, opening another root evicts t's idle lane.
    let _other = open_theorem(&engine, project.path(), "u").unwrap();
    let checked_out = engine
        .checkout(attempt(&suffix.state), after_intro)
        .unwrap();
    assert_eq!(checked_out.attempt, Some(after_intro));
    assert!(checked_out.goals.contains("P -> P"));
}

#[test]
fn pet_eviction_replay_uses_the_dune_selected_nested_workspace() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    fs::write(project.path().join("dune"), "(dirs theories)\n").unwrap();
    let theories = project.path().join("theories");
    fs::create_dir(&theories).unwrap();
    fs::write(
        theories.join("dune"),
        "(rocq.theory (name Demo) (generate_project_file))\n",
    )
    .unwrap();
    fs::write(
        theories.join("A.v"),
        "Lemma imported : True. Proof. exact I. Qed.\n",
    )
    .unwrap();
    fs::write(
        theories.join("Main.v"),
        "From Demo Require Import A.\nTheorem t : True. Admitted.\n",
    )
    .unwrap();
    let build = Command::new("dune")
        .args(["build", "theories/A.vo"])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "dependency build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 1);
    let root = attempt(&open_theorem(&engine, project.path(), "t").unwrap());

    // Force the only PET lane out through another project. A cache lookup must
    // report the missing owner and let replay prepare `theories/` through
    // Dune; spawning a replacement at the attached workspace root loses the
    // generated load path required by `Require Import A`.
    let evictor = tempfile::tempdir().unwrap();
    initialize_dune_project(evictor.path());
    fs::write(
        evictor.path().join("Main.v"),
        "Theorem evictor : True. Admitted.\n",
    )
    .unwrap();
    let _ = open_theorem(&engine, evictor.path(), "evictor").unwrap();

    let result = check_one(&engine, root, "exact imported.").unwrap();
    assert!(
        result.error.is_none(),
        "nested replay failed: {:?}",
        result.error
    );
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn failed_checkout_on_source_change_leaves_the_original_cursor_usable() {
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let source = lab.source();
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let current = attempt(&check_one(&lab.engine, root, "intro P.").unwrap().state);
    fs::write(
        lab.path().join("Main.v"),
        source.replacen("P -> P", "P -> True", 1),
    )
    .unwrap();
    assert_eq!(
        err_kind(lab.engine.checkout(current, root)),
        ErrorKind::DeclarationChanged
    );

    fs::write(lab.path().join("Main.v"), source).unwrap();
    assert_eq!(
        lab.engine.inspect(current).unwrap().attempt,
        Some(current),
        "a failed replay must not replace or retire the original selection"
    );
}

#[test]
fn native_goals_are_open_and_admission_is_rejected() {
    let lab = Lab::new(true_theorems());
    let open = open_theorem(&lab.engine, lab.path(), "t").unwrap();
    assert!(open.focused_goals > 0, "real unfinished theorem has a goal");
    let id = attempt(&open);
    assert_eq!(
        rejected_check_kind(check_one(&lab.engine, id, "admit.")),
        ErrorKind::InvalidRequest
    );
    assert_eq!(
        rejected_check_kind(check_one(&lab.engine, id, "Admitted.")),
        ErrorKind::InvalidRequest
    );
    let still_open = lab.engine.inspect(id).unwrap();
    assert_eq!(still_open.attempt, Some(id));
    assert_eq!(still_open.given_up_goals, 0);
}

#[test]
fn solved_check_automatically_publishes_after_durable_attempt() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let solved = check_one(&lab.engine, root, "exact I.").unwrap();
    assert!(solved.error.is_none());
    assert_eq!(solved.state.lifecycle, ProofLifecycle::Completed);
    assert!(solved.state.attempt.is_none());
    assert!(lab.source().contains("Qed."));
    let declarations = main_declarations(&lab.engine, lab.path()).unwrap();
    assert!(
        declarations
            .iter()
            .any(|item| item.identity.constant() == Some("t"))
    );
}

#[test]
fn ordered_check_selects_the_first_fully_accepted_fragment() {
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let result = lab
        .engine
        .check(
            root,
            &[
                "not_a_tactic.".into(),
                "intro P.".into(),
                "intros P HP. exact HP.".into(),
            ],
        )
        .unwrap();
    assert_eq!(result.selected, Some(1));
    assert_eq!(result.rejected.len(), 1);
    assert_eq!(result.rejected[0].kind, ErrorKind::ProofStepFailed);
    assert!(result.error.is_none());
    assert_ne!(result.state.attempt, Some(root));
    assert_eq!(result.state.lifecycle, ProofLifecycle::Open);
}

#[test]
fn ordered_check_restarts_after_a_rejected_multi_sentence_prefix() {
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let result = lab
        .engine
        .check(
            root,
            &["intro P. fail.".into(), "intros P HP. exact HP.".into()],
        )
        .unwrap();
    assert_eq!(result.selected, Some(1));
    assert_eq!(result.rejected.len(), 1);
    assert_eq!(result.rejected[0].kind, ErrorKind::ProofStepFailed);
    assert!(result.error.is_none());
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert!(lab.source().contains("Qed."));
}

#[test]
fn ordered_check_validates_the_complete_array_before_selection() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let before = lab.source();
    let error = lab
        .engine
        .check(root, &["exact I.".into(), "  ".into()])
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
    assert_eq!(error.message, "attempt 1 is empty");
    assert_eq!(lab.source(), before);
    assert_eq!(lab.engine.inspect(root).unwrap().attempt, Some(root));
}

#[test]
fn ordered_check_stops_at_an_accepted_unsolved_fragment() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let before = lab.source();
    let result = lab
        .engine
        .check(root, &["idtac.".into(), "exact I.".into()])
        .unwrap();
    assert_eq!(result.selected, Some(0));
    assert!(result.rejected.is_empty());
    assert_eq!(result.state.lifecycle, ProofLifecycle::Open);
    assert_eq!(
        lab.source(),
        before,
        "the later solving fragment must not win"
    );
}

#[test]
fn all_rejected_check_fragments_leave_the_base_selected() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let result = lab
        .engine
        .check(root, &["not_a_tactic.".into(), "also_not_a_tactic.".into()])
        .unwrap();
    assert_eq!(result.selected, None);
    assert_eq!(result.rejected.len(), 2);
    assert_eq!(result.state.attempt, Some(root));
    assert_eq!(lab.engine.inspect(root).unwrap().attempt, Some(root));
}

#[test]
fn try_attempts_are_ordered_multi_sentence_and_side_effect_free() {
    let lab = Lab::new(true_theorems());
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let before = lab.source();
    let result = lab
        .engine
        .try_attempts(
            id,
            &[
                "idtac. exact I.".into(),
                "idtac. not_a_tactic.".into(),
                "idtac.".into(),
            ],
        )
        .unwrap();
    assert_eq!(result.len(), 3);
    assert!(result[0].error.is_none(), "first fragment succeeds");
    assert!(result[0].solved, "PET reports hypothetical completion");
    assert_eq!(
        result[0].state.as_ref().unwrap().attempt,
        None,
        "a hypothetical result must not expose the shared base attempt"
    );
    assert_eq!(
        result[1]
            .error
            .as_ref()
            .expect("a later-sentence failure stays in place")
            .kind,
        ErrorKind::ProofStepFailed
    );
    assert!(result[1].state.is_none(), "partial fragments are atomic");
    assert!(
        result[2].error.is_none(),
        "later fragment is not poisoned by a failure"
    );
    assert_eq!(result[2].state.as_ref().unwrap().attempt, None);
    assert_eq!(
        lab.engine.inspect(id).unwrap().attempt,
        Some(id),
        "try cannot append"
    );
    assert_eq!(lab.source(), before, "try cannot publish");
}

#[test]
fn every_attempt_operation_revalidates_dune_before_using_cached_pet_state() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    fs::write(lab.path().join("dune"), "(rocq.theory (name Main)\n").unwrap();

    assert_eq!(
        err_kind(lab.engine.check(id, &["idtac.".into()])),
        ErrorKind::InvalidConfiguration
    );
    assert_eq!(
        err_kind(lab.engine.try_attempts(id, &["idtac.".into()])),
        ErrorKind::InvalidConfiguration
    );
    assert_eq!(
        err_kind(lab.engine.inspect(id)),
        ErrorKind::InvalidConfiguration
    );
    assert_eq!(
        err_kind(lab.engine.checkout(id, id)),
        ErrorKind::InvalidConfiguration
    );
    assert_eq!(
        err_kind(lab.engine.query_goals(lab.path(), id)),
        ErrorKind::InvalidConfiguration
    );
    assert_eq!(
        err_kind(
            lab.engine
                .query_expression_type(lab.path(), Some(id), "True".into(), None)
        ),
        ErrorKind::InvalidConfiguration
    );
}

#[test]
fn typed_queries_are_contextual_validated_and_read_only() {
    let lab = Lab::new("Definition d : True := I.\nTheorem t : True. Admitted.\n");
    let t = declaration_id(&lab.engine, lab.path(), "t");
    let d = declaration_id(&lab.engine, lab.path(), "d");
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let before = lab.source();
    lab.engine
        .query_search(lab.path(), None, "True".into(), Some(&t))
        .unwrap();
    lab.engine.query_statement(lab.path(), &t).unwrap();
    lab.engine.query_definition(lab.path(), &d).unwrap();
    lab.engine.query_proof(lab.path(), &t).unwrap();
    lab.engine.query_assumptions(lab.path(), &d).unwrap();
    lab.engine.query_dependencies(lab.path(), &d).unwrap();
    lab.engine
        .query_expression_type(lab.path(), None, "I".into(), Some(&t))
        .unwrap();
    lab.engine
        .query_notation(lab.path(), None, "nat".into(), Some(&t))
        .unwrap();
    assert_eq!(
        lab.engine.query_goals(lab.path(), id).unwrap().attempt,
        Some(id)
    );
    assert_eq!(
        err_kind(lab.engine.query_expression_type(
            lab.path(),
            None,
            "I). Admitted. Theorem injected : False := I".into(),
            None,
        )),
        ErrorKind::InvalidRequest
    );
    assert_eq!(lab.source(), before, "all query variants are read-only");
}

#[test]
fn named_semantic_queries_do_not_report_missing_constants_as_success() {
    let lab = Lab::new("Theorem t : True. Proof. exact I. Qed.\n");
    let t = declaration_id(&lab.engine, lab.path(), "t");
    let result = lab.engine.query_statement(lab.path(), &t);
    assert!(
        result.is_ok(),
        "existing valid source declaration must be queryable: {result:?}"
    );
    let text = result.unwrap();
    assert!(
        text.contains("t") && !text.contains("not found") && !text.contains("not defined"),
        "PET did not resolve target: {text}"
    );
    let proof = lab.engine.query_proof(lab.path(), &t).unwrap();
    assert!(
        proof.contains("t") && !proof.contains("not defined"),
        "PET did not print the completed theorem: {proof}"
    );
}

#[test]
fn prove_existing_source_requires_native_build_and_pet_assumption_audit() {
    let good = Lab::new("Theorem t : True. Proof. exact I. Qed.\n");
    let state = open_theorem(&good.engine, good.path(), "t").unwrap();
    assert_eq!(state.lifecycle, ProofLifecycle::Completed);
    assert!(state.attempt.is_none());

    let bad = Lab::new("Theorem t : True. Proof. exact 0. Qed.\n");
    let result = open_theorem(&bad.engine, bad.path(), "t");
    assert_eq!(result.unwrap_err().kind, ErrorKind::InvalidDeclaration);
}

#[test]
fn existing_closed_source_uses_the_publication_assumption_policy() {
    let authorized = Lab::new("Axiom ax : True.\nTheorem t : True. Proof. exact ax. Qed.\n");
    let state = open_theorem(&authorized.engine, authorized.path(), "t").unwrap();
    assert_eq!(state.lifecycle, ProofLifecycle::Completed);
    assert!(state.attempt.is_none());

    let admitted =
        Lab::new("Theorem hole : True. Admitted.\nTheorem t : True. Proof. exact hole. Qed.\n");
    assert_eq!(
        err_kind(open_theorem(&admitted.engine, admitted.path(), "t")),
        ErrorKind::UnfinishedDependency
    );
}

#[test]
fn declaration_metadata_never_serializes_source_text_as_proof_status() {
    let lab = Lab::new("Theorem t : True. Proof. exact 0. Qed.\n");
    let declarations = main_declarations(&lab.engine, lab.path()).unwrap();
    let item = declarations
        .iter()
        .find(|item| item.identity.constant() == Some("t"))
        .unwrap();
    let json = serde_json::to_value(item).unwrap();
    assert!(json.get("status").is_none());
}

#[test]
fn source_content_edits_reject_then_restore_latest_view_replays_trace() {
    let lab = Lab::new(true_theorems());
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let source = lab.source();
    // Same byte length: freshness must be content based, not mtime/size based.
    fs::write(
        lab.path().join("Main.v"),
        source.replacen("True", "False", 1),
    )
    .unwrap();
    assert_eq!(
        err_kind(lab.engine.inspect(id)),
        ErrorKind::DeclarationChanged
    );
    fs::write(lab.path().join("Main.v"), source).unwrap();
    assert!(lab.engine.inspect(id).is_ok());
}

#[test]
fn tiny_trace_watermark_replays_and_diagnostics_do_not_leak_state_parent() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let private = state.path().join("private-spill");
    let engine = Engine::new(EngineConfig {
        state_parent: private.clone(),
        trace_memory_bytes: 1,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let result = check_one(&engine, id, "idtac.").unwrap();
    assert!(result.error.is_none());
    let message = format!(
        "{:?}",
        rejected_check_kind(check_one(&engine, id, "admit."))
    );
    assert!(!message.contains(private.to_string_lossy().as_ref()));
}

#[test]
fn concurrent_competing_solves_publish_once_then_retire() {
    let lab = Arc::new(Lab::new(true_theorems()));
    let a = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let b = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let gate = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for id in [a, b] {
        let lab = lab.clone();
        let gate = gate.clone();
        workers.push(thread::spawn(move || {
            gate.wait();
            check_one(&lab.engine, id, "exact I.")
        }));
    }
    let outcomes: Vec<_> = workers.into_iter().map(|x| x.join().unwrap()).collect();
    assert!(
        outcomes
            .iter()
            .any(|x| x.as_ref().is_ok_and(|r| r.error.is_none())),
        "one racing branch must publish"
    );
    let source = lab.source();
    assert!(
        source.contains(
            "Theorem t : True.
Proof."
        ),
        "the solved declaration must be materialized"
    );
    assert_eq!(
        source.matches("Admitted.").count(),
        1,
        "only unrelated u may remain admitted; t must close exactly once"
    );
    compile(lab.path());
    let completed = open_theorem(&lab.engine, lab.path(), "t").unwrap();
    assert_eq!(completed.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn same_file_distinct_theorems_can_publish_without_corruption() {
    let lab = Lab::new(true_theorems());
    let t = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let u = attempt(&open_theorem(&lab.engine, lab.path(), "u").unwrap());
    let t = check_one(&lab.engine, t, "exact I.").unwrap();
    assert!(t.error.is_none());
    assert_eq!(
        err_kind(check_one(&lab.engine, u, "exact I.")),
        ErrorKind::DeclarationChanged,
    );
    let u = check_one(
        &lab.engine,
        attempt(&open_theorem(&lab.engine, lab.path(), "u").unwrap()),
        "exact I.",
    )
    .unwrap();
    assert!(u.error.is_none());
    let source = lab.source();
    assert!(!source.contains("Admitted"));
    compile(lab.path());
}

#[test]
fn completed_source_does_not_create_an_active_attempt() {
    let lab = Lab::new("Theorem t : True. Proof. exact I. Qed.\n");
    let state = open_theorem(&lab.engine, lab.path(), "t").unwrap();
    assert_eq!(state.lifecycle, ProofLifecycle::Completed);
    assert!(state.attempt.is_none());
}

#[test]
fn trust_audit_rejects_an_unfinished_dependency_without_publishing() {
    let lab = Lab::new("Theorem unfinished : True. Admitted.\nTheorem t : True. Admitted.\n");
    let result = check_one(
        &lab.engine,
        attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
        "exact unfinished.",
    )
    .unwrap();
    assert_eq!(
        result
            .error
            .expect("untrusted admitted dependency must be rejected")
            .kind,
        ErrorKind::UnfinishedDependency
    );
    assert_eq!(result.state.lifecycle, ProofLifecycle::Open);
    assert!(result.state.attempt.is_some());
    assert!(
        lab.source().contains("Theorem t : True. Admitted."),
        "failed trust audit must not publish"
    );
}

#[test]
fn trust_audit_does_not_treat_tactic_text_as_a_dependency() {
    let lab = Lab::new("Theorem unfinished : True. Admitted.\nTheorem t : True. Admitted.\n");
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let result = check_one(&lab.engine, id, "idtac \"unfinished\". exact I.").unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert!(
        lab.source()
            .contains("Theorem unfinished : True. Admitted.")
    );
}

#[test]
fn native_trust_audit_accepts_only_frozen_explicit_axioms() {
    let lab = Lab::new("Axiom ax : True.\nTheorem t : True. Admitted.\n");
    let result = check_one(
        &lab.engine,
        attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
        "exact ax.",
    )
    .unwrap();
    assert!(result.error.is_none());
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert!(lab.source().contains("Axiom ax : True."));
    assert!(!lab.source().contains("Admitted."));
}

#[test]
fn native_trust_audit_matches_qualified_multiline_axiom_types() {
    let lab = Lab::new(
        "Module M.\nAxiom ax : forall P : Prop, P -> P.\nEnd M.\nTheorem t : True. Admitted.\n",
    );
    let result = check_one(
        &lab.engine,
        attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
        "exact (M.ax True I).",
    )
    .unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn native_trust_audit_resolves_explicit_axioms_across_compilation_units() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(project.path().join("dune"), "(dirs theories)\n").unwrap();
    let theories = project.path().join("theories");
    fs::create_dir(&theories).unwrap();
    fs::write(
        theories.join("dune"),
        "(rocq.theory (name Main) (generate_project_file))\n",
    )
    .unwrap();
    fs::write(theories.join("A.v"), "Axiom witness : True.\n").unwrap();
    fs::write(theories.join("B.v"), "Axiom witness : False.\n").unwrap();
    fs::write(
        theories.join("Main.v"),
        "From Main Require Import A B.\nTheorem t : True. Admitted.\n",
    )
    .unwrap();
    let dependencies = Command::new("dune")
        .args(["build", "theories/A.vo", "theories/B.vo"])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        dependencies.status.success(),
        "dependency build failed: {}",
        String::from_utf8_lossy(&dependencies.stderr)
    );
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let opened = open_theorem(&engine, project.path(), "t").unwrap();
    let result = check_one(&engine, attempt(&opened), "exact Main.A.witness.").unwrap();
    assert!(
        result.error.is_none(),
        "PET-resolved explicit axiom must survive cross-file audit: {:?}",
        result.error
    );
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    let compiled = Command::new("dune")
        .args(["build", "theories/Main.vo"])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(compiled.status.success());
}

#[test]
fn trust_audit_does_not_index_unrelated_dune_sources() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Broken.v"),
        "This is intentionally not Rocq syntax.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let files = engine.list_files(project.path()).unwrap();
    assert!(
        files.iter().any(|file| file.0 == "Broken.v"),
        "the unrelated broken source must remain in Dune's selected theory"
    );
    let main = files
        .iter()
        .find(|file| file.0 == "Main.v")
        .expect("Dune selects Main.v");
    let declaration = engine
        .list_decls(project.path(), main)
        .unwrap()
        .into_iter()
        .find(|declaration| declaration.identity.constant() == Some("t"))
        .expect("PET indexes t from Main.v");
    // Design note: lazy discovery requires the caller to choose Main.v. A
    // workspace-wide test helper would itself ask PET to parse Broken.v and
    // could not isolate the close-time trust audit exercised below.
    let opened = engine.open(project.path(), declaration.identity).unwrap();
    let result = check_one(&engine, attempt(&opened), "exact I.").unwrap();
    assert!(
        result.error.is_none(),
        "an assumption-free proof must not parse unrelated sources: {:?}",
        result.error
    );
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn native_timeout_and_output_bounds_cleanup_through_path_wrappers() {
    const CHILD: &str = "ROCQ_ENGINE_WRAPPER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let path = wrapper.path().join("pet");
        fs::write(&path, "#!/bin/sh\nif [ \"$ROCQ_WRAPPER_MODE\" = hang ]; then sleep 4; exit 0; fi\nprintf 'Content-Length: 8388609\n\n'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for mode in ["hang", "noisy"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "native_timeout_and_output_bounds_cleanup_through_path_wrappers",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("ROCQ_WRAPPER_MODE", mode)
                .env("ROCQ_PET_BIN", &path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "isolated {mode} wrapper test failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    let hanging = std::env::var("ROCQ_WRAPPER_MODE").unwrap() == "hang";
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(1),
        close_timeout: Some(Duration::from_secs(1)),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let result = open_theorem(&engine, project.path(), "t");
    if hanging {
        assert_eq!(err_kind(result), ErrorKind::ProofTimeout);
    } else {
        assert_eq!(result.unwrap_err().kind, ErrorKind::InvalidConfiguration);
    }
    assert!(
        !project.path().read_dir().unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rocq-engine-native-")),
        "owned native inputs must be cleaned after fault"
    );
}

#[test]
fn close_build_timeout_is_distinct_from_pet_timeout() {
    const CHILD: &str = "ROCQ_ENGINE_BUILD_TIMEOUT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let actual = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v rocq"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let rocq = wrapper.path().join("rocq");
        fs::write(&rocq, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then exec {} \"$@\"; fi\nif [ \"$1\" = dep ]; then exec {} \"$@\"; fi\nsource=\"\"; for x in \"$@\"; do if [ -f \"$x\" ]; then source=\"$x\"; fi; done\nif [ -n \"$source\" ] && grep -q 'Qed\\.' \"$source\"; then sleep 3; fi\nexec {} \"$@\"\n", actual.trim(), actual.trim(), actual.trim())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&rocq, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "close_build_timeout_is_distinct_from_pet_timeout",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    wrapper.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let lab = Lab::with_close_timeout(
        "Theorem t : True. Admitted.\n",
        Some(Duration::from_secs(1)),
    );
    let result = check_one(
        &lab.engine,
        attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
        "exact I.",
    )
    .unwrap();
    assert!(
        matches!(
            result.error.as_ref().map(|error| error.kind),
            None | Some(ErrorKind::BuildTimeout)
        ),
        "unexpected close result: {:?}",
        result.error
    );
    assert_eq!(result.state.lifecycle, ProofLifecycle::Open);
    assert!(result.state.attempt.is_some());
    assert!(lab.source().contains("Admitted."));
}

#[test]
fn default_close_waits_for_native_build_beyond_interactive_deadline() {
    const CHILD: &str = "ROCQ_ENGINE_NO_CLOSE_DEADLINE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let actual = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v rocq"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let rocq = wrapper.path().join("rocq");
        fs::write(
            &rocq,
            format!(
                "#!/bin/sh\nif [ \"$1\" = compile ]; then sleep 2; fi\nexec {} \"$@\"\n",
                actual.trim()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&rocq, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "default_close_waits_for_native_build_beyond_interactive_deadline",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    wrapper.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let lab = Lab::with_close_timeout("Theorem t : True. Admitted.\n", None);
    let result = check_one(
        &lab.engine,
        attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
        "exact I.",
    )
    .unwrap();
    assert!(result.error.is_none(), "close failed: {:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert!(lab.source().contains("Qed."));
}

#[test]
fn dune_theory_project_can_publish_and_independently_build() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    fs::create_dir(project.path().join("theories")).unwrap();
    fs::write(
        project.path().join("theories/dune"),
        "(rocq.theory (name Demo) (generate_project_file))\n",
    )
    .unwrap();
    fs::write(
        project.path().join("theories/Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let opened = open_theorem(&engine, project.path(), "t").unwrap();
    assert!(
        check_one(&engine, attempt(&opened), "exact I.")
            .unwrap()
            .error
            .is_none()
    );
    let output = Command::new("dune")
        .args(["build"])
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "independent Dune build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dune_subdirectory_attachment_publishes_through_workspace_build() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    let theory = project.path().join("theories");
    fs::create_dir(&theory).unwrap();
    fs::write(
        theory.join("dune"),
        "(rocq.theory (name Demo) (generate_project_file))\n",
    )
    .unwrap();
    fs::write(theory.join("Main.v"), "Theorem t : True. Admitted.\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let opened = open_theorem(&engine, &theory, "t").unwrap();
    let attempt = attempt(&opened);
    assert_eq!(
        engine
            .query_goals(project.path(), attempt)
            .unwrap()
            .focused_goals,
        1,
        "workspace root and attached subdirectory must share one ProjectState"
    );
    let result = check_one(&engine, attempt, "exact I.").unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(
        fs::read_to_string(theory.join("Main.v"))
            .unwrap()
            .contains("Qed.")
    );
    assert!(!theory.join(".rocq-engine").exists());
    assert!(
        Command::new("dune")
            .arg("clean")
            .current_dir(project.path())
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn dune_subdirectory_audit_uses_dune_load_paths() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    let theory = project.path().join("theories");
    fs::create_dir(&theory).unwrap();
    fs::write(
        theory.join("dune"),
        "(rocq.theory (name Demo) (generate_project_file))\n",
    )
    .unwrap();
    fs::write(
        theory.join("Main.v"),
        "Axiom trusted : True.\nTheorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let opened = open_theorem(&engine, &theory, "t").unwrap();
    let result = check_one(&engine, attempt(&opened), "exact trusted.").unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn pet_ranges_preserve_enclosing_modules_during_publication() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    fs::write(
        project.path().join("dune"),
        "(rocq.theory (name Demo) (generate_project_file))\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Module Nested.\nTheorem t : True. Admitted.\nEnd Nested.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let opened = open_theorem(&engine, project.path(), "t").unwrap();
    let result = check_one(&engine, attempt(&opened), "exact I.").unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert_eq!(
        fs::read_to_string(project.path().join("Main.v")).unwrap(),
        "Module Nested.\nTheorem t : True.\nProof.\nexact I.\nQed.\n\nEnd Nested.\n"
    );
}

#[test]
fn same_named_nested_declarations_have_distinct_ids_and_pet_states() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    fs::write(
        project.path().join("dune"),
        "(rocq.theory (name Demo) (generate_project_file))\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Module A.\nTheorem same : True. Admitted.\nEnd A.\n\
         Module B.\nTheorem same : False. Admitted.\nEnd B.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let declarations = main_declarations(&engine, project.path()).unwrap();
    let same = declarations
        .iter()
        .filter(|declaration| declaration.identity.constant() == Some("same"))
        .collect::<Vec<_>>();
    assert_eq!(
        same.len(),
        2,
        "PET document declarations must preserve duplicate leaves"
    );
    assert_eq!(
        same.iter()
            .map(|declaration| declaration.identity.qualified_path.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            vec!["Demo".into(), "Main".into(), "A".into(), "same".into()],
            vec!["Demo".into(), "Main".into(), "B".into(), "same".into()],
        ])
    );

    let a = same
        .iter()
        .find(|declaration| declaration.identity.qualified_path.contains(&"A".into()))
        .unwrap();
    let b = same
        .iter()
        .find(|declaration| declaration.identity.qualified_path.contains(&"B".into()))
        .unwrap();
    let a_state = engine.open(project.path(), a.identity.clone()).unwrap();
    assert!(
        a_state.goals.contains("True"),
        "A.same opened the wrong PET node"
    );
    let a_result = check_one(&engine, attempt(&a_state), "idtac.").unwrap();
    assert!(a_result.error.is_none(), "{:?}", a_result.error);
    assert_eq!(a_result.state.lifecycle, ProofLifecycle::Open);

    let b_state = engine.open(project.path(), b.identity.clone()).unwrap();
    assert!(
        b_state.goals.contains("False"),
        "B.same opened the wrong PET node"
    );
    let b_result = check_one(&engine, attempt(&b_state), "idtac.").unwrap();
    assert!(b_result.error.is_none(), "{:?}", b_result.error);
    assert_eq!(b_result.state.lifecycle, ProofLifecycle::Open);
}

#[test]
fn same_leaf_nested_queries_trust_and_writeback_stay_on_the_exact_pet_node() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Module A.\nAxiom witness : True.\nTheorem same : True. Admitted.\nEnd A.\n\
         Module B.\nAxiom witness : False.\nTheorem same : False. Admitted.\nEnd B.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let declarations = main_declarations(&engine, project.path()).unwrap();
    let same = declarations
        .iter()
        .filter(|declaration| declaration.identity.constant() == Some("same"))
        .collect::<Vec<_>>();
    let a = same
        .iter()
        .find(|declaration| declaration.identity.qualified_path.contains(&"A".into()))
        .unwrap()
        .identity
        .clone();
    let b = same
        .iter()
        .find(|declaration| declaration.identity.qualified_path.contains(&"B".into()))
        .unwrap()
        .identity
        .clone();

    let a_about = engine.query_statement(project.path(), &a).unwrap();
    let b_about = engine.query_statement(project.path(), &b).unwrap();
    assert!(a_about.contains("True"), "wrong A.same query: {a_about}");
    assert!(b_about.contains("False"), "wrong B.same query: {b_about}");

    let a_open = engine.open(project.path(), a.clone()).unwrap();
    let a_closed = check_one(&engine, attempt(&a_open), "exact witness.").unwrap();
    assert!(a_closed.error.is_none(), "{:?}", a_closed.error);
    assert_eq!(a_closed.state.lifecycle, ProofLifecycle::Completed);
    let after_a = fs::read_to_string(project.path().join("Main.v")).unwrap();
    assert!(after_a.contains(
        "Module A.\nAxiom witness : True.\nTheorem same : True.\nProof.\nexact witness.\nQed."
    ));
    assert!(after_a.contains("Module B.\nAxiom witness : False.\nTheorem same : False. Admitted."));
    let a_assumptions = engine.query_assumptions(project.path(), &a).unwrap();
    assert!(
        a_assumptions.contains("witness"),
        "A.same lost its intended explicit axiom: {a_assumptions}"
    );

    let b_open = engine.open(project.path(), b.clone()).unwrap();
    let b_closed = check_one(&engine, attempt(&b_open), "exact witness.").unwrap();
    assert!(b_closed.error.is_none(), "{:?}", b_closed.error);
    assert_eq!(b_closed.state.lifecycle, ProofLifecycle::Completed);
    let final_source = fs::read_to_string(project.path().join("Main.v")).unwrap();
    assert_eq!(final_source.matches("exact witness.").count(), 2);
    assert!(!final_source.contains("Admitted."));
    compile(project.path());
}

#[test]
fn abandon_retires_every_source_version_root_for_one_declaration() {
    let first_source = "Theorem t : True. Admitted.\n";
    let second_source = "(* second source version *)\nTheorem t : True. Admitted.\n";
    let lab = Lab::new(first_source);
    let identity = declaration_id(&lab.engine, lab.path(), "t");

    let first = lab.engine.open(lab.path(), identity.clone()).unwrap();
    let first_attempt = attempt(&first);
    fs::write(lab.path().join("Main.v"), second_source).unwrap();
    let second = lab.engine.open(lab.path(), identity.clone()).unwrap();
    let second_attempt = attempt(&second);
    assert_ne!(first_attempt, second_attempt);

    lab.engine.abandon(lab.path(), identity.clone()).unwrap();

    // Both immutable roots must have been retired. Reopening either exact
    // source snapshot must allocate a fresh root rather than resurrecting an
    // abandoned branch that happened not to be first in the attempt map.
    fs::write(lab.path().join("Main.v"), first_source).unwrap();
    let reopened_first = lab.engine.open(lab.path(), identity.clone()).unwrap();
    assert_ne!(attempt(&reopened_first), first_attempt);
    fs::write(lab.path().join("Main.v"), second_source).unwrap();
    let reopened_second = lab.engine.open(lab.path(), identity).unwrap();
    assert_ne!(attempt(&reopened_second), second_attempt);
}

#[test]
fn contention_reference_model_keeps_each_public_prefix_independent() {
    let lab = Arc::new(Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n"));
    // Establish every public attempt before the concurrency barrier. A failed
    // setup inside one worker would otherwise strand the remaining workers at
    // the barrier and turn an assertion failure into a test-suite deadlock.
    let roots = (0..12)
        .map(|_| attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()))
        .collect::<Vec<_>>();
    let gate = Arc::new(Barrier::new(12));
    let mut workers = Vec::new();
    for (index, root) in roots.into_iter().enumerate() {
        let lab = lab.clone();
        let gate = gate.clone();
        workers.push(thread::spawn(move || {
            gate.wait();
            let one = check_one(
                &lab.engine,
                root,
                if index % 2 == 0 {
                    "intro P."
                } else {
                    "intro Q."
                },
            )
            .unwrap();
            assert!(one.error.is_none());
            let two = check_one(&lab.engine, attempt(&one.state), "idtac.").unwrap();
            assert!(two.error.is_none());
            assert_ne!(two.state.attempt, one.state.attempt);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn engine_drop_removes_only_its_owned_spill_child() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n",
    )
    .unwrap();
    let parent = tempfile::tempdir().unwrap();
    let keep = parent.path().join("caller-file");
    fs::write(&keep, "keep").unwrap();
    {
        let engine = Engine::new(EngineConfig {
            state_parent: parent.path().to_owned(),
            trace_memory_bytes: 1,
            operation_timeout: Duration::from_secs(10),
            close_timeout: Some(Duration::from_secs(10)),
            runtime_cache_bytes: 1,
            max_pet_processes: 4,
        })
        .unwrap();
        let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
        assert!(check_one(&engine, id, "idtac.").unwrap().error.is_none());
    }
    assert_eq!(
        fs::read_to_string(keep).unwrap(),
        "keep",
        "drop never deletes caller-owned state-parent contents"
    );
    assert_eq!(
        parent.path().read_dir().unwrap().count(),
        1,
        "drop must remove its unique spill child"
    );
}

#[test]
fn self_reference_and_new_axiom_are_rejected_while_trusted_dependency_publishes() {
    let self_ref = Lab::new("Theorem t : True. Admitted.\n");
    let result = check_one(
        &self_ref.engine,
        attempt(&open_theorem(&self_ref.engine, self_ref.path(), "t").unwrap()),
        "exact t.",
    )
    .unwrap();
    assert!(
        result.error.is_some() || !result.rejected.is_empty(),
        "a theorem must never prove itself through its old admission"
    );
    assert!(self_ref.source().contains("Admitted."));
    let id = attempt(&open_theorem(&self_ref.engine, self_ref.path(), "t").unwrap());
    assert_eq!(
        rejected_check_kind(check_one(&self_ref.engine, id, "Axiom forged : True.")),
        ErrorKind::InvalidRequest
    );

    let trusted = Lab::new("Definition trusted : True := I.\nTheorem t : True. Admitted.\n");
    let result = check_one(
        &trusted.engine,
        attempt(&open_theorem(&trusted.engine, trusted.path(), "t").unwrap()),
        "exact trusted.",
    )
    .unwrap();
    assert!(
        result.error.is_none(),
        "ordinary kernel-checked dependency remains valid"
    );
    compile(trusted.path());
}

#[test]
fn query_variants_do_not_collapse_definition_and_dependencies_or_notation_and_type() {
    let lab = Lab::new("Definition d : True := I.\n");
    let d = declaration_id(&lab.engine, lab.path(), "d");
    let definition = lab.engine.query_definition(lab.path(), &d).unwrap();
    let dependencies = lab.engine.query_dependencies(lab.path(), &d).unwrap();
    assert_ne!(
        definition, dependencies,
        "Dependencies must be a dependency query, not an alias for Definition"
    );
    let typ = lab
        .engine
        .query_expression_type(lab.path(), None, "nat".into(), Some(&d))
        .unwrap();
    let notation = lab
        .engine
        .query_notation(lab.path(), None, "nat".into(), Some(&d))
        .unwrap();
    assert_ne!(
        typ, notation,
        "notation interpretation must not be an expression-type alias"
    );
}

#[test]
fn interactive_source_audit_requires_structured_pet_and_forbids_textual_repl() {
    let engine =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine.rs"))
            .unwrap();
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/mod.rs"))
            .unwrap();
    assert!(engine.contains("pet"));
    for required in [
        "Command::new(pet_binary)",
        "petanque/setWorkspace",
        "petanque/get_state_at_pos",
        "petanque/run",
        "petanque/goals",
    ] {
        assert!(
            source.contains(required),
            "interactive engine must contain structured PET operation {required}"
        );
    }
    for forbidden in [".arg(\"repl\")", "Rocq <", "extract_goals(", "goal_count("] {
        assert!(
            !source.contains(forbidden),
            "interactive engine must not retain textual REPL/console-goal path: {forbidden}"
        );
    }
}

#[test]
fn real_pet_is_invoked_for_open_check_inspect_and_try() {
    const CHILD: &str = "ROCQ_ENGINE_PET_AUDIT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let log = wrapper.path().join("pet.log");
        let proxy = pet_recording_proxy(wrapper.path());
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "real_pet_is_invoked_for_open_check_inspect_and_try",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", &real)
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env("ROCQ_PET_BIN", &proxy)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "PET subprocess audit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !fs::read_to_string(log).unwrap_or_default().is_empty(),
            "the PET wrapper must observe a real PET spawn"
        );
        return;
    }
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let starts_before = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    let stepped = check_one(&lab.engine, id, "intro P.").unwrap();
    assert!(stepped.error.is_none());
    let id = attempt(&stepped.state);
    assert_eq!(lab.engine.inspect(id).unwrap().attempt, Some(id));
    let results = lab
        .engine
        .try_attempts(id, &["exact H.".into(), "idtac.".into()])
        .unwrap();
    assert_eq!(results.len(), 2);
    let starts_after = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        starts_after, starts_before,
        "check, inspect, and try forks must reuse the live PET state"
    );
}

#[test]
fn pet_structured_goal_sets_report_focused_and_shelved_without_console_parsing() {
    let lab =
        Lab::new("Theorem pair : True /\\ True. Admitted.\nTheorem pending : True. Admitted.\n");
    let pair = attempt(&open_theorem(&lab.engine, lab.path(), "pair").unwrap());
    let split = check_one(&lab.engine, pair, "split.").unwrap();
    assert!(split.error.is_none());
    assert_eq!(
        split.state.focused_goals, 2,
        "PET focused goal array must retain both subgoals"
    );
    assert_eq!(split.state.unfocused_goals, 0);
    assert_eq!(split.state.shelved_goals, 0);
    let pending = attempt(&open_theorem(&lab.engine, lab.path(), "pending").unwrap());
    let shelved = check_one(&lab.engine, pending, "shelve.").unwrap();
    assert!(shelved.error.is_none());
    assert_eq!(shelved.state.focused_goals, 0);
    assert_eq!(
        shelved.state.shelved_goals, 1,
        "PET shelf must not be confused with solved proof"
    );
    assert_eq!(shelved.state.given_up_goals, 0);
}

#[test]
fn replay_safe_pet_faults_recover_in_call_and_timeout_recovers_next_call() {
    const CHILD: &str = "ROCQ_ENGINE_PET_RECOVERY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let counter = wrapper.path().join("count");
        let pet = wrapper.path().join("pet");
        fs::write(&pet, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then exec \"$ROCQ_ENGINE_REAL_PET\" \"$@\"; fi\nn=0; [ -f '{0}' ] && n=$(cat '{0}'); n=$((n+1)); echo $n > '{0}'\nif [ $n -eq 1 ]; then case \"$ROCQ_ENGINE_PET_FAULT\" in hang) sleep 4;; malformed) printf 'Content-Length: 1\\n\\n{{';; overflow) printf 'Content-Length: 8388609\\n\\n';; death) :;; esac; exit 0; fi\nexec \"$ROCQ_ENGINE_REAL_PET\" \"$@\"\n", counter.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pet, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for fault in ["hang", "malformed", "overflow", "death"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "replay_safe_pet_faults_recover_in_call_and_timeout_recovers_next_call",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("ROCQ_ENGINE_PET_FAULT", fault)
                .env("ROCQ_ENGINE_REAL_PET", &real)
                .env("ROCQ_PET_BIN", &pet)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "PET {fault} recovery child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            fs::write(&counter, "0").unwrap();
        }
        return;
    }
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(1),
        close_timeout: Some(Duration::from_secs(1)),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let first = open_theorem(&engine, project.path(), "t");
    let id = match std::env::var("ROCQ_ENGINE_PET_FAULT").unwrap().as_str() {
        "hang" => {
            assert_eq!(err_kind(first), ErrorKind::ProofTimeout);
            attempt(&open_theorem(&engine, project.path(), "t").unwrap())
        }
        // Replay-safe transport/protocol loss is transparent to the caller;
        // the runtime discards the failed disposable PET and retries once.
        "death" | "malformed" | "overflow" => attempt(&first.unwrap()),
        other => panic!("unknown PET fault mode {other}"),
    };
    assert!(
        check_one(&engine, id, "intro P.").unwrap().error.is_none(),
        "fresh PET must reconstruct the prefix after fault"
    );
}

#[test]
fn tiny_runtime_cache_evicts_pet_states_but_replays_authoritative_prefixes() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(project.path().join("Main.v"), "Theorem a : forall P : Prop, P -> P. Admitted.\nTheorem b : forall P : Prop, P -> P. Admitted.\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let a = attempt(&open_theorem(&engine, project.path(), "a").unwrap());
    let a = attempt(&check_one(&engine, a, "intro P.").unwrap().state);
    let b = attempt(&open_theorem(&engine, project.path(), "b").unwrap());
    let _ = check_one(&engine, b, "intro Q.").unwrap();
    let replayed = engine.inspect(a).unwrap();
    assert_eq!(replayed.attempt, Some(a));
    assert!(
        replayed.goals.contains("P"),
        "evicted PET state must be reconstructed from trace prefix"
    );
}

#[test]
fn synthetic_declare_runs_in_pet_state_without_source_or_workspace_copy() {
    const CHILD: &str = "ROCQ_ENGINE_SYNTHETIC_PROXY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let log = wrapper.path().join("start.jsonl");
        let proxy = pet_recording_proxy(wrapper.path());
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "synthetic_declare_runs_in_pet_state_without_source_or_workspace_copy",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", &real)
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env("ROCQ_PET_BIN", &proxy)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "synthetic direct PET test failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(project.path().join("Main.v"), "\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let _ = engine
        .declare(
            project.path(),
            DeclarationKind::Theorem,
            DeclarationIdentity {
                file: rocq_engine::FileId("Main.v".into()),
                qualified_path: vec!["Main".into(), "Main".into(), "synthetic".into()],
            },
            "True".into(),
        )
        .unwrap();
    let log = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()).unwrap();
    let items = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        items.iter().any(|item| item["method"] == "petanque/run"
            && item["params"]["tac"] == "Theorem synthetic : True."),
        "synthetic declaration must be submitted directly to PET state"
    );
    assert_eq!(
        fs::read_to_string(project.path().join("Main.v")).unwrap(),
        "\n"
    );
    assert!(
        !state.path().join("state/pet-workspaces").exists(),
        "direct PET execution must not create a mirrored workspace"
    );
}

#[test]
fn nested_declare_uses_pet_module_context_and_writes_before_end() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Module Outer.\nModule Inner.\nEnd Inner.\nEnd Outer.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let opened = engine
        .declare(
            project.path(),
            DeclarationKind::Theorem,
            DeclarationIdentity {
                file: rocq_engine::FileId("Main.v".into()),
                qualified_path: vec![
                    "Main".into(),
                    "Main".into(),
                    "Outer".into(),
                    "Inner".into(),
                    "fresh".into(),
                ],
            },
            "True".into(),
        )
        .unwrap();
    assert!(opened.goals.contains("True"));
    let result = check_one(&engine, attempt(&opened), "exact I.").unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert_eq!(
        fs::read_to_string(project.path().join("Main.v")).unwrap(),
        "Module Outer.\nModule Inner.\nTheorem fresh : True.\nProof.\nexact I.\nQed.\nEnd Inner.\nEnd Outer.\n"
    );
    compile(project.path());
}

#[test]
fn declare_rejects_a_nonexistent_or_wrong_compilation_unit_context() {
    let lab = Lab::new("Module Existing.\nEnd Existing.\n");
    for qualified_path in [
        vec![
            "Main".into(),
            "Main".into(),
            "Missing".into(),
            "fresh".into(),
        ],
        vec!["Wrong".into(), "Library".into(), "fresh".into()],
    ] {
        let error = lab
            .engine
            .declare(
                lab.path(),
                DeclarationKind::Theorem,
                DeclarationIdentity {
                    file: rocq_engine::FileId("Main.v".into()),
                    qualified_path,
                },
                "True".into(),
            )
            .unwrap_err();
        assert!(matches!(
            error.kind,
            ErrorKind::InvalidConfiguration | ErrorKind::InvalidDeclaration
        ));
    }
    assert_eq!(lab.source(), "Module Existing.\nEnd Existing.\n");
}

#[test]
fn existing_theorem_pet_replay_uses_dune_selected_source_directly() {
    const CHILD: &str = "ROCQ_ENGINE_HEADER_PROXY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let log = wrapper.path().join("start.jsonl");
        let proxy = pet_recording_proxy(wrapper.path());
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "existing_theorem_pet_replay_uses_dune_selected_source_directly",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", &real)
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env("ROCQ_PET_BIN", &proxy)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "direct PET source test failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let lab = Lab::new(
        "From Stdlib Require Import Init.Logic.\nModule M.\nTheorem t : True. Admitted.\nEnd M.\nTheorem later : True. Admitted.\n",
    );
    let _ = open_theorem(&lab.engine, lab.path(), "t").unwrap();
    let log = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()).unwrap();
    let item: serde_json::Value = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|item| {
            item["method"] == "petanque/run"
                && item["params"]["tac"]
                    .as_str()
                    .is_some_and(|tactic| tactic.starts_with("Theorem t : True."))
        })
        .unwrap();
    let path = PathBuf::from(item["path"].as_str().unwrap());
    let content = item["content"].as_str().unwrap();
    assert_eq!(path, lab.path().join("Main.v"));
    assert!(
        content.contains("From Stdlib Require Import Init.Logic.")
            && content.contains("Module M.")
            && content.contains("Theorem t : True."),
        "PET receives the original Dune-selected source"
    );
    assert!(
        content.contains("Theorem t : True. Admitted.")
            && content.contains("Theorem later : True. Admitted."),
        "the wrapper must not rewrite or mirror the source before PET opens it"
    );
}

#[test]
fn real_pet_focus_stack_reports_unfocused_goals() {
    let lab = Lab::new("Theorem pair : True /\\ True. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "pair").unwrap());
    let result = check_one(&lab.engine, root, "split. Focus 2.").unwrap();
    assert!(result.error.is_none());
    assert_eq!(result.state.focused_goals, 1);
    assert!(
        result.state.unfocused_goals >= 1,
        "PET stack must expose temporarily unfocused sibling goals"
    );
}

#[test]
fn runtime_cache_pressure_rotates_pet_pid_and_replays() {
    const CHILD: &str = "ROCQ_ENGINE_PET_ROTATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let log = wrapper.path().join("pids");
        let pet = wrapper.path().join("pet");
        fs::write(
            &pet,
            format!(
                "#!/bin/sh\necho $$ >> '{}'\nexec \"$ROCQ_ENGINE_REAL_PET\" \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pet, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_cache_pressure_rotates_pet_pid_and_replays",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_PET_PID_LOG", &log)
            .env("ROCQ_ENGINE_REAL_PET", &real)
            .env("ROCQ_PET_BIN", &pet)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "PET rotation child failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let pids = fs::read_to_string(log).unwrap();
        assert!(
            pids.lines().count() >= 2,
            "cache pressure must rotate PET PID, got {pids:?}"
        );
        return;
    }
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let id = attempt(&check_one(&engine, id, "intro P.").unwrap().state);
    assert_eq!(engine.inspect(id).unwrap().attempt, Some(id));
}

#[test]
fn production_engine_uses_no_unsafe_code() {
    // Keep this invariant independent of the module layout: production code is
    // now split into the dune/ and pet/ submodules, so a flat filename list
    // would silently stop checking newly moved process-management code.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut source = String::new();
    let mut pending = vec![root];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                source.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    assert!(
        !source.contains("unsafe {"),
        "DESIGN §10 explicitly requires no unsafe production code; use a safe process-management library instead"
    );
}

#[test]
fn version_identity_is_root_frozen_and_never_overwritten_by_a_later_open() {
    const CHILD: &str = "ROCQ_ENGINE_VERSION_ABA_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let pet_real = real_pet_executable();
        let rocq_real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v rocq"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let pet_v = wrapper.path().join("pet.version");
        let rocq_v = wrapper.path().join("rocq.version");
        fs::write(&pet_v, "pet-a\n").unwrap();
        fs::write(&rocq_v, "rocq-a\n").unwrap();
        for (name, real, version) in [
            ("pet", pet_real.to_str().unwrap(), &pet_v),
            ("rocq", rocq_real.trim(), &rocq_v),
        ] {
            let path = wrapper.path().join(name);
            fs::write(&path,format!("#!/bin/sh\nif [ \"$1\" = --version ]; then cat '{}'; exit 0; fi\nexec {} \"$@\"\n",version.display(),real)).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "version_identity_is_root_frozen_and_never_overwritten_by_a_later_open",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PET_V", &pet_v)
            .env("ROCQ_V", &rocq_v)
            .env("ROCQ_PET_BIN", wrapper.path().join("pet"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    wrapper.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "version frozen identity child failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let lab = Lab::new("Theorem a : True. Admitted.\nTheorem b : True. Admitted.\n");
    let a = attempt(&open_theorem(&lab.engine, lab.path(), "a").unwrap());
    fs::write(std::env::var("PET_V").unwrap(), "pet-b\n").unwrap();
    let b = attempt(&open_theorem(&lab.engine, lab.path(), "b").unwrap());
    assert!(lab.engine.inspect(a).is_ok());
    assert!(lab.engine.inspect(b).is_ok());
    assert!(lab.engine.inspect(a).is_ok());
    fs::write(std::env::var("ROCQ_V").unwrap(), "rocq-b\n").unwrap();
    assert!(lab.engine.inspect(b).is_ok());
}

#[test]
fn pet_is_the_only_pet_owner_and_engine_drop_reaps_it_before_runtime_cleanup() {
    let engine =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine.rs"))
            .unwrap();
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/mod.rs"))
            .unwrap();
    assert!(engine.contains("pet: pet::PetRuntime"));
    assert!(
        !engine.contains("pet_pool")
            && !engine.contains("detach_all_pets")
            && !engine.contains("struct Pet {"),
        "Engine must delegate PET ownership rather than retaining a second pool/helper"
    );
    assert!(
        runtime.contains("struct PetProcess")
            && runtime.contains("impl Drop for PetProcess")
            && runtime.contains("killpg"),
        "PetRuntime owns and group-reaps every native PET child"
    );
}

#[test]
fn pet_capacity_is_configured_not_hard_coded_and_lock_accounting_is_local() {
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/mod.rs"))
            .unwrap()
            + &fs::read_to_string(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/runtime.rs"),
            )
            .unwrap();
    assert!(runtime.contains("max_processes: usize"));
    assert!(
        !runtime.contains("(0..4)"),
        "the number of lanes must derive from max_pet_processes, not a hidden constant"
    );
    assert!(
        runtime.contains("*active += 1") && runtime.contains("active.saturating_sub(1)"),
        "runtime capacity must be accounted only when a PET is installed or removed"
    );
    assert!(
        runtime.contains("reserve_with_reaper") && runtime.contains("evict_idle_lane"),
        "capacity exhaustion must use bounded idle-lane reclamation"
    );
}

/// T2c1b2 uses a project pool containing multiple independent lanes.  The
/// pool, rather than a project-only single lane, is the same-project
/// concurrency boundary.
#[test]
fn pet_b2_lanes_are_root_scoped_for_same_project_parallelism() {
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/runtime.rs"))
            .unwrap();
    assert!(runtime.contains("struct ProjectPool"));
    assert!(runtime.contains("lanes: Vec<Arc<ProjectLane>>"));
    assert!(runtime.contains("state.lanes.iter().find"));
    assert!(
        !runtime.contains("BTreeMap<PathBuf, Arc<ProjectLane>>"),
        "a project-only lane map serializes all same-project roots and prevents configured parallelism"
    );
}

#[test]
fn current_proof_view_is_a_latest_transient_boundary_not_a_frozen_wrapper() {
    let view = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/mod.rs"))
        .unwrap();
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/mod.rs"))
            .unwrap();
    for forbidden in [
        "FrozenTheorem",
        "ReplayTactic",
        "frozen_marker",
        "observation_key",
        "frozen_pet_document",
        "ensure_frozen_workspace",
    ] {
        assert!(
            !view.contains(forbidden),
            "CurrentProofView must own latest transient material, not delegate {forbidden} to frozen replay state"
        );
    }
    for forbidden in ["FrozenTheorem", "ReplayTactic", "frozen_marker"] {
        assert!(
            !runtime.contains(forbidden),
            "PetRuntime must operate only on CurrentProofView/native commands, not frozen engine types ({forbidden})"
        );
    }
}

#[test]
fn pet_executor_is_real_and_typed_query_validation_precedes_native_work() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let attempt = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let solved = check_one(&lab.engine, attempt, "exact I.").unwrap();
    assert!(solved.error.is_none());
    assert_eq!(solved.state.lifecycle, ProofLifecycle::Completed);

    assert_eq!(
        err_kind(lab.engine.query_expression_type(
            lab.path(),
            None,
            "I). Admitted. Theorem injected : False := I".into(),
            None,
        )),
        ErrorKind::InvalidRequest,
        "injection is rejected before a future native query executor could spawn"
    );
    assert_eq!(
        err_kind(
            lab.engine
                .query_expression_type(lab.path(), None, "I). Admitted.".into(), None,)
        ),
        ErrorKind::InvalidRequest,
        "a valid native query explicitly reports the missing executor"
    );

    let types =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/types.rs")).unwrap();
    for obsolete in [
        "pub enum Query",
        "pub enum QueryResult",
        "pub struct NewDeclaration",
    ] {
        assert!(
            !types.contains(obsolete),
            "MCP request DTO must not be retained in engine types: {obsolete}"
        );
    }
    let engine =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine.rs"))
            .unwrap();
    assert!(engine.contains("pub fn query_expression_type"));
    let parser =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet/mod.rs"))
            .unwrap();
    assert!(
        parser.contains("Sha256::digest") && parser.contains("source_digest"),
        "PET-derived source anchors must contain real source digests"
    );
}

#[test]
fn state_parent_inside_project_is_not_snapshotted_as_user_input() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    let source = "Theorem t : forall P : Prop, P -> P. Admitted.\n";
    fs::write(project.path().join("Main.v"), source).unwrap();
    let state_parent = project.path().join("engine-state");
    let engine = Engine::new(EngineConfig {
        state_parent: state_parent.clone(),
        trace_memory_bytes: 1,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let stepped = check_one(&engine, id, "intro P.").unwrap();
    assert!(
        stepped.error.is_none(),
        "engine-owned runtime/spill writes under project must not invalidate frozen user inputs"
    );
    assert_eq!(
        fs::read_to_string(project.path().join("Main.v")).unwrap(),
        source,
        "active proof never pollutes user source before close"
    );
    assert!(state_parent.exists());
}

#[test]
fn incompatible_view_detaches_pet_before_a_restored_view_replays() {
    const CHILD: &str = "ROCQ_ENGINE_REBASE_DETACH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = real_pet_executable();
        let log = wrapper.path().join("pet-starts.jsonl");
        let proxy = pet_recording_proxy(wrapper.path());
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "incompatible_view_detaches_pet_before_a_restored_view_replays",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", &real)
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env("ROCQ_PET_BIN", &proxy)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated current-view PET-detach test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let lab = Lab::new(true_theorems());
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let source = lab.source();
    fs::write(
        lab.path().join("Main.v"),
        source.replacen("True", "False", 1),
    )
    .unwrap();
    assert_eq!(
        err_kind(lab.engine.inspect(id)),
        ErrorKind::DeclarationChanged
    );
    fs::write(lab.path().join("Main.v"), source).unwrap();
    assert!(lab.engine.inspect(id).is_ok());

    let starts = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()).unwrap();
    assert!(
        starts.lines().count() >= 2,
        "an incompatible view must detach the old PET before the restored view starts a fresh replay"
    );
}

#[test]
fn different_files_same_project_publish_concurrently_without_epoch_cross_talk() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(project.path().join("A.v"), "Theorem a : True. Admitted.\n").unwrap();
    fs::write(project.path().join("B.v"), "Theorem b : True. Admitted.\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Arc::new(
        Engine::new(EngineConfig {
            state_parent: state.path().join("state"),
            trace_memory_bytes: 1024,
            operation_timeout: Duration::from_secs(10),
            close_timeout: Some(Duration::from_secs(10)),
            runtime_cache_bytes: 1024,
            max_pet_processes: 4,
        })
        .unwrap(),
    );
    let a = attempt(&open_theorem(&engine, project.path(), "a").unwrap());
    let b = attempt(&open_theorem(&engine, project.path(), "b").unwrap());
    let gate = Arc::new(Barrier::new(2));
    let mut joins = Vec::new();
    for id in [a, b] {
        let e = engine.clone();
        let g = gate.clone();
        joins.push(thread::spawn(move || {
            g.wait();
            check_one(&e, id, "exact I.")
        }));
    }
    for join in joins {
        let result = join.join().unwrap().unwrap();
        assert!(
            result.error.is_none(),
            "unrelated file proof must publish despite sibling epoch change: {:?}",
            result.error
        );
    }
    for file in ["A.v", "B.v"] {
        let source = fs::read_to_string(project.path().join(file)).unwrap();
        assert!(!source.contains("Admitted."));
        let out = Command::new("rocq")
            .args(["compile", file])
            .current_dir(project.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{file}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn state_parent_inside_project_remains_stable_across_repeated_catalog_and_open() {
    let project = tempfile::tempdir().unwrap();
    initialize_dune_project(project.path());
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : forall P : Prop, P -> P. Admitted.\n",
    )
    .unwrap();
    let state_parent = project.path().join(".engine-state");
    let engine = Engine::new(EngineConfig {
        state_parent: state_parent.clone(),
        trace_memory_bytes: 1,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Some(Duration::from_secs(10)),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    for _ in 0..4 {
        let declarations = main_declarations(&engine, project.path()).unwrap();
        assert_eq!(
            declarations
                .iter()
                .filter(|d| d.identity.constant() == Some("t"))
                .count(),
            1,
            "engine runtime documents must never become project declarations"
        );
        let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
        assert!(check_one(&engine, id, "idtac.").unwrap().error.is_none());
    }
    let names: Vec<_> = fs::read_dir(&state_parent)
        .unwrap()
        .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|x| x.ends_with(".v") && !x.starts_with("PetHeader_")),
        "runtime must not accumulate project-visible PET documents: {names:?}"
    );
}

#[test]
fn declaration_refresh_drops_pet_entries_removed_from_source() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    assert_eq!(main_declarations(&lab.engine, lab.path()).unwrap().len(), 1);
    fs::write(lab.path().join("Main.v"), "").unwrap();
    assert!(
        main_declarations(&lab.engine, lab.path())
            .unwrap()
            .is_empty()
    );
}
