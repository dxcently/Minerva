//! Putting a dragged selection on the system clipboard.
//!
//! Two writers, because neither one is enough on its own. **OSC 52** is the
//! terminal's own clipboard escape: no dependency, survives ssh, and goes
//! through tmux with the usual passthrough wrapper — but it is
//! fire-and-forget, and a VTE-based terminal (GNOME Terminal and its
//! relatives) refuses clipboard writes by default without telling anyone.
//! So a **clipboard helper** on `PATH` gets the same string. Both setting
//! the same text is harmless, and between them every terminal worth using
//! is covered.

use std::io::Write;
use std::process::{Command, Stdio};

use base64::Engine;

/// Copy `text`; false if nothing took it.
pub fn copy(text: &str) -> bool {
    let mut out = std::io::stdout();
    let osc = out
        .write_all(osc52(text).as_bytes())
        .and_then(|()| out.flush())
        .is_ok();
    helper(text) || osc
}

/// `ESC ] 52 ; c ; <base64> BEL`, wrapped for tmux when we are inside it.
fn osc52(text: &str) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{payload}\x07");
    if std::env::var_os("TMUX").is_some() {
        // tmux forwards an escape only inside its passthrough, and only
        // with every inner ESC doubled.
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    }
}

/// The first clipboard helper on `PATH` that takes the text. Spawning is
/// the existence check; these tools daemonise to serve the selection, so
/// a thread reaps them rather than the UI waiting.
fn helper(text: &str) -> bool {
    for (bin, args) in [
        ("wl-copy", &[][..]),
        ("xclip", &["-selection", "clipboard"][..]),
        ("xsel", &["--clipboard", "--input"][..]),
        ("pbcopy", &[][..]),
    ] {
        // A helper for a display server that is not running would take the
        // text and then fail, hiding the next candidate behind it.
        let usable = match bin {
            "wl-copy" => std::env::var_os("WAYLAND_DISPLAY").is_some(),
            "xclip" | "xsel" => std::env::var_os("DISPLAY").is_some(),
            _ => true,
        };
        let Some(mut child) = usable
            .then(|| {
                Command::new(bin)
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()
            })
            .flatten()
        else {
            continue;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut pipe| pipe.write_all(text.as_bytes()).is_ok());
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        if wrote {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_escape_carries_the_text_base64() {
        assert!(osc52("hi").starts_with("\x1b]52;c;"));
        assert!(osc52("hi").contains(&base64::engine::general_purpose::STANDARD.encode("hi")));
    }
}
