//! Structural proof that every implemented matrix case has one real command
//! slot in a checked-in JSONL trace. Runtime replay supplies semantic evidence.
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    io::BufRead,
    path::{Path, PathBuf},
};

#[derive(Debug)]
struct TraceShape {
    commands: usize,
    has_start: bool,
    has_kill: bool,
}

#[test]
fn every_implemented_case_maps_once_to_a_real_command_trace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = rocq_e2e::open_trace(root.join("TRACE_CASES.jsonl.zst")).unwrap();
    let mut cases = HashSet::new();
    let mut slots = HashSet::new();
    let mut implemented_per_trace = BTreeMap::<PathBuf, usize>::new();
    let mut implemented = 0usize;
    let mut unmapped = 0usize;
    let mut excluded = BTreeMap::<String, usize>::new();

    for (line, source) in manifest.lines().enumerate() {
        let value: Value = serde_json::from_str(&source.unwrap())
            .unwrap_or_else(|error| panic!("manifest line {}: {error}", line + 1));
        let family = value["family"].as_str().unwrap();
        let case = value["case"].as_str().unwrap();
        let trace = value["trace"].as_str().unwrap();
        let index = value["case_index"].as_u64().unwrap();
        assert!(cases.insert((family.to_owned(), case.to_owned())));
        assert!(
            slots.insert((trace.to_owned(), index)),
            "duplicate case slot"
        );
        match value["implementation"].as_str().unwrap() {
            "implemented" => {
                implemented += 1;
                *implemented_per_trace.entry(root.join(trace)).or_default() += 1;
            }
            "unmapped" => unmapped += 1,
            "excluded" => {
                let exclusion = &value["exclusion"];
                let code = exclusion["code"].as_str().expect("exclusion code");
                let reason = exclusion["reason"].as_str().expect("exclusion reason");
                assert!(!reason.is_empty());
                *excluded.entry(code.to_owned()).or_default() += 1;
            }
            other => panic!("unknown implementation state {other}"),
        }
    }

    assert_eq!(
        implemented + unmapped + excluded.values().sum::<usize>(),
        147_091
    );
    assert_eq!(implemented, 146_977);
    assert_eq!(unmapped, 0);
    assert_eq!(excluded.values().sum::<usize>(), 114);
    assert_eq!(excluded.len(), 8);
    assert!(implemented > 0);
    for (path, mapped_commands) in implemented_per_trace {
        assert!(
            path.is_file(),
            "implemented trace is missing: {}",
            path.display()
        );
        let shape = trace_shape(&path);
        assert!(
            shape.has_start,
            "trace never starts server: {}",
            path.display()
        );
        assert!(
            shape.has_kill,
            "trace never kills server: {}",
            path.display()
        );
        assert!(
            shape.commands >= mapped_commands,
            "{} maps {mapped_commands} cases but contains only {} commands",
            path.display(),
            shape.commands
        );
    }
}

/// Inspect one trace in constant memory. Full strict parsing occurs in the
/// process runner; this check protects the case-to-command cardinality contract.
fn trace_shape(path: &Path) -> TraceShape {
    let reader = rocq_e2e::open_trace(path).unwrap();
    let mut shape = TraceShape {
        commands: 0,
        has_start: false,
        has_kill: false,
    };
    for (line, source) in reader.lines().enumerate() {
        let value: Value = serde_json::from_str(&source.unwrap())
            .unwrap_or_else(|error| panic!("{} line {}: {error}", path.display(), line + 1));
        match value["event"].as_str() {
            Some("command") => {
                shape.commands += 1;
                assert!(value["user"].is_string());
                assert!(value["command"]["tool"].is_string());
                assert!(value["command"]["args"].is_object());
                assert!(value["expected"].is_object());
            }
            Some("server_start") => shape.has_start = true,
            Some("server_kill") => shape.has_kill = true,
            Some("user_connect" | "user_disconnect") => assert!(value["user"].is_string()),
            other => panic!(
                "{} line {}: invalid event {other:?}",
                path.display(),
                line + 1
            ),
        }
    }
    shape
}
