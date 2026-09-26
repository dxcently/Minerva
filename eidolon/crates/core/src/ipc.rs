//! The local unix-socket plumbing the bridge listeners and the swarm doorbell
//! share: bind privately, read one bounded request.
//!
//! Three listeners (the Claude hook socket, the MCP bridge socket, the swarm
//! doorbell) and their clients used to repeat this by hand, and the repeats
//! had drifted: a failed `chmod 0600` was ignored — the socket sat at the
//! umask's mercy until someone noticed — bridge writes had no timeout, and a
//! request was read with `read_to_end`, so any process that could reach the
//! socket could make the server allocate without bound. The listeners now
//! fail closed: a socket that cannot be made private is not bound at all,
//! and an implausible request is refused before its bytes are buffered.

use std::path::Path;

/// The largest request a listener will read. Tool input can be a whole file;
/// it is not 64 MiB. A peer offering more is a bug or hostile, and the
/// connection is dropped before those bytes reach memory.
pub const MAX_REQUEST: u64 = 64 * 1024 * 1024;

/// Bind `path`, replacing a stale socket file, and refuse to serve from it
/// unless it is private: the `chmod 0600` is part of binding, and failing it
/// removes the socket rather than leaving a group- or world-readable listener
/// behind.
pub fn bind_private(path: &Path) -> anyhow::Result<tokio::net::UnixListener> {
    use anyhow::Context as _;
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)
        .with_context(|| format!("binding {}", path.display()))?;
    let secured = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    };
    if let Err(e) = secured {
        drop(listener);
        let _ = std::fs::remove_file(path);
        return Err(e).with_context(|| format!("securing {}", path.display()));
    }
    Ok(listener)
}

/// Read one request — everything up to the peer's half-close — bounded by
/// `max`. Over the limit is `InvalidData`, and the caller's answer to an
/// implausible peer is no reply at all: drop the connection.
pub async fn read_request_bounded<S>(stream: &mut S, max: u64) -> std::io::Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    // `take` stops at the cap without reading past it, so the refused bytes
    // are never buffered.
    let mut limited = stream.take(max + 1);
    let mut buf = Vec::new();
    limited.read_to_end(&mut buf).await?;
    if buf.len() as u64 > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a request of {} bytes exceeds the {max}-byte limit", buf.len()),
        ));
    }
    Ok(buf)
}

/// [`read_request_bounded`] at the ordinary limit.
pub async fn read_request<S>(stream: &mut S) -> std::io::Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    read_request_bounded(stream, MAX_REQUEST).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_bound_socket_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private.sock");
        let _listener = bind_private(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "the socket must be 0600"
        );
    }

    #[tokio::test]
    async fn a_request_within_the_limit_survives() {
        let mut r = &b"a bridge payload of ordinary size"[..];
        let got = read_request_bounded(&mut r, 1024).await.unwrap();
        assert_eq!(got, b"a bridge payload of ordinary size");
    }

    #[tokio::test]
    async fn an_over_limit_request_is_refused_without_buffering_it() {
        // An endless peer: whatever the cap, there is always more coming.
        let mut r = tokio::io::repeat(b'x');
        let err = read_request_bounded(&mut r, 4096).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn exactly_the_limit_is_not_over_the_limit() {
        let payload = vec![b'x'; 4096];
        let mut r = &payload[..];
        assert_eq!(read_request_bounded(&mut r, 4096).await.unwrap(), payload);
    }
}
