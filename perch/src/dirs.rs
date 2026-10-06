//! The folder picker's two routes (hub.md section 7a): `GET /hub/dirs?path=` lists the subdirectories of one folder
//! under the allowed roots, and `POST /hub/dirs {parent, name}` makes ONE new folder in one. Directories only: no file
//! name, size or content is ever in an answer.
//!
//! The rules, the same ones [`crate::tree`] applies to a session's cwd (its deny list and 8.3 test are reused):
//!
//! ```text
//!   1  path: absolute, no NUL, no `\`, no `.`/`..` segment, <= 4096 B             -> 404
//!   2  canonicalize (a symlink out of the roots dies here), must start_with a root -> 404
//!   3  every segment below that root against the deny list and the 8.3 shape,
//!      on the canonical remainder and on the spelling that was asked              -> 404
//!   4  must be a directory; read_dir bounded by SCAN_MAX
//!   5  entries: real directories only (a link is not offered), denied names, 8.3
//!      aliases and dot-folders dropped; sorted by name; cap MAX_ENTRIES           -> truncated
//! ```
//!
//! Every refusal is the same empty 404, so a denied name cannot be told from a missing one. With no path the answer is
//! the roots themselves, labelled for a person (`/mnt/c/Users/me` is `C:\Users\me`).

use std::path::{Path, PathBuf};

use serde::Serialize;

pub use crate::tree::{MAX_ENTRIES, SCAN_MAX};

const MAX_PATH: usize = 4096;
/// A folder name a person types is short; this is generous.
pub const MAX_NAME: usize = 100;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Dir {
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Debug)]
pub struct Listing {
    /// The folder listed (absolute), empty for the roots.
    pub path: String,
    /// What a person calls it: `C:\Users\me\work`, `~/work`.
    pub label: String,
    /// `None` at the roots; `""` (the roots) above a root; else the parent folder.
    pub parent: Option<String>,
    /// From the root down to this folder, for a breadcrumb.
    pub crumbs: Vec<Dir>,
    pub dirs: Vec<Dir>,
    pub truncated: bool,
}

/// The query: only `path`, at most once, decoded once. `None` is the 400.
pub fn parse_query(query: &str) -> Option<String> {
    if query.is_empty() {
        return Some(String::new());
    }
    let mut path = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if crate::files::decode(key)? != "path" || path.is_some() {
            return None;
        }
        path = Some(crate::files::decode(value)?);
    }
    path
}

/// A path as a person reads it: `/mnt/c/Users/me` is `C:\Users\me`, a path under `home` is `~/...`.
pub fn label(path: &Path, home: Option<&Path>) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("/mnt/") {
        let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic() {
            return format!("{}:\\{}", drive.to_ascii_uppercase(), tail.replace('/', "\\"));
        }
    }
    if let Some(home) = home {
        if path == home {
            return "~".into();
        }
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    text.into_owned()
}

/// Steps 1-3 and the directory test: the canonical folder and the root (floor) that holds it, or `None`.
pub fn resolve(roots: &[PathBuf], raw: &str) -> Option<(PathBuf, PathBuf)> {
    if raw.len() > MAX_PATH || raw.contains('\0') || raw.contains('\\') || !raw.starts_with('/') {
        return None;
    }
    if raw.split('/').any(|s| s == "." || s == "..") {
        return None;
    }
    let real = Path::new(raw).canonicalize().ok()?;
    // The shallowest root that holds it: the one that makes the most segments judged.
    let floor = roots.iter().filter(|root| real.starts_with(root)).min_by_key(|root| root.components().count())?;
    let name_of = |c: std::path::Component| c.as_os_str().to_string_lossy().into_owned();
    if real.strip_prefix(floor).ok()?.components().any(|c| crate::tree::refused(&name_of(c))) {
        return None;
    }
    // The spelling that was asked, too: on NTFS `realpath` keeps an 8.3 alias as it was typed.
    if let Ok(asked) = Path::new(raw).strip_prefix(floor)
        && asked.components().any(|c| crate::tree::refused(&name_of(c)))
    {
        return None;
    }
    real.is_dir().then(|| (real, floor.clone()))
}

/// The roots, labelled: the picker's first screen.
pub fn roots_listing(roots: &[PathBuf], home: Option<&Path>) -> Listing {
    let dirs = roots.iter().map(|root| Dir { name: label(root, home), path: root.to_string_lossy().into_owned() }).collect();
    Listing { path: String::new(), label: String::new(), parent: None, crumbs: Vec::new(), dirs, truncated: false }
}

/// A folder's subfolders, blocking (the caller runs it under `spawn_blocking`). `None` is the 404.
pub fn list(roots: &[PathBuf], home: Option<&Path>, raw: &str) -> Option<Listing> {
    list_bounded(roots, home, raw, SCAN_MAX)
}

fn list_bounded(roots: &[PathBuf], home: Option<&Path>, raw: &str, scan_max: usize) -> Option<Listing> {
    let trimmed = if raw.len() > 1 { raw.trim_end_matches('/') } else { raw };
    let (real, floor) = resolve(roots, trimmed)?;
    let (mut scanned, mut cut) = (0usize, false);
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&real).ok()? {
        if scanned >= scan_max {
            cut = true;
            break;
        }
        scanned += 1;
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.file_name().into_string() else { continue };
        // `d_type`, never followed: a link is not offered, so the picker cannot lead out by one.
        let Ok(kind) = entry.file_type() else { continue };
        if !kind.is_dir() || name.starts_with('.') || crate::tree::refused(&name) {
            continue;
        }
        names.push(name);
    }
    names.sort_by_key(|n| n.to_lowercase());
    let truncated = cut || names.len() > MAX_ENTRIES;
    names.truncate(MAX_ENTRIES);
    let dirs = names.into_iter().map(|name| Dir { path: real.join(&name).to_string_lossy().into_owned(), name }).collect();

    let mut crumbs = vec![Dir { name: label(&floor, home), path: floor.to_string_lossy().into_owned() }];
    let mut at = floor.clone();
    for part in real.strip_prefix(&floor).ok()?.components() {
        at.push(part);
        crumbs.push(Dir { name: part.as_os_str().to_string_lossy().into_owned(), path: at.to_string_lossy().into_owned() });
    }
    let parent = if real == floor { String::new() } else { real.parent()?.to_string_lossy().into_owned() };
    Some(Listing { path: real.to_string_lossy().into_owned(), label: label(&real, home), parent: Some(parent), crumbs, dirs, truncated })
}

#[derive(Debug, PartialEq)]
pub enum MakeError {
    BadName(&'static str),
    /// The parent is not a folder under a root (or is a denied one).
    Outside,
    Exists,
    Io(String),
}

/// A folder name a person may create: no separator, no `..`, no control character, no leading dot, at most
/// [`MAX_NAME`] characters, not a denied or 8.3-shaped name.
pub fn check_name(name: &str) -> std::result::Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("a folder needs a name");
    }
    if name.chars().count() > MAX_NAME {
        return Err("that name is too long (100 characters at most)");
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("a name cannot contain / or \\ or ..");
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("a name cannot contain control characters");
    }
    if name.starts_with('.') {
        return Err("a name cannot start with a dot");
    }
    if name != name.trim() || name.ends_with('.') {
        return Err("a name cannot start or end with a space, or end with a dot");
    }
    if crate::tree::refused(name) {
        return Err("that name is not allowed");
    }
    Ok(())
}

/// Makes `parent/name` (one level: `create_dir`, not `create_dir_all`), and answers its canonical path.
pub fn make(roots: &[PathBuf], parent: &str, name: &str) -> std::result::Result<PathBuf, MakeError> {
    check_name(name).map_err(MakeError::BadName)?;
    let parent = if parent.len() > 1 { parent.trim_end_matches('/') } else { parent };
    let (real, _floor) = resolve(roots, parent).ok_or(MakeError::Outside)?;
    let target = real.join(name);
    match std::fs::create_dir(&target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(MakeError::Exists),
        Err(e) => return Err(MakeError::Io(e.kind().to_string())),
    }
    // What was made is where it was asked for: the parent was canonical and the name has no separator.
    let made = target.canonicalize().map_err(|e| MakeError::Io(e.kind().to_string()))?;
    if made.parent() != Some(real.as_path()) {
        let _ = std::fs::remove_dir(&target);
        return Err(MakeError::Outside);
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<tmp>/a` and `<tmp>/ok` are the roots; `<tmp>/outside` is beside them.
    fn tree() -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in ["a/b", "a/c", "ok/inner", "outside"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        (dir, vec![root.join("a"), root.join("ok")])
    }

    #[test]
    fn the_roots_come_first_and_are_labelled() {
        assert_eq!(label(Path::new("/mnt/c/Users/me"), None), "C:\\Users\\me");
        assert_eq!(label(Path::new("/mnt/d"), None), "D:\\");
        assert_eq!(label(Path::new("/home/u"), Some(Path::new("/home/u"))), "~");
        assert_eq!(label(Path::new("/home/u/Minerva"), Some(Path::new("/home/u"))), "~/Minerva");
        assert_eq!(label(Path::new("/mnt/cc/x"), None), "/mnt/cc/x");
        let got = roots_listing(&[PathBuf::from("/mnt/c/Users/me"), PathBuf::from("/home/u")], Some(Path::new("/home/u")));
        let names: Vec<&str> = got.dirs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["C:\\Users\\me", "~"]);
        assert!(got.parent.is_none() && got.path.is_empty());
    }

    /// Mutation: list files, links, dot folders or denied names and this goes red.
    #[test]
    fn a_listing_is_subfolders_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in ["b", "Zed", "a", ".hidden", ".git", "secrets", "GIT~1", "id_rsa", "x/y"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(root.join("file.txt"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("x"), root.join("link")).unwrap();
        let got = list(std::slice::from_ref(&root), None, root.to_str().unwrap()).unwrap();
        let names: Vec<&str> = got.dirs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "x", "Zed"], "case-insensitive order, no file, link, dot folder, denied name or alias");
        assert_eq!(got.parent.as_deref(), Some(""), "a root's parent is the roots");
        assert_eq!(got.crumbs.len(), 1);
        let inner = list(std::slice::from_ref(&root), None, root.join("x").to_str().unwrap()).unwrap();
        assert_eq!(inner.parent.as_deref(), Some(root.to_str().unwrap()));
        assert_eq!(inner.crumbs.len(), 2);
    }

    /// Mutation: drop the starts_with check, the `..` test or the deny test and one of these opens.
    #[test]
    fn a_path_outside_the_roots_or_denied_is_refused() {
        let (_dir, roots) = tree();
        let a = roots[0].to_str().unwrap().to_string();
        assert!(list(&roots, None, &a).is_some());
        let outside = roots[0].parent().unwrap().join("outside");
        std::os::unix::fs::symlink(&outside, roots[0].join("escape")).unwrap();
        std::fs::create_dir(roots[0].join(".git")).unwrap();
        std::fs::create_dir(roots[0].join("GIT~1")).unwrap();
        std::fs::write(roots[0].join("f"), "x").unwrap();
        let bad_paths = [
            format!("{a}/.."),
            roots[0].parent().unwrap().to_str().unwrap().to_string(),
            outside.to_str().unwrap().to_string(),
            format!("{a}/escape"),
            format!("{a}/.git"),
            format!("{a}/GIT~1"),
            format!("{a}/b/../../outside"),
            "/".to_string(),
            "/etc".to_string(),
            "relative/path".to_string(),
            format!("{a}\\b"),
            format!("{a}/nonexistent"),
            format!("{a}/\0"),
            format!("{a}/f"),
        ];
        for bad in &bad_paths {
            assert!(list(&roots, None, bad).is_none(), "{bad:?}");
        }
        // A link that stays inside the roots is followed and judged by what it reaches.
        std::os::unix::fs::symlink(roots[0].join("b"), roots[0].join("inside")).unwrap();
        assert_eq!(list(&roots, None, &format!("{a}/inside")).unwrap().path, format!("{a}/b"));
    }

    #[test]
    fn the_scan_is_bounded_and_says_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for n in 0..30 {
            std::fs::create_dir(root.join(format!("d{n:02}"))).unwrap();
        }
        let cut = list_bounded(std::slice::from_ref(&root), None, root.to_str().unwrap(), 10).unwrap();
        assert!(cut.truncated && cut.dirs.len() <= 10);
        assert!(!list_bounded(std::slice::from_ref(&root), None, root.to_str().unwrap(), 30).unwrap().truncated);
    }

    /// Mutation: drop one name rule and its case goes red.
    #[test]
    fn a_new_folder_name_is_one_plain_name() {
        for ok in ["work", "my project", "a.b", "x-1_2"] {
            assert_eq!(check_name(ok), Ok(()), "{ok}");
        }
        let long = "n".repeat(101);
        for bad in [
            "", " ", "a/b", "a\\b", "..", "a..b", ".hidden", ".", "a\nb", "a\tb", "a\0b", " lead", "trail ", "dot.", "GIT~1", ".git",
            "secrets", long.as_str(),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(check_name(&"n".repeat(100)), Ok(()));
    }

    #[test]
    fn a_folder_is_made_once_inside_a_root() {
        let (_dir, roots) = tree();
        let a = roots[0].to_str().unwrap().to_string();
        let made = make(&roots, &a, "fresh").unwrap();
        assert_eq!(made, roots[0].join("fresh"));
        assert!(made.is_dir());
        assert_eq!(make(&roots, &a, "fresh"), Err(MakeError::Exists));
        assert!(matches!(make(&roots, &a, "x/y"), Err(MakeError::BadName(_))));
        let outside = roots[0].parent().unwrap();
        assert_eq!(make(&roots, outside.to_str().unwrap(), "evil"), Err(MakeError::Outside));
        assert!(!outside.join("evil").exists());
        assert_eq!(make(&roots, &format!("{a}/nonexistent"), "x"), Err(MakeError::Outside));
        assert_eq!(make(&roots, &format!("{a}/.."), "x"), Err(MakeError::Outside));
        assert!(!roots[0].join("nonexistent").exists());
    }

    #[test]
    fn the_query_is_path_once() {
        assert_eq!(parse_query(""), Some(String::new()));
        assert_eq!(parse_query("path=%2Fa%2Fb"), Some("/a/b".into()));
        for bad in ["x=1", "path=1&path=2", "path", "path=%zz", "path=1&x=2"] {
            assert_eq!(parse_query(bad), None, "{bad}");
        }
    }
}
