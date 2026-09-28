//! Public-contract tests for trace syntax. Process crossings are exercised by
//! the replay CLI against the external server binary, never by linking it.
use rocq_e2e::{Event, load_trace, parse_trace};
use std::{collections::BTreeSet, io::Cursor, path::Path};

#[test]
fn five_event_jsonl_contract_is_public_and_ordered() {
    let source = r#"{"event":"server_start"}
{"event":"user_connect","user":"alice"}
{"event":"command","user":"alice","command":{"tool":"query","args":{"kind":"goals"}},"expected":{"kind":"invalid_request","message":"call start first"}}
{"event":"user_disconnect","user":"alice"}
{"event":"server_kill"}
"#;
    let trace = parse_trace(Cursor::new(source)).unwrap();
    assert!(matches!(trace.events[0].event, Event::ServerStart));
    assert!(matches!(trace.events[1].event, Event::UserConnect { .. }));
    assert!(matches!(trace.events[2].event, Event::Command { .. }));
    assert!(matches!(
        trace.events[3].event,
        Event::UserDisconnect { .. }
    ));
    assert!(matches!(trace.events[4].event, Event::ServerKill));
}

#[test]
fn command_is_exactly_the_user_command_expected_triple() {
    for invalid in [
        r#"{"event":"command","command":{"tool":"query","args":{}},"expected":{}}"#,
        r#"{"event":"command","user":"alice","command":{"tool":"query","args":{}}}"#,
        r#"{"event":"command","user":"alice","command":{"tool":"unknown","args":{}},"expected":{}}"#,
        r#"{"event":"command","user":"alice","command":{"tool":"query"},"expected":{}}"#,
    ] {
        assert!(
            parse_trace(Cursor::new(invalid)).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn parallel_expectations_are_strict_trace_metadata() {
    let valid = r#"{"event":"command","user":"alice","command":{"tool":"query","args":{"kind":"goals"}},"expected":{"$one_of":[{"text":"a"},{"text":"b"}]},"parallel_group":"race"}
"#;
    assert_eq!(parse_trace(Cursor::new(valid)).unwrap().events.len(), 1);
    for invalid in [
        r#"{"event":"command","user":"alice","command":{"tool":"query","args":{}},"expected":{"$one_of":[{"text":"a"}]}}"#,
        r#"{"event":"command","user":"alice","command":{"tool":"query","args":{}},"expected":{},"parallel_group":" "}"#,
    ] {
        assert!(
            parse_trace(Cursor::new(invalid)).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn checked_in_suite_covers_exactly_the_current_ten_tools() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let trace = load_trace(root.join("traces/current_protocol.jsonl")).unwrap();
    let tools = trace
        .events
        .iter()
        .filter_map(|located| match &located.event {
            Event::Command { command, .. } => Some(command.tool.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        tools,
        BTreeSet::from([
            "abandon",
            "check",
            "declare",
            "list_decls",
            "list_files",
            "prove",
            "query",
            "rewind",
            "start",
            "try",
        ])
    );
    assert!(matches!(
        trace.events.first().unwrap().event,
        Event::ServerStart
    ));
    assert!(matches!(
        trace.events.last().unwrap().event,
        Event::ServerKill
    ));
    for file in ["dune-project", "dune", "Main.v"] {
        assert!(root.join("fixtures/current/project").join(file).is_file());
    }
}
