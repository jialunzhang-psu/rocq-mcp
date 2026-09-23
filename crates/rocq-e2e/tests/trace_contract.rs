//! Public-contract tests for trace syntax. Process crossings are exercised by
//! the replay CLI against the external server binary, never by linking it.
use rocq_e2e::{Event, parse_trace};
use std::io::Cursor;

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
fn fault_and_parallel_expectations_are_strict_trace_metadata() {
    let valid = r#"{"event":"command","user":"alice","command":{"tool":"check","args":{"commands":"exact I."}},"expected":{"$transport":"lost"}}
{"event":"command","user":"alice","command":{"tool":"query","args":{"kind":"goals"}},"expected":{"$one_of":[{"text":"a"},{"text":"b"}]},"parallel_group":"race"}
"#;
    assert_eq!(parse_trace(Cursor::new(valid)).unwrap().events.len(), 2);
    for invalid in [
        r#"{"event":"command","user":"alice","command":{"tool":"check","args":{}},"expected":{"$transport":"lost","kind":"x"}}"#,
        r#"{"event":"command","user":"alice","command":{"tool":"query","args":{}},"expected":{"$one_of":[{"text":"a"}]}}"#,
        r#"{"event":"command","user":"alice","command":{"tool":"query","args":{}},"expected":{},"parallel_group":" "}"#,
    ] {
        assert!(
            parse_trace(Cursor::new(invalid)).is_err(),
            "accepted {invalid}"
        );
    }
}
