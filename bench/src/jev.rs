//! jev as the scouts' chooser: `choose(context, options)` → one probability per
//! option, over jev's loopback `/call`. The loop starts its own jev service with
//! a fresh random token, so there is no standing secret to manage.

use crate::json::{self, Json};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// jev's byte budgets (`JEV_OPTION_TOKENS`, `JEV_CONTEXT_TOKENS`): a label
/// longer than this is one the chooser cannot see the end of.
pub const OPTION_BYTES: usize = 32;
pub const CONTEXT_BYTES: usize = 192;

pub trait Chooser {
    fn choose(&self, context: &str, options: &[String]) -> io::Result<Vec<f64>>;
}

pub struct HttpJev {
    pub port: u16,
    pub token: String,
}

impl HttpJev {
    fn post(&self, body: &str) -> io::Result<String> {
        let mut s = TcpStream::connect(("127.0.0.1", self.port))?;
        s.set_read_timeout(Some(Duration::from_secs(120)))?;
        write!(
            s,
            "POST /call HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.token,
            body.len(),
            body
        )?;
        let mut resp = String::new();
        s.read_to_string(&mut resp)?;
        let (head, body) = resp
            .split_once("\r\n\r\n")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no HTTP body"))?;
        if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            return Ok(dechunk(body));
        }
        Ok(body.to_string())
    }

    pub fn healthy(&self) -> bool {
        let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) else { return false };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        if write!(s, "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").is_err() {
            return false;
        }
        let mut resp = String::new();
        s.read_to_string(&mut resp).is_ok() && resp.starts_with("HTTP/1.1 200")
    }
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 || tail.len() < n {
            break;
        }
        out.push_str(&tail[..n]);
        rest = tail[n..].trim_start_matches("\r\n");
    }
    out
}

impl Chooser for HttpJev {
    fn choose(&self, context: &str, options: &[String]) -> io::Result<Vec<f64>> {
        let opts: Vec<String> = options.iter().map(|o| json::quote(o)).collect();
        let body = format!(
            r#"{{"method":"choose","args":{{"context":{},"options":[{}]}}}}"#,
            json::quote(context),
            opts.join(",")
        );
        let v = json::parse(&self.post(&body)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if v.get("ok").and_then(Json::as_bool) != Some(true) {
            let err = v.get("error").and_then(Json::as_str).unwrap_or("jev said no");
            return Err(io::Error::new(io::ErrorKind::Other, format!("jev: {err}")));
        }
        let probs = v
            .get("result")
            .and_then(|r| r.get("probs"))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "jev reply has no probs"))?;
        Ok(options.iter().map(|o| probs.get(o).and_then(Json::as_f64).unwrap_or(0.0)).collect())
    }
}

/// jev's server as a child of the loop, killed when dropped.
pub struct JevService {
    child: Child,
    pub client: HttpJev,
}

impl JevService {
    pub fn start(cmd: &[String], port: u16, ckpt: Option<&str>) -> io::Result<Self> {
        let (exe, args) = cmd
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty jev_cmd"))?;
        let token = random_token()?;
        let mut c = Command::new(exe);
        c.args(args)
            .env("EIDOLON_SERVICE_PORT", port.to_string())
            .env("EIDOLON_SERVICE_TOKEN", &token)
            // The air gap, set where the process is actually started: `entail`
            // loads openjev's weights through transformers, and some versions
            // still make a hub metadata check on load even though the weights
            // are already on disk (jev/get-openjev.sh put them there). Without
            // these two a run with no route to the internet is not guaranteed
            // clean. `extensions/jev/extension.rn` sets the same two on its
            // own service command, the other way this process gets started.
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(p) = ckpt {
            c.env("JEVLIKE_CKPT", p);
        }
        let child = c.spawn()?;
        let svc = JevService { child, client: HttpJev { port, token } };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !svc.client.healthy() {
            if Instant::now() > deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "jev did not answer /health in 60 s"));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Ok(svc)
    }
}

impl Drop for JevService {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn random_token() -> io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// The last `max` bytes of `s` on a char boundary: for a path, the end is the
/// part that names the thing.
pub fn tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = s.len() - max;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// The first `max` bytes of `s` on a char boundary.
pub fn head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    /// Prefers options containing `favour`, else scores by position.
    pub struct FakeJev {
        pub favour: String,
    }

    impl Chooser for FakeJev {
        fn choose(&self, _context: &str, options: &[String]) -> io::Result<Vec<f64>> {
            let raw: Vec<f64> = options
                .iter()
                .enumerate()
                .map(|(i, o)| if !self.favour.is_empty() && o.contains(&self.favour) { 10.0 } else { 1.0 / (i as f64 + 2.0) })
                .collect();
            let sum: f64 = raw.iter().sum();
            Ok(raw.iter().map(|r| r / sum).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_filename_end() {
        assert_eq!(tail("/home/mford/Music/very/deep/path/song.mp3", 12), "ath/song.mp3");
        assert_eq!(tail("short", 32), "short");
        assert_eq!(tail("aé", 1), "");
    }

    #[test]
    fn dechunks() {
        assert_eq!(dechunk("4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), "Wikipedia");
    }
}
