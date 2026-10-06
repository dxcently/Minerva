//! Getting release files: `https://...` (GitHub Releases), `http://127.0.0.1:...` (the tests' fake server) or
//! `file:///dir` (a release folder on disk). Nothing fetched is trusted here; `update` verifies signatures.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub struct Source {
    base: String,
}

pub const MAX_SMALL: u64 = 1 << 20; // a manifest or a signature
pub const MAX_BUNDLE: u64 = 1 << 30;

impl Source {
    pub fn new(base: &str) -> Source {
        Source { base: base.trim_end_matches('/').to_string() }
    }

    fn file_path(&self, name: &str) -> Option<PathBuf> {
        self.base.strip_prefix("file://").map(|d| Path::new(d).join(name))
    }

    /// Like `get`, but a file that is not there (no such file, HTTP 404) is `Ok(None)`.
    pub fn get_optional(&self, name: &str, max: u64, total: Option<Duration>) -> Result<Option<Vec<u8>>> {
        match self.reader(name, total)? {
            None => Ok(None),
            Some(r) => {
                let mut buf = Vec::new();
                r.take(max + 1).read_to_end(&mut buf).with_context(|| format!("reading {name}"))?;
                if buf.len() as u64 > max {
                    bail!("{name} is larger than {max} bytes; refusing");
                }
                Ok(Some(buf))
            }
        }
    }

    /// The whole file, at most `max` bytes; `total` bounds the whole request (None: a minute of silence is the limit).
    pub fn get(&self, name: &str, max: u64, total: Option<Duration>) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.reader(name, total)?.with_context(|| format!("{name} is not there"))?.take(max + 1).read_to_end(&mut buf).with_context(|| format!("reading {name}"))?;
        if buf.len() as u64 > max {
            bail!("{name} is larger than {max} bytes; refusing");
        }
        Ok(buf)
    }

    /// Stream to `dest` (created 0644 by the umask), at most `max` bytes.
    pub fn get_to(&self, name: &str, dest: &Path, max: u64) -> Result<u64> {
        let mut r = self.reader(name, None)?.with_context(|| format!("{name} is not there"))?.take(max + 1);
        let mut f = File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
        let n = std::io::copy(&mut r, &mut f).with_context(|| format!("downloading {name}"))?;
        f.flush()?;
        if n > max {
            bail!("{name} is larger than {max} bytes; refusing");
        }
        Ok(n)
    }

    fn reader(&self, name: &str, total: Option<Duration>) -> Result<Option<Box<dyn Read + Send + Sync>>> {
        if name.contains(['/', '\\']) || name.starts_with('.') {
            bail!("refusing to fetch {name:?}");
        }
        if let Some(p) = self.file_path(name) {
            return match File::open(&p) {
                Ok(f) => Ok(Some(Box::new(f))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e).with_context(|| format!("opening {}", p.display())),
            };
        }
        let url = format!("{}/{}", self.base, name);
        if !allowed_url(&url) {
            bail!("refusing a non-HTTPS release address: {url}");
        }
        let mut req = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(60))
            .user_agent(concat!("minerva-installer/", env!("MINERVA_BASE_VERSION")))
            .build()
            .get(&url);
        if let Some(t) = total {
            req = req.timeout(t);
        }
        match req.call() {
            Ok(resp) => Ok(Some(Box::new(resp.into_reader()))),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("GET {url}")),
        }
    }
}

/// `https://anything`, or plain `http://` only to the loopback host itself. The host is parsed out and compared exactly:
/// a prefix test would let `http://127.0.0.1.evil.example/` or `http://localhost.evil.example/` through.
pub fn allowed_url(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false; // userinfo: `http://127.0.0.1@evil.example/` has host evil.example
    }
    let host = if let Some(h) = authority.strip_prefix('[') {
        match h.split_once(']') {
            Some((inner, tail)) if tail.is_empty() || (tail.starts_with(':') && tail[1..].chars().all(|c| c.is_ascii_digit())) => format!("[{inner}]"),
            _ => return false,
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h.to_string(),
            Some(_) => return false,
            None => authority.to_string(),
        }
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

#[cfg(test)]
mod tests {
    use super::allowed_url;

    #[test]
    fn only_https_and_exact_loopback_hosts() {
        for ok in ["https://github.com/x", "http://127.0.0.1:48093/a", "http://127.0.0.1/a", "http://localhost:8080/a", "http://[::1]:9/a", "http://[::1]/a"] {
            assert!(allowed_url(ok), "{ok}");
        }
        for bad in [
            "http://127.0.0.1.evil.example/a",
            "http://localhost.evil.example/a",
            "http://127.0.0.1@evil.example/a",
            "http://evil.example/127.0.0.1",
            "http://localhostx/a",
            "http://127.0.0.10/a",
            "http://[::1]x/a",
            "ftp://127.0.0.1/a",
            "http://127.0.0.1:80x/a",
        ] {
            assert!(!allowed_url(bad), "{bad}");
        }
    }
}
