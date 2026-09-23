//! Process-boundary execution of every checked-in declarative trace.
use rocq_e2e::{TraceRunner, open_trace};
#[path = "e2e_traces/fault_assertions.rs"]
mod fault_assertions;
#[path = "e2e_traces/fixture_config.rs"]
mod fixture_config;
#[path = "e2e_traces/fixture_hooks.rs"]
mod fixture_hooks;
use std::{
    fs,
    io::BufRead,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{sync::Semaphore, task::JoinSet};

#[tokio::test]
async fn complete_declarative_trace_corpus() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rocq-e2e");
    let mut cases = trace_cases(&root).unwrap();
    if !cfg!(feature = "fault-injection") {
        cases.retain(|(fixture, _)| fixture != "declare_race" && fixture != "fault");
    }
    cases.sort();
    if let Some(filter) = std::env::var_os("ROCQ_E2E_TRACE_FILTER") {
        let filter = filter.to_string_lossy();
        cases.retain(|(_, path)| path.to_string_lossy().contains(filter.as_ref()));
        assert!(!cases.is_empty(), "trace filter matched no cases");
    } else {
        assert_eq!(
            cases.len(),
            if cfg!(feature = "fault-injection") {
                1_957
            } else {
                1_915
            },
            "trace corpus changed; update the coverage manifest"
        );
        let event_count = cases
            .iter()
            .map(|(_, path)| count_events(path).expect("checked-in trace must be readable"))
            .sum::<usize>();
        assert_eq!(
            event_count,
            if cfg!(feature = "fault-injection") {
                169_255
            } else {
                168_689
            },
            "trace event corpus changed; update the coverage manifest"
        );
    }
    let concurrency = std::env::var("ROCQ_E2E_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        // A trace can spawn a server, PET and native compiler at once. The
        // old 64-way default risked multiplying their peak RSS into host OOM.
        .unwrap_or(4);
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks = JoinSet::new();
    for (fixture, trace) in cases {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let root = root.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let result = run_case(&root, &fixture, &trace).await;
            (trace, result)
        });
    }
    while let Some(joined) = tasks.join_next().await {
        let (trace, result) = joined.expect("trace task panicked");
        result.unwrap_or_else(|error| panic!("{} failed: {error}", trace.display()));
    }
}

async fn run_case(root: &Path, fixture: &str, trace: &Path) -> rocq_e2e::Result<()> {
    let lab = tempfile::tempdir().map_err(|source| rocq_e2e::TraceError::Io {
        operation: "create e2e lab",
        source,
    })?;
    copy_tree(&root.join("fixtures").join(fixture), lab.path()).map_err(|source| {
        rocq_e2e::TraceError::Io {
            operation: "copy e2e fixture",
            source,
        }
    })?;
    let server = PathBuf::from(env!("CARGO_BIN_EXE_rocq-mcp"));
    let config = fixture_config::build_config(fixture, trace, lab.path(), &server)?;
    let mut runner = fixture_hooks::attach_hooks(
        TraceRunner::new(config)?,
        fixture,
        trace,
        lab.path(),
        &server,
    )?;
    if fixture == "fault" {
        runner = fault_assertions::attach(runner, trace, lab.path());
    }
    runner.run_file(trace).await
}

/// Count physical non-empty events without materializing multi-gigabyte
/// boundary traces; replay performs strict streaming parsing afterward.
fn count_events(path: &Path) -> std::io::Result<usize> {
    let reader = open_trace(path)?;
    let mut count = 0usize;
    for line in reader.lines() {
        if !line?.trim().is_empty() {
            count += 1;
        }
    }
    Ok(count)
}

fn trace_cases(root: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut cases = Vec::new();
    for group in fs::read_dir(root.join("traces"))? {
        let group = group?;
        if group.file_type()?.is_dir() {
            let fixture = group.file_name().to_string_lossy().into_owned();
            collect_traces(&group.path(), &fixture, &mut cases)?;
        }
    }
    Ok(cases)
}

fn collect_traces(
    directory: &Path,
    fixture: &str,
    cases: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_traces(&entry.path(), fixture, cases)?;
        } else if matches!(
            entry.path().extension().and_then(|value| value.to_str()),
            Some("jsonl" | "zst")
        ) {
            cases.push((fixture.to_owned(), entry.path()));
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&destination)?;
            copy_tree(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}
