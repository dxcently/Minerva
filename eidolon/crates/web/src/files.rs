//! Static files for `--ui-dir`: one percent-decode, a refusal for anything
//! that could name a file outside the directory, and the type table.
//!
//! Every refusal is a 404 except a file over [`MAX_FILE`] (413). The resolved
//! path is canonicalized and must stay under the root canonicalized once at
//! start, which is what stops a symlink out. A hardlink out is served: a path
//! check cannot see one, and making it needs write access to the directory.
//! IO errors collapse into a status; a browser can act on none of them.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hyper::StatusCode;

/// Over it is 413, not 404, so the page's author is not sent hunting for a
/// typo; existence is already observable from a 200.
pub const MAX_FILE: u64 = 32 * 1024 * 1024;

/// A UI directory, canonicalized once at start and then only read from.
#[derive(Clone)]
pub struct Static {
    root: PathBuf,
}

impl Static {
    /// Called before the bind: a door that cannot serve its directory does not come up.
    pub fn new(dir: &Path) -> Result<Self> {
        let root = dir.canonicalize().with_context(|| format!("--ui-dir {} is not an existing directory", dir.display()))?;
        anyhow::ensure!(root.is_dir(), "--ui-dir {} is not a directory", root.display());
        Ok(Self { root })
    }

    /// The canonical path and its size: the file checked is the file read.
    pub fn resolve(&self, path: &str) -> Result<(PathBuf, u64), StatusCode> {
        let Some(relative) = relative(path) else { return Err(StatusCode::NOT_FOUND) };
        let named = if relative.as_os_str().is_empty() { self.root.join("index.html") } else { self.root.join(relative) };
        let real = named.canonicalize().map_err(|_| StatusCode::NOT_FOUND)?;
        if !real.starts_with(&self.root) {
            return Err(StatusCode::NOT_FOUND);
        }
        let meta = std::fs::metadata(&real).map_err(|_| StatusCode::NOT_FOUND)?;
        if !meta.is_file() {
            return Err(StatusCode::NOT_FOUND);
        }
        if meta.len() > MAX_FILE {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        Ok((real, meta.len()))
    }
}

/// Capped again at read time, in case the file grew since [`Static::resolve`].
pub fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(MAX_FILE).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Empty for `/`, which [`Static::resolve`] makes `index.html`. A trailing `/`
/// is an empty segment, so a directory is never named.
fn relative(raw: &str) -> Option<PathBuf> {
    let decoded = decode(raw)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }
    let rest = decoded.strip_prefix('/')?;
    if rest.is_empty() {
        return Some(PathBuf::new());
    }
    if rest.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
        return None;
    }
    Some(rest.split('/').collect())
}

/// Once only, so no input has two spellings that reach different files. A
/// malformed escape is refused, not guessed at.
fn decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let high = hex(*bytes.get(i + 1)?)?;
        let low = hex(*bytes.get(i + 2)?)?;
        out.push(high << 4 | low);
        i += 3;
    }
    String::from_utf8(out).ok()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// With `nosniff` on every static response, this table is all a browser
/// believes. Text types say `utf-8` so a browser does not guess.
pub fn content_type(path: &Path) -> &'static str {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn decoding_happens_once_and_malformed_escapes_are_refused() {
        assert_eq!(decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(decode("/%2e%2e").as_deref(), Some("/.."));
        assert_eq!(decode("/%2E%2e/secret").as_deref(), Some("/../secret"));
        assert_eq!(decode("/%00").as_deref(), Some("/\0"));
        assert_eq!(decode("/%2541").as_deref(), Some("/%41"), "one pass, not two");
        assert_eq!(decode("/a+b").as_deref(), Some("/a+b"), "`+` is a literal in a path");
        assert_eq!(decode("/\u{1f355}").as_deref(), Some("/\u{1f355}"));
        assert_eq!(decode("/aaa%"), None, "a `%` with nothing after it");
        assert_eq!(decode("/aa%2"), None, "a `%` with one hex digit");
        assert_eq!(decode("/a%zz"), None, "a `%` with two non-hex digits");
        assert_eq!(decode("/%ff"), None, "decoded bytes that are not UTF-8");
    }

    #[test]
    fn every_segment_that_could_leave_the_directory_is_refused() {
        for raw in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/..%2fsecret.txt",
            "/%2e%2e%2fsecret.txt",
            "/assets/../../secret.txt",
            "/assets/..%2f..%2fsecret.txt",
            "/..",
            "/.",
            "/assets/..",
            "//secret.txt",
            "/assets//secret.txt",
            "/..\\secret.txt",
            "/%5c..%5csecret.txt",
            "/assets\\app.js",
            "/%00",
            "/assets/%00.js",
            "/assets/",
            "relative",
            "/secret.txt%",
        ] {
            assert!(relative(raw).is_none(), "{raw} must not name a file");
        }
    }

    #[test]
    fn the_paths_that_are_inside_stay_inside() {
        assert_eq!(relative("/").unwrap(), PathBuf::new(), "/ is the directory itself");
        assert_eq!(relative("/index.html").unwrap(), Path::new("index.html"));
        assert_eq!(relative("/assets/app.js").unwrap(), Path::new("assets/app.js"));
        assert_eq!(relative("/assets/a%20b.css").unwrap(), Path::new("assets/a b.css"));
        assert_eq!(relative("/...").unwrap(), Path::new("..."), "three dots are a name");
    }

    #[test]
    fn a_ui_dir_that_is_not_a_directory_is_refused_at_start() {
        let dir = temp();
        let missing = dir.path().join("nowhere");
        let Err(err) = Static::new(&missing) else { panic!("a path that is not there must refuse the start") };
        let err = format!("{err:#}");
        assert!(err.contains(&missing.display().to_string()), "the refusal names the path: {err}");
        assert!(err.contains("not an existing directory"), "{err}");

        let file = dir.path().join("a.txt");
        std::fs::write(&file, "not a directory").unwrap();
        let Err(err) = Static::new(&file) else { panic!("a file must refuse the start") };
        let err = format!("{err:#}");
        assert!(err.contains(&file.display().to_string()), "the refusal names the path: {err}");
        assert!(err.contains("not a directory"), "{err}");
    }

    #[test]
    fn a_symlink_out_of_the_directory_resolves_out_of_it() {
        let dir = temp();
        let ui = dir.path().join("ui");
        std::fs::create_dir(&ui).unwrap();
        std::fs::write(ui.join("index.html"), "hi").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "s3cret").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), ui.join("escape.txt")).unwrap();
        std::os::unix::fs::symlink(ui.join("index.html"), ui.join("again.html")).unwrap();
        let static_ = Static::new(&ui).unwrap();

        assert_eq!(static_.resolve("/again.html").unwrap().1, 2, "a link that stays inside is a file");
        assert_eq!(static_.resolve("/index.html").unwrap().0, ui.join("index.html").canonicalize().unwrap());
        assert_eq!(static_.resolve("/escape.txt").unwrap_err(), StatusCode::NOT_FOUND);
        assert_eq!(static_.resolve("/../secret.txt").unwrap_err(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn resolve_serves_the_index_for_the_root_and_nothing_for_a_directory() {
        let dir = temp();
        let ui = dir.path().join("ui");
        std::fs::create_dir_all(ui.join("assets")).unwrap();
        std::fs::write(ui.join("index.html"), "<html></html>").unwrap();
        std::fs::write(ui.join("assets/app.js"), "let x = 1;\n").unwrap();
        let static_ = Static::new(&ui).unwrap();

        let (path, len) = static_.resolve("/").unwrap();
        assert_eq!(path, ui.join("index.html").canonicalize().unwrap());
        assert_eq!(len, 13);
        assert_eq!(static_.resolve("/assets/app.js").unwrap().1, 11);
        assert_eq!(static_.resolve("/assets").unwrap_err(), StatusCode::NOT_FOUND, "a directory is not a file");
        assert_eq!(static_.resolve("/assets/").unwrap_err(), StatusCode::NOT_FOUND);
        assert_eq!(static_.resolve("/missing.js").unwrap_err(), StatusCode::NOT_FOUND);
    }

    /// `set_len` makes the files sparse, so they cost nothing on disk.
    #[test]
    fn a_file_over_the_cap_is_refused_without_being_read() {
        let dir = temp();
        let ui = dir.path().join("ui");
        std::fs::create_dir(&ui).unwrap();
        let big = ui.join("big.bin");
        std::fs::File::create(&big).unwrap().set_len(MAX_FILE + 1).unwrap();
        std::fs::File::create(ui.join("at-cap.bin")).unwrap().set_len(MAX_FILE).unwrap();
        let static_ = Static::new(&ui).unwrap();

        assert_eq!(static_.resolve("/big.bin").unwrap_err(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(static_.resolve("/at-cap.bin").unwrap().1, MAX_FILE, "the cap itself is served");
    }

    #[test]
    fn the_cap_bounds_what_is_read_and_the_type_table_is_whole() {
        let dir = temp();
        let path = dir.path().join("t");
        std::fs::write(&path, b"1234567890").unwrap();
        assert_eq!(read_capped(&path).unwrap(), b"1234567890");
        assert!(read_capped(&dir.path().join("nope")).is_err(), "a missing file is an error here");

        let types = [
            ("a.html", "text/html; charset=utf-8"),
            ("a.js", "text/javascript; charset=utf-8"),
            ("a.mjs", "text/javascript; charset=utf-8"),
            ("a.css", "text/css; charset=utf-8"),
            ("a.json", "application/json; charset=utf-8"),
            ("a.svg", "image/svg+xml; charset=utf-8"),
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.ico", "image/x-icon"),
            ("a.woff", "font/woff"),
            ("a.woff2", "font/woff2"),
            ("a.ttf", "font/ttf"),
            ("a.wasm", "application/wasm"),
            ("a.map", "application/json; charset=utf-8"),
            ("a.txt", "text/plain; charset=utf-8"),
            ("a.JS", "text/javascript; charset=utf-8"),
            ("app.js.map", "application/json; charset=utf-8"),
            ("a.unknownext", "application/octet-stream"),
            ("noextension", "application/octet-stream"),
            (".hidden", "application/octet-stream"),
        ];
        for (name, want) in types {
            assert_eq!(content_type(Path::new(name)), want, "{name}");
        }
    }
}
