//! Integrity checks for the case-to-trace aggregation contract.
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

#[test]
fn every_matrix_case_occupies_one_unique_bounded_trace_slot() {
    let expected = BTreeMap::from([
        ("start_parameters", (240, 24)),
        ("start_paths", (99, 3)),
        ("start_failures", (24, 15)),
        ("search_valid", (3_600, 12)),
        ("search_invalid", (30_720, 24)),
        ("search_boundaries", (86_016, 48)),
        ("query_kind", (576, 12)),
        ("query_target", (1_440, 84)),
        ("query_expression", (528, 12)),
        ("query_failures", (162, 72)),
        ("declare_parameters", (9_600, 24)),
        ("declare_boundaries", (1_440, 9)),
        ("declare_failures", (36, 24)),
        ("prove", (1_536, 1_212)),
        ("prove_failures", (21, 12)),
        ("check", (9_216, 96)),
        ("check_escaping_heads", (576, 12)),
        ("check_publication", (162, 78)),
        ("check_multi_parameters", (768, 12)),
        ("check_multi_order", (144, 12)),
        ("check_multi_failures", (12, 9)),
        ("two_users", (15, 15)),
        ("three_users", (36, 36)),
        ("environment_change", (84, 84)),
        ("publication_fault", (36, 36)),
        ("simultaneous_requests", (4, 4)),
    ]);
    let expected = expected
        .into_iter()
        .map(|(family, counts)| (family.to_owned(), counts))
        .collect::<BTreeMap<_, _>>();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("TRACE_CASES.jsonl.zst");
    let reader = rocq_e2e::open_trace(path).unwrap();
    let mut cases = HashSet::new();
    let mut slots = HashSet::new();
    let mut family_cases = BTreeMap::new();
    let mut family_traces: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for (index, line) in reader.lines().enumerate() {
        let value: Value = serde_json::from_str(&line.unwrap())
            .unwrap_or_else(|error| panic!("case line {}: {error}", index + 1));
        let object = value.as_object().expect("case record must be an object");
        let family = object["family"].as_str().expect("family string");
        let case = object["case"].as_str().expect("case string");
        let trace = object["trace"].as_str().expect("trace string");
        let case_index = object["case_index"].as_u64().expect("case_index integer");
        assert!(case_index < 2_048);
        assert!(
            object["axes"]
                .as_object()
                .is_some_and(|axes| !axes.is_empty())
        );
        assert!(matches!(
            object["implementation"].as_str(),
            Some("unmapped" | "implemented" | "excluded")
        ));
        assert!(
            cases.insert((family.to_owned(), case.to_owned())),
            "duplicate case"
        );
        assert!(
            slots.insert((trace.to_owned(), case_index)),
            "duplicate trace case slot"
        );
        *family_cases.entry(family.to_owned()).or_insert(0usize) += 1;
        family_traces
            .entry(family.to_owned())
            .or_default()
            .insert(trace.to_owned());
    }
    let actual = family_cases
        .into_iter()
        .map(|(family, cases)| {
            let traces = family_traces[&family].len();
            (family, (cases, traces))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    assert_eq!(cases.len(), 147_091);
    assert_eq!(
        family_traces.values().map(HashSet::len).sum::<usize>(),
        1_981
    );
}

#[test]
fn crash_and_simultaneous_rows_have_their_declared_trace_shape() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = rocq_e2e::open_trace(root.join("TRACE_CASES.jsonl.zst")).unwrap();
    let mut faults = 0;
    let mut simultaneous = 0;
    for row in manifest.lines() {
        let row: Value = serde_json::from_str(&row.unwrap()).unwrap();
        let family = row["family"].as_str().unwrap();
        if !matches!(family, "publication_fault" | "simultaneous_requests") {
            continue;
        }
        assert_eq!(row["implementation"], "implemented");
        let trace = root.join(row["trace"].as_str().unwrap());
        let events = BufReader::new(File::open(&trace).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        if family == "publication_fault" {
            faults += 1;
            let axes = &row["axes"];
            let stem = format!(
                "{}__{}__{}",
                axes["point"].as_str().unwrap(),
                axes["replacement"].as_str().unwrap(),
                axes["recovery"].as_str().unwrap()
            );
            assert_eq!(trace.file_stem().unwrap(), stem.as_str());
            assert!(trace.to_string_lossy().contains("/traces/fault/"));
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event["expected"]["$transport"] == "lost")
                    .count(),
                1
            );
            let project = if axes["replacement"] == "multi_file" {
                "project_multi"
            } else {
                "project_single"
            };
            assert!(
                events
                    .iter()
                    .any(|event| event["command"]["tool"] == "start"
                        && event["command"]["args"]["project_path"] == project)
            );
            if axes["replacement"] == "multi_file" {
                assert!(
                    events
                        .iter()
                        .any(|event| event["command"]["tool"] == "declare"
                            && event["command"]["args"]["library"] == "Demo.Fresh")
                );
            }
        } else {
            simultaneous += 1;
            let grouped = events
                .iter()
                .enumerate()
                .filter(|(_, event)| event["parallel_group"] == "race")
                .collect::<Vec<_>>();
            assert_eq!(grouped.len(), 2);
            assert_eq!(grouped[1].0, grouped[0].0 + 1);
            assert_ne!(grouped[0].1["user"], grouped[1].1["user"]);
        }
    }
    assert_eq!(faults, 36);
    assert_eq!(simultaneous, 4);
}
