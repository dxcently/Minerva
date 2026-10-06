//! `GET /hub/file?session=<id>&path=<rel>`: the bytes of ONE image file under a live (or saved, read-only) session's cwd, so a picture a
//! tool wrote into the chat's folder (`shot.png`) can be looked at. eidolon's tool results are text, so this is the
//! only way such an image reaches the page.
//!
//! The path rules are `/hub/tree`'s, because it is [`tree::resolve`] that applies them: the deny list and the 8.3
//! alias shape on every segment (of the path, of the cwd below its `--root`, and of what a link resolved to), the
//! canonical path under the cwd, and so no link out. What is added here is about the file:
//!
//! ```text
//!   a regular file (a FIFO or a directory is not read)                          -> 404
//!   the extension is png, jpg, jpeg, gif or webp (any case); svg is NOT one     -> 415
//!   the size is at most [`MAX_BYTES`]                                           -> 413
//!   the first bytes are that format's magic, and agree with the extension       -> 415
//! ```
//!
//! Everything before those is the same empty 404 as the tree, so a refused name cannot be told from a missing one.
//! The answer is the bytes with `nosniff`, a `default-src 'none'; sandbox` policy and `no-cache` (the file may be
//! rewritten under the same name). The listing slots of [`tree::Slots`] bound the blocking reads.

use std::io::Read as _;
use std::path::Path;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::HeaderValue;
use hyper::{Response, StatusCode};

use crate::serve::{BoxBody, Ctx, problem, status};
use crate::sessions::id_ok;
use crate::tree;

/// The largest file served.
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;

/// Why a file was not served, or what it is.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    NotFound,
    /// 404 with a sentence: a saved chat whose folder has moved, vanished or left the roots.
    Gone,
    /// 415: not a png, jpeg, gif or webp by name, or not by content.
    Type,
    /// 413.
    TooBig,
}

/// The media type of a format's leading bytes.
fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The type a name's extension claims, if it is one of the four.
fn by_name(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// Opens `path` for reading without ever blocking or following a link, and answers the file with its size **from the open descriptor**:
/// `O_NOFOLLOW` (a link swapped in after the path was resolved is refused), `O_NONBLOCK` (a FIFO swapped in does not wait for a writer,
/// which would hold a slot, a blocking thread and the answer for ever) and `O_CLOEXEC`; then `fstat` on the fd, which must be a regular
/// file. The size that is judged is the size of the file that is read, not of whatever the name pointed at a moment ago.
pub(crate) fn open_regular(path: &Path) -> std::io::Result<(std::fs::File, u64)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    Ok((file, meta.len()))
}

/// Blocking: the file `rel` names under `root`, with its media type. `max` is [`MAX_BYTES`] outside the tests.
pub fn read(root: &Path, floor: &Path, rel: &str, max: u64) -> Result<(&'static str, Vec<u8>), Refusal> {
    read_between(root, floor, rel, max, |_| {})
}

/// [`read`], with `between` run after the path is resolved and before the file is opened: the tests swap the file there.
fn read_between(root: &Path, floor: &Path, rel: &str, max: u64, between: impl FnOnce(&Path)) -> Result<(&'static str, Vec<u8>), Refusal> {
    let real = tree::resolve(root, floor, rel).ok_or(Refusal::NotFound)?;
    between(&real);
    // Opened first: a directory, a FIFO or a link is not a file at all (404), whatever it is called; then the names are judged.
    let (file, len) = open_regular(&real).map_err(|_| Refusal::NotFound)?;
    // The name the caller gave is judged, and so is the one a link led to: neither may be a text file under a png's name.
    let claimed = real.file_name().and_then(|n| n.to_str()).and_then(by_name);
    let named = rel.rsplit('/').next().and_then(by_name);
    let (Some(claimed), Some(named)) = (claimed, named) else { return Err(Refusal::Type) };
    if len > max {
        return Err(Refusal::TooBig);
    }
    // One byte past the cap: a file that grew since the `fstat` is still bounded.
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(max + 1).read_to_end(&mut bytes).map_err(|_| Refusal::NotFound)?;
    if bytes.len() as u64 > max {
        return Err(Refusal::TooBig);
    }
    match sniff(&bytes) {
        Some(kind) if kind == claimed && kind == named => Ok((kind, bytes)),
        _ => Err(Refusal::Type),
    }
}

/// `GET /hub/file`: the query is [`tree::parse_query`]'s (400), the session must be a live door of ours or a saved chat with a known folder ([`tree::base`]; 404 unknown,
/// 409 `{"state"}` otherwise), and a full house of slots is a 503; the rest is [`read`].
pub async fn serve(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some((id, rel)) = tree::parse_query(query) else { return status(StatusCode::BAD_REQUEST) };
    if !id_ok(&id) {
        return status(StatusCode::NOT_FOUND);
    }
    let (root, saved) = match tree::base(ctx, &id) {
        Ok(tree::Base::Live(root)) => (root, false),
        Ok(tree::Base::Saved(root)) => (root, true),
        Err(refusal) => return *refusal,
    };
    let Some(permit) = ctx.tree_slots.take() else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "too many reads in flight; retry");
    };
    let registry = ctx.registry.clone();
    let done = tokio::task::spawn_blocking(move || {
        let _slot = permit;
        let Some((root, floor)) = registry.tree_cwd(&root) else { return Err(if saved { Refusal::Gone } else { Refusal::NotFound }) };
        read(&root, &floor, &rel, MAX_BYTES)
    });
    match done.await {
        Ok(Ok((kind, bytes))) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", HeaderValue::from_static(kind))
            .header("content-length", bytes.len())
            .header("x-content-type-options", HeaderValue::from_static("nosniff"))
            .header("cache-control", HeaderValue::from_static("private, no-cache"))
            .header("content-security-policy", HeaderValue::from_static("default-src 'none'; sandbox"))
            .header("content-disposition", HeaderValue::from_static("inline"))
            .body(Full::new(Bytes::from(bytes)).map_err(|never| match never {}).boxed_unsync())
            .unwrap(),
        Ok(Err(Refusal::Gone)) => problem(StatusCode::NOT_FOUND, tree::SAVED_GONE),
        Ok(Err(Refusal::Type)) => problem(StatusCode::UNSUPPORTED_MEDIA_TYPE, "that file is not a png, jpeg, gif or webp image"),
        Ok(Err(Refusal::TooBig)) => problem(StatusCode::PAYLOAD_TOO_LARGE, "that image is over 20 MiB"),
        _ => status(StatusCode::NOT_FOUND),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn get(d: &tempfile::TempDir, rel: &str) -> Result<(&'static str, Vec<u8>), Refusal> {
        read(d.path(), d.path(), rel, MAX_BYTES)
    }

    #[test]
    fn the_four_formats_are_served_by_name_and_by_content() {
        let d = dir();
        let cases: [(&str, &[u8], &str); 5] = [
            ("a.png", PNG, "image/png"),
            ("b.JPG", &[0xff, 0xd8, 0xff, 0xe0], "image/jpeg"),
            ("c.jpeg", &[0xff, 0xd8, 0xff, 0xdb], "image/jpeg"),
            ("d.gif", b"GIF89a....", "image/gif"),
            ("e.webp", b"RIFF\x01\0\0\0WEBPVP8 ", "image/webp"),
        ];
        for (name, bytes, kind) in cases {
            std::fs::write(d.path().join(name), bytes).unwrap();
            assert_eq!(get(&d, name), Ok((kind, bytes.to_vec())), "{name}");
        }
    }

    /// Mutation: add `svg` to `by_name` and the svg case goes red; skip the sniff and the disguised one does.
    #[test]
    fn svg_text_and_disguised_files_are_415() {
        let d = dir();
        std::fs::write(d.path().join("x.svg"), "<svg xmlns='http://www.w3.org/2000/svg'><script>1</script></svg>").unwrap();
        std::fs::write(d.path().join("notes.txt"), "hello").unwrap();
        std::fs::write(d.path().join("noext"), PNG).unwrap();
        std::fs::write(d.path().join("fake.png"), "<html><script>alert(1)</script>").unwrap();
        std::fs::write(d.path().join("gif-as.png"), b"GIF89a....").unwrap();
        std::fs::write(d.path().join("short.png"), "").unwrap();
        std::fs::write(d.path().join("png.txt"), PNG).unwrap();
        for name in ["x.svg", "notes.txt", "noext", "fake.png", "gif-as.png", "short.png", "png.txt"] {
            assert_eq!(get(&d, name), Err(Refusal::Type), "{name}");
        }
        // A png under a link whose name is not an image is not served under the link's name either.
        std::os::unix::fs::symlink("a.png", d.path().join("alias.txt")).unwrap();
        std::fs::write(d.path().join("a.png"), PNG).unwrap();
        assert_eq!(get(&d, "alias.txt"), Err(Refusal::Type));
        std::os::unix::fs::symlink("fake.png", d.path().join("looks.png")).unwrap();
        assert_eq!(get(&d, "looks.png"), Err(Refusal::Type));
    }

    /// Mutation: drop the `meta.len() > max` test and this goes red; drop the `take` bound and a file that grew is unbounded.
    #[test]
    fn a_file_over_the_cap_is_413() {
        let d = dir();
        let mut big = PNG.to_vec();
        big.resize(100, 0);
        std::fs::write(d.path().join("big.png"), &big).unwrap();
        assert!(read(d.path(), d.path(), "big.png", 100).is_ok(), "exactly at the cap is served");
        assert_eq!(read(d.path(), d.path(), "big.png", 99), Err(Refusal::TooBig));
    }

    /// Mutation: bypass `tree::resolve` and the escapes, the denied name and the alias go red.
    #[test]
    fn every_way_out_or_to_a_denied_name_is_not_found() {
        let outer = dir();
        let work = outer.path().join("work");
        std::fs::create_dir_all(work.join(".git")).unwrap();
        std::fs::create_dir(work.join("sub")).unwrap();
        std::fs::write(outer.path().join("out.png"), PNG).unwrap();
        std::fs::write(work.join(".git/h.png"), PNG).unwrap();
        std::fs::write(work.join("secret.png"), PNG).unwrap();
        std::fs::write(work.join("a~1.png"), PNG).unwrap();
        std::fs::create_dir(work.join("GIT~1")).unwrap();
        std::fs::write(work.join("GIT~1/h.png"), PNG).unwrap();
        std::os::unix::fs::symlink("../out.png", work.join("up.png")).unwrap();
        std::os::unix::fs::symlink(outer.path().join("out.png"), work.join("abs.png")).unwrap();
        std::os::unix::fs::symlink(".git", work.join("hist")).unwrap();
        std::os::unix::fs::symlink(".git/h.png", work.join("viaq.png")).unwrap();
        std::fs::write(work.join("sub/ok.png"), PNG).unwrap();
        let abs = outer.path().join("out.png").display().to_string();
        for rel in [
            "../out.png",
            "sub/../../out.png",
            &abs,
            "/etc/passwd",
            "up.png",
            "abs.png",
            ".git/h.png",
            "hist/h.png",
            "viaq.png",
            "secret.png",
            "a~1.png",
            "GIT~1/h.png",
            "sub\\ok.png",
            "sub/",
            "sub",
            "missing.png",
            "",
            "sub//ok.png",
            "./sub/ok.png",
        ] {
            assert_eq!(read(&work, &work, rel, MAX_BYTES), Err(Refusal::NotFound), "{rel:?}");
        }
        assert!(read(&work, &work, "sub/ok.png", MAX_BYTES).is_ok());
    }

    /// The file is judged by the descriptor it was opened as: a FIFO swapped in after the path was resolved does not block the read,
    /// so four such reads at once leave every slot free. Mutation: `File::open` (no `O_NONBLOCK`) hangs all four threads.
    #[test]
    fn a_fifo_swapped_in_after_the_check_does_not_hold_a_thread() {
        use std::os::unix::ffi::OsStrExt as _;
        let d = dir();
        let root = d.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        for n in 0..4 {
            let (root, tx) = (root.clone(), tx.clone());
            std::thread::spawn(move || {
                let name = format!("s{n}.png");
                std::fs::write(root.join(&name), PNG).unwrap();
                let swap = |real: &Path| {
                    std::fs::remove_file(real).unwrap();
                    let fifo = std::ffi::CString::new(real.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
                };
                tx.send(read_between(&root, &root, &name, MAX_BYTES, swap)).unwrap();
            });
        }
        for _ in 0..4 {
            assert_eq!(rx.recv_timeout(Duration::from_secs(5)).expect("a read that does not hang"), Err(Refusal::NotFound));
        }
    }

    /// A link swapped in after the check is refused too (`O_NOFOLLOW`).
    #[test]
    fn a_link_swapped_in_after_the_check_is_not_followed() {
        let d = dir();
        std::fs::write(d.path().join("a.png"), PNG).unwrap();
        std::fs::write(d.path().join("other.png"), PNG).unwrap();
        let swap = |real: &Path| {
            std::fs::remove_file(real).unwrap();
            std::os::unix::fs::symlink(d.path().join("other.png"), real).unwrap();
        };
        assert_eq!(read_between(d.path(), d.path(), "a.png", MAX_BYTES, swap), Err(Refusal::NotFound));
    }

    /// A FIFO under a png's name is not opened (it would block the read).
    #[test]
    fn a_fifo_is_not_read() {
        use std::os::unix::ffi::OsStrExt as _;
        let d = dir();
        let fifo = std::ffi::CString::new(d.path().join("p.png").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(get(&d, "p.png"), Err(Refusal::NotFound));
    }
}
