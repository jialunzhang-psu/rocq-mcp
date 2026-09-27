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
        ("start_parameters", (120, 12)),
        ("start_paths", (81, 3)),
        ("start_failures", (12, 6)),
        ("search_valid", (96, 12)),
        ("search_invalid", (480, 12)),
        ("search_boundaries", (288, 12)),
        ("query_kind", (576, 12)),
        ("query_target", (360, 12)),
        ("query_expression", (528, 12)),
        ("query_failures", (108, 69)),
        ("declare_parameters", (4_800, 12)),
        ("declare_boundaries", (1_440, 9)),
        ("declare_failures", (15, 9)),
        ("dune_duplicate_theory", (3, 3)),
        ("prove", (960, 732)),
        ("prove_failures", (15, 9)),
        ("check", (4_608, 48)),
        ("check_escaping_heads", (576, 12)),
        ("check_publication", (72, 36)),
        ("try_parameters", (768, 12)),
        ("try_order", (144, 12)),
        ("try_failures", (12, 9)),
        ("two_users", (15, 15)),
        ("three_users", (36, 36)),
        ("environment_change", (18, 18)),
        ("publication_fault", (36, 36)),
        ("simultaneous_requests", (5, 5)),
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
    assert_eq!(cases.len(), 16_172);
    assert_eq!(
        family_traces.values().map(HashSet::len).sum::<usize>(),
        1_175
    );
}

#[test]
fn simultaneous_rows_have_their_declared_trace_shape() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = rocq_e2e::open_trace(root.join("TRACE_CASES.jsonl.zst")).unwrap();
    let mut simultaneous = 0;
    let mut boundaries = HashSet::new();
    for row in manifest.lines() {
        let row: Value = serde_json::from_str(&row.unwrap()).unwrap();
        let family = row["family"].as_str().unwrap();
        if family != "simultaneous_requests" {
            continue;
        }
        assert_eq!(row["implementation"], "implemented");
        let trace = root.join(row["trace"].as_str().unwrap());
        let events = BufReader::new(File::open(&trace).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        simultaneous += 1;
        let grouped = events
            .iter()
            .enumerate()
            .filter(|(_, event)| event["parallel_group"] == "race")
            .collect::<Vec<_>>();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[1].0, grouped[0].0 + 1);
        assert_ne!(grouped[0].1["user"], grouped[1].1["user"]);
        let boundary = row["axes"]["boundary"].as_str().unwrap();
        boundaries.insert(boundary.to_owned());
        let projects = events
            .iter()
            .filter(|event| event["command"]["tool"] == "start")
            .filter_map(|event| event["command"]["args"]["project_path"].as_str())
            .collect::<HashSet<_>>();
        let declarations = events
            .iter()
            .filter(|event| event["command"]["tool"] == "prove")
            .map(|event| {
                let id = &event["command"]["args"]["declaration"];
                (
                    id["file"].as_str().unwrap(),
                    id["qualified_path"].to_string(),
                )
            })
            .collect::<HashSet<_>>();
        let files = declarations
            .iter()
            .map(|(file, _)| *file)
            .collect::<HashSet<_>>();
        let relation = (projects.len(), files.len(), declarations.len());
        let expected = match boundary {
            "same_theorem_double_close" => (1, 1, 1),
            "different_theorem_same_file_close" => (1, 1, 2),
            "different_file_same_project_close" => (1, 2, 2),
            "different_project_close" => (2, 1, 2),
            "read_during_close" => (1, 1, 1),
            _ => panic!("unknown simultaneous boundary {boundary}"),
        };
        assert_eq!(relation, expected, "{}", trace.display());
    }
    assert_eq!(
        boundaries,
        HashSet::from([
            "same_theorem_double_close".to_owned(),
            "different_theorem_same_file_close".to_owned(),
            "different_file_same_project_close".to_owned(),
            "different_project_close".to_owned(),
            "read_during_close".to_owned(),
        ])
    );
    assert_eq!(simultaneous, boundaries.len());
}

#[test]
fn multi_user_relation_axes_match_the_materialized_projects_and_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = rocq_e2e::open_trace(root.join("TRACE_CASES.jsonl.zst")).unwrap();
    for row in manifest.lines() {
        let row: Value = serde_json::from_str(&row.unwrap()).unwrap();
        let family = row["family"].as_str().unwrap();
        if !matches!(family, "two_users" | "three_users") {
            continue;
        }
        let trace = root.join(row["trace"].as_str().unwrap());
        let events = BufReader::new(File::open(&trace).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        let mut projects = BTreeMap::<String, String>::new();
        let mut declarations = BTreeMap::<String, (String, String)>::new();
        for event in &events {
            let Some(user) = event["user"].as_str() else {
                continue;
            };
            match event["command"]["tool"].as_str() {
                Some("start") => {
                    projects.entry(user.to_owned()).or_insert_with(|| {
                        event["command"]["args"]["project_path"]
                            .as_str()
                            .unwrap()
                            .to_owned()
                    });
                }
                Some("prove") => {
                    declarations.entry(user.to_owned()).or_insert_with(|| {
                        let id = &event["command"]["args"]["declaration"];
                        let path = id["qualified_path"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|part| part.as_str().unwrap())
                            .collect::<Vec<_>>()
                            .join(".");
                        (id["file"].as_str().unwrap().to_owned(), path)
                    });
                }
                _ => {}
            }
        }
        let expected_users = if family == "two_users" { 2 } else { 3 };
        assert_eq!(projects.len(), expected_users, "{}", trace.display());
        assert_eq!(declarations.len(), expected_users, "{}", trace.display());
        let project_count = projects.values().collect::<HashSet<_>>().len();
        let file_count = declarations
            .values()
            .map(|(file, _)| file)
            .collect::<HashSet<_>>()
            .len();
        let declaration_count = declarations
            .values()
            .map(|(_, declaration)| declaration)
            .collect::<HashSet<_>>()
            .len();
        let variant = row["axes"]["variant"].as_str().unwrap();
        let expected = match variant {
            value if value.starts_with("same_theorem") || value.starts_with("all_same") => {
                (1, 1, 1)
            }
            "different_theorem_same_file" | "all_different_same_file" => (1, 1, expected_users),
            "different_file_same_project" | "different_files_same_project" => {
                (1, expected_users, expected_users)
            }
            "different_project" | "different_projects" => (expected_users, 1, expected_users),
            // The remaining three-user variants contain a same-theorem pair
            // plus one distinct theorem in the same physical source file.
            _ if family == "three_users" => (1, 1, 2),
            _ => panic!("unknown multi-user variant {variant}"),
        };
        assert_eq!(
            (project_count, file_count, declaration_count),
            expected,
            "{family}/{variant} materialized the wrong relation in {}",
            trace.display()
        );
    }
}
