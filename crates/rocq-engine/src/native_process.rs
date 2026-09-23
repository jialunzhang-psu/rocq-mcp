//! Bounded native subprocess execution for build and trust transactions.

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 1024 * 1024;

pub(crate) struct NativeOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) timed_out: bool,
    pub(crate) overflow: bool,
}

/// Runs one process in its own process group, bounds both output streams, and
/// kills/reaps the complete group at the deadline. Spawn/read/wait failures are
/// returned without exposing physical paths.
pub(crate) fn run(
    program: &str,
    args: impl IntoIterator<Item = OsString>,
    cwd: &Path,
    timeout: Duration,
) -> std::io::Result<NativeOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = Pid::from_raw(child.id() as i32);
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = killpg(pid, Signal::SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("native stdout pipe missing"));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = killpg(pid, Signal::SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("native stderr pipe missing"));
        }
    };
    let stdout = thread::spawn(move || bounded_read(stdout));
    let stderr = thread::spawn(move || bounded_read(stderr));
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (status, false);
        }
        if Instant::now() >= deadline {
            let _ = killpg(pid, Signal::SIGKILL);
            break (child.wait()?, true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    // The group may still contain descendants holding inherited pipe ends even
    // after the direct child exits. They are disposable transaction children.
    let _ = killpg(pid, Signal::SIGKILL);
    let (stdout, stdout_overflow) = stdout.join().unwrap_or_else(|_| (Vec::new(), true));
    let (stderr, stderr_overflow) = stderr.join().unwrap_or_else(|_| (Vec::new(), true));
    Ok(NativeOutput {
        status,
        stdout,
        stderr,
        timed_out,
        overflow: stdout_overflow || stderr_overflow,
    })
}

fn bounded_read(mut input: impl Read) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let overflow = input
        .by_ref()
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > OUTPUT_LIMIT;
    bytes.truncate(OUTPUT_LIMIT);
    // Continue draining so a child cannot block before the coordinator kills it.
    let _ = std::io::copy(&mut input, &mut std::io::sink());
    (bytes, overflow)
}
