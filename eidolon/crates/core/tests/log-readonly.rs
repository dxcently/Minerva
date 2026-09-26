//! The read path must not repair. `SessionLog::open` truncates a torn tail
//! so a session about to *run* never appends past a half-written record —
//! correct for resume, catastrophic for a reader pointed at a live
//! session: the repair would cut the record the writer is mid-way through
//! appending, and every append after the cut lands past a gap no future
//! open can repair. A reader therefore takes `open_readonly`, which
//! verifies everything it can see and touches nothing. The torn tail here
//! stands in for the crash-mid-append that a concurrent poller races.

use std::io::Write;
use std::path::Path;

use eidolon_core::session::Session;
use eidolon_core::{Message, RecordKind};

/// A log with two whole records, then a frame header promising a body that
/// was never written — the on-disk shape of a crash mid-append.
fn torn_log(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("live.eid");
    {
        let mut s = Session::create(&path, "prov:m", dir, None).unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("one")))
            .unwrap();
        s.append(RecordKind::UserMessage(Message::user_text("two")))
            .unwrap();
    }
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    // len = 40 little-endian, crc unread, body absent.
    f.write_all(&[40, 0, 0, 0, 1, 2, 3, 4]).unwrap();
    path
}

#[test]
fn readonly_sees_the_whole_records_and_leaves_the_torn_tail_as_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = torn_log(dir.path());
    let before = std::fs::metadata(&path).unwrap().len();

    let s = Session::open_readonly(&path).unwrap();
    // SessionStart + two user messages; the torn fourth is absent, not an
    // error — it was never whole.
    assert_eq!(s.records().len(), 3);
    // The file is byte-for-byte as found: no repair, no truncation.
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before);
}

#[test]
fn the_running_open_still_repairs_what_the_reader_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = torn_log(dir.path());
    let torn_len = std::fs::metadata(&path).unwrap().len();

    // The reader's restraint did not strand the torn tail: the open that
    // exists to run the session still repairs it away.
    let before_open = std::fs::metadata(&path).unwrap().len();
    let s = Session::open(&path).unwrap();
    assert_eq!(s.records().len(), 3);
    assert!(std::fs::metadata(&path).unwrap().len() < torn_len);
    assert_eq!(before_open, torn_len);
}
