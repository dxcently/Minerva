//! The only way the swarm touches the target: one command in, its output back.
//! Nothing is installed on the box, so a scout sees what stock tools print and
//! a fix is a command run as the box's own user. ssh today; anything that can
//! run a command (WinRM for a Windows image) fits the same trait.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

pub trait Transport {
    /// Run `cmd` through the target's shell as the login user.
    fn run(&self, cmd: &str) -> io::Result<Out>;
    /// Run `cmd` as root (`sudo -S`, password on stdin).
    fn run_root(&self, cmd: &str) -> io::Result<Out>;
}

/// `argv` is the ssh invocation up to the host (`ssh -p 2222 -i KEY user@host`);
/// the remote command is appended as one argument, which ssh hands to the
/// remote login shell. The sudo password is read from `sudo_file` per call and
/// written to ssh's stdin, so it is never in an argv, a script or a log.
pub struct SshTransport {
    pub argv: Vec<String>,
    pub sudo_file: Option<PathBuf>,
}

impl SshTransport {
    fn exec(&self, remote: &str, stdin: Option<&[u8]>) -> io::Result<Out> {
        let (cmd, args) = self
            .argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty ssh command"))?;
        let mut child = Command::new(cmd)
            .args(args)
            .arg(remote)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(data)?;
        }
        let out = child.wait_with_output()?;
        Ok(Out {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

impl Transport for SshTransport {
    fn run(&self, cmd: &str) -> io::Result<Out> {
        self.exec(cmd, None)
    }

    fn run_root(&self, cmd: &str) -> io::Result<Out> {
        match &self.sudo_file {
            None => self.exec(&sudo_line(cmd, false), None),
            // A target that needs a password: read it per call, hand it to
            // sudo on stdin, and zero the buffer after.
            Some(path) => {
                let mut pw = std::fs::read(path)?;
                pw.push(b'\n');
                let out = self.exec(&sudo_line(cmd, true), Some(&pw));
                pw.iter_mut().for_each(|b| *b = 0);
                out
            }
        }
    }
}

/// The remote line that runs `cmd` as root. `sh -c` so a pipeline or `&&`
/// chain runs wholly as root rather than only its first word.
///
/// Without a password (`with_stdin == false`): `sudo -n`. The practice image
/// grants mford `NOPASSWD: ALL` (measured 2026-09-26), and `-n` never
/// prompts, so a target that *does* want a password fails at once with a
/// clear rc instead of hanging on a prompt nobody will answer.
/// With one: `sudo -S -p ''`, the password on stdin and the prompt kept out
/// of stderr.
pub fn sudo_line(cmd: &str, with_stdin: bool) -> String {
    let wrapped = format!("sh -c {}", shell_quote(cmd));
    if with_stdin {
        format!("sudo -S -p '' {wrapped}")
    } else {
        format!("sudo -n {wrapped}")
    }
}

/// One POSIX single-quoted word.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A leading `~/` means the invoking user's home, as it would in a shell.
pub fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => p.to_string(),
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::cell::RefCell;

    /// Answers by the first rule whose needle the command contains; records
    /// every command and whether it was asked for as root.
    #[derive(Default)]
    pub struct FakeTransport {
        pub rules: Vec<(String, Out)>,
        pub calls: RefCell<Vec<(bool, String)>>,
    }

    impl FakeTransport {
        pub fn answer(mut self, needle: &str, stdout: &str) -> Self {
            self.rules.push((needle.into(), Out { ok: true, stdout: stdout.into(), stderr: String::new() }));
            self
        }
        fn reply(&self, root: bool, cmd: &str) -> io::Result<Out> {
            self.calls.borrow_mut().push((root, cmd.to_string()));
            Ok(self
                .rules
                .iter()
                .find(|(n, _)| cmd.contains(n.as_str()))
                .map(|(_, o)| o.clone())
                .unwrap_or(Out { ok: true, stdout: String::new(), stderr: String::new() }))
        }
    }

    impl Transport for FakeTransport {
        fn run(&self, cmd: &str) -> io::Result<Out> {
            self.reply(false, cmd)
        }
        fn run_root(&self, cmd: &str) -> io::Result<Out> {
            self.reply(true, cmd)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_survives_embedded_single_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn root_line_is_non_interactive_without_a_password_and_stdin_with_one() {
        assert_eq!(sudo_line("id -u", false), "sudo -n sh -c 'id -u'");
        assert_eq!(sudo_line("a && b | c", true), "sudo -S -p '' sh -c 'a && b | c'");
    }

    /// Live: the real transport against the practice guest. Needs the guest
    /// up and `MINERVA_SSH` set to the ssh argv up to the host, e.g.
    /// `wsl -e ssh -o BatchMode=yes … -p 2222 -i ~/.ssh/minerva_agent mford@127.0.0.1`.
    /// Run with `cargo test --ignored live_guest -- --nocapture`.
    #[test]
    #[ignore]
    fn live_guest_runs_as_user_and_as_root() {
        let argv: Vec<String> = std::env::var("MINERVA_SSH")
            .expect("MINERVA_SSH")
            .split_whitespace()
            .map(String::from)
            .collect();
        let t = SshTransport { argv, sudo_file: None };
        let user = t.run("id -u").unwrap();
        assert!(user.ok, "user run failed: {}", user.stderr);
        assert_eq!(user.stdout.trim(), "1000", "expected mford (uid 1000)");
        let root = t.run_root("id -u && echo chain-ran").unwrap();
        assert!(root.ok, "root run failed: {}", root.stderr);
        assert_eq!(root.stdout.trim(), "0\nchain-ran", "sudo -n sh -c must run the whole chain as root");
        println!("live: user uid={} root chain ok", user.stdout.trim());
    }
}
