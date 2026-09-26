//! The one JSON shape a record has: `trace_line`, the serializer
//! `eidolon log --json` exports through. These tests hold the parts a
//! reader in another language depends on — externally tagged kinds, one
//! line per record, every line newline-terminated — with no second file
//! on disk: the journal is the truth, the export is a door.

use eidolon_core::message::{ContentBlock, Message};
use eidolon_core::session::{RecordKind, Session, trace_line};

#[test]
fn a_record_is_one_externally_tagged_json_line() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("s.eid");
    let mut s = Session::create(&log, "ollama:m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("hi"))).unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text("ok")]))).unwrap();

    let records = s.records();
    let lines: Vec<String> = records
        .iter()
        .map(|r| trace_line(r).unwrap().trim_end().to_string())
        .collect();
    assert_eq!(lines.len(), records.len(), "one line per record");

    let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(first["id"], 0);
    assert_eq!(first["parent"], serde_json::Value::Null);
    assert_eq!(first["kind"]["SessionStart"]["model"], "ollama:m");

    let last: serde_json::Value = serde_json::from_str(&lines[lines.len() - 1]).unwrap();
    assert_eq!(last["id"], 2);
    assert_eq!(last["parent"], 1);
    assert_eq!(last["kind"]["AssistantMessage"]["role"], "assistant");
    assert_eq!(last["kind"]["AssistantMessage"]["content"][0]["type"], "text");

    // The line carries its own newline, so an exporter is a loop of
    // write_alls and nothing else.
    assert!(trace_line(records.first().unwrap()).unwrap().ends_with('\n'));
}
