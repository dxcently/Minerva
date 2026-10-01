//! The proxy a confined browser context is forced through.
//!
//! Chrome sends every request of that context here: pages, frames, popups,
//! workers, and each hop of a redirect, since the browser follows redirects
//! itself. A request to an origin not on the list is refused without this
//! process ever connecting to it or looking it up. Plain HTTP arrives as
//! `GET http://host/path`, HTTPS as `CONNECT host:443`.
//!
//! Chrome's own background requests land here too and are refused like any
//! other, which is why what a page *tried* to open is read from the page
//! (`browser.rs`), not from this proxy's refusals.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::origin::origin_of;

type Allowed = Arc<Mutex<Vec<String>>>;

pub struct Proxy {
    pub addr: SocketAddr,
    allowed: Allowed,
}

impl Proxy {
    pub async fn start() -> std::io::Result<Proxy> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let allowed = Allowed::default();
        let shared = allowed.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let allowed = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(sock, allowed).await;
                });
            }
        });
        Ok(Proxy { addr, allowed })
    }

    /// Replace the list of origins requests may reach.
    pub fn allow(&self, origins: Vec<String>) {
        *self.allowed.lock().unwrap() = origins;
    }
}

fn check(allowed: &Mutex<Vec<String>>, origin: &str) -> bool {
    allowed.lock().unwrap().iter().any(|a| a == origin)
}

async fn serve(mut client: TcpStream, rules: Allowed) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
        let mut chunk = [0u8; 4096];
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let rest = &buf[head_end..];
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let (method, target, version) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or("HTTP/1.1"),
    );

    if method.eq_ignore_ascii_case("CONNECT") {
        let Ok(origin) = origin_of(&format!("https://{target}")) else {
            return client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        };
        if !check(&rules, &origin) {
            return client
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
        let Ok(mut upstream) = TcpStream::connect(target).await else {
            return client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
        };
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        upstream.write_all(rest).await?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        return Ok(());
    }

    let Some(after) = target.strip_prefix("http://") else {
        return client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
    };
    let Ok(origin) = origin_of(target) else {
        return client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
    };
    if !check(&rules, &origin) {
        // Closed with no answer, so the page sees a network error rather
        // than a page it could read.
        return Ok(());
    }
    let split = after.find(['/', '?']).unwrap_or(after.len());
    let authority = &after[..split];
    let path = if split == after.len() {
        "/"
    } else {
        &after[split..]
    };
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    let has_port = if hostport.starts_with('[') {
        hostport.contains("]:")
    } else {
        hostport.contains(':')
    };
    let addr = if has_port {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };

    let mut out = format!("{method} {path} {version}\r\n");
    for line in lines.filter(|l| !l.is_empty()) {
        let name = line
            .split(':')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "proxy-connection" | "proxy-authorization" | "connection" | "keep-alive"
        ) {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out.push_str("Connection: close\r\n\r\n");

    let Ok(mut upstream) = TcpStream::connect(&addr).await else {
        return client
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
            .await;
    };
    upstream.write_all(out.as_bytes()).await?;
    upstream.write_all(rest).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}
