//! Test-only evidence that the named publication fault stopped at its window.

use rocq_e2e::{Event, TraceError, TraceRunner};
use std::{fs, path::Path};

/// Observe copied source after a lost close response, before recovery starts.
/// The multi-file checks prove `between_replacements` really left one source
/// replacement visible while the Dune metadata replacement was absent.
pub fn attach(runner: TraceRunner, trace: &Path, lab: &Path) -> TraceRunner {
    let stem = trace
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let mut axes = stem.split("__");
    let point = axes.next().unwrap_or("").to_owned();
    let multi = axes.next() == Some("multi_file");
    let lab = lab.to_path_buf();
    runner.with_after_event_hook(move |_line, event| {
        if !matches!(event, Event::Command { command, .. } if command.tool == "check") || !multi {
            return Ok(());
        }
        let source = lab.join("project_multi/theories/Fresh.v");
        let dune = lab.join("project_multi/theories/dune");
        let source_present = source.is_file();
        let metadata_present = fs::read_to_string(&dune)
            .map_err(|source| TraceError::Io { operation: "read fault Dune metadata", source })?
            .contains("Fresh");
        let expected = match point.as_str() {
            "after_promote" | "before_replace" => (false, false),
            "between_replacements" => (true, false),
            "after_replace" | "after_final_build" | "before_ack" => (true, true),
            _ => return Err(TraceError::InvalidConfiguration {
                path: dune.clone(), message: format!("unknown publication fault point: {point}"),
            }),
        };
        if (source_present, metadata_present) != expected {
            return Err(TraceError::InvalidConfiguration {
                path: dune,
                message: format!("fault {point} observed source/metadata presence ({source_present}, {metadata_present}), expected {expected:?}"),
            });
        }
        Ok(())
    })
}
