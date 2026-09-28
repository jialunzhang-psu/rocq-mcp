//! Bounded Dune subprocess execution.
//!
//! This module owns process groups, output bounds, and optional native
//! deadlines. Dune discovery remains in the parent adapter.

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

// Bounded Dune subprocess execution.
const DUNE_OUTPUT_LIMIT: usize = 64 * 1024 * 1024;

/// One subprocess result. `timed_out` reports an explicit deadline; `overflow`
/// means output was truncated without changing process exit semantics.
pub(crate) struct NativeOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) timed_out: bool,
    pub(crate) overflow: bool,
}

/// Runs one Dune command with bounded output and an optional total deadline.
/// `build_dir` overrides Dune's configured directory for isolated metadata or
/// builds; `None` retains Dune's native build/lock behavior. A Dune failure is
/// never reclassified by comparing it with a different build directory.
/// Commands using that real build directory are serialized per canonical
/// workspace so concurrent cold starts cannot corrupt Dune's lock bootstrap;
/// isolated `build_dir` calls remain independent.
///
/// A command rejected normally by Dune and an explicit deadline are final.
/// Abnormal process death or a broken child pipe gets one fresh-process retry
/// inside the original total deadline. This is the sole Dune recovery policy;
/// callers must not add command-specific retry loops.
pub(crate) fn run_dune(
    args: impl IntoIterator<Item = OsString>,
    cwd: &Path,
    timeout: Option<Duration>,
    build_dir: Option<&Path>,
) -> std::io::Result<NativeOutput> {
    // Dune serializes established build directories itself, but two cold
    // clients can race while creating its global lock file and one then sees
    // an empty/corrupt lock. Keep commands using the real build directory
    // single-file per workspace; isolated metadata directories do not share
    // that state and remain concurrent.
    let workspace_gate = build_dir.is_none().then(|| dune_workspace_gate(cwd));
    let _workspace_guard = workspace_gate.as_ref().map(|gate| {
        gate.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    let mut args = args.into_iter().collect::<Vec<_>>();
    if let Some(directory) = build_dir {
        args.push(OsString::from("--build-dir"));
        args.push(directory.as_os_str().to_owned());
    }
    let deadline = timeout
        .map(|limit| {
            Instant::now().checked_add(limit).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "native deadline overflows",
                )
            })
        })
        .transpose()?;
    let mut retried = false;
    loop {
        match run_dune_once(&args, cwd, deadline) {
            Ok(output)
                if !retried
                    && !output.timed_out
                    && output.status.signal().is_some()
                    && deadline.is_none_or(|limit| Instant::now() < limit) =>
            {
                retried = true;
            }
            Err(AttemptError::Transport(_))
                if !retried && deadline.is_none_or(|limit| Instant::now() < limit) =>
            {
                retried = true;
            }
            Ok(output) => return Ok(output),
            Err(error) => return Err(error.into_io()),
        }
    }
}

/// Return the process-local command gate for one canonical Dune workspace.
/// Weak registry entries avoid retaining every project ever attached to this
/// server; holding the returned `Arc` keeps the selected gate alive.
fn dune_workspace_gate(cwd: &Path) -> Arc<Mutex<()>> {
    static GATES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let key = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned());
    let mut gates = GATES
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|_, gate| gate.strong_count() != 0);
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}

/// Whether one attempt failed before a child existed or lost an already-owned
/// child transport. Only the latter is safe for `run_dune` to replay once.
enum AttemptError {
    Start(std::io::Error),
    Transport(std::io::Error),
}

impl AttemptError {
    fn into_io(self) -> std::io::Error {
        match self {
            Self::Start(error) | Self::Transport(error) => error,
        }
    }
}

/// Execute exactly one Dune child. Process creation errors are final; errors
/// after a child exists are transport failures eligible for `run_dune`'s one
/// retry. The absolute deadline prevents a retry from resetting the budget.
fn run_dune_once(
    args: &[OsString],
    cwd: &Path,
    deadline: Option<Instant>,
) -> Result<NativeOutput, AttemptError> {
    let mut command = Command::new("dune");
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    #[cfg(target_os = "linux")]
    {
        // Do not leave a compiler/build tree behind if the MCP process is
        // terminated abruptly while Dune is running.
        unsafe {
            command.pre_exec(move || {
                let result =
                    nix::libc::prctl(nix::libc::PR_SET_PDEATHSIG, nix::libc::SIGKILL, 0, 0, 0);
                if result == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    let mut child = command.spawn().map_err(AttemptError::Start)?;
    let pid = Pid::from_raw(child.id() as i32);
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_child_group(&mut child, pid);
            return Err(AttemptError::Transport(std::io::Error::other(
                "Dune stdout pipe missing",
            )));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_child_group(&mut child, pid);
            return Err(AttemptError::Transport(std::io::Error::other(
                "Dune stderr pipe missing",
            )));
        }
    };
    let stdout = thread::spawn(move || bounded_read(stdout, DUNE_OUTPUT_LIMIT));
    let stderr = thread::spawn(move || bounded_read(stderr, DUNE_OUTPUT_LIMIT));
    let waited = wait_for_dune(&mut child, pid, deadline);
    let (status, timed_out) = match waited {
        Ok(result) => result,
        Err(error) => {
            terminate_child_group(&mut child, pid);
            let _ = stdout.join();
            let _ = stderr.join();
            return Err(AttemptError::Transport(error));
        }
    };
    // The group may still contain descendants holding inherited pipe ends even
    // after the direct child exits. They are disposable transaction children.
    let _ = killpg(pid, Signal::SIGKILL);
    let (stdout, stdout_overflow) = stdout
        .join()
        .map_err(|_| AttemptError::Transport(std::io::Error::other("Dune stdout reader failed")))?
        .map_err(AttemptError::Transport)?;
    let (stderr, stderr_overflow) = stderr
        .join()
        .map_err(|_| AttemptError::Transport(std::io::Error::other("Dune stderr reader failed")))?
        .map_err(AttemptError::Transport)?;
    Ok(NativeOutput {
        status,
        stdout,
        stderr,
        timed_out,
        overflow: stdout_overflow || stderr_overflow,
    })
}

fn wait_for_dune(
    child: &mut std::process::Child,
    pid: Pid,
    deadline: Option<Instant>,
) -> std::io::Result<(ExitStatus, bool)> {
    let Some(deadline) = deadline else {
        // Design note: waiting directly avoids a polling loop for terminating
        // compiler/build commands when the caller chose no deadline.
        return child.wait().map(|status| (status, false));
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        if Instant::now() >= deadline {
            let _ = killpg(pid, Signal::SIGKILL);
            return child.wait().map(|status| (status, true));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_child_group(child: &mut std::process::Child, pid: Pid) {
    let _ = killpg(pid, Signal::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

fn bounded_read(mut input: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    let overflow = bytes.len() > limit;
    bytes.truncate(limit);
    // Continue draining so a child cannot block before the coordinator kills it.
    std::io::copy(&mut input, &mut std::io::sink())?;
    Ok((bytes, overflow))
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    #[test]
    fn dune_recovery_retries_only_abnormal_transport_failure() {
        const CHILD: &str = "ROCQ_ENGINE_DUNE_RECOVERY_TEST_CHILD";
        const DIRECTORY: &str = "ROCQ_ENGINE_DUNE_RECOVERY_TEST_DIRECTORY";
        if std::env::var_os(CHILD).is_some() {
            let directory = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
            let recovered = run_dune(
                [OsString::from("recover")],
                &directory,
                Some(Duration::from_secs(2)),
                None,
            )
            .unwrap();
            assert!(recovered.status.success());
            assert_eq!(recovered.stdout, b"recovered\n");

            let rejected = run_dune(
                [OsString::from("reject")],
                &directory,
                Some(Duration::from_secs(2)),
                None,
            )
            .unwrap();
            assert_eq!(rejected.status.code(), Some(2));
            assert_eq!(
                fs::read_to_string(directory.join("reject-count")).unwrap(),
                "x\n"
            );

            let timed_out = run_dune(
                [OsString::from("timeout")],
                &directory,
                Some(Duration::from_millis(50)),
                None,
            )
            .unwrap();
            assert!(timed_out.timed_out);
            assert_eq!(
                fs::read_to_string(directory.join("timeout-count")).unwrap(),
                "x\n"
            );
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("dune");
        fs::write(
            &script,
            r#"#!/bin/sh
set -eu
case "$1" in
  recover)
    if [ ! -e recovery-marker ]; then
      : > recovery-marker
      kill -KILL $$
    fi
    echo recovered
    ;;
  reject)
    echo x >> reject-count
    exit 2
    ;;
  timeout)
    echo x >> timeout-count
    sleep 10
    ;;
esac
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        let path = std::env::join_paths(std::iter::once(directory.path().to_owned()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        // Design note: PATH is changed only in an isolated child test process;
        // parallel tests in this harness never observe the fake Dune binary.
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("dune_recovery_retries_only_abnormal_transport_failure")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env(DIRECTORY, directory.path())
            .env("PATH", path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn dune_same_workspace_commands_are_serialized() {
        const CHILD: &str = "ROCQ_ENGINE_DUNE_SERIAL_TEST_CHILD";
        const DIRECTORY: &str = "ROCQ_ENGINE_DUNE_SERIAL_TEST_DIRECTORY";
        if std::env::var_os(CHILD).is_some() {
            let directory = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
            let gate = Arc::new(Barrier::new(3));
            let calls = (0..2)
                .map(|_| {
                    let directory = directory.clone();
                    let gate = Arc::clone(&gate);
                    thread::spawn(move || {
                        gate.wait();
                        run_dune(
                            [OsString::from("probe")],
                            &directory,
                            Some(Duration::from_secs(2)),
                            None,
                        )
                        .unwrap()
                    })
                })
                .collect::<Vec<_>>();
            gate.wait();
            for call in calls {
                assert!(call.join().unwrap().status.success());
            }
            assert!(!directory.join("overlap").exists());
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("dune");
        fs::write(
            &script,
            r#"#!/bin/sh
set -eu
if ! mkdir command-active 2>/dev/null; then
  : > overlap
  exit 9
fi
trap 'rmdir command-active' EXIT
sleep 0.1
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        let path = std::env::join_paths(std::iter::once(directory.path().to_owned()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("dune_same_workspace_commands_are_serialized")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env(DIRECTORY, directory.path())
            .env("PATH", path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn dune_build_contention_obeys_deadline_then_succeeds() {
        let project = tempfile::tempdir().unwrap();
        let signal_dir = tempfile::tempdir().unwrap();
        let fifo = signal_dir.path().join("ready");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            project.path().join("dune-project"),
            "(lang dune 3.22)\n(name lockprobe)\n",
        )
        .unwrap();
        fs::write(
            project.path().join("dune"),
            format!(
                "(rule (target x) (action (bash \"echo ready > {}; sleep 1; touch x\")))\n",
                fifo.display()
            ),
        )
        .unwrap();
        let mut owner = Command::new("dune")
            .args(["build", "x"])
            .current_dir(project.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut signal = String::new();
        fs::File::open(&fifo)
            .unwrap()
            .read_to_string(&mut signal)
            .unwrap();
        assert_eq!(signal.trim(), "ready");
        let short = run_dune(
            [OsString::from("build"), OsString::from("x")],
            project.path(),
            Some(Duration::from_millis(100)),
            None,
        )
        .unwrap();
        assert!(short.timed_out);
        assert!(owner.wait().unwrap().success());
        let long = run_dune(
            [OsString::from("build"), OsString::from("x")],
            project.path(),
            Some(Duration::from_secs(5)),
            None,
        )
        .unwrap();
        assert!(
            long.status.success() && !long.timed_out && !long.overflow,
            "status={:?} timeout={} overflow={} stderr={}",
            long.status,
            long.timed_out,
            long.overflow,
            String::from_utf8_lossy(&long.stderr)
        );
    }

    #[test]
    fn invalid_dune_rule_is_not_retried_as_build_dir_contention() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("dune-project"), "(lang dune 3.22)\n").unwrap();
        fs::write(project.path().join("dune"), "(not-a-dune-stanza)\n").unwrap();
        let output = run_dune(
            [OsString::from("describe"), OsString::from("rules")],
            project.path(),
            Some(Duration::from_secs(2)),
            None,
        )
        .unwrap();
        assert!(!output.status.success());
        assert!(!output.timed_out);
        assert!(!output.stderr.is_empty());
    }
}
