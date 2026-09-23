//! Public-API adversarial tests for the generic prefix-forest contract.

use std::{
    collections::HashSet,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use tempfile::TempDir;
use trace_forest::{
    ActionKey, CallError, CloseOutcome, Config, CursorId, Error, RootKey, TraceForest,
};

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Root(String);

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct Action(String);

/// Deliberately fails the deterministic spill codec to exercise flight cleanup.
#[derive(Debug, serde::Deserialize)]
struct UnencodableAction;

impl serde::Serialize for UnencodableAction {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(serde::ser::Error::custom("intentional test codec failure"))
    }
}

type Forest = TraceForest<Root, Action>;

fn key(text: &str) -> RootKey {
    RootKey::new(text).unwrap()
}

fn action_key(text: &str) -> ActionKey {
    ActionKey::new(text).unwrap()
}

fn forest(parent: &TempDir, watermark: usize) -> Forest {
    Forest::new(Config::new(watermark, parent.path())).unwrap()
}

fn open(forest: &Forest, name: &str) -> CursorId {
    forest
        .open(key(name), || Ok::<_, &'static str>(Root("theorem".into())))
        .unwrap()
}

fn step(forest: &Forest, parent: CursorId, name: &str) -> CursorId {
    forest
        .step(parent, action_key(name), || {
            Ok::<_, &'static str>(Action(name.to_owned()))
        })
        .unwrap()
}

#[test]
fn prefix_ordering_and_branching_are_immutable() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, usize::MAX);
    let root = open(&forest, "r");
    let a = step(&forest, root, "a");
    let ab = step(&forest, a, "b");
    let c = step(&forest, root, "c");

    let (root_value, actions) = forest.inspect(ab).unwrap().into_parts();
    assert_eq!(root_value, Root("theorem".into()));
    assert_eq!(actions, vec![Action("a".into()), Action("b".into())]);
    assert_eq!(forest.inspect(c).unwrap().actions(), &[Action("c".into())]);
    assert_eq!(forest.inspect(a).unwrap().actions(), &[Action("a".into())]);
}

#[test]
fn canonical_keys_are_idempotent_and_suppress_following_callbacks() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, usize::MAX);
    let calls = AtomicUsize::new(0);
    let root = forest
        .open(key("r"), || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, &'static str>(Root("theorem".into()))
        })
        .unwrap();
    let duplicate_root = forest
        .open(key("r"), || -> Result<Root, &'static str> {
            panic!("a same-key root callback must not run")
        })
        .unwrap();
    assert_eq!(root, duplicate_root);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let child = forest
        .step(root, action_key("a"), || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, &'static str>(Action("first".into()))
        })
        .unwrap();
    let duplicate_child = forest
        .step(root, action_key("a"), || -> Result<Action, &'static str> {
            panic!("a same-edge callback must not run")
        })
        .unwrap();
    assert_eq!(child, duplicate_child);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn concurrent_same_edge_has_one_preparation_and_one_cursor() {
    let directory = TempDir::new().unwrap();
    let forest = Arc::new(forest(&directory, usize::MAX));
    let root = open(&forest, "r");
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let leader_forest = Arc::clone(&forest);
    let leader_calls = Arc::clone(&calls);
    let leader = thread::spawn(move || {
        leader_forest.step(root, action_key("a"), || {
            leader_calls.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, &'static str>(Action("a".into()))
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let follower_forest = Arc::clone(&forest);
    let follower = thread::spawn(move || {
        follower_forest.step(root, action_key("a"), || -> Result<Action, &'static str> {
            panic!("follower callback must be single-flighted away")
        })
    });
    // Give the follower a scheduling window to join the published flight. The
    // leader is still blocked, so a global lock implementation would deadlock.
    thread::sleep(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    assert_eq!(
        leader.join().unwrap().unwrap(),
        follower.join().unwrap().unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_and_panicking_preparation_do_not_poison_a_retry() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, usize::MAX);
    assert!(matches!(
        forest.open(key("r"), || Err::<Root, _>("no")),
        Err(CallError::Callback("no"))
    ));
    let root = open(&forest, "r");

    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            let _ = forest.step(
                root,
                action_key("panic"),
                || -> Result<Action, &'static str> { panic!("application panic") },
            );
        }))
        .is_err()
    );
    let child = step(&forest, root, "panic");
    assert_eq!(
        forest.inspect(child).unwrap().actions(),
        &[Action("panic".into())]
    );
}

#[test]
fn unrelated_roots_progress_while_callback_or_close_effect_blocks() {
    let directory = TempDir::new().unwrap();
    let forest = Arc::new(forest(&directory, usize::MAX));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocked_forest = Arc::clone(&forest);
    let blocked = thread::spawn(move || {
        blocked_forest.open(key("blocked"), || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, &'static str>(Root("blocked".into()))
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let other = Arc::clone(&forest);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || done_tx.send(open(&other, "other")).unwrap());
    let other_root = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("unrelated open serialized behind a callback");
    release_tx.send(()).unwrap();
    blocked.join().unwrap().unwrap();

    let (close_entered_tx, close_entered_rx) = mpsc::channel();
    let (close_release_tx, close_release_rx) = mpsc::channel();
    let closing_forest = Arc::clone(&forest);
    let closer = thread::spawn(move || {
        closing_forest.close(other_root, |_| {
            close_entered_tx.send(()).unwrap();
            close_release_rx.recv().unwrap();
            Ok::<_, &'static str>(())
        })
    });
    close_entered_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let unrelated = Arc::clone(&forest);
    let (step_done_tx, step_done_rx) = mpsc::channel();
    thread::spawn(move || {
        step_done_tx
            .send(step(&unrelated, open(&unrelated, "third"), "x"))
            .unwrap()
    });
    step_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("close effect serialized unrelated root");
    close_release_tx.send(()).unwrap();
    assert_eq!(closer.join().unwrap().unwrap(), CloseOutcome::Closed);
}

#[test]
fn close_blocks_steps_retires_every_branch_and_runs_one_effect() {
    let directory = TempDir::new().unwrap();
    let forest = Arc::new(forest(&directory, usize::MAX));
    let root = open(&forest, "r");
    let branch = step(&forest, root, "old");
    let effects = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let leader_forest = Arc::clone(&forest);
    let leader_effects = Arc::clone(&effects);
    let leader = thread::spawn(move || {
        leader_forest.close(root, |_| {
            leader_effects.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, &'static str>(())
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        forest.step(root, action_key("late"), || Ok::<_, &'static str>(Action(
            "late".into()
        ))),
        Err(CallError::Forest(Error::Closing))
    ));
    let waiter_forest = Arc::clone(&forest);
    let waiter_effects = Arc::clone(&effects);
    let waiter = thread::spawn(move || {
        waiter_forest.close(branch, |_| -> Result<(), &'static str> {
            waiter_effects.fetch_add(1, Ordering::SeqCst);
            panic!("waiter effect must not run after a successful leader")
        })
    });
    thread::sleep(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    assert_eq!(leader.join().unwrap().unwrap(), CloseOutcome::Closed);
    assert_eq!(
        waiter.join().unwrap().unwrap(),
        CloseOutcome::AlreadyRetired
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert!(matches!(forest.inspect(root), Err(Error::UnknownCursor)));
    assert!(matches!(forest.inspect(branch), Err(Error::UnknownCursor)));
    let reopened = forest
        .open(key("r"), || Ok::<_, &'static str>(Root("again".into())))
        .unwrap();
    assert_ne!(reopened, root);
    assert_eq!(
        forest.inspect(reopened).unwrap().root(),
        &Root("again".into())
    );
}

#[test]
fn failed_and_panicking_close_reopen_the_family() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, usize::MAX);
    let root = open(&forest, "r");
    assert!(matches!(
        forest.close(root, |_| Err::<(), _>("no")),
        Err(CallError::Callback("no"))
    ));
    let branch = step(&forest, root, "after-failure");
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            let _ = forest.close(branch, |_| -> Result<(), &'static str> {
                panic!("effect panic")
            });
        }))
        .is_err()
    );
    let after_panic = step(&forest, branch, "after-panic");
    assert_eq!(forest.inspect(after_panic).unwrap().actions().len(), 2);
    assert!(matches!(
        forest.close(after_panic, |_| Ok::<_, &'static str>(())),
        Ok(CloseOutcome::Closed)
    ));
}

fn owned_spill_directory(parent: &TempDir) -> std::path::PathBuf {
    fs::read_dir(parent.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("trace-forest-")
        })
        .expect("forest must create a unique owned spill child")
}

#[test]
fn low_watermark_spills_and_cold_inspect_is_transparent() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, 1);
    let root = open(&forest, "r");
    let child = step(&forest, root, "a");
    let spill = owned_spill_directory(&directory);
    assert!(
        fs::read_dir(&spill).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "bin")),
        "low watermark did not create a spill segment"
    );
    let view = forest.inspect(child).unwrap();
    assert_eq!(view.root(), &Root("theorem".into()));
    assert_eq!(view.actions(), &[Action("a".into())]);
}

#[test]
fn framed_spill_rejects_in_place_corruption() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, 0);
    let root = open(&forest, "r");
    let spill = owned_spill_directory(&directory);
    let segment = fs::read_dir(spill)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .unwrap();
    let mut bytes = fs::read(&segment).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    fs::write(segment, bytes).unwrap();
    assert!(matches!(forest.inspect(root), Err(Error::CorruptSpill)));
}

#[test]
fn oversized_payload_spills_and_drop_removes_only_owned_directory() {
    let directory = TempDir::new().unwrap();
    let sentinel = directory.path().join("caller-owned");
    fs::create_dir(&sentinel).unwrap();
    fs::write(sentinel.join("keep"), "keep").unwrap();
    let spill;
    {
        let forest = forest(&directory, 1);
        let root = forest
            .open(key("huge"), || {
                Ok::<_, &'static str>(Root("a root much larger than one byte".into()))
            })
            .unwrap();
        spill = owned_spill_directory(&directory);
        assert!(
            fs::read_dir(&spill).unwrap().next().is_some(),
            "oversized payload was retained without spill"
        );
        assert_eq!(
            forest.inspect(root).unwrap().root(),
            &Root("a root much larger than one byte".into())
        );
    }
    assert!(
        !spill.exists(),
        "drop must remove its unique child directory"
    );
    assert_eq!(fs::read_to_string(sentinel.join("keep")).unwrap(), "keep");
}

#[test]
fn high_contention_topology_matches_the_reference_edge_set() {
    let directory = TempDir::new().unwrap();
    let forest = Arc::new(forest(&directory, usize::MAX));
    let root = open(&forest, "r");
    let callbacks = Arc::new(AtomicUsize::new(0));
    let workers = 12;
    let first_width = 24;
    let barrier = Arc::new(Barrier::new(workers));
    let first_results = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut joins = Vec::new();
    for _ in 0..workers {
        let forest = Arc::clone(&forest);
        let callbacks = Arc::clone(&callbacks);
        let barrier = Arc::clone(&barrier);
        let results = Arc::clone(&first_results);
        joins.push(thread::spawn(move || {
            barrier.wait();
            for index in 0..first_width {
                let name = format!("p{index}");
                let cursor = forest
                    .step(root, action_key(&name), || {
                        callbacks.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, &'static str>(Action(name.clone()))
                    })
                    .unwrap();
                results.lock().unwrap().push((index, cursor));
            }
        }));
    }
    for join in joins {
        join.join().unwrap();
    }
    let first: Vec<_> = first_results.lock().unwrap().clone();
    let unique_first: HashSet<_> = first.iter().map(|(_, cursor)| *cursor).collect();
    assert_eq!(unique_first.len(), first_width);

    let second_width = 12;
    let barrier = Arc::new(Barrier::new(workers));
    let second_results = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut joins = Vec::new();
    for _ in 0..workers {
        let forest = Arc::clone(&forest);
        let callbacks = Arc::clone(&callbacks);
        let barrier = Arc::clone(&barrier);
        let results = Arc::clone(&second_results);
        let parents: Vec<_> = unique_first.iter().copied().collect();
        joins.push(thread::spawn(move || {
            barrier.wait();
            for (parent_index, parent) in parents.into_iter().enumerate() {
                for child_index in 0..second_width {
                    let name = format!("q{child_index}");
                    let cursor = forest
                        .step(parent, action_key(&name), || {
                            callbacks.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, &'static str>(Action(name.clone()))
                        })
                        .unwrap();
                    results
                        .lock()
                        .unwrap()
                        .push((parent_index, child_index, cursor));
                }
            }
        }));
    }
    for join in joins {
        join.join().unwrap();
    }
    let second = second_results.lock().unwrap();
    let reference: HashSet<_> = second
        .iter()
        .map(|(parent, child, _)| (*parent, *child))
        .collect();
    let actual: HashSet<_> = second.iter().map(|(_, _, cursor)| *cursor).collect();
    assert_eq!(reference.len(), first_width * second_width);
    assert_eq!(actual.len(), first_width * second_width);
    assert_eq!(
        callbacks.load(Ordering::SeqCst),
        first_width + first_width * second_width,
        "each reference edge must prepare exactly once"
    );
    for (parent_index, child_index, cursor) in second.iter().take(64) {
        let view = forest.inspect(*cursor).unwrap();
        assert_eq!(view.actions().len(), 2);
        assert_eq!(view.actions()[1], Action(format!("q{child_index}")));
        assert!(view.actions()[0].0.starts_with('p'));
        assert!(*parent_index < first_width);
    }
}

#[test]
fn spill_batches_many_payloads_in_sublinear_number_of_segments() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, 1);
    let root = open(&forest, "r");
    let payloads = 40;
    for index in 0..payloads {
        step(&forest, root, &format!("a{index}"));
    }
    let spill = owned_spill_directory(&directory);
    let segment_count = fs::read_dir(spill)
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "bin")
        })
        .count();
    assert!(
        segment_count < payloads / 2,
        "{segment_count} individual segment files for {payloads} payloads is not grouped spilling"
    );
}

#[cfg(unix)]
#[test]
fn spill_admission_failure_does_not_publish_root_or_edge() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, 0);
    let spill = owned_spill_directory(&directory);
    fs::set_permissions(&spill, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(matches!(
        forest.open(key("r"), || Ok::<_, &'static str>(Root("theorem".into()))),
        Err(CallError::Forest(Error::SpillUnavailable))
    ));
    fs::set_permissions(&spill, fs::Permissions::from_mode(0o700)).unwrap();
    let root = open(&forest, "r");

    fs::set_permissions(&spill, fs::Permissions::from_mode(0o500)).unwrap();
    let existing_segments: Vec<_> = fs::read_dir(&spill)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    for segment in &existing_segments {
        fs::set_permissions(segment, fs::Permissions::from_mode(0o400)).unwrap();
    }
    assert!(matches!(
        forest.step(root, action_key("a"), || Ok::<_, &'static str>(Action(
            "a".into()
        ))),
        Err(CallError::Forest(Error::SpillUnavailable))
    ));
    for segment in &existing_segments {
        fs::set_permissions(segment, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::set_permissions(&spill, fs::Permissions::from_mode(0o700)).unwrap();
    let child = step(&forest, root, "a");
    assert_eq!(
        forest.inspect(child).unwrap().actions(),
        &[Action("a".into())]
    );
}

#[test]
fn concurrent_same_root_has_one_preparation_and_one_cursor() {
    let directory = TempDir::new().unwrap();
    let forest = Arc::new(forest(&directory, usize::MAX));
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let leader_forest = Arc::clone(&forest);
    let leader_calls = Arc::clone(&calls);
    let leader = thread::spawn(move || {
        leader_forest.open(key("r"), || {
            leader_calls.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, &'static str>(Root("theorem".into()))
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let follower_forest = Arc::clone(&forest);
    let follower = thread::spawn(move || {
        follower_forest.open(key("r"), || -> Result<Root, &'static str> {
            panic!("follower root callback must not run")
        })
    });
    thread::sleep(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    assert_eq!(
        leader.join().unwrap().unwrap(),
        follower.join().unwrap().unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_close_leader_allows_waiter_to_acquire_and_retry_effect() {
    let directory = TempDir::new().unwrap();
    let forest = Arc::new(forest(&directory, usize::MAX));
    let root = open(&forest, "r");
    let effects = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let leader_forest = Arc::clone(&forest);
    let leader_effects = Arc::clone(&effects);
    let leader = thread::spawn(move || {
        leader_forest.close(root, |_| {
            leader_effects.fetch_add(1, Ordering::SeqCst);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Err::<(), &'static str>("failed leader")
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let waiter_forest = Arc::clone(&forest);
    let waiter_effects = Arc::clone(&effects);
    let waiter = thread::spawn(move || {
        waiter_forest.close(root, |_| {
            waiter_effects.fetch_add(1, Ordering::SeqCst);
            Ok::<_, &'static str>(())
        })
    });
    thread::sleep(Duration::from_millis(50));
    release_tx.send(()).unwrap();
    assert!(matches!(
        leader.join().unwrap(),
        Err(CallError::Callback("failed leader"))
    ));
    assert_eq!(waiter.join().unwrap().unwrap(), CloseOutcome::Closed);
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    assert!(matches!(forest.inspect(root), Err(Error::UnknownCursor)));
}

#[test]
fn codec_failure_clears_the_edge_flight_for_later_retry() {
    let directory = TempDir::new().unwrap();
    let forest =
        TraceForest::<Root, UnencodableAction>::new(Config::new(usize::MAX, directory.path()))
            .unwrap();
    let root = forest
        .open(key("r"), || Ok::<_, &'static str>(Root("theorem".into())))
        .unwrap();
    let calls = AtomicUsize::new(0);
    for _ in 0..2 {
        assert!(matches!(
            forest.step(root, action_key("bad"), || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &'static str>(UnencodableAction)
            }),
            Err(CallError::Forest(Error::PayloadCodec))
        ));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "codec failure must not leave a stuck or published edge flight"
    );
}

#[test]
fn retirement_releases_a_family_before_admitting_new_families() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, 32);
    for index in 0..50 {
        let name = format!("r{index}");
        let root = forest
            .open(key(&name), || {
                Ok::<_, &'static str>(Root(format!("payload-{index:04}")))
            })
            .unwrap();
        let child = step(&forest, root, "a");
        assert!(matches!(
            forest.close(child, |_| Ok::<_, &'static str>(())),
            Ok(CloseOutcome::Closed)
        ));
    }
    let final_root = forest
        .open(key("final"), || {
            Ok::<_, &'static str>(Root("final payload".into()))
        })
        .unwrap();
    assert_eq!(
        forest.inspect(final_root).unwrap().root(),
        &Root("final payload".into())
    );
}

#[test]
fn generic_errors_do_not_leak_spill_paths_or_payloads() {
    let directory = TempDir::new().unwrap();
    let non_directory = directory.path().join("not-a-directory");
    fs::write(&non_directory, "private payload and path marker").unwrap();
    let error = match TraceForest::<Root, Action>::new(Config::new(1, &non_directory)) {
        Err(error) => error,
        Ok(_) => panic!("a file cannot be a spill parent"),
    };
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(&non_directory.display().to_string()));
    assert!(!diagnostic.contains("private payload and path marker"));
    assert!(matches!(RootKey::new([]), Err(Error::InvalidKey)));
    assert!(matches!(ActionKey::new([]), Err(Error::InvalidKey)));
    assert!(RootKey::new(vec![0; 4096]).is_ok());
    assert!(ActionKey::new(vec![0; 4096]).is_ok());
    assert!(matches!(
        RootKey::new(vec![0; 4097]),
        Err(Error::InvalidKey)
    ));
    assert!(matches!(
        ActionKey::new(vec![0; 4097]),
        Err(Error::InvalidKey)
    ));
}

#[test]
fn completed_families_leave_no_public_key_or_cursor_blacklist() {
    let directory = TempDir::new().unwrap();
    let forest = forest(&directory, 8);
    let root_key = key("reusable-root");
    let mut old_cursors = Vec::new();
    for generation in 0..100 {
        let root = forest
            .open(root_key.clone(), || {
                Ok::<_, &'static str>(Root(format!("generation-{generation}")))
            })
            .unwrap();
        let leaf = step(&forest, root, "a");
        assert!(matches!(
            forest.close(leaf, |_| Ok::<_, &'static str>(())),
            Ok(CloseOutcome::Closed)
        ));
        old_cursors.extend([root, leaf]);
    }
    for cursor in old_cursors {
        assert!(matches!(forest.inspect(cursor), Err(Error::UnknownCursor)));
    }
    let new_root = forest
        .open(root_key, || Ok::<_, &'static str>(Root("fresh".into())))
        .unwrap();
    assert_eq!(
        forest.inspect(new_root).unwrap().root(),
        &Root("fresh".into())
    );
}

#[test]
fn close_winning_during_step_preparation_cannot_publish_a_late_edge() {
    let directory = TempDir::new().unwrap();
    // Keep these payloads resident so a close winner also exercises admission
    // rollback rather than merely discarding an already-spilled pointer.
    let forest = Arc::new(forest(&directory, 4096));
    let root = open(&forest, "r");
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let step_forest = Arc::clone(&forest);
    let in_flight_step = thread::spawn(move || {
        step_forest.step(root, action_key("late"), || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, &'static str>(Action("late".into()))
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        forest.close(root, |_| Ok::<_, &'static str>(())),
        Ok(CloseOutcome::Closed)
    ));
    release_tx.send(()).unwrap();
    assert!(matches!(
        in_flight_step.join().unwrap(),
        Err(CallError::Forest(Error::Retired | Error::UnknownCursor))
    ));
    assert!(matches!(forest.inspect(root), Err(Error::UnknownCursor)));
    let reopened = forest
        .open(key("r"), || Ok::<_, &'static str>(Root("reopened".into())))
        .unwrap();
    assert_eq!(forest.inspect(reopened).unwrap().actions(), &[]);
    assert_eq!(
        forest.inspect(reopened).unwrap().root(),
        &Root("reopened".into())
    );
}
