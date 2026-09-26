//! The brain: one turn to a hosted model through eidolon, its answer parsed
//! back as a JSON object. The loop asks the brain what to do; the brain does
//! not touch the target or keep state. It is the teacher in the loop — it
//! proposes actions and judges the chooser's picks — never trained itself.
//!
//! eidolon prints the model's text on stdout and its own session/cost lines on
//! stderr (verified 2026-09-26), so the answer is read from stdout alone. The
//! prompt asks for one JSON object; a model that still wraps it in prose is
//! tolerated by pulling the first balanced object out of the reply.

use crate::json::{self, Json};
use std::io;
use std::process::{Command, Stdio};

pub trait Brain {
    /// Run one turn on `prompt` and return the model's reply as a JSON object.
    fn decide(&self, prompt: &str) -> io::Result<Json>;
}

/// `run_cmd` is eidolon up to but not including the model flag, e.g.
/// `["…/eidolon", "run"]` (or `["wsl", "-e", "…/eidolon", "run"]` from
/// Windows). The loop appends `-m <model> <prompt>`.
pub struct EidolonBrain {
    pub run_cmd: Vec<String>,
    pub model: String,
}

impl Brain for EidolonBrain {
    fn decide(&self, prompt: &str) -> io::Result<Json> {
        let (exe, head) = self
            .run_cmd
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty run_cmd"))?;
        let out = Command::new(exe)
            .args(head)
            .arg("-m")
            .arg(&self.model)
            .arg(prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("eidolon exited {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()),
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        parse_reply(&stdout)
    }
}

/// The model's reply as a JSON object: the whole reply if it parses, else the
/// first balanced `{…}` inside it.
pub fn parse_reply(reply: &str) -> io::Result<Json> {
    let trimmed = reply.trim();
    if let Ok(v @ Json::Obj(_)) = json::parse(trimmed) {
        return Ok(v);
    }
    let obj = extract_json_object(reply)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no JSON object in the brain's reply"))?;
    match json::parse(obj) {
        Ok(v @ Json::Obj(_)) => Ok(v),
        Ok(_) => Err(io::Error::new(io::ErrorKind::InvalidData, "brain reply was not a JSON object")),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("brain reply parse: {e}"))),
    }
}

/// The first brace-balanced object in `s`, string- and escape-aware so a `}`
/// inside a JSON string does not close it early.
pub fn extract_json_object(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let start = b.iter().position(|&c| c == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..b.len() {
        let c = b[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Replays queued JSON strings, one per `decide` call; records every prompt.
    pub struct FakeBrain {
        pub replies: RefCell<VecDeque<String>>,
        pub prompts: RefCell<Vec<String>>,
    }

    impl FakeBrain {
        pub fn new(replies: &[&str]) -> Self {
            Self {
                replies: RefCell::new(replies.iter().map(|s| s.to_string()).collect()),
                prompts: RefCell::new(vec![]),
            }
        }
    }

    impl Brain for FakeBrain {
        fn decide(&self, prompt: &str) -> io::Result<Json> {
            self.prompts.borrow_mut().push(prompt.to_string());
            let reply = self
                .replies
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "FakeBrain ran dry"))?;
            parse_reply(&reply)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clean_object() {
        let v = parse_reply(r#"{"action":"scout","role":"files"}"#).unwrap();
        assert_eq!(v.get("action").and_then(Json::as_str), Some("scout"));
    }

    #[test]
    fn pulls_the_object_out_of_prose() {
        let reply = "Sure!\nHere is my decision:\n{\"action\": \"fix\", \"note\": \"has a } brace\"}\nHope that helps.";
        let v = parse_reply(reply).unwrap();
        assert_eq!(v.get("action").and_then(Json::as_str), Some("fix"));
        assert_eq!(v.get("note").and_then(Json::as_str), Some("has a } brace"));
    }

    #[test]
    fn nested_objects_balance() {
        let reply = "prefix {\"a\":{\"b\":1},\"c\":2} suffix";
        let obj = extract_json_object(reply).unwrap();
        assert_eq!(obj, r#"{"a":{"b":1},"c":2}"#);
    }

    #[test]
    fn a_reply_with_no_object_errors() {
        assert!(parse_reply("no json here").is_err());
    }
}
