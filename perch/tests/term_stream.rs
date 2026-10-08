//! `GET /hub/term/events?since=...`: the one SSE for every terminal (slice T1c), over a real `/bin/sh`.
//!
//! Every perch here has its own temp HOME (the harness's), and the shells it starts are `/bin/sh`. A test that starts a shell runs its
//! body inside [`with_perch`], which stops the perch with SIGTERM after the body, panic or not: the shutdown ends every terminal and every
//! stream, so no `sh` or `sleep` outlives a test. The frames are read through the harness's [`Pane`]; a frame is one `data:` line of JSON,
//! and the output inside a frame is base64, so output is checked by decoding the frames, never by searching the raw text.

#![cfg(feature = "test-bins")]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// A perch with its own `--root`, `/bin/sh` for a login shell, and the builder's knobs. The body runs against it, then the perch stops.
fn with_perch(builder: Builder, body: impl FnOnce(&Perch)) {
    let mut perch = builder.start();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&perch)));
    let _ = perch.stop();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn with_shells(body: impl FnOnce(&Perch)) {
    with_perch(Perch::builder().hub().env("SHELL", "/bin/sh"), body);
}

fn open_one(perch: &Perch) -> String {
    let answer = perch.post_json("/hub/term", json!({}));
    assert_eq!(answer.status, 201, "{}: {}", answer.head, answer.body);
    body_id(&answer)
}

/// The screen's text, read until `want` is true of it or ten seconds pass: a shell answers a moment after its input.
fn screen_until(perch: &Perch, id: &str, want: impl Fn(&str) -> bool) -> String {
    screen_until_within(perch, id, Duration::from_secs(10), want)
}

/// [`screen_until`] for a shell that has a lot of output to get through (the screen copy reads a few MB/s in a debug build).
fn screen_until_within(perch: &Perch, id: &str, within: Duration, want: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + within;
    loop {
        let answer = perch.get(&format!("/hub/term/{id}/text?lines=50"));
        assert_eq!(answer.status, 200, "{}", answer.head);
        let text = serde_json::from_str::<Value>(&answer.body).unwrap()["text"].as_str().unwrap().to_string();
        if want(&text) || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `bytes` typed into terminal `id`, as the page sends them: a raw octet-stream body, `Connection: close`.
fn type_into(perch: &Perch, id: &str, bytes: &[u8]) -> Answer {
    let head = format!(
        "POST /hub/term/{id}/input HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAuthorization: {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        perch.addr,
        perch.auth(),
        bytes.len()
    );
    let mut sock = TcpStream::connect(&perch.addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    sock.write_all(head.as_bytes()).unwrap();
    sock.write_all(bytes).unwrap();
    let mut raw = Vec::new();
    let _ = sock.read_to_end(&mut raw);
    Answer::parse(&raw)
}

/// The stream, opened with the given `since` (`t1:4096,t2:0`, or none).
fn stream_from(perch: &Perch, since: &str) -> Pane {
    let query = if since.is_empty() { String::new() } else { format!("?since={since}") };
    Pane::open_path(perch, &format!("/hub/term/events{query}"))
}

/// Pump the stream until `ready` is true of the frames so far, the stream ends, or ten seconds pass.
fn wait_frames(pane: &mut Pane, ready: impl Fn(&[Value]) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ready(&pane.frames()) {
            return true;
        }
        let (_, finished) = pane.data();
        if finished || pane.eof || Instant::now() >= deadline {
            return false;
        }
        pane.pump();
    }
}

/// Until a frame of this type for this terminal has arrived.
fn until(pane: &mut Pane, kind: &str, id: &str) -> bool {
    wait_frames(pane, |frames| frames.iter().any(|f| f["type"] == kind && f["id"] == id))
}

/// Until the output a terminal has carried (its reset's data and its `term-out` data, decoded) contains `needle`.
fn until_output(pane: &mut Pane, id: &str, needle: &str) -> bool {
    wait_frames(pane, |frames| String::from_utf8_lossy(&output_in(frames, id)).contains(needle))
}

/// The first frame of this type for this terminal so far.
fn frame(pane: &Pane, kind: &str, id: &str) -> Option<Value> {
    pane.frames().into_iter().find(|f| f["type"] == kind && f["id"] == id)
}

/// Every `term-out` frame for this terminal so far, in order.
fn frames_of(pane: &Pane, id: &str) -> Vec<Value> {
    pane.frames().into_iter().filter(|f| f["type"] == "term-out" && f["id"] == id).collect()
}

/// The bytes a frame's `data` carries.
fn data_of(frame: &Value) -> Vec<u8> {
    decode(frame["data"].as_str().expect("a data string"))
}

/// The output bytes terminal `id` has carried in these frames, in order: a reset's first chunk, then each `term-out`.
fn output_in(frames: &[Value], id: &str) -> Vec<u8> {
    frames
        .iter()
        .filter(|f| (f["type"] == "term-reset" || f["type"] == "term-out") && f["id"] == id)
        .flat_map(data_of)
        .collect()
}

fn output_so_far(pane: &Pane, id: &str) -> Vec<u8> {
    output_in(&pane.frames(), id)
}

/// The events of a stream's text that are whole: each is closed by a blank line, so the piece after the last blank line (still arriving)
/// is left out.
fn whole_events(text: &str) -> Vec<&str> {
    let mut events: Vec<&str> = text.split("\n\n").collect();
    events.pop();
    events
}

/// The first whole `term-reset` with `snap` true in `text` whose rebuilt screen shows `marker`, if there is one yet.
fn snapshot_showing(text: &str, marker: &str) -> Option<Value> {
    whole_events(text).into_iter().find_map(|event| {
        let frame: Value = serde_json::from_str(event.trim_start_matches("data: ")).ok()?;
        let shows = String::from_utf8_lossy(&decode(frame["data"].as_str()?)).contains(marker);
        (frame["type"] == "term-reset" && frame["snap"] == true && shows).then_some(frame)
    })
}

/// Read what the stream has sent until `done` is true of its text so far, the stream ends, or ten seconds pass.
fn pump_until(pane: &mut Pane, done: impl Fn(&str) -> bool) -> bool {
    pump_until_within(pane, Duration::from_secs(10), done)
}

/// [`pump_until`] for a stream with megabytes queued ahead of what it wants: it reads in batches ([`read_batch`]) and looks once per
/// batch, since [`wait_frames`] would parse every frame after each read.
fn pump_until_within(pane: &mut Pane, within: Duration, done: impl Fn(&str) -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        let (text, finished) = pane.data();
        if done(&text) {
            return true;
        }
        if finished || pane.eof || Instant::now() >= deadline {
            return false;
        }
        read_batch(pane);
    }
}

/// Up to 4 MiB of what the stream has sent, for as long as the socket has something ready (each read waits at most the socket's 50 ms).
fn read_batch(pane: &mut Pane) {
    let mut buf = vec![0u8; 64 * 1024];
    for _ in 0..64 {
        match pane.sock.read(&mut buf) {
            Ok(0) => {
                pane.eof = true;
                return;
            }
            Ok(n) => pane.raw.extend_from_slice(&buf[..n]),
            Err(_) => return,
        }
    }
}
/// The frames that carry a reset of terminal `id`: its `term-reset`, then the `term-out` frames at the reset's seq, in order.
fn reset_frames<'a>(frames: &'a [Value], id: &str) -> Vec<&'a Value> {
    let start = frames.iter().position(|f| f["type"] == "term-reset" && f["id"] == id).expect("a reset for the terminal");
    let seq = frames[start]["seq"].clone();
    frames[start..]
        .iter()
        .take_while(|f| (f["type"] == "term-reset" || f["type"] == "term-out") && f["id"] == id && f["seq"] == seq)
        .collect()
}

/// Standard padded base64, decoded here: the test does not share the perch's encoder.
fn decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            other => panic!("not base64: {other}"),
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

#[test]
fn the_first_frame_is_the_roster_and_a_new_terminal_joins_with_its_whole_ring() {
    with_shells(|perch| {
        let id = open_one(perch);
        assert_eq!(type_into(perch, &id, b"echo hi$((1+1))\r").status, 204);
        screen_until(perch, &id, |text| text.contains("hi2"));

        let mut pane = stream_from(perch, "");
        assert!(wait_frames(&mut pane, |frames| frames.first().is_some_and(|f| f["type"] == "term-list")), "the roster is the first frame: {:?}", pane.data().0);
        let first = pane.frames().into_iter().next().unwrap();
        assert!(first["terms"].as_array().unwrap().iter().any(|t| t["id"] == id.as_str()), "the terminal is in the roster");

        assert!(until(&mut pane, "term-reset", &id), "the terminal joins with a reset: {:?}", pane.data().0);
        let reset = frame(&pane, "term-reset", &id).unwrap();
        assert_eq!(reset["snap"], false, "a ring that starts at byte 0 is an exact replay");
        let data = data_of(&reset);
        assert!(String::from_utf8_lossy(&data).contains("hi2"), "the reset carries the shell's answer");
        assert_eq!(reset["seq"].as_u64(), Some(data.len() as u64), "an exact replay ends at the seq it names");
    });
}

#[test]
fn a_seq_the_ring_still_holds_resumes_as_term_out_from_that_seq() {
    with_shells(|perch| {
        let id = open_one(perch);
        // The typed line holds `m$((2*3))`, the output is `m6`: the markers are on screen only once the shell has answered.
        assert_eq!(type_into(perch, &id, b"echo m$((2*3))\r").status, 204);
        screen_until(perch, &id, |text| text.contains("m6"));

        let mut live = stream_from(perch, "");
        assert!(until(&mut live, "term-reset", &id), "the live stream joins: {:?}", live.data().0);
        let at = frame(&live, "term-reset", &id).unwrap()["seq"].as_u64().unwrap();

        assert_eq!(type_into(perch, &id, b"echo n$((7*8))\r").status, 204);
        assert!(until_output(&mut live, &id, "n56"), "the live stream has the second answer: {:?}", live.data().0);

        let mut resumed = stream_from(perch, &format!("{id}:{at}"));
        assert!(until_output(&mut resumed, &id, "n56"), "the resumed stream has the second answer: {:?}", resumed.data().0);
        assert!(frame(&resumed, "term-reset", &id).is_none(), "a seq the ring holds is not reset");
        assert!(!String::from_utf8_lossy(&output_so_far(&resumed, &id)).contains("m6"), "nothing before the seq is sent again");
        let out = frames_of(&resumed, &id);
        assert_eq!(out[0]["seq"].as_u64().unwrap() - data_of(&out[0]).len() as u64, at, "the first frame starts at the seq asked for");
    });
}

#[test]
fn a_seq_past_the_end_is_a_reset_and_an_exact_one_at_the_start_of_the_ring() {
    with_shells(|perch| {
        let id = open_one(perch);
        let mut pane = stream_from(perch, &format!("{id}:999999999"));
        assert!(until(&mut pane, "term-reset", &id), "a seq past the end: {:?}", pane.data().0);
        assert_eq!(frame(&pane, "term-reset", &id).unwrap()["snap"], false, "the ring still starts at byte 0");
    });
}

#[test]
fn a_small_ring_rebuilds_a_mode_the_shell_set_before_the_ring_moved_past_it() {
    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").arg("--knob").arg("term_ring=4096");
    with_perch(builder, |perch| {
        let id = open_one(perch);
        // Application cursor keys on (DECCKM), then more output than the 4 KiB ring holds, then a marker on screen.
        assert_eq!(type_into(perch, &id, b"printf '\\033[?1h'; seq 1 2000; echo done$((1+1999))\r").status, 204);
        screen_until(perch, &id, |text| text.contains("done2000"));

        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id), "the terminal joins: {:?}", pane.data().0);
        let reset = frame(&pane, "term-reset", &id).unwrap();
        assert_eq!(reset["snap"], true, "the ring no longer starts at byte 0, so the screen is rebuilt");
        let data = data_of(&reset);
        assert!(data.windows(5).any(|w| w == b"\x1b[?1h"), "the rebuilt screen sets DECCKM: {:?}", String::from_utf8_lossy(&data[..data.len().min(120)]));
        assert!(data.starts_with(b"\x1bc"), "the rebuilt screen starts with a reset");
    });
}

#[test]
fn a_small_ring_rebuilds_the_scroll_region_the_shell_set() {
    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").arg("--knob").arg("term_ring=4096");
    with_perch(builder, |perch| {
        let id = open_one(perch);
        // A scroll region from line 2 to line 10, then more output than the 4 KiB ring holds, then a marker on screen.
        assert_eq!(type_into(perch, &id, b"printf '\\033[2;10r'; seq 1 2000; echo done$((1+1999))\r").status, 204);
        screen_until(perch, &id, |text| text.contains("done2000"));

        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id), "the terminal joins: {:?}", pane.data().0);
        let reset = frame(&pane, "term-reset", &id).unwrap();
        assert_eq!(reset["snap"], true, "the ring no longer starts at byte 0, so the screen is rebuilt");
        let data = data_of(&reset);
        assert!(data.windows(7).any(|w| w == b"\x1b[2;10r"), "the rebuilt screen sets the region: {:?}", String::from_utf8_lossy(&data[..data.len().min(120)]));
    });
}

#[test]
fn a_seq_the_ring_has_passed_is_a_snap_reset_and_one_it_holds_resumes() {
    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").arg("--knob").arg("term_ring=4096");
    with_perch(builder, |perch| {
        let id = open_one(perch);
        // About 11 KB of output: the 4 KiB ring has moved on from byte 0 before anyone joins.
        assert_eq!(type_into(perch, &id, b"seq 1 2000; echo end$((1+1))\r").status, 204);
        screen_until(perch, &id, |text| text.contains("end2"));

        let mut live = stream_from(perch, "");
        assert!(until(&mut live, "term-reset", &id), "the live stream joins: {:?}", live.data().0);
        let reset = frame(&live, "term-reset", &id).unwrap();
        assert_eq!(reset["snap"], true, "the ring no longer starts at byte 0");
        let end = reset["seq"].as_u64().unwrap();
        assert!(end >= 10_000, "the shell wrote more than the ring holds: {end}");

        // A seq at byte 0 is one the ring has passed: a snapshot, at the end or past it (more output may have come since).
        let mut passed = stream_from(perch, &format!("{id}:0"));
        assert!(until(&mut passed, "term-reset", &id), "a seq the ring has passed is reset: {:?}", passed.data().0);
        let again = frame(&passed, "term-reset", &id).unwrap();
        assert_eq!(again["snap"], true, "a snapshot, not a replay");
        assert!(again["seq"].as_u64().unwrap() >= end, "the reset is at or past the end the live join had");

        // 100 bytes before the end is inside the ring: the stream resumes there with the bytes it has, and no reset.
        let inside = end - 100;
        let mut resumed = stream_from(perch, &format!("{id}:{inside}"));
        assert!(until(&mut resumed, "term-out", &id), "the resumed stream carries output: {:?}", resumed.data().0);
        assert!(frame(&resumed, "term-reset", &id).is_none(), "a seq the ring holds is not reset");
        let first = frames_of(&resumed, &id).remove(0);
        let carried = data_of(&first).len() as u64;
        assert!(carried >= 100, "the 100 bytes before the end are sent: {carried}");
        assert_eq!(first["seq"].as_u64().unwrap() - carried, inside, "the first byte sent is the one asked for");
    });
}

#[test]
fn a_live_client_the_ring_has_passed_is_reset_with_a_snapshot() {
    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").arg("--knob").arg("term_ring=4096");
    with_perch(builder, |perch| {
        let id = open_one(perch);
        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id), "the terminal joins: {:?}", pane.data().0);
        // The page reads nothing while the shell writes 16 MB: more than the socket buffers hold, so the stream is blocked with its cursor
        // far behind the 4 KiB ring, and the snapshot comes when it reads again. (At 2 MB the buffers can take it all, and the stream
        // keeps pace, so no snapshot is ever due.)
        let flood = b"dd if=/dev/zero bs=1024 count=16384 2>/dev/null | tr '\\0' x; echo end$((1+1))\r";
        assert_eq!(type_into(perch, &id, flood).status, 204);
        assert!(screen_until_within(perch, &id, Duration::from_secs(60), |text| text.contains("end2")).contains("end2"), "the shell finished");

        // The ring passes the page's cursor early in the flood, so snapshots come as it goes on: the one that shows the last line is the
        // one taken at the shell's end.
        let shows_the_end = |text: &str| snapshot_showing(text, "end2").is_some();
        assert!(pump_until_within(&mut pane, Duration::from_secs(30), shows_the_end), "the client the ring passed is reset with a snapshot");
        let reset = snapshot_showing(&pane.data().0, "end2").unwrap();
        assert_eq!(reset["id"], id.as_str());
        assert!(reset["seq"].as_u64().unwrap() >= 2_000_000, "the snapshot is at the end of the shell's output: {}", reset["seq"]);
    });
}

#[test]
fn a_lagged_change_channel_is_listed_again_and_a_terminal_that_left_is_closed() {
    with_shells(|perch| {
        let kept = open_one(perch);
        let gone = open_one(perch);
        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &kept) && until(&mut pane, "term-reset", &gone), "both join: {:?}", pane.data().0);
        // The page reads nothing while the shell writes 16 MB: the socket fills (a few MB of kernel buffers), the stream stops taking
        // changes, and 300 of them wait in the channel, which holds 256, so it lags.
        let flood = b"dd if=/dev/zero bs=1024 count=16384 2>/dev/null | tr '\\0' x; echo end$((1+1))\r";
        assert_eq!(type_into(perch, &kept, flood).status, 204);
        let flood_done = |text: &str| text.contains("end2");
        assert!(screen_until_within(perch, &kept, Duration::from_secs(60), flood_done).contains("end2"), "the shell finished");
        assert_eq!(perch.post(&format!("/hub/term/{gone}/close")).status, 202);
        for n in 0..300 {
            // A resize that changes nothing emits nothing, so the size alternates.
            let cols = if n % 2 == 0 { 100 } else { 101 };
            assert_eq!(perch.post_json(&format!("/hub/term/{kept}/resize"), json!({"cols": cols, "rows": 30})).status, 200, "resize {n}");
        }

        let listed_twice = |text: &str| whole_events(text).iter().filter(|e| e.contains("\"type\":\"term-list\"")).count() >= 2;
        assert!(pump_until(&mut pane, listed_twice), "a lagged stream lists the terminals again");
        let frames = pane.frames();
        let last_list = frames.iter().rev().find(|f| f["type"] == "term-list").unwrap();
        let listed = last_list["terms"].as_array().unwrap();
        assert!(listed.iter().all(|t| t["id"] != gone.as_str()), "the new roster does not list the closed one");
        let closed = |f: &Value| f["type"] == "term-closed" && f["id"] == gone.as_str();
        assert!(frames.iter().any(closed), "the terminal that left is closed on the page");
    });
}

#[test]
fn a_burst_of_writes_is_flushed_in_a_few_frames_not_one_per_write() {
    with_shells(|perch| {
        let id = open_one(perch);
        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id), "the terminal joins: {:?}", pane.data().0);
        // 2000 writes of one byte each, one after another from the shell.
        assert_eq!(type_into(perch, &id, b"i=0; while [ $i -lt 2000 ]; do printf x; i=$((i+1)); done; echo end$((1+1))\r").status, 204);
        assert!(until_output(&mut pane, &id, "end2"), "the burst arrives: {:?}", pane.data().0);
        let writes = output_so_far(&pane, &id).iter().filter(|&&b| b == b'x').count();
        let frames = frames_of(&pane, &id).len();
        assert!(writes >= 2000, "the 2000 bytes arrive: {writes}");
        assert!(frames < 500, "{frames} term-out frames for {writes} bytes written one at a time: the output is flushed on a 16 ms tick");
    });
}

#[test]
fn a_pager_is_rebuilt_as_the_screen_it_drew() {
    if std::process::Command::new("less").arg("--version").output().is_err() {
        eprintln!("less is not installed: the pager test does not run");
        return;
    }
    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").arg("--knob").arg("term_ring=4096");
    with_perch(builder, |perch| {
        let id = open_one(perch);
        let file = perch.work.join("lines.txt");
        let lines: String = (1..=200).map(|n| format!("line {n}\n")).collect();
        std::fs::write(&file, lines).unwrap();
        // The shell writes past the 4 KiB ring first, so the join after less has drawn is a snapshot.
        let typed = format!("seq 1 2000; less '{}'\r", file.display());
        assert_eq!(type_into(perch, &id, typed.as_bytes()).status, 204);
        let live = screen_until(perch, &id, |text| text.contains("line 30"));
        assert!(live.contains("line 30"), "less drew its first screen: {live:?}");

        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id), "the terminal joins: {:?}", pane.data().0);
        let reset = frame(&pane, "term-reset", &id).unwrap();
        assert_eq!(reset["snap"], true, "the ring has moved on, so the pager is rebuilt");
        let data = data_of(&reset);
        assert!(data.windows(8).any(|w| w == b"\x1b[?1049h"), "the pager's alternate screen is set");
        let rebuilt = String::from_utf8_lossy(&data).to_string();
        let rows: Vec<&str> = live.lines().map(str::trim_end).filter(|row| row.starts_with("line ")).collect();
        assert!(rows.len() >= 20, "the live screen shows the file's lines: {}", rows.len());
        for row in rows {
            assert!(rebuilt.contains(row), "the rebuilt screen has the row {row:?}");
        }
    });
}

#[test]
fn live_output_is_cut_into_frames_of_at_most_32_kib_raw() {
    with_shells(|perch| {
        let id = open_one(perch);
        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id), "the terminal joins: {:?}", pane.data().0);
        assert_eq!(type_into(perch, &id, b"seq 1 20000; echo end$((1+1))\r").status, 204);
        assert!(until_output(&mut pane, &id, "end2"), "the output arrives");
        let out = frames_of(&pane, &id);
        assert!(out.len() > 1, "more than one term-out frame for {} bytes", output_so_far(&pane, &id).len());
        for f in &out {
            assert!(data_of(f).len() <= 32 * 1024, "a frame carries at most 32 KiB raw, got {}", data_of(f).len());
        }
        assert!(String::from_utf8_lossy(&output_so_far(&pane, &id)).contains("20000"), "the output is all there");
    });
}

#[test]
fn a_replay_longer_than_one_frame_goes_out_in_32_kib_frames_all_at_the_reset_seq() {
    with_shells(|perch| {
        let id = open_one(perch);
        assert_eq!(type_into(perch, &id, b"seq 1 20000; echo end$((1+1))\r").status, 204);
        screen_until(perch, &id, |text| text.contains("end2"));

        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id), "the terminal joins: {:?}", pane.data().0);
        let reset = frame(&pane, "term-reset", &id).unwrap();
        assert_eq!(reset["snap"], false, "the ring still starts at byte 0, so the reset is a replay");
        let seq = reset["seq"].as_u64().unwrap();
        assert!(seq > 32 * 1024, "the replay is more than one frame: {seq} bytes");
        let whole = |frames: &[Value]| reset_frames(frames, &id).iter().map(|f| data_of(f).len() as u64).sum::<u64>() == seq;
        assert!(wait_frames(&mut pane, whole), "the whole replay arrives: {:?}", pane.data().0);

        let frames = pane.frames();
        let parts = reset_frames(&frames, &id);
        let sizes: Vec<usize> = parts.iter().map(|f| data_of(f).len()).collect();
        assert_eq!(sizes[0], 32 * 1024, "the reset's own frame is a full frame: {sizes:?}");
        assert!(sizes.iter().all(|&n| n <= 32 * 1024), "no frame carries more than 32 KiB raw: {sizes:?}");
        assert_eq!(sizes.len() as u64, seq.div_ceil(32 * 1024), "the replay is cut into whole frames and a short last one: {sizes:?}");
        assert!(parts.iter().all(|f| f["seq"].as_u64() == Some(seq)), "every chunk carries the reset's seq");
        let replay: Vec<u8> = parts.iter().flat_map(|f| data_of(f)).collect();
        assert_eq!(replay.len() as u64, seq, "the replay is as long as its seq");
        let text = String::from_utf8_lossy(&replay);
        assert!(text.contains("1\r\n2\r\n3\r\n"), "the replay holds the first lines the shell printed (after its login banner)");
        assert!(text.contains("end2"), "and the marker at the end");

        // The same bytes read the other way: byte 0 is in the ring, so a stream from it resumes as live output, cut the same way.
        let mut again = stream_from(perch, &format!("{id}:0"));
        let carries_replay = |frames: &[Value]| output_in(frames, &id).len() as u64 >= seq;
        assert!(wait_frames(&mut again, carries_replay), "the stream from byte 0 carries the replay: {:?}", again.data().0);
        assert!(frame(&again, "term-reset", &id).is_none(), "byte 0 is in the ring, so the stream resumes and is not reset");
        assert_eq!(&output_so_far(&again, &id)[..seq as usize], &replay[..], "the reset and the resumed stream agree byte for byte");
        assert!(frames_of(&again, &id).iter().all(|f| data_of(f).len() <= 32 * 1024), "the resumed frames are cut the same way");
    });
}

#[test]
fn a_terminal_opened_and_closed_while_the_stream_is_open_is_announced() {
    with_shells(|perch| {
        let mut pane = stream_from(perch, "");
        assert!(wait_frames(&mut pane, |frames| frames.iter().any(|f| f["type"] == "term-list")), "the roster arrives");
        let id = open_one(perch);
        assert!(until(&mut pane, "term-open", &id), "the open is announced: {:?}", pane.data().0);
        assert_eq!(frame(&pane, "term-open", &id).unwrap()["id"], id.as_str());
        assert!(until(&mut pane, "term-reset", &id), "and joins with a reset");

        assert_eq!(perch.post(&format!("/hub/term/{id}/close")).status, 202);
        assert!(until(&mut pane, "term-closed", &id), "the close is announced: {:?}", pane.data().0);
    });
}

#[test]
fn size_title_hold_and_exit_are_announced() {
    with_shells(|perch| {
        let id = open_one(perch);
        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id));

        assert_eq!(perch.post_json(&format!("/hub/term/{id}/resize"), json!({"cols": 100, "rows": 30})).status, 200);
        assert!(until(&mut pane, "term-size", &id), "the size: {:?}", pane.data().0);
        assert_eq!(frame(&pane, "term-size", &id).unwrap()["cols"], 100);

        assert_eq!(type_into(perch, &id, b"printf '\\033]0;hello-title\\007'\r").status, 204);
        assert!(wait_frames(&mut pane, |frames| frames.iter().any(|f| f["type"] == "term-title" && f["title"] == "hello-title")), "the title: {:?}", pane.data().0);

        assert_eq!(perch.post_json(&format!("/hub/term/{id}/hold"), json!({"hold": true})).status, 200);
        assert!(until(&mut pane, "term-hold", &id), "the hold: {:?}", pane.data().0);
        assert_eq!(frame(&pane, "term-hold", &id).unwrap()["hold"], true);

        assert_eq!(type_into(perch, &id, b"exit 3\r").status, 204);
        assert!(until(&mut pane, "term-exit", &id), "the shell's end: {:?}", pane.data().0);
        assert_eq!(frame(&pane, "term-exit", &id).unwrap()["code"], 3);
    });
}

#[test]
fn a_keep_alive_is_seen_after_a_silence_when_the_knob_shortens_it() {
    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").arg("--knob").arg("term_keepalive=100");
    with_perch(builder, |perch| {
        let mut pane = stream_from(perch, "");
        assert!(pane.read_until(": keep-alive", Duration::from_secs(5)), "a keep-alive line: {:?}", pane.data().0);
    });
}

#[test]
fn every_frame_is_one_data_line_with_a_string_type_and_the_rest_is_a_keep_alive() {
    let builder = Perch::builder().hub().env("SHELL", "/bin/sh").arg("--knob").arg("term_keepalive=100");
    with_perch(builder, |perch| {
        let id = open_one(perch);
        let mut pane = stream_from(perch, "");
        assert!(until(&mut pane, "term-reset", &id));
        assert_eq!(type_into(perch, &id, b"echo ok$((1+1))\r").status, 204);
        assert!(until_output(&mut pane, &id, "ok2"));
        assert!(pane.read_until(": keep-alive", Duration::from_secs(5)), "a keep-alive after the silence");
        // Only whole events: the last event may still be arriving, and a half line is not a frame yet.
        let (text, _) = pane.data();
        let whole = &text[..text.rfind("\n\n").map_or(0, |at| at + 2)];
        let mut data_lines = 0;
        for line in whole.lines() {
            if let Some(json) = line.strip_prefix("data: ") {
                let value: Value = serde_json::from_str(json).unwrap_or_else(|e| panic!("a frame parses: {e}: {json}"));
                assert!(value["type"].is_string(), "every frame has a string type: {json}");
                data_lines += 1;
            } else {
                assert!(line.is_empty() || line.starts_with(':'), "only frames, blank lines and keep-alives: {line:?}");
            }
        }
        assert!(data_lines >= 3, "several frames were read: {data_lines}");
    });
}

#[test]
fn a_perch_stop_ends_the_stream_with_a_stopping_end_and_the_process_exits_in_under_three_seconds() {
    let mut perch = Perch::builder().hub().env("SHELL", "/bin/sh").start();
    let id = open_one(&perch);
    let mut pane = stream_from(&perch, "");
    assert!(until(&mut pane, "term-reset", &id), "the stream is up: {:?}", pane.data().0);

    let started = Instant::now();
    let _ = perch.stop();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "the perch exited in {took:?}");
    assert!(pane.read_to_end(Duration::from_secs(3)), "the stream ended: {:?}", pane.data().0);
    assert_eq!(pane.end_frame()["reason"], "stopping");
}

#[test]
fn the_gate_and_the_refusals_are_the_routes_own() {
    with_shells(|perch| {
        let anonymous = Request::new(&perch.addr, "GET", "/hub/term/events").send(&perch.addr);
        assert_eq!(anonymous.status, 401, "no credential: {}", anonymous.head);

        let post = perch.apireq("POST", "/hub/term/events").send(&perch.addr);
        assert_eq!(post.status, 405, "{}", post.head);
        assert_eq!(post.header("allow").as_deref(), Some("GET"), "{}", post.head);

        for path in ["/hub/term/events?since=t1", "/hub/term/events?since=a:1", "/hub/term/events?since=t1:1&since=t1:2"] {
            let refused = perch.apireq("GET", path).send(&perch.addr);
            assert_eq!(refused.status, 400, "{path}: {}", refused.head);
        }
    });
}
