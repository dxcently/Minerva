//! The terminal host's two dependencies, doing what slice T1a assumes of them: `portable-pty` 0.9.0 gives a PTY pair and a login-style
//! child that leads its own session, and `alacritty_terminal` 0.26.0 turns the bytes that come off the master into a screen that reads
//! back as plain text, and answers a cursor-position query through its event listener.
//!
//! It is the scout's probe, kept as a test, so a version bump that changes any of this fails here and not in the middle of T1a. It runs a
//! real `/bin/sh`, and every wait is bounded.

use std::io::Read as _;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// What the screen asked the terminal to write back to the program (`Event::PtyWrite`): the answers to DSR and friends.
#[derive(Clone)]
struct Replies(Arc<Mutex<Vec<String>>>);

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            self.0.lock().unwrap().push(text);
        }
    }
}

#[test]
fn a_shell_on_a_pty_reads_back_as_plain_text_through_alacritty() {
    let pair = native_pty_system().openpty(PtySize { rows: 6, cols: 30, pixel_width: 0, pixel_height: 0 }).expect("openpty");
    let mut command = CommandBuilder::new("/bin/sh");
    // Colour, a second line, a cursor-position query (DSR 6), then more rows than the screen has, so some scroll into history.
    command.args(["-c", r#"printf 'hello\033[31mred\033[0m\r\nline2\r\n\033[6n'; for i in 1 2 3 4 5 6 7 8; do echo row$i; done; sleep 1"#]);
    let mut child = pair.slave.spawn_command(command).expect("spawning /bin/sh on the pty");
    drop(pair.slave);
    let pid = child.process_id().expect("the child has a pid");
    let leader = pair.master.process_group_leader();
    let mut reader = pair.master.try_clone_reader().expect("the master's reader");

    let size = TermSize::new(30, 6);
    let config = Config { scrolling_history: 100, ..Config::default() };
    let replies = Replies(Arc::new(Mutex::new(Vec::new())));
    let mut term = Term::new(config, &size, replies.clone());
    let mut parser: Processor = Processor::new();

    // The read ends with EOF or EIO when the shell is gone and the slave side has closed; the thread hands back what the screen holds.
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => parser.advance(&mut term, &buf[..n]),
            }
        }
        let history = term.grid().history_size() as i32;
        let rows: Vec<String> = (-history..term.screen_lines() as i32)
            .map(|line| term.bounds_to_string(Point::new(Line(line), Column(0)), Point::new(Line(line), Column(term.columns() - 1))).trim_end().to_string())
            .collect();
        let _ = done.send((rows, history));
    });
    let (rows, history) = result.recv_timeout(Duration::from_secs(10)).expect("the shell's output ended within 10 s");
    let status = child.wait().expect("the shell's exit");
    assert!(status.success(), "{status:?}");

    // The colour escape is gone and the text around it is joined: this is `term_read`'s view.
    assert_eq!(rows[0], "hellored", "{rows:?}");
    assert_eq!(rows[1], "line2", "{rows:?}");
    let text = rows.join("\n");
    assert!(!text.contains('\u{1b}'), "plain text has no escape bytes: {text:?}");
    // Eight rows after those, in order, with some of the first ones scrolled into history (6 screen rows, 100 of history).
    let from_row1: Vec<&str> = rows.iter().map(String::as_str).skip_while(|row| *row != "row1").filter(|row| !row.is_empty()).collect();
    assert_eq!(from_row1, ["row1", "row2", "row3", "row4", "row5", "row6", "row7", "row8"], "{rows:?}");
    assert!(history > 0, "the screen scrolled into its history, got {history}: {rows:?}");

    // DSR 6 is answered by the screen, through the listener, as a cursor position report: `ESC [ <row> ; <col> R`.
    let replies = replies.0.lock().unwrap().clone();
    assert_eq!(replies.len(), 1, "one query, one reply: {replies:?}");
    assert!(replies[0].starts_with("\u{1b}[") && replies[0].ends_with('R') && replies[0].contains(';'), "{:?}", replies[0]);

    // A `portable-pty` child is a session and process-group leader, which is what T1a's close (killpg and a /proc sweep) relies on. The
    // group was asked for right after the spawn, while the shell was still in its `sleep`, and the answer is the pty's foreground group.
    assert_eq!(leader, Some(pid as i32), "the child leads its own group");
}
