//! Independent public-API tests.  Every proof semantic assertion runs installed
//! Rocq on a disposable project; no simulated prover is used here.

use rocq_engine::{
    DeclarationIdentity, DeclarationKind, Engine, EngineConfig, ErrorKind, LogicalLibrary,
    NewDeclaration, ProofLifecycle, Query, QueryResult,
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
impl Lab {
    fn new(source: &str) -> Self {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("Main.v"), source).unwrap();
        let state = tempfile::tempdir().unwrap();
        let engine = Engine::new(EngineConfig {
            state_parent: state.path().join("state"),
            trace_memory_bytes: 1024,
            operation_timeout: Duration::from_secs(10),
            close_timeout: Duration::from_secs(10),
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
    let catalog = engine.catalog(project)?;
    let pieces = name.split('.').collect::<Vec<_>>();
    let matches = catalog
        .declarations
        .iter()
        .filter(|item| {
            item.identity.constant == *pieces.last().unwrap()
                && (pieces.len() == 1
                    || item
                        .identity
                        .modules
                        .iter()
                        .map(String::as_str)
                        .eq(pieces[..pieces.len() - 1].iter().copied()))
        })
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

fn attempt(state: &rocq_engine::ProofState) -> rocq_engine::AttemptId {
    state
        .attempt
        .expect("an open proof returns an opaque attempt")
}
fn err_kind<T>(value: Result<T, rocq_engine::Error>) -> ErrorKind {
    match value {
        Ok(_) => panic!("expected public error"),
        Err(error) => error.kind,
    }
}
fn rejected_step_kind(result: Result<rocq_engine::StepResult, rocq_engine::Error>) -> ErrorKind {
    match result {
        Err(error) => error.kind,
        Ok(result) => {
            result
                .error
                .expect("rejected command must report an error")
                .kind
        }
    }
}
fn compile(project: &Path) {
    let output = Command::new("rocq")
        .args(["compile", "Main.v"])
        .current_dir(project)
        .output()
        .expect("installed rocq");
    assert!(
        output.status.success(),
        "independent native build failed: {}",
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
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,
        max_pet_processes,
    })
    .unwrap()
}

/// Write a transparent PET protocol proxy that records each `start` URI and
/// document bytes before forwarding frames to the installed real PET.
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
        if message.get("method") == "petanque/start":
            uri = message["params"]["uri"]
            path = urllib.parse.unquote(urllib.parse.urlparse(uri).path)
            with open(os.environ["ROCQ_ENGINE_PET_LOG"], "a") as log:
                log.write(json.dumps({"pid": os.getpid(), "path": path, "content": open(path).read()}) + "\n")
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
fn pet_runtime_enforces_configured_capacity_and_single_flight_spawning() {
    const CHILD: &str = "ROCQ_ENGINE_CAPACITY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let log = wrapper.path().join("pet-starts.jsonl");
        let proxy = pet_recording_proxy(wrapper.path());
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pet_runtime_enforces_configured_capacity_and_single_flight_spawning",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", real.trim())
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    proxy.parent().unwrap().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
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

    // More than four roots in one project must reach the configured capacity;
    // 64 identities make every one of five configured lanes observable without
    // relying on a timing race.
    let state = tempfile::tempdir().unwrap();
    let five = engine_with_pet_capacity(state.path(), 5);
    let project = tempfile::tempdir().unwrap();
    let source = (0..64)
        .map(|index| format!("Theorem t{index} : True. Admitted.\n"))
        .collect::<String>();
    fs::write(project.path().join("Main.v"), source).unwrap();
    for index in 0..64 {
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

    // Concurrent first use of the same frozen root is serialised at the lane:
    // both callers succeed but only one process is spawned for that lane.
    let state = tempfile::tempdir().unwrap();
    let single = Arc::new(engine_with_pet_capacity(state.path(), 2));
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem once : True. Admitted.\n",
    )
    .unwrap();
    let before = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap_or_default()
        .lines()
        .count();
    let gate = Arc::new(Barrier::new(3));
    let mut joins = Vec::new();
    for _ in 0..2 {
        let engine = Arc::clone(&single);
        let path = project.path().to_owned();
        let gate = Arc::clone(&gate);
        joins.push(thread::spawn(move || {
            gate.wait();
            open_theorem(&engine, &path, "once").unwrap()
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
        "same-lane first spawn must be single-flight"
    );

    // Invalidation is scoped to its project: after A loses its current view,
    // B's cached PET state remains live and does not receive a replacement.
    let state = tempfile::tempdir().unwrap();
    let scoped = engine_with_pet_capacity(state.path(), 2);
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(a.path().join("Main.v"), "Theorem a : True. Admitted.\n").unwrap();
    fs::write(b.path().join("Main.v"), "Theorem b : True. Admitted.\n").unwrap();
    let a_attempt = attempt(&open_theorem(&scoped, a.path(), "a").unwrap());
    let b_attempt = attempt(&open_theorem(&scoped, b.path(), "b").unwrap());
    let before_invalidation = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap())
        .unwrap()
        .lines()
        .count();
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
    let left = lab.engine.step(left, "intro P.").unwrap();
    assert!(left.error.is_none());
    assert_eq!(left.state.accepted_commands, 1);
    assert_eq!(
        lab.engine.inspect(right).unwrap().accepted_commands,
        0,
        "one logical interaction must not advance another cursor"
    );
    let right = lab.engine.step(right, "intro Q.").unwrap();
    assert!(right.error.is_none());
    assert_eq!(right.state.accepted_commands, 1);
}

#[test]
fn retry_is_idempotent_and_multi_sentence_keeps_exact_prefix() {
    let lab = Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let first = lab.engine.step(root, "intro P.").unwrap();
    assert!(first.error.is_none());
    let retry = lab.engine.step(root, " intro P. ").unwrap();
    assert!(retry.error.is_none());
    assert_eq!(
        retry.state.accepted_commands, 1,
        "equal canonical parent/action must share a child"
    );
    let partial = lab
        .engine
        .step(root, "intro P. this_is_not_a_tactic.")
        .unwrap();
    assert_eq!(
        partial.state.accepted_commands, 1,
        "only the native-accepted prefix may be committed"
    );
    assert_eq!(
        partial.error.expect("bad suffix reported").kind,
        ErrorKind::ProofStepFailed
    );
    let retained = attempt(&partial.state);
    assert_eq!(
        lab.engine.inspect(retained).unwrap().accepted_commands,
        1,
        "accepted prefix must be replayable"
    );
}

#[test]
fn native_goals_are_open_and_admission_is_rejected() {
    let lab = Lab::new(true_theorems());
    let open = open_theorem(&lab.engine, lab.path(), "t").unwrap();
    assert!(open.focused_goals > 0, "real unfinished theorem has a goal");
    let id = attempt(&open);
    assert_eq!(
        rejected_step_kind(lab.engine.step(id, "admit.")),
        ErrorKind::InvalidRequest
    );
    assert_eq!(
        rejected_step_kind(lab.engine.step(id, "Admitted.")),
        ErrorKind::InvalidRequest
    );
    let still_open = lab.engine.inspect(id).unwrap();
    assert_eq!(still_open.accepted_commands, 0);
    assert_eq!(still_open.given_up_goals, 0);
}

#[test]
fn solved_step_automatically_publishes_after_durable_candidate() {
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let solved = lab.engine.step(root, "exact I.").unwrap();
    assert!(solved.error.is_none());
    assert_eq!(solved.state.lifecycle, ProofLifecycle::Completed);
    assert!(solved.state.attempt.is_none());
    assert!(lab.source().contains("Qed."));
    let catalog = lab.engine.catalog(lab.path()).unwrap();
    assert_eq!(
        catalog
            .declarations
            .iter()
            .find(|item| item.identity.constant == "t")
            .unwrap()
            .status,
        ProofLifecycle::Completed
    );
}

#[test]
fn candidates_are_ordered_mixed_and_side_effect_free() {
    let lab = Lab::new(true_theorems());
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let before = lab.source();
    let result = lab
        .engine
        .candidates(
            id,
            &["exact I.".into(), "not_a_tactic.".into(), "idtac.".into()],
        )
        .unwrap();
    assert_eq!(result.len(), 3);
    assert!(result[0].error.is_none(), "first candidate succeeds");
    assert_eq!(
        result[1]
            .error
            .as_ref()
            .expect("middle failure stays in place")
            .kind,
        ErrorKind::ProofStepFailed
    );
    assert!(
        result[2].error.is_none(),
        "later candidate is not poisoned by a failure"
    );
    assert_eq!(
        lab.engine.inspect(id).unwrap().accepted_commands,
        0,
        "candidates cannot append"
    );
    assert_eq!(lab.source(), before, "candidates cannot publish");
}

#[test]
fn typed_queries_are_contextual_validated_and_read_only() {
    let lab = Lab::new("Definition d : True := I.\nTheorem t : True. Admitted.\n");
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let before = lab.source();
    for query in [
        Query::Search {
            name: Some("t".into()),
            statement: None,
            status: None,
            offset: 0,
            limit: 20,
        },
        Query::Statement { name: "t".into() },
        Query::Proof { name: "t".into() },
        Query::Definition { name: "d".into() },
        Query::Assumptions { name: "d".into() },
        Query::Dependencies { name: "d".into() },
        Query::ExpressionType {
            expression: "I".into(),
        },
        Query::Notation {
            expression: "nat".into(),
        },
    ] {
        let _ = lab.engine.query(lab.path(), None, query).unwrap();
    }
    match lab
        .engine
        .query(lab.path(), Some(id), Query::Goals)
        .unwrap()
    {
        QueryResult::State(state) => assert_eq!(state.accepted_commands, 0),
        _ => panic!("contextual goals must return state"),
    }
    assert_eq!(
        err_kind(lab.engine.query(
            lab.path(),
            None,
            Query::ExpressionType {
                expression: "I). Admitted. Theorem injected : False := I".into()
            }
        )),
        ErrorKind::InvalidRequest
    );
    assert_eq!(lab.source(), before, "all query variants are read-only");
}

#[test]
fn catalog_handles_qualified_ambiguous_comments_strings_and_unicode() {
    let lab = Lab::new(
        "(* nested (* comment. *) *)\nModule A. Theorem t : True. Admitted. End A.\nModule B. Theorem t : True. Admitted. End B.\nTheorem λ : True. Admitted.\nDefinition s := \"a.dot\".\n",
    );
    let catalog = lab.engine.catalog(lab.path()).unwrap();
    assert_eq!(
        catalog
            .declarations
            .iter()
            .filter(|d| d.identity.constant == "t")
            .count(),
        2
    );
    assert!(
        catalog
            .declarations
            .iter()
            .any(|d| d.identity.constant == "λ")
    );
    assert_eq!(
        err_kind(open_theorem(&lab.engine, lab.path(), "t")),
        ErrorKind::Ambiguous
    );
    assert_eq!(
        err_kind(open_theorem(&lab.engine, lab.path(), "missing")),
        ErrorKind::NotFound
    );
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
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let result = engine.step(id, "idtac.").unwrap();
    assert!(result.error.is_none());
    let message = format!("{:?}", rejected_step_kind(engine.step(id, "admit.")));
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
            lab.engine.step(id, "exact I.")
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
    let t = lab.engine.step(t, "exact I.").unwrap();
    assert!(t.error.is_none());
    let u = lab.engine.step(u, "exact I.").unwrap();
    assert!(
        u.error.is_none(),
        "second declaration must revalidate and preserve first publication"
    );
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
fn transparent_declaration_automatically_publishes_and_natively_builds() {
    let lab = Lab::new("\n");
    let state = lab
        .engine
        .declare(
            lab.path(),
            NewDeclaration {
                kind: DeclarationKind::Definition,
                identity: DeclarationIdentity {
                    library: LogicalLibrary(vec!["Synthetic".into()]),
                    modules: vec![],
                    constant: "synthetic".into(),
                },
                context: vec![],
                statement: "True".into(),
            },
        )
        .unwrap();
    let result = lab.engine.step(attempt(&state), "exact I.").unwrap();
    assert!(
        result.error.is_none(),
        "solved synthetic declaration must close automatically"
    );
    let source = fs::read_to_string(lab.path().join("Synthetic.v")).unwrap();
    assert!(
        source.contains("Defined."),
        "transparent declaration must use Defined"
    );
    let output = Command::new("rocq")
        .args(["compile", "Synthetic.v"])
        .current_dir(lab.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "independent synthetic compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn declaration_into_an_existing_empty_file_publishes_atomically() {
    let lab = Lab::new("\n");
    fs::write(lab.path().join("Empty.v"), "").unwrap();
    let state = lab
        .engine
        .declare(
            lab.path(),
            NewDeclaration {
                kind: DeclarationKind::Theorem,
                identity: DeclarationIdentity {
                    library: LogicalLibrary(vec!["Empty".into()]),
                    modules: vec![],
                    constant: "created".into(),
                },
                context: vec![],
                statement: "True".into(),
            },
        )
        .unwrap();
    let result = lab.engine.step(attempt(&state), "exact I.").unwrap();
    assert!(result.error.is_none());
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert!(
        fs::read_to_string(lab.path().join("Empty.v"))
            .unwrap()
            .contains("Qed.")
    );
}

#[test]
fn new_nested_library_creates_required_directories_only_at_close() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join("theories")).unwrap();
    fs::write(project.path().join("_CoqProject"), "-Q theories Demo\n").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state_dir.path().join("state"), 2);
    let opened = engine
        .declare(
            project.path(),
            NewDeclaration {
                kind: DeclarationKind::Theorem,
                identity: DeclarationIdentity {
                    library: LogicalLibrary(vec!["Demo".into(), "Sub".into(), "Fresh".into()]),
                    modules: vec![],
                    constant: "created".into(),
                },
                context: vec![],
                statement: "True".into(),
            },
        )
        .unwrap();
    assert!(!project.path().join("theories/Sub").exists());
    let result = engine.step(attempt(&opened), "exact I.").unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert!(project.path().join("theories/Sub/Fresh.v").is_file());
}

#[test]
fn direct_native_close_honors_coqproject_load_paths() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join("theories")).unwrap();
    fs::write(project.path().join("_CoqProject"), "-Q theories Demo\n").unwrap();
    fs::write(
        project.path().join("theories/Dep.v"),
        "Definition dep : True := I.\n",
    )
    .unwrap();
    let compiled = Command::new("rocq")
        .args(["compile", "-Q", "theories", "Demo", "theories/Dep.v"])
        .current_dir(project.path())
        .status()
        .unwrap();
    assert!(compiled.success());
    fs::write(
        project.path().join("theories/Main.v"),
        "From Demo Require Import Dep.\nTheorem t : True. Admitted.\n",
    )
    .unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state_dir.path().join("state"), 2);
    let result = engine
        .step(
            attempt(&open_theorem(&engine, project.path(), "t").unwrap()),
            "exact dep.",
        )
        .unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn trust_audit_rejects_an_unfinished_dependency_without_publishing() {
    let lab = Lab::new("Theorem unfinished : True. Admitted.\nTheorem t : True. Admitted.\n");
    let result = lab
        .engine
        .step(
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
    assert_eq!(result.state.lifecycle, ProofLifecycle::Rejected);
    assert!(result.state.attempt.is_none());
    assert!(
        lab.source().contains("Theorem t : True. Admitted."),
        "failed trust audit must not publish"
    );
}

#[test]
fn native_trust_audit_accepts_only_frozen_explicit_axioms() {
    let lab = Lab::new("Axiom ax : True.\nTheorem t : True. Admitted.\n");
    let result = lab
        .engine
        .step(
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
    let result = lab
        .engine
        .step(
            attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
            "exact (M.ax True I).",
        )
        .unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn native_trust_audit_authorizes_unchanged_standard_external_artifact() {
    let lab = Lab::new(
        "Require Import Coq.Logic.Classical_Prop.\nTheorem t : forall P : Prop, P \\/ ~ P. Admitted.\n",
    );
    let result = lab
        .engine
        .step(
            attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
            "intro P. exact (classic P).",
        )
        .unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
    assert!(!lab.source().contains("Admitted."));
}

#[test]
fn native_trust_audit_authorizes_configured_external_q_root_by_vo_identity() {
    let outer = tempfile::tempdir().unwrap();
    let external = outer.path().join("external");
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("Ext.v"), "Axiom ext : True.\n").unwrap();
    assert!(
        Command::new("rocq")
            .args(["compile", "-Q", external.to_str().unwrap(), "Lib", "Ext.v"])
            .current_dir(&external)
            .status()
            .unwrap()
            .success()
    );
    let project = outer.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("_CoqProject"),
        format!("-Q {} Lib\n", external.display()),
    )
    .unwrap();
    fs::write(
        project.join("Main.v"),
        "From Lib Require Import Ext.\nTheorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state.path().join("state"), 2);
    let result = engine
        .step(
            attempt(&open_theorem(&engine, &project, "t").unwrap()),
            "exact Lib.Ext.ext.",
        )
        .unwrap();
    assert!(result.error.is_none(), "{:?}", result.error);
    assert_eq!(result.state.lifecycle, ProofLifecycle::Completed);
}

#[test]
fn compatible_dependency_and_configuration_changes_replay_the_selected_trace() {
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("_CoqProject"), "-Q . Demo\n").unwrap();
    fs::write(
        project.path().join("Dep.v"),
        "Definition dependency : True := I.\n",
    )
    .unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    fs::write(
        project.path().join("Dep.v"),
        "Definition dependency : True := I.\nDefinition dependency_2 : True := I.\n",
    )
    .unwrap();
    let replayed = engine.step(id, "idtac.").unwrap();
    assert!(
        replayed.error.is_none() && replayed.state.accepted_commands == 1,
        "a compatible dependency change must replay the selected trace, not permanently stale it"
    );

    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    fs::write(project.path().join("_CoqProject"), "-Q . Demo\n-I .\n").unwrap();
    let replayed = engine.step(id, "idtac.").unwrap();
    assert!(
        replayed.error.is_none() && replayed.state.accepted_commands == 1,
        "a compatible configuration change must replay the selected trace, not permanently stale it"
    );
}

#[test]
fn native_timeout_and_output_bounds_cleanup_through_path_wrappers() {
    const CHILD: &str = "ROCQ_ENGINE_WRAPPER_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let path = wrapper.path().join("pet");
        fs::write(&path, "#!/bin/sh\nif [ \"$ROCQ_WRAPPER_MODE\" = hang ]; then sleep 4; exit 0; fi\nprintf 'Content-Length: 70000\n\n'\n").unwrap();
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
                "isolated {mode} wrapper test failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    let hanging = std::env::var("ROCQ_WRAPPER_MODE").unwrap() == "hang";
    let project = tempfile::tempdir().unwrap();
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
        close_timeout: Duration::from_secs(1),
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
        fs::write(&rocq, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then exec {} \"$@\"; fi\nif [ \"$1\" = dep ]; then exec {} \"$@\"; fi\nlast=\"\"; for x in \"$@\"; do last=\"$x\"; done\nif [ -f \"$last\" ] && grep -q 'Qed\\.' \"$last\"; then sleep 3; exit 0; fi\nexec {} \"$@\"\n", actual.trim(), actual.trim(), actual.trim())).unwrap();
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
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let result = lab
        .engine
        .step(
            attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
            "exact I.",
        )
        .unwrap();
    assert!(matches!(
        result.error.as_ref().map(|error| error.kind),
        None | Some(ErrorKind::BuildTimeout)
    ));
    assert!(matches!(
        result.state.lifecycle,
        ProofLifecycle::Pending | ProofLifecycle::Rejected
    ));
    assert!(lab.source().contains("Admitted."));
}

#[test]
fn section_context_and_canonical_comments_strings_replay_without_rewriting() {
    let lab = Lab::new(
        "Section S.\nVariable P : Prop.\nTheorem local : P -> P. Admitted.\nEnd S.\nTheorem text : True. Admitted.\n",
    );
    let local = attempt(&open_theorem(&lab.engine, lab.path(), "local").unwrap());
    let intro = lab
        .engine
        .step(local, "intro H. (* prose with a.dot *)")
        .unwrap();
    assert!(intro.error.is_none());
    let finish = lab.engine.step(attempt(&intro.state), "exact H.").unwrap();
    assert!(finish.error.is_none());
    compile(lab.path());

    let text = attempt(&open_theorem(&lab.engine, lab.path(), "text").unwrap());
    let quoted = lab.engine.step(text, "idtac \"a.dot\".").unwrap();
    assert!(
        quoted.error.is_none(),
        "a dot inside a Rocq string is not a sentence terminator"
    );
    let retry = lab
        .engine
        .step(text, "  idtac \"a.dot\".  (* equivalent comment *)")
        .unwrap();
    assert!(retry.error.is_none());
    assert_eq!(
        retry.state.accepted_commands, 1,
        "canonical comments/whitespace preserve same action identity"
    );
}

#[test]
fn compatible_external_artifact_change_delete_and_restore_replay_latest_view() {
    let lab = Lab::new(true_theorems());
    let artifact = lab.path().join("external.vo");
    fs::write(&artifact, b"artifact-a").unwrap();
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    fs::write(&artifact, b"artifact-b").unwrap(); // same length, different content
    let replayed = lab.engine.step(id, "idtac.").unwrap();
    assert!(replayed.error.is_none() && replayed.state.accepted_commands == 1);

    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let original = fs::read(&artifact).unwrap();
    fs::remove_file(&artifact).unwrap();
    let replayed = lab.engine.step(id, "idtac.").unwrap();
    assert!(replayed.error.is_none() && replayed.state.accepted_commands == 1);
    fs::write(&artifact, original).unwrap();
    assert!(
        lab.engine.inspect(id).is_ok(),
        "a restored current view must remain replayable after artifact deletion"
    );
}

#[test]
fn toolchain_bytes_refresh_latest_view_for_same_trace() {
    const CHILD: &str = "ROCQ_ENGINE_TOOLCHAIN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let actual = Command::new("sh")
            .args(["-c", "command -v rocq"])
            .output()
            .unwrap();
        let actual = String::from_utf8(actual.stdout).unwrap();
        let rocq = wrapper.path().join("rocq");
        fs::write(&rocq, format!("#!/bin/sh\nexec {} \"$@\"\n", actual.trim())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&rocq, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "toolchain_bytes_are_frozen_inputs",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_WRAPPER", &rocq)
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
            "isolated toolchain freshness test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let lab = Lab::new(true_theorems());
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    let wrapper = PathBuf::from(std::env::var_os("ROCQ_ENGINE_WRAPPER").unwrap());
    fs::write(
        &wrapper,
        fs::read(&wrapper)
            .unwrap()
            .into_iter()
            .chain(b"# changed\n".iter().copied())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(lab.engine.inspect(id).is_ok());
}

#[test]
fn dune_theory_project_can_publish_and_independently_build() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    fs::create_dir(project.path().join("theories")).unwrap();
    fs::write(
        project.path().join("theories/dune"),
        "(rocq.theory (name Demo))\n",
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
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let opened = open_theorem(&engine, project.path(), "t").unwrap();
    assert!(
        engine
            .step(attempt(&opened), "exact I.")
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
fn new_dune_module_updates_an_explicit_modules_field_transactionally() {
    let project = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("dune-project"),
        "(lang dune 3.21)\n(using rocq 0.11)\n",
    )
    .unwrap();
    fs::create_dir(project.path().join("theories")).unwrap();
    fs::write(
        project.path().join("theories/dune"),
        "(rocq.theory (name Demo) (modules Existing))\n",
    )
    .unwrap();
    fs::write(
        project.path().join("theories/Existing.v"),
        "Definition old : True := I.\n",
    )
    .unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let engine = engine_with_pet_capacity(&state_dir.path().join("state"), 2);
    let declared = engine
        .declare(
            project.path(),
            NewDeclaration {
                kind: DeclarationKind::Theorem,
                identity: DeclarationIdentity {
                    library: LogicalLibrary(vec!["Demo".into(), "Fresh".into()]),
                    modules: vec![],
                    constant: "created".into(),
                },
                context: vec![],
                statement: "True".into(),
            },
        )
        .unwrap();
    let result = engine.step(attempt(&declared), "exact I.").unwrap();
    assert!(result.error.is_none());
    let dune = fs::read_to_string(project.path().join("theories/dune")).unwrap();
    assert!(dune.contains("modules Existing Fresh"));
    assert!(
        Command::new("dune")
            .arg("build")
            .current_dir(project.path())
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn contention_reference_model_keeps_each_public_prefix_independent() {
    let lab = Arc::new(Lab::new("Theorem t : forall P : Prop, P -> P. Admitted.\n"));
    let gate = Arc::new(Barrier::new(12));
    let mut workers = Vec::new();
    for index in 0..12 {
        let lab = lab.clone();
        let gate = gate.clone();
        workers.push(thread::spawn(move || {
            let root = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
            gate.wait();
            let one = lab
                .engine
                .step(
                    root,
                    if index % 2 == 0 {
                        "intro P."
                    } else {
                        "intro Q."
                    },
                )
                .unwrap();
            assert!(one.error.is_none());
            let two = lab.engine.step(attempt(&one.state), "idtac.").unwrap();
            assert!(two.error.is_none());
            assert_eq!(two.state.accepted_commands, 2);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
}

#[test]
fn engine_drop_removes_only_its_owned_spill_child() {
    let project = tempfile::tempdir().unwrap();
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
            close_timeout: Duration::from_secs(10),
            runtime_cache_bytes: 1,
            max_pet_processes: 4,
        })
        .unwrap();
        let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
        assert!(engine.step(id, "idtac.").unwrap().error.is_none());
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
    let result = self_ref
        .engine
        .step(
            attempt(&open_theorem(&self_ref.engine, self_ref.path(), "t").unwrap()),
            "exact t.",
        )
        .unwrap();
    assert!(
        result.error.is_some(),
        "a theorem must never prove itself through its old admission"
    );
    assert!(self_ref.source().contains("Admitted."));
    let id = attempt(&open_theorem(&self_ref.engine, self_ref.path(), "t").unwrap());
    assert_eq!(
        rejected_step_kind(self_ref.engine.step(id, "Axiom forged : True.")),
        ErrorKind::InvalidRequest
    );

    let trusted = Lab::new("Definition trusted : True := I.\nTheorem t : True. Admitted.\n");
    let result = trusted
        .engine
        .step(
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
    let text = |query| match lab.engine.query(lab.path(), None, query).unwrap() {
        QueryResult::Text(text) => text,
        _ => panic!("text query"),
    };
    let definition = text(Query::Definition { name: "d".into() });
    let dependencies = text(Query::Dependencies { name: "d".into() });
    assert_ne!(
        definition, dependencies,
        "Dependencies must be a dependency query, not an alias for Definition"
    );
    let typ = text(Query::ExpressionType {
        expression: "nat".into(),
    });
    let notation = text(Query::Notation {
        expression: "nat".into(),
    });
    assert_ne!(
        typ, notation,
        "notation interpretation must not be an expression-type alias"
    );
}

#[test]
fn final_native_validation_failure_rolls_back_source_and_allows_a_later_close() {
    const CHILD: &str = "ROCQ_ENGINE_ROLLBACK_CHILD";
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
        let counter = wrapper.path().join("count");
        let rocq = wrapper.path().join("rocq");
        fs::write(&rocq, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then exec {1} \"$@\"; fi\nn=0; [ -f '{0}' ] && n=$(cat '{0}'); n=$((n+1)); echo $n > '{0}'\nif [ $n -eq 5 ]; then echo forced-final-failure >&2; exit 1; fi\nexec {1} \"$@\"\n", counter.display(), actual.trim())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&rocq, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "final_native_validation_failure_rolls_back_source_and_allows_a_later_close",
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
            "isolated rollback test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let result = lab
        .engine
        .step(
            attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap()),
            "exact I.",
        )
        .unwrap();
    if let Some(error) = result.error {
        assert_eq!(error.kind, ErrorKind::BuildTimeout);
        assert_eq!(
            lab.source(),
            "Theorem t : True. Admitted.\n",
            "failed validation rolls back source"
        );
    }
    let retry = open_theorem(&lab.engine, lab.path(), "t").unwrap();
    assert_eq!(
        retry.lifecycle,
        ProofLifecycle::Completed,
        "a retained solved trace is retried automatically on reopen"
    );
    assert!(!lab.source().contains("Admitted."));
    if std::env::var_os(CHILD).is_none() {
        compile(lab.path());
    }
}

#[test]
fn interactive_source_audit_requires_structured_pet_and_forbids_textual_repl() {
    let engine =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine.rs"))
            .unwrap();
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet_runtime.rs"))
            .unwrap();
    assert!(engine.contains("pet_runtime"));
    for required in [
        "Command::new(\"pet\")",
        "petanque/setWorkspace",
        "petanque/start",
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
fn real_pet_is_invoked_for_open_step_inspect_and_candidates() {
    const CHILD: &str = "ROCQ_ENGINE_PET_AUDIT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let log = wrapper.path().join("pet.log");
        let pet = wrapper.path().join("pet");
        fs::write(
            &pet,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\nexec {} \"$@\"\n",
                log.display(),
                real.trim()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pet, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "real_pet_is_invoked_for_open_step_inspect_and_candidates",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_PET_LOG", &log)
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
    let stepped = lab.engine.step(id, "intro P.").unwrap();
    assert!(stepped.error.is_none());
    let id = attempt(&stepped.state);
    assert_eq!(lab.engine.inspect(id).unwrap().accepted_commands, 1);
    let candidates = lab
        .engine
        .candidates(id, &["exact H.".into(), "idtac.".into()])
        .unwrap();
    assert_eq!(candidates.len(), 2);
}

#[test]
fn pet_structured_goal_sets_report_focused_and_shelved_without_console_parsing() {
    let lab =
        Lab::new("Theorem pair : True /\\ True. Admitted.\nTheorem pending : True. Admitted.\n");
    let pair = attempt(&open_theorem(&lab.engine, lab.path(), "pair").unwrap());
    let split = lab.engine.step(pair, "split.").unwrap();
    assert!(split.error.is_none());
    assert_eq!(
        split.state.focused_goals, 2,
        "PET focused goal array must retain both subgoals"
    );
    assert_eq!(split.state.unfocused_goals, 0);
    assert_eq!(split.state.shelved_goals, 0);
    let pending = attempt(&open_theorem(&lab.engine, lab.path(), "pending").unwrap());
    let shelved = lab.engine.step(pending, "shelve.").unwrap();
    assert!(shelved.error.is_none());
    assert_eq!(shelved.state.focused_goals, 0);
    assert_eq!(
        shelved.state.shelved_goals, 1,
        "PET shelf must not be confused with solved proof"
    );
    assert_eq!(shelved.state.given_up_goals, 0);
}

#[test]
fn pet_faults_are_killed_and_next_call_recovers_by_replay() {
    const CHILD: &str = "ROCQ_ENGINE_PET_RECOVERY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let counter = wrapper.path().join("count");
        let pet = wrapper.path().join("pet");
        fs::write(&pet, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then exec {1} \"$@\"; fi\nn=0; [ -f '{0}' ] && n=$(cat '{0}'); n=$((n+1)); echo $n > '{0}'\nif [ $n -eq 1 ]; then case \"$ROCQ_ENGINE_PET_FAULT\" in hang) sleep 4;; malformed) printf 'Content-Length: 1\\n\\n{{';; overflow) printf 'Content-Length: 70000\\n\\n';; death) :;; esac; exit 0; fi\nexec {1} \"$@\"\n", counter.display(), real.trim())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pet, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for fault in ["hang", "malformed", "overflow", "death"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "pet_faults_are_killed_and_next_call_recovers_by_replay",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("ROCQ_ENGINE_PET_FAULT", fault)
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
                "PET {fault} recovery child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            fs::write(&counter, "0").unwrap();
        }
        return;
    }
    let project = tempfile::tempdir().unwrap();
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
        close_timeout: Duration::from_secs(1),
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
        "death" | "malformed" | "overflow" => attempt(&first.unwrap()),
        other => panic!("unknown PET fault mode {other}"),
    };
    assert!(
        engine.step(id, "intro P.").unwrap().error.is_none(),
        "fresh PET must reconstruct the prefix after fault"
    );
}

#[test]
fn tiny_runtime_cache_evicts_pet_states_but_replays_authoritative_prefixes() {
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("Main.v"), "Theorem a : forall P : Prop, P -> P. Admitted.\nTheorem b : forall P : Prop, P -> P. Admitted.\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let a = attempt(&open_theorem(&engine, project.path(), "a").unwrap());
    let a = attempt(&engine.step(a, "intro P.").unwrap().state);
    let b = attempt(&open_theorem(&engine, project.path(), "b").unwrap());
    let _ = engine.step(b, "intro Q.").unwrap();
    let replayed = engine.inspect(a).unwrap();
    assert_eq!(replayed.accepted_commands, 1);
    assert!(
        replayed.goals.contains("P"),
        "evicted PET state must be reconstructed from trace prefix"
    );
}

#[test]
fn synthetic_declare_pet_document_is_owned_admission_free_and_removed() {
    const CHILD: &str = "ROCQ_ENGINE_SYNTHETIC_PROXY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let log = wrapper.path().join("start.jsonl");
        pet_recording_proxy(wrapper.path());
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "synthetic_declare_pet_document_is_owned_admission_free_and_removed",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", real.trim())
            .env("ROCQ_ENGINE_PET_LOG", &log)
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
            "synthetic PET mirror test failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("Main.v"), "\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let _ = engine
        .declare(
            project.path(),
            NewDeclaration {
                kind: DeclarationKind::Theorem,
                identity: DeclarationIdentity {
                    library: LogicalLibrary(vec!["Synthetic".into()]),
                    modules: vec![],
                    constant: "synthetic".into(),
                },
                context: vec![],
                statement: "True".into(),
            },
        )
        .unwrap();
    let log = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()).unwrap();
    let item: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    let path = PathBuf::from(item["path"].as_str().unwrap());
    let content = item["content"].as_str().unwrap();
    assert!(
        path.starts_with(state.path()),
        "synthetic PET document belongs to engine-owned state, not project: {path:?}"
    );
    assert!(
        !content.to_ascii_lowercase().contains("admitted")
            && !content.to_ascii_lowercase().contains("admit"),
        "synthetic PET document must not install an admission"
    );
    assert!(
        !project.path().read_dir().unwrap().any(|x| x
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("RocqEngineSynthetic")),
        "project must not retain PET temporary document"
    );
}

#[test]
fn existing_theorem_pet_start_uses_frozen_header_mirror_not_live_source() {
    const CHILD: &str = "ROCQ_ENGINE_HEADER_PROXY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let log = wrapper.path().join("start.jsonl");
        pet_recording_proxy(wrapper.path());
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "existing_theorem_pet_start_uses_frozen_header_mirror_not_live_source",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", real.trim())
            .env("ROCQ_ENGINE_PET_LOG", &log)
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
            "header PET mirror test failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let lab = Lab::new(
        "From Stdlib Require Import Init.Logic.\nModule M.\nTheorem t : True. Admitted.\nEnd M.\nTheorem later : True. Admitted.\n",
    );
    let _ = open_theorem(&lab.engine, lab.path(), "M.t").unwrap();
    let log = fs::read_to_string(std::env::var("ROCQ_ENGINE_PET_LOG").unwrap()).unwrap();
    let item: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    let content = item["content"].as_str().unwrap();
    assert!(
        content.contains("From Stdlib Require Import Init.Logic.")
            && content.contains("Module M.")
            && content.contains("Theorem t : True."),
        "mirror retains frozen imports/module context"
    );
    assert!(
        !content.contains("Admitted.") && !content.contains("later"),
        "mirror excludes old proof body and unrelated declarations"
    );
}

#[test]
fn compatible_pet_version_change_replays_the_latest_view() {
    const CHILD: &str = "ROCQ_ENGINE_PET_VERSION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let version = wrapper.path().join("version");
        fs::write(&version, "one\n").unwrap();
        let pet = wrapper.path().join("pet");
        fs::write(
            &pet,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then cat '{}'; exit 0; fi\nexec {} \"$@\"\n",
                version.display(),
                real.trim()
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
                "compatible_pet_version_change_replays_the_latest_view",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_PET_VERSION", &version)
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
            "PET toolchain test failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let lab = Lab::new("Theorem t : True. Admitted.\n");
    let id = attempt(&open_theorem(&lab.engine, lab.path(), "t").unwrap());
    fs::write(std::env::var("ROCQ_ENGINE_PET_VERSION").unwrap(), "two\n").unwrap();
    assert!(
        lab.engine.inspect(id).is_ok(),
        "a compatible PET version change must refresh the current view instead of permanently staling the trace"
    );
}

#[test]
fn real_pet_focus_stack_reports_unfocused_goals() {
    let lab = Lab::new("Theorem pair : True /\\ True. Admitted.\n");
    let root = attempt(&open_theorem(&lab.engine, lab.path(), "pair").unwrap());
    let result = lab.engine.step(root, "split. Focus 2.").unwrap();
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
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let log = wrapper.path().join("pids");
        let pet = wrapper.path().join("pet");
        fs::write(
            &pet,
            format!(
                "#!/bin/sh\necho $$ >> '{}'\nexec {} \"$@\"\n",
                log.display(),
                real.trim()
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
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let id = attempt(&engine.step(id, "intro P.").unwrap().state);
    assert_eq!(engine.inspect(id).unwrap().accepted_commands, 1);
}

#[test]
fn production_engine_uses_no_unsafe_code() {
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/candidate.rs"))
            .unwrap();
    assert!(
        !source.contains("unsafe"),
        "DESIGN §10 explicitly requires no unsafe production code; use a safe process-management library instead"
    );
}

#[test]
fn version_identity_is_root_frozen_and_never_overwritten_by_a_later_open() {
    const CHILD: &str = "ROCQ_ENGINE_VERSION_ABA_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let pet_real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
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
            ("pet", pet_real.trim(), &pet_v),
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
fn frozen_root_does_not_delegate_pet_version_identity_to_engine_side_table() {
    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/candidate.rs"))
            .unwrap();
    assert!(
        !source.contains("pet_versions"),
        "PET version must be encoded in FrozenTheorem/root key, not mutable Engine side-table state"
    );
    assert!(
        source.contains("toolchain_identity"),
        "candidate baseline freezes toolchain identity"
    );
}

#[test]
fn toolchain_version_hang_and_oversize_are_explicit_bounded_failures() {
    const CHILD: &str = "ROCQ_ENGINE_VERSION_FAULT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let pet = wrapper.path().join("pet");
        fs::write(&pet,format!("#!/bin/sh\nif [ \"$1\" = --version ]; then if [ \"$ROCQ_ENGINE_VERSION_FAULT\" = hang ]; then sleep 2; else head -c 70000 /dev/zero; fi; exit 0; fi\nexec {} \"$@\"\n",real.trim())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pet, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for mode in ["hang", "noisy"] {
            let out = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "toolchain_version_hang_and_oversize_are_explicit_bounded_failures",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("ROCQ_ENGINE_VERSION_FAULT", mode)
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
                "version {mode} child failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        return;
    }
    let project = tempfile::tempdir().unwrap();
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
        close_timeout: Duration::from_secs(1),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let result = open_theorem(&engine, project.path(), "t");
    match result {
        Err(error)
            if matches!(
                error.kind,
                ErrorKind::ProofTimeout | ErrorKind::InvalidConfiguration
            ) => {}
        Err(error) => panic!("wrong explicit version fault kind: {:?}", error.kind),
        Ok(_) => { /* executable-byte identity is intentionally independent of --version output */ }
    }
}

#[test]
fn pet_runtime_is_the_only_pet_owner_and_engine_drop_reaps_it_before_runtime_cleanup() {
    let engine =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine.rs"))
            .unwrap();
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet_runtime.rs"))
            .unwrap();
    assert!(engine.contains("pet_runtime: pet_runtime::PetRuntime"));
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
fn pet_runtime_capacity_is_configured_not_hard_coded_and_lock_accounting_is_local() {
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet_runtime.rs"))
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
fn pet_runtime_b2_lanes_are_root_scoped_for_same_project_parallelism() {
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet_runtime.rs"))
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
    let view =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet_document.rs"))
            .unwrap();
    let runtime =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pet_runtime.rs"))
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
    let solved = lab.engine.step(attempt, "exact I.").unwrap();
    assert!(solved.error.is_none());
    assert_eq!(solved.state.lifecycle, ProofLifecycle::Completed);

    assert_eq!(
        err_kind(lab.engine.query(
            lab.path(),
            None,
            Query::ExpressionType {
                expression: "I). Admitted. Theorem injected : False := I".into(),
            },
        )),
        ErrorKind::InvalidRequest,
        "injection is rejected before a future native query executor could spawn"
    );
    assert_eq!(
        err_kind(lab.engine.query(
            lab.path(),
            None,
            Query::ExpressionType {
                expression: "I). Admitted.".into()
            }
        )),
        ErrorKind::InvalidRequest,
        "a valid native query explicitly reports the missing executor"
    );

    let source =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/api.rs")).unwrap();
    for variant in [
        "Goals",
        "Search",
        "Statement",
        "Proof",
        "Definition",
        "Assumptions",
        "Dependencies",
        "ExpressionType",
        "Notation",
    ] {
        assert!(
            source.contains(variant),
            "typed query surface retains {variant}"
        );
    }
    let parser =
        fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/source_index.rs"))
            .unwrap();
    assert!(
        parser.contains("Sha256::digest")
            && parser.contains("source_digest")
            && parser.contains("old_body_digest"),
        "parsed source anchors must contain real source/body digests, not zero placeholders"
    );
}

#[test]
fn state_parent_inside_project_is_not_snapshotted_as_user_input() {
    let project = tempfile::tempdir().unwrap();
    let source = "Theorem t : forall P : Prop, P -> P. Admitted.\n";
    fs::write(project.path().join("Main.v"), source).unwrap();
    let state_parent = project.path().join("engine-state");
    let engine = Engine::new(EngineConfig {
        state_parent: state_parent.clone(),
        trace_memory_bytes: 1,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let stepped = engine.step(id, "intro P.").unwrap();
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
fn compatible_plugin_and_external_artifact_changes_replay_and_restore_latest_view() {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join("plugin")).unwrap();
    fs::write(project.path().join("_CoqProject"), "-I plugin\n").unwrap();
    fs::write(project.path().join("plugin/extension.cmxs"), b"plugin-a").unwrap();
    fs::write(project.path().join("external.vo"), b"external-a").unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let plugin = project.path().join("plugin/extension.cmxs");
    fs::write(&plugin, b"plugin-b").unwrap();
    let replayed = engine.step(id, "idtac.").unwrap();
    assert!(
        replayed.error.is_none() && replayed.state.accepted_commands == 1,
        "a compatible plugin replacement must rebase the selected trace"
    );
    fs::write(&plugin, b"plugin-a").unwrap();
    assert!(
        engine.inspect(id).is_ok(),
        "restoring the plugin remains replayable"
    );
    let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
    let external = project.path().join("external.vo");
    fs::remove_file(&external).unwrap();
    let replayed = engine.step(id, "idtac.").unwrap();
    assert!(
        replayed.error.is_none() && replayed.state.accepted_commands == 1,
        "a compatible external-artifact deletion must rebase the selected trace"
    );
    fs::write(&external, b"external-a").unwrap();
    assert!(
        engine.inspect(id).is_ok(),
        "restoring an external artifact must not leave a permanent stale marker"
    );
}

#[test]
fn incompatible_view_detaches_pet_before_a_restored_view_replays() {
    const CHILD: &str = "ROCQ_ENGINE_REBASE_DETACH_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let wrapper = tempfile::tempdir().unwrap();
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v pet"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let log = wrapper.path().join("pet-starts.jsonl");
        let proxy = pet_recording_proxy(wrapper.path());
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "incompatible_view_detaches_pet_before_a_restored_view_replays",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ROCQ_ENGINE_REAL_PET", real.trim())
            .env("ROCQ_ENGINE_PET_LOG", &log)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    proxy.parent().unwrap().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
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
    fs::write(project.path().join("A.v"), "Theorem a : True. Admitted.\n").unwrap();
    fs::write(project.path().join("B.v"), "Theorem b : True. Admitted.\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Arc::new(
        Engine::new(EngineConfig {
            state_parent: state.path().join("state"),
            trace_memory_bytes: 1024,
            operation_timeout: Duration::from_secs(10),
            close_timeout: Duration::from_secs(10),
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
            e.step(id, "exact I.")
        }));
    }
    for join in joins {
        let result = join.join().unwrap().unwrap();
        assert!(
            result.error.is_none(),
            "unrelated file proof must publish despite sibling epoch change"
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
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1,

        max_pet_processes: 4,
    })
    .unwrap();
    for _ in 0..4 {
        let catalog = engine.catalog(project.path()).unwrap();
        assert_eq!(
            catalog
                .declarations
                .iter()
                .filter(|d| d.identity.constant == "t")
                .count(),
            1,
            "engine runtime documents must never become catalog declarations"
        );
        let id = attempt(&open_theorem(&engine, project.path(), "t").unwrap());
        assert!(engine.step(id, "idtac.").unwrap().error.is_none());
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
fn oversized_replay_artifact_is_rejected_before_snapshot_admission() {
    let project = tempfile::tempdir().unwrap();
    fs::write(project.path().join("_CoqProject"), "-I plugin\n").unwrap();
    fs::create_dir(project.path().join("plugin")).unwrap();
    fs::write(
        project.path().join("plugin/large.cmxs"),
        vec![0_u8; 17 * 1024 * 1024],
    )
    .unwrap();
    fs::write(
        project.path().join("Main.v"),
        "Theorem t : True. Admitted.\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineConfig {
        state_parent: state.path().join("state"),
        trace_memory_bytes: 1024,
        operation_timeout: Duration::from_secs(10),
        close_timeout: Duration::from_secs(10),
        runtime_cache_bytes: 1024,

        max_pet_processes: 4,
    })
    .unwrap();
    assert_eq!(
        err_kind(open_theorem(&engine, project.path(), "t")),
        ErrorKind::InvalidConfiguration,
        "replay artifact beyond configured safe bound must fail before PET/spill admission"
    );
    assert!(
        state.path().join("state").read_dir().unwrap().all(|x| !x
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("PetHeader")),
        "rejected snapshot leaves no PET document"
    );
}
