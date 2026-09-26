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
        let path = self.sudo_file.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no sudo_file configured; fixes are disabled")
        })?;
        let mut pw = std::fs::read(path)?;
        pw.push(b'\n');
        // `-p ''` keeps sudo's prompt out of stderr; `sh -c` so a pipeline or
        // `&&` chain runs wholly as root rather than only its first word.
        let out = self.exec(&format!("sudo -S -p '' sh -c {}", shell_quote(cmd)), Some(&pw));
        pw.iter_mut().for_each(|b| *b = 0);
        out
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
    fn root_without_a_sudo_file_refuses() {
        let t = SshTransport { argv: vec!["ssh".into()], sudo_file: None };
        assert_eq!(t.run_root("true").unwrap_err().kind(), io::ErrorKind::NotFound);
    }
}
