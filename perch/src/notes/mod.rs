//! `/hub/notes[/...]`: the Notepad (slice N1): `<home>/notes/<name>.md`, written `0600`, with a trash beside it.
//!
//! A note is one plain text file. Its **name** is the file's stem and the only thing a request calls it by, and a name is
//! `^[a-z0-9][a-z0-9-]{0,47}$` that is not `trash` (a route). A name is parsed into a [`Name`] before it is joined to any path, nothing
//! here percent-decodes a path, and so no spelling of a request reaches a file but `<home>/notes/<name>.md` (or its twin in the trash).
//!
//! ```text
//!   GET  /hub/notes                      [{name, title, size, mtime}], newest first, 2000 at most
//!   POST /hub/notes {text?, title?}      201 {name, sha, mtime}: a new note, named by `names::birth_name`
//!   GET  /hub/notes/<n>                  {name, text, sha, mtime}
//!   POST /hub/notes/<n> {text, base}     200 {sha, mtime} | 409 {sha} (base is stale) | 413 | 400 (NUL)
//!   POST /hub/notes/<n>/rename {title}   200 {name}: the file becomes `names::slug(title)`, or `-2`, `-3` on a clash
//!   POST /hub/notes/<n>/trash            204: a rename into <home>/trash/notes/
//!   POST /hub/notes/<n>/send {..}        mail::send_note, which owns the body and the answer
//!   GET  /hub/notes/trash                [{name, title, size, mtime}]
//!   POST /hub/notes/trash/<n>/restore    200 {name} | 404 | 409 (a note has that name)
//!   POST /hub/notes/trash/empty {confirm: true}   200 {removed}: only plain `<name>.md` files directly inside the trash
//! ```
//!
//! **The etag** is the sha256 of the file, in lower-case hex. A save names the sha it was made from (`base`), and a save whose base is not
//! the file's sha now is a 409 that carries the sha now, so two tabs on one note are a visible conflict and never a silent overwrite.
//!
//! **Files.** Written by `harnox::fs::write_atomic_0600` (temp file `0600`, fsync, rename), in a directory made `0700` (an existing one
//! that is looser is tightened). Read through [`crate::rawfile::open_regular`]: `O_NOFOLLOW` and `O_NONBLOCK`, then `fstat` must say a
//! regular file, so a link or a FIFO planted under a note's name is a 404 and never read. A listing takes only `<name>.md` entries whose
//! `lstat` says regular file, so a symlink, a dotfile, a temp file or a directory is not a note. The folder itself must be a plain
//! directory of ours, not a link to one. Hand-placed files are never removed: the trash is emptied of listed notes only.
//!
//! **Bounds.** A note is at most 256 KiB of UTF-8 with no NUL (413, 400). At most 2000 are listed, and a create or a restore that would
//! make the 2001st is a 409, as is a trash that already holds 2000. A directory is scanned for at most [`SCAN_LIMIT`] entries, a listing
//! reads at most [`TITLE_WINDOW`] bytes of each note for its title, and a JSON body is read under its own cap.
//!
//! **One writer.** Every call that changes something takes [`State`]'s lock and does its file work on a blocking thread, so a check and the
//! move or write after it are not interleaved with another request of this perch. Reads take no lock: a note is replaced whole by a rename.
//!
//! **No home.** A perch started without `--home` has no notes: every route is the empty 404, as it was while this module was a stub.
//!
//! [`create`] is what mail's "save as note" calls. `POST /hub/notes/<n>/send` is a notes route that hands the request to
//! [`crate::mail::send_note`] once the name has passed.

use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::UNIX_EPOCH;

use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};

use crate::names;
use crate::serve::{body_of, json, problem, status, BoxBody, Ctx};

/// One note's text is at most this many bytes.
pub const MAX_TEXT: usize = 256 * 1024;
/// At most this many notes are listed, kept, or held in the trash.
pub const MAX_NOTES: usize = 2000;
const MAX_NAME: usize = 48;
/// A directory is looked at for at most this many entries, whatever is in it.
const SCAN_LIMIT: usize = 20_000;
/// A listing reads this many bytes of a note, no more, to find its title.
const TITLE_WINDOW: u64 = 4096;
/// A name that is a route, so it is not a note's.
const RESERVED: &[&str] = &["trash"];
/// A JSON body that carries a note: the text with every byte escaped as `\u00XX`, and room for the other fields.
const BODY_CAP: usize = (MAX_TEXT + 1024) * 6;
/// A JSON body that carries a title or a flag.
const SMALL_CAP: usize = 8 * 1024;
/// How many fresh birth names are tried before the create gives up, and how many `-n` suffixes a rename or a trash tries.
const TRIES: usize = 16;
const SUFFIXES: usize = 1000;

const NOT_PLAIN: &str = "the notes folder is not a plain directory of this user's";
const TOO_BIG: &str = "a note is 256 KiB at most";
const FULL: &str = "there are 2000 notes already; trash some first";
const TRASH_FULL: &str = "the notes trash holds 2000 already; empty it first";

/// What the notes keep in memory: the lock every read-then-write takes. `Default` is how [`Ctx`] gets one before any hook runs.
#[derive(Default)]
pub struct State {
    lock: Mutex<()>,
}

/// A note's name, checked. The only form in which one is joined to a path.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Name(String);

impl Name {
    fn parse(text: &str) -> Option<Name> {
        let bytes = text.as_bytes();
        let shape = (1..=MAX_NAME).contains(&bytes.len())
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        (shape && !RESERVED.contains(&text)).then(|| Name(text.to_string()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn file(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.md", self.0))
    }
}

/// Whether `text` is a note's name.
pub fn name_ok(text: &str) -> bool {
    Name::parse(text).is_some()
}

/// The first non-empty line of `text`, control characters dropped, at most 120 characters.
pub fn title_of(text: &str) -> String {
    for line in text.lines() {
        let clean: String = line.chars().filter(|c| !c.is_control()).collect();
        let clean = clean.trim();
        if !clean.is_empty() {
            return clean.chars().take(names::MAX_TITLE).collect::<String>().trim_end().to_string();
        }
    }
    String::new()
}

/// `base`, or with `-<n>` on it, still a legal name: the base is cut so the whole fits in [`MAX_NAME`].
fn suffixed(base: &str, n: usize) -> String {
    if n <= 1 {
        return base.to_string();
    }
    let tail = format!("-{n}");
    let head = &base[..base.len().min(MAX_NAME - tail.len())];
    format!("{}{tail}", head.trim_end_matches('-'))
}

/// The first of `base`, `base-2`, `base-3`, ... that is a name and that `free` accepts.
fn first_free(base: &str, mut free: impl FnMut(&Name) -> bool) -> Option<Name> {
    (1..=SUFFIXES).find_map(|n| Name::parse(&suffixed(base, n)).filter(|name| free(name)))
}

/// What a note starts with: the title on a line of its own (when there is one), a blank line, the text.
fn compose(title: &str, text: &str) -> Result<String, Fail> {
    let title = names::clean_title(title).map_err(|why| Fail::Why(StatusCode::BAD_REQUEST, why))?;
    let body = match (title.is_empty(), text.is_empty()) {
        (true, _) => text.to_string(),
        (false, true) => format!("{title}\n"),
        (false, false) => format!("{title}\n\n{text}"),
    };
    check_text(&body)?;
    Ok(body)
}

/// The bounds on a note's text: 256 KiB of bytes (413) and no NUL (400). UTF-8 is the type's.
fn check_text(text: &str) -> Result<(), Fail> {
    if text.len() > MAX_TEXT {
        return Err(Fail::Why(StatusCode::PAYLOAD_TOO_LARGE, TOO_BIG));
    }
    if text.contains('\0') {
        return Err(Fail::Why(StatusCode::BAD_REQUEST, "a note cannot contain NUL"));
    }
    Ok(())
}

/// Why a request is refused. `Missing` is the empty 404 of a note that is not there, or is not a plain file.
#[derive(Debug)]
enum Fail {
    Missing,
    Why(StatusCode, &'static str),
    Stale(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for Fail {
    fn from(e: std::io::Error) -> Self {
        Fail::Io(e)
    }
}

impl Fail {
    fn response(self) -> Response<BoxBody> {
        match self {
            Fail::Missing => status(StatusCode::NOT_FOUND),
            Fail::Why(code, why) => problem(code, why),
            Fail::Stale(sha) => json(
                StatusCode::CONFLICT,
                &serde_json::json!({ "error": "that note changed since this copy was read", "sha": sha }),
            ),
            Fail::Io(e) => {
                tracing::warn!(error = %e, "perch: the notes folder could not be used");
                problem(StatusCode::SERVICE_UNAVAILABLE, "that could not be done in the notes folder")
            }
        }
    }
}

/// A file that cannot be opened as a plain note is a 404 when it is not there or not a regular file, and the filesystem's trouble otherwise.
fn open_fail(e: std::io::Error) -> Fail {
    use std::io::ErrorKind::{InvalidInput, NotFound};
    let gone = matches!(e.kind(), NotFound | InvalidInput)
        || matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENXIO | libc::ENOTDIR | libc::EISDIR));
    if gone { Fail::Missing } else { Fail::Io(e) }
}

fn sha_of(bytes: &[u8]) -> String {
    harnox::crypto::hex(&harnox::crypto::sha256(bytes))
}

fn mtime_of(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|since| since.as_secs())
}

/// The two folders: where the notes are, and where the trashed ones wait.
struct Dirs {
    notes: PathBuf,
    trash: PathBuf,
}

impl Dirs {
    fn of(home: &Path) -> Self {
        Self { notes: home.join("notes"), trash: home.join("trash").join("notes") }
    }
}

/// Whether `dir` is there. Anything at that path but a real directory (a link to one included) is an error: the notes live in a
/// plain directory or nowhere.
fn folder(dir: &Path) -> Result<bool, Fail> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(Fail::Why(StatusCode::SERVICE_UNAVAILABLE, NOT_PLAIN)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// [`folder`] for a call that needs the notes there.
fn need_folder(dir: &Path) -> Result<(), Fail> {
    if folder(dir)? { Ok(()) } else { Err(Fail::Missing) }
}

/// Makes `dir` `0700` if it is not there, and checks it is a plain directory that is ours and closed to everyone else.
fn make_folder(dir: &Path) -> Result<(), Fail> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // `create_dir_all` takes a link to a directory for a directory: look at the thing itself.
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(Fail::Why(StatusCode::SERVICE_UNAVAILABLE, NOT_PLAIN));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Whether anything at all is at `path`: a link, a FIFO or a directory is a name taken. An error that is not "not found" counts as taken.
fn taken(path: &Path) -> bool {
    !matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// A regular file `<name>.md` directly in a folder, as a listing found it.
struct Found {
    name: Name,
    path: PathBuf,
    size: u64,
    mtime: Option<u64>,
}

/// The notes in `dir`: entries called `<name>.md` whose `lstat` says regular file, nothing else, and at most [`SCAN_LIMIT`] looked at.
fn scan(dir: &Path) -> std::io::Result<Vec<Found>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries.take(SCAN_LIMIT).flatten() {
        let file = entry.file_name();
        let Some(name) = file.to_str().and_then(|file| file.strip_suffix(".md")).and_then(Name::parse) else { continue };
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.is_file() {
            found.push(Found { name, path, size: meta.len(), mtime: mtime_of(&meta) });
        }
    }
    Ok(found)
}

/// One note in a listing. `mtime` is unix seconds, `null` when the filesystem does not say.
#[derive(serde::Serialize)]
struct Row {
    name: String,
    title: String,
    size: u64,
    mtime: Option<u64>,
}

/// The title of the note at `path`, from the first [`TITLE_WINDOW`] bytes; empty when it cannot be read as one.
fn title_in(path: &Path) -> String {
    let Ok((file, _)) = crate::rawfile::open_regular(path) else { return String::new() };
    let mut head = Vec::new();
    if file.take(TITLE_WINDOW).read_to_end(&mut head).is_err() {
        return String::new();
    }
    // The window may end inside a character: the text up to there is what counts.
    let text = std::str::from_utf8(&head).unwrap_or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]).unwrap_or_default());
    title_of(text)
}

/// The listing of `dir`: newest first, then by name, at most [`MAX_NOTES`].
fn rows(dir: &Path) -> Result<Vec<Row>, Fail> {
    if !folder(dir)? {
        return Ok(Vec::new());
    }
    let mut found = scan(dir)?;
    found.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.name.0.cmp(&b.name.0)));
    found.truncate(MAX_NOTES);
    Ok(found.into_iter().map(|f| Row { title: title_in(&f.path), name: f.name.0, size: f.size, mtime: f.mtime }).collect())
}

/// The bytes of the note at `path`, whole, and its mtime. A note over the cap is not read past it.
fn open_note(path: &Path) -> Result<(Vec<u8>, Option<u64>), Fail> {
    let (file, len) = crate::rawfile::open_regular(path).map_err(open_fail)?;
    if len > MAX_TEXT as u64 {
        return Err(Fail::Why(StatusCode::PAYLOAD_TOO_LARGE, TOO_BIG));
    }
    let mtime = file.metadata().ok().and_then(|meta| mtime_of(&meta));
    let mut bytes = Vec::with_capacity(len as usize);
    // One byte past the cap: a file that grew since the `fstat` is still bounded.
    file.take(MAX_TEXT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_TEXT {
        return Err(Fail::Why(StatusCode::PAYLOAD_TOO_LARGE, TOO_BIG));
    }
    Ok((bytes, mtime))
}

/// `GET /hub/notes/<n>`'s answer.
#[derive(serde::Serialize)]
struct Whole {
    name: String,
    text: String,
    sha: String,
    mtime: Option<u64>,
}

fn read(dirs: &Dirs, name: &Name) -> Result<Whole, Fail> {
    need_folder(&dirs.notes)?;
    let (bytes, mtime) = open_note(&name.file(&dirs.notes))?;
    let sha = sha_of(&bytes);
    match String::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Ok(Whole { name: name.0.clone(), text, sha, mtime }),
        _ => Err(Fail::Why(StatusCode::UNPROCESSABLE_ENTITY, "that file is not plain UTF-8 text")),
    }
}

/// `POST /hub/notes/<n>`'s answer: what the next save names as its base.
#[derive(serde::Serialize)]
struct Saved {
    sha: String,
    mtime: Option<u64>,
}

fn save(dirs: &Dirs, name: &Name, text: &str, base: &str) -> Result<Saved, Fail> {
    check_text(text)?;
    need_folder(&dirs.notes)?;
    let path = name.file(&dirs.notes);
    let (current, mtime) = open_note(&path)?;
    let sha = sha_of(&current);
    if sha != base {
        return Err(Fail::Stale(sha));
    }
    if current == text.as_bytes() {
        return Ok(Saved { sha, mtime });
    }
    harnox::fs::write_atomic_0600(&path, text.as_bytes())?;
    Ok(Saved { sha: sha_of(text.as_bytes()), mtime: std::fs::symlink_metadata(&path).ok().and_then(|meta| mtime_of(&meta)) })
}

/// What a create made: the name to use, and what the first save names as its base.
#[derive(serde::Serialize)]
pub struct Created {
    pub name: String,
    pub sha: String,
    pub mtime: Option<u64>,
}

fn create_in(dirs: &Dirs, title: &str, text: &str) -> Result<Created, Fail> {
    let body = compose(title, text)?;
    make_folder(&dirs.notes)?;
    if scan(&dirs.notes)?.len() >= MAX_NOTES {
        return Err(Fail::Why(StatusCode::CONFLICT, FULL));
    }
    let name = (0..TRIES)
        .filter_map(|_| Name::parse(&names::birth_name(names::random_u64())))
        .find(|name| !taken(&name.file(&dirs.notes)))
        .ok_or(Fail::Why(StatusCode::CONFLICT, "no free note name came up; try again"))?;
    let path = name.file(&dirs.notes);
    harnox::fs::write_atomic_0600(&path, body.as_bytes())?;
    Ok(Created { name: name.0, sha: sha_of(body.as_bytes()), mtime: std::fs::symlink_metadata(&path).ok().and_then(|meta| mtime_of(&meta)) })
}

fn rename(dirs: &Dirs, name: &Name, title: &str) -> Result<Name, Fail> {
    let title = names::clean_title(title).map_err(|why| Fail::Why(StatusCode::BAD_REQUEST, why))?;
    need_folder(&dirs.notes)?;
    let from = name.file(&dirs.notes);
    if !std::fs::symlink_metadata(&from).is_ok_and(|meta| meta.is_file()) {
        return Err(Fail::Missing);
    }
    let Some(base) = names::slug(&title) else {
        return Err(Fail::Why(StatusCode::BAD_REQUEST, "that title makes no file name; use some letters or digits"));
    };
    // The note's own name is always free to it, so a title that already names it changes nothing.
    let to = first_free(&base, |candidate| candidate == name || !taken(&candidate.file(&dirs.notes)))
        .ok_or(Fail::Why(StatusCode::CONFLICT, "no free name for that title"))?;
    if to != *name {
        std::fs::rename(&from, to.file(&dirs.notes))?;
    }
    Ok(to)
}

fn trash(dirs: &Dirs, name: &Name) -> Result<(), Fail> {
    need_folder(&dirs.notes)?;
    let from = name.file(&dirs.notes);
    if !std::fs::symlink_metadata(&from).is_ok_and(|meta| meta.is_file()) {
        return Err(Fail::Missing);
    }
    make_folder(&dirs.trash)?;
    if scan(&dirs.trash)?.len() >= MAX_NOTES {
        return Err(Fail::Why(StatusCode::CONFLICT, TRASH_FULL));
    }
    // A note of the same name was trashed before: this one waits under `-2`, and neither is lost.
    let to = first_free(name.as_str(), |candidate| !taken(&candidate.file(&dirs.trash)))
        .ok_or(Fail::Why(StatusCode::CONFLICT, "the trash has no free name for that note"))?;
    std::fs::rename(&from, to.file(&dirs.trash))?;
    Ok(())
}

fn restore(dirs: &Dirs, name: &Name) -> Result<Name, Fail> {
    need_folder(&dirs.trash)?;
    let from = name.file(&dirs.trash);
    if !std::fs::symlink_metadata(&from).is_ok_and(|meta| meta.is_file()) {
        return Err(Fail::Missing);
    }
    make_folder(&dirs.notes)?;
    let to = name.file(&dirs.notes);
    if taken(&to) {
        return Err(Fail::Why(StatusCode::CONFLICT, "a note with that name already exists; rename it or trash it first"));
    }
    if scan(&dirs.notes)?.len() >= MAX_NOTES {
        return Err(Fail::Why(StatusCode::CONFLICT, FULL));
    }
    std::fs::rename(&from, &to)?;
    Ok(name.clone())
}

/// Removes the notes in the trash for good, and nothing else in it: not a file that is not a plain `<name>.md`, not a link, not a folder.
fn empty(dirs: &Dirs) -> Result<usize, Fail> {
    if !folder(&dirs.trash)? {
        return Ok(0);
    }
    let mut removed = 0;
    for found in scan(&dirs.trash)? {
        match std::fs::remove_file(&found.path) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(error = %e, "perch: a trashed note could not be removed"),
        }
    }
    Ok(removed)
}

/// Where a request goes. Names are parsed here: a request that names no note is `Nobody` before anything else is asked.
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    List,
    Create,
    Read(Name),
    Save(Name),
    Rename(Name),
    Trash(Name),
    Send(Name),
    TrashList,
    Restore(Name),
    TrashEmpty,
    /// A route that exists, asked with a method it does not take.
    NotAllowed,
    Nobody,
}

/// `rest` is the path below `/hub/notes`, split on `/` and never decoded: `%2e%2e` is not a name.
fn pick(method: &Method, rest: &str) -> Pick {
    let (get, post) = (*method == Method::GET, *method == Method::POST);
    let by = |allowed: bool, pick: Pick| if allowed { pick } else { Pick::NotAllowed };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [""] if get => Pick::List,
        [""] => by(post, Pick::Create),
        ["trash"] => by(get, Pick::TrashList),
        ["trash", "empty"] => by(post, Pick::TrashEmpty),
        ["trash", name, "restore"] => Name::parse(name).map_or(Pick::Nobody, |name| by(post, Pick::Restore(name))),
        [name] => Name::parse(name).map_or(Pick::Nobody, |name| if get { Pick::Read(name) } else { by(post, Pick::Save(name)) }),
        [name, "rename"] => Name::parse(name).map_or(Pick::Nobody, |name| by(post, Pick::Rename(name))),
        [name, "trash"] => Name::parse(name).map_or(Pick::Nobody, |name| by(post, Pick::Trash(name))),
        [name, "send"] => Name::parse(name).map_or(Pick::Nobody, |name| by(post, Pick::Send(name))),
        _ => Pick::Nobody,
    }
}

/// The file work of a request that changes something: on a blocking thread, under the lock.
async fn locked<T: Send + 'static>(state: &Arc<State>, work: impl FnOnce() -> Result<T, Fail> + Send + 'static) -> Result<T, Fail> {
    let state = state.clone();
    joined(
        tokio::task::spawn_blocking(move || {
            let _held = state.lock.lock().unwrap_or_else(PoisonError::into_inner);
            work()
        })
        .await,
    )
}

/// The file work of a request that only reads: on a blocking thread, with no lock. A note is replaced whole by a rename, so a read
/// sees one version or the other, and a long listing does not hold up a save.
async fn unlocked<T: Send + 'static>(work: impl FnOnce() -> Result<T, Fail> + Send + 'static) -> Result<T, Fail> {
    joined(tokio::task::spawn_blocking(work).await)
}

fn joined<T>(done: Result<Result<T, Fail>, tokio::task::JoinError>) -> Result<T, Fail> {
    done.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "perch: a notes call did not finish");
        Err(Fail::Why(StatusCode::INTERNAL_SERVER_ERROR, "that did not finish"))
    })
}

fn reply<T: serde::Serialize>(code: StatusCode, done: Result<T, Fail>) -> Response<BoxBody> {
    match done {
        Ok(value) => json(code, &value),
        Err(fail) => fail.response(),
    }
}

/// A JSON object of `T` under `cap` (413 past it, 400 when it is not a `T`). An empty body is `T`'s default where there is one.
/// Only an object: serde would read `["text", "sha"]` as a struct too, and `[true]` as `{"confirm": true}`.
async fn asked<T: serde::de::DeserializeOwned>(
    req: Request<Incoming>,
    cap: usize,
    shape: &str,
    default: Option<T>,
) -> Result<T, Box<Response<BoxBody>>> {
    let (_, bytes) = body_of(req, cap).await?;
    if let (Some(default), true) = (default, bytes.is_empty()) {
        return Ok(default);
    }
    let refused = || Box::new(problem(StatusCode::BAD_REQUEST, shape));
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if !value.is_object() {
        return Err(refused());
    }
    serde_json::from_value(value).map_err(|_| refused())
}

#[derive(serde::Deserialize, Default)]
struct Make {
    text: Option<String>,
    title: Option<String>,
}

#[derive(serde::Deserialize)]
struct Write {
    text: String,
    base: String,
}

#[derive(serde::Deserialize)]
struct Retitle {
    title: String,
}

#[derive(serde::Deserialize)]
struct Confirm {
    confirm: bool,
}

/// Makes a note named by `names::birth_name` whose text is `title`, a blank line, `text` (either may be empty), for `POST /hub/notes` and
/// for mail's "save as note". Refused as the route would be; the `Err` is the response to send.
pub async fn create(ctx: &Ctx, title: &str, text: &str) -> Result<Created, Box<Response<BoxBody>>> {
    let Some(home) = ctx.registry.home() else { return Err(Box::new(status(StatusCode::NOT_FOUND))) };
    let (dirs, title, text) = (Dirs::of(home), title.to_string(), text.to_string());
    locked(&ctx.notes, move || create_in(&dirs, &title, &text)).await.map_err(|fail| Box::new(fail.response()))
}

/// A note as mail reads it to send it: the whole text, opened the way `GET /hub/notes/<n>` opens it.
pub struct Loaded {
    pub name: String,
    /// The first non-empty line, as the listing shows it.
    pub title: String,
    pub text: String,
    pub sha: String,
}

/// The note called `name`, for mail. A name that is not a note's name, a note that is not there or is not a plain file, and one that is
/// not text are refused as the route would refuse them; the `Err` is the response to send.
pub async fn load(ctx: &Ctx, name: &str) -> Result<Loaded, Box<Response<BoxBody>>> {
    let Some(name) = Name::parse(name) else { return Err(Box::new(status(StatusCode::NOT_FOUND))) };
    let Some(home) = ctx.registry.home() else { return Err(Box::new(status(StatusCode::NOT_FOUND))) };
    let dirs = Dirs::of(home);
    let whole = unlocked(move || read(&dirs, &name)).await.map_err(|fail| Box::new(fail.response()))?;
    Ok(Loaded { title: title_of(&whole.text), name: whole.name, text: whole.text, sha: whole.sha })
}

/// `/hub/notes` and everything under it. `rest` is `""` for the bare path, `<n>`, `<n>/rename`, `trash`, and so on.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let picked = pick(req.method(), rest);
    match picked {
        Pick::Nobody => return status(StatusCode::NOT_FOUND),
        Pick::NotAllowed => return status(StatusCode::METHOD_NOT_ALLOWED),
        _ => {}
    }
    // A perch started without `--home` has no notes: every route is the empty 404, as it was before this slice (the page reads it as "off").
    let Some(home) = ctx.registry.home() else { return status(StatusCode::NOT_FOUND) };
    let (dirs, state) = (Dirs::of(home), &ctx.notes);
    match picked {
        Pick::List => reply(StatusCode::OK, unlocked(move || rows(&dirs.notes)).await),
        Pick::TrashList => reply(StatusCode::OK, unlocked(move || rows(&dirs.trash)).await),
        Pick::Read(name) => reply(StatusCode::OK, unlocked(move || read(&dirs, &name)).await),
        Pick::Create => {
            let made = match asked(req, BODY_CAP, "expected {\"text\"?: string, \"title\"?: string}", Some(Make::default())).await {
                Ok(made) => made,
                Err(refusal) => return *refusal,
            };
            let done = create(ctx, made.title.as_deref().unwrap_or(""), made.text.as_deref().unwrap_or("")).await;
            match done {
                Ok(created) => json(StatusCode::CREATED, &created),
                Err(refusal) => *refusal,
            }
        }
        Pick::Save(name) => {
            let write = match asked::<Write>(req, BODY_CAP, "expected {\"text\": string, \"base\": the sha it was read at}", None).await {
                Ok(write) => write,
                Err(refusal) => return *refusal,
            };
            reply(StatusCode::OK, locked(state, move || save(&dirs, &name, &write.text, &write.base)).await)
        }
        Pick::Rename(name) => {
            let retitle = match asked::<Retitle>(req, SMALL_CAP, "expected {\"title\": string}", None).await {
                Ok(retitle) => retitle,
                Err(refusal) => return *refusal,
            };
            let done = locked(state, move || rename(&dirs, &name, &retitle.title)).await;
            reply(StatusCode::OK, done.map(|name| serde_json::json!({ "name": name.0 })))
        }
        Pick::Trash(name) => locked(state, move || trash(&dirs, &name)).await.map_or_else(Fail::response, |()| status(StatusCode::NO_CONTENT)),
        Pick::Restore(name) => {
            let done = locked(state, move || restore(&dirs, &name)).await;
            reply(StatusCode::OK, done.map(|name| serde_json::json!({ "name": name.0 })))
        }
        Pick::TrashEmpty => {
            match asked::<Confirm>(req, SMALL_CAP, "expected {\"confirm\": true}", None).await {
                Ok(Confirm { confirm: true }) => {}
                Ok(_) => return problem(StatusCode::BAD_REQUEST, "expected {\"confirm\": true}"),
                Err(refusal) => return *refusal,
            }
            let done = locked(state, move || empty(&dirs)).await;
            reply(StatusCode::OK, done.map(|removed| serde_json::json!({ "removed": removed })))
        }
        Pick::Send(name) => crate::mail::send_note(ctx, req, name.as_str()).await,
        Pick::Nobody | Pick::NotAllowed => unreachable!("answered above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, Dirs) {
        let home = tempfile::tempdir().unwrap();
        let dirs = Dirs::of(home.path());
        (home, dirs)
    }

    fn made(dirs: &Dirs, text: &str) -> Name {
        let created = create_in(dirs, "", text).unwrap_or_else(|_| panic!("a note is made"));
        Name::parse(&created.name).unwrap()
    }

    #[test]
    fn a_name_is_the_grammar_and_not_a_route() {
        let (longest, too_long) = ("x".repeat(48), "x".repeat(49));
        for ok in ["a", "0", "quiet-owl-lantern", "a-", "9-9", longest.as_str(), "empty", "send", "rename"] {
            assert!(name_ok(ok), "{ok:?}");
        }
        for bad in [
            "", "-a", "A", "Aa", "a_b", "a.b", "a b", "a/b", "a\\b", "..", ".", ".a", "a..b", "a%2fb", "%2e%2e", "a\0b", "é", "a\n", "trash",
            too_long.as_str(), "a.md",
        ] {
            assert!(!name_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_title_is_the_first_non_empty_line_without_control_characters_and_at_most_120_characters() {
        assert_eq!(title_of(""), "");
        assert_eq!(title_of("\n  \n\t\n"), "");
        assert_eq!(title_of("\n\n  hello world  \nsecond"), "hello world");
        assert_eq!(title_of("\r\n\r\nwindows\r\nline"), "windows");
        assert_eq!(title_of("a\u{7}b\u{1b}[31mc\0d"), "ab[31mcd", "control characters are dropped, not replaced");
        assert_eq!(title_of("\u{7}\u{8}\nreal"), "real", "a line of only control characters is empty");
        assert_eq!(title_of(&"é".repeat(300)).chars().count(), 120);
        assert_eq!(title_of(&format!("{}  tail", "b".repeat(118))), "b".repeat(118), "trimmed after the cut");
        assert_eq!(title_of("# markdown stays"), "# markdown stays");
    }

    #[test]
    fn a_suffix_always_fits_the_name_grammar() {
        assert_eq!(suffixed("groceries", 1), "groceries");
        assert_eq!(suffixed("groceries", 2), "groceries-2");
        let long = "a".repeat(48);
        for n in [2, 9, 10, 99, 100, 999, 1000] {
            let name = suffixed(&long, n);
            assert!(name_ok(&name) && name.ends_with(&format!("-{n}")), "{name}");
        }
        // Cut where a dash is the last character kept: the dash goes, so the suffix does not double it.
        let dashed = format!("{}-bb", "a".repeat(45));
        assert_eq!(suffixed(&dashed, 2), format!("{}-2", "a".repeat(45)));
        assert_eq!(first_free("trash", |_| true).unwrap().0, "trash-2", "a route is never a name");
        assert_eq!(first_free("a", |n| n.0 == "a-4").unwrap().0, "a-4");
        assert!(first_free("a", |_| false).is_none(), "bounded");
    }

    #[test]
    fn a_request_names_a_note_only_by_its_grammar_and_never_decoded() {
        let nobody = |method: &Method, rest: &str| pick(method, rest) == Pick::Nobody;
        for rest in [
            "..", "../x", "x/..", "%2e%2e", "a%2fb", "a%00b", "%2e%2e%2fetc", "A", "-a", "a_b", ".hidden", "x.md", "a/b/c/d", "//x", "/x", "x/",
            "x//rename", "trash/..", "trash/x/restore/y", "trash/A/restore", "trash/trash", "trash/trash/restore", "trash/../restore", "x/rename/y",
            "x/trash/y", "x/Send", "x/%73end", "\\x", "x\\", "x/..%2f", "x ", " x",
        ] {
            for method in [Method::GET, Method::POST] {
                assert!(nobody(&method, rest), "{method} {rest:?} is no route: {:?}", pick(&method, rest));
            }
        }
        let n = |s: &str| Name::parse(s).unwrap();
        assert_eq!(pick(&Method::GET, ""), Pick::List);
        assert_eq!(pick(&Method::POST, ""), Pick::Create);
        assert_eq!(pick(&Method::GET, "x"), Pick::Read(n("x")));
        assert_eq!(pick(&Method::POST, "x"), Pick::Save(n("x")));
        assert_eq!(pick(&Method::POST, "x/rename"), Pick::Rename(n("x")));
        assert_eq!(pick(&Method::POST, "x/trash"), Pick::Trash(n("x")));
        assert_eq!(pick(&Method::POST, "x/send"), Pick::Send(n("x")));
        assert_eq!(pick(&Method::GET, "trash"), Pick::TrashList);
        assert_eq!(pick(&Method::POST, "trash/empty"), Pick::TrashEmpty);
        assert_eq!(pick(&Method::POST, "trash/x/restore"), Pick::Restore(n("x")));
        // A note called `empty` or `send` is still a note, and `trash` is the one name that is a route.
        assert_eq!(pick(&Method::GET, "empty"), Pick::Read(n("empty")));
        assert_eq!(pick(&Method::POST, "trash/empty"), Pick::TrashEmpty);
        for (method, rest) in [
            (Method::PUT, ""), (Method::DELETE, "x"), (Method::GET, "x/rename"), (Method::GET, "x/trash"), (Method::GET, "x/send"),
            (Method::POST, "trash"), (Method::GET, "trash/empty"), (Method::GET, "trash/x/restore"), (Method::HEAD, "x"),
        ] {
            assert_eq!(pick(&method, rest), Pick::NotAllowed, "{method} {rest:?}");
        }
    }

    #[test]
    fn a_new_note_starts_with_its_title_on_a_line_of_its_own() {
        assert_eq!(compose("", "just text").unwrap(), "just text");
        assert_eq!(compose("groceries", "").unwrap(), "groceries\n");
        assert_eq!(compose("  groceries  ", "milk").unwrap(), "groceries\n\nmilk");
        assert_eq!(compose("", "").unwrap(), "");
        assert!(matches!(compose("a\nb", ""), Err(Fail::Why(StatusCode::BAD_REQUEST, _))));
        assert!(matches!(compose(&"t".repeat(121), ""), Err(Fail::Why(StatusCode::BAD_REQUEST, _))));
        assert!(matches!(compose("", &"x".repeat(MAX_TEXT + 1)), Err(Fail::Why(StatusCode::PAYLOAD_TOO_LARGE, _))));
        assert!(compose("", &"x".repeat(MAX_TEXT)).is_ok());
        assert!(matches!(compose("", "a\0b"), Err(Fail::Why(StatusCode::BAD_REQUEST, _))));
        // The cap is in bytes, and the title counts toward it.
        assert!(matches!(compose("t", &"x".repeat(MAX_TEXT - 1)), Err(Fail::Why(StatusCode::PAYLOAD_TOO_LARGE, _))));
        assert!(compose("", &"é".repeat(MAX_TEXT / 2)).is_ok());
        assert!(compose("", &"é".repeat(MAX_TEXT / 2 + 1)).is_err());
    }

    #[test]
    fn the_sha_is_the_files_sha256_in_lower_case_hex() {
        assert_eq!(sha_of(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn the_folder_must_be_a_plain_directory_that_is_ours_and_closed() {
        let (home, dirs) = dirs();
        assert!(!folder(&dirs.notes).unwrap(), "not there yet");
        assert!(rows(&dirs.notes).unwrap().is_empty());
        std::fs::create_dir(&dirs.notes).unwrap();
        std::fs::set_permissions(&dirs.notes, std::fs::Permissions::from_mode(0o755)).unwrap();
        make_folder(&dirs.notes).unwrap();
        assert_eq!(std::fs::metadata(&dirs.notes).unwrap().permissions().mode() & 0o777, 0o700, "a looser folder is closed");
        // A link to a directory is not the directory.
        let elsewhere = home.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::remove_dir(&dirs.notes).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &dirs.notes).unwrap();
        assert!(matches!(folder(&dirs.notes), Err(Fail::Why(StatusCode::SERVICE_UNAVAILABLE, _))));
        assert!(matches!(make_folder(&dirs.notes), Err(Fail::Why(StatusCode::SERVICE_UNAVAILABLE, _))));
        assert!(matches!(create_in(&dirs, "", "x"), Err(Fail::Why(StatusCode::SERVICE_UNAVAILABLE, _))));
        assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing was written through the link");
    }

    #[test]
    fn a_scan_takes_regular_note_files_and_nothing_else() {
        let (_home, dirs) = dirs();
        make_folder(&dirs.notes).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s3cret").unwrap();
        std::fs::write(dirs.notes.join("real.md"), "real").unwrap();
        std::fs::write(dirs.notes.join(".dot.md"), "dot").unwrap();
        std::fs::write(dirs.notes.join("real.txt"), "txt").unwrap();
        std::fs::write(dirs.notes.join("Upper.md"), "up").unwrap();
        std::fs::write(dirs.notes.join("trash.md"), "reserved").unwrap();
        std::fs::write(dirs.notes.join("real.0000.tmp"), "temp").unwrap();
        std::fs::create_dir(dirs.notes.join("dir.md")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dirs.notes.join("link.md")).unwrap();
        let listed: Vec<String> = scan(&dirs.notes).unwrap().into_iter().map(|f| f.name.0).collect();
        assert_eq!(listed, ["real"]);
        assert!(matches!(read(&dirs, &Name::parse("link").unwrap()), Err(Fail::Missing)), "a link is not read");
        assert!(matches!(read(&dirs, &Name::parse("dir").unwrap()), Err(Fail::Missing)), "a directory is not read");
    }

    #[test]
    fn a_save_needs_the_sha_it_was_read_at_and_an_unchanged_text_writes_nothing() {
        let (_home, dirs) = dirs();
        let name = made(&dirs, "first");
        let whole = read(&dirs, &name).unwrap();
        assert_eq!((whole.text.as_str(), whole.sha.as_str()), ("first", sha_of(b"first").as_str()));
        let saved = save(&dirs, &name, "second", &whole.sha).unwrap();
        assert_eq!(saved.sha, sha_of(b"second"));
        match save(&dirs, &name, "third", &whole.sha) {
            Err(Fail::Stale(current)) => assert_eq!(current, saved.sha, "the 409 carries the sha now"),
            _ => panic!("a stale base is a conflict"),
        }
        assert_eq!(read(&dirs, &name).unwrap().text, "second", "the loser wrote nothing");
        let path = name.file(&dirs.notes);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        save(&dirs, &name, "second", &saved.sha).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before, "the same text is not rewritten");
        assert!(matches!(save(&dirs, &Name::parse("nope").unwrap(), "x", &saved.sha), Err(Fail::Missing)));
    }

    #[test]
    fn a_rename_takes_the_slug_and_a_clash_gets_a_suffix_that_still_fits() {
        let (_home, dirs) = dirs();
        let (a, b) = (made(&dirs, "a"), made(&dirs, "b"));
        let first = rename(&dirs, &a, "Build Checklist").unwrap();
        assert_eq!(first.0, "build-checklist");
        assert_eq!(rename(&dirs, &b, "build checklist!").unwrap().0, "build-checklist-2");
        assert_eq!(rename(&dirs, &first, "BUILD  checklist").unwrap(), first, "its own name is free to it");
        assert!(matches!(rename(&dirs, &first, "!!!"), Err(Fail::Why(StatusCode::BAD_REQUEST, _))));
        assert!(matches!(rename(&dirs, &first, ""), Err(Fail::Why(StatusCode::BAD_REQUEST, _))));
        assert!(matches!(rename(&dirs, &first, "a\nb"), Err(Fail::Why(StatusCode::BAD_REQUEST, _))));
        assert!(matches!(rename(&dirs, &Name::parse("nope").unwrap(), "x"), Err(Fail::Missing)));
        let long = "word ".repeat(20);
        let (c, d) = (made(&dirs, "c"), made(&dirs, "d"));
        let (long_a, long_b) = (rename(&dirs, &c, &long).unwrap(), rename(&dirs, &d, &long).unwrap());
        assert_eq!(long_a.0.len(), MAX_NAME, "the slug is cut at 48");
        assert!(long_b.0.len() <= MAX_NAME && long_b.0.ends_with("-2") && long_a != long_b, "{long_b:?}");
        assert_eq!(rename(&dirs, &long_a, "trash").unwrap().0, "trash-2", "`trash` is a route, not a name");
        assert_eq!(std::fs::read_to_string(first.file(&dirs.notes)).unwrap(), "a", "the text came along");
    }

    #[test]
    fn a_trashed_note_waits_beside_its_name_and_comes_back_only_to_a_free_one() {
        let (_home, dirs) = dirs();
        let a = made(&dirs, "one");
        trash(&dirs, &a).unwrap();
        assert!(!taken(&a.file(&dirs.notes)) && taken(&a.file(&dirs.trash)));
        assert_eq!(std::fs::metadata(&dirs.trash).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(matches!(trash(&dirs, &a), Err(Fail::Missing)), "it is not in the notes now");
        // The same name again: both are kept.
        std::fs::write(a.file(&dirs.notes), "two").unwrap();
        trash(&dirs, &a).unwrap();
        let mut waiting: Vec<String> = scan(&dirs.trash).unwrap().into_iter().map(|f| f.name.0).collect();
        waiting.sort();
        assert_eq!(waiting, [a.0.clone(), format!("{}-2", a.0)]);
        // Restore: a free name comes back, a taken one is a conflict.
        assert_eq!(restore(&dirs, &a).unwrap(), a);
        assert!(matches!(restore(&dirs, &a), Err(Fail::Missing)));
        let second = Name::parse(&format!("{}-2", a.0)).unwrap();
        std::fs::write(second.file(&dirs.notes), "squatter").unwrap();
        assert!(matches!(restore(&dirs, &second), Err(Fail::Why(StatusCode::CONFLICT, _))));
        assert_eq!(std::fs::read_to_string(second.file(&dirs.notes)).unwrap(), "squatter", "nothing was overwritten");
        assert!(taken(&second.file(&dirs.trash)), "and the trashed one is still there");
    }

    #[test]
    fn the_trash_empties_of_listed_notes_only() {
        let (_home, dirs) = dirs();
        let (a, b) = (made(&dirs, "a"), made(&dirs, "b"));
        trash(&dirs, &a).unwrap();
        trash(&dirs, &b).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("target.md"), "keep").unwrap();
        std::fs::write(dirs.trash.join("keep.txt"), "x").unwrap();
        std::fs::write(dirs.trash.join("Odd.md"), "x").unwrap();
        std::fs::write(dirs.trash.join(".dot.md"), "x").unwrap();
        std::fs::create_dir(dirs.trash.join("sub.md")).unwrap();
        std::fs::write(dirs.trash.join("sub.md").join("in.md"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path().join("target.md"), dirs.trash.join("ln.md")).unwrap();
        assert_eq!(empty(&dirs).unwrap(), 2);
        let mut left: Vec<String> = std::fs::read_dir(&dirs.trash).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, [".dot.md", "Odd.md", "keep.txt", "ln.md", "sub.md"]);
        assert_eq!(std::fs::read_to_string(outside.path().join("target.md")).unwrap(), "keep");
        assert_eq!(empty(&dirs).unwrap(), 0);
    }

    /// The lock, deterministically (a race between real requests rarely shows on a fast filesystem): a second writer waits for the first to
    /// be done, and a reader does not wait for either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writers_take_turns_and_readers_do_not_wait_for_them() {
        use std::time::Duration;
        let state = Arc::new(State::default());
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let note = |log: &Arc<Mutex<Vec<&'static str>>>, what: &'static str| log.lock().unwrap().push(what);
        let (first_state, first_log) = (state.clone(), log.clone());
        let first = tokio::spawn(async move {
            locked(&first_state, move || {
                note(&first_log, "first in");
                std::thread::sleep(Duration::from_millis(300));
                note(&first_log, "first out");
                Ok(())
            })
            .await
        });
        while !log.lock().unwrap().contains(&"first in") {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let (second_state, second_log) = (state.clone(), log.clone());
        let second = tokio::spawn(async move {
            locked(&second_state, move || {
                note(&second_log, "second in");
                note(&second_log, "second out");
                Ok(())
            })
            .await
        });
        let reader_log = log.clone();
        unlocked(move || {
            note(&reader_log, "read");
            Ok(())
        })
        .await
        .unwrap();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(*log.lock().unwrap(), ["first in", "read", "first out", "second in", "second out"]);
    }

    #[test]
    fn the_count_is_bounded_both_ways() {
        let (_home, dirs) = dirs();
        make_folder(&dirs.notes).unwrap();
        for n in 0..MAX_NOTES {
            std::fs::write(dirs.notes.join(format!("n{n}.md")), format!("note {n}")).unwrap();
        }
        assert_eq!(rows(&dirs.notes).unwrap().len(), MAX_NOTES);
        assert!(matches!(create_in(&dirs, "", "one too many"), Err(Fail::Why(StatusCode::CONFLICT, _))));
        std::fs::write(dirs.notes.join("extra.md"), "x").unwrap();
        assert_eq!(rows(&dirs.notes).unwrap().len(), MAX_NOTES, "2001 on disk, 2000 listed");
        make_folder(&dirs.trash).unwrap();
        for n in 0..MAX_NOTES {
            std::fs::write(dirs.trash.join(format!("t{n}.md")), "t").unwrap();
        }
        assert!(matches!(trash(&dirs, &Name::parse("extra").unwrap()), Err(Fail::Why(StatusCode::CONFLICT, _))));
        std::fs::remove_file(dirs.notes.join("extra.md")).unwrap();
        assert!(matches!(restore(&dirs, &Name::parse("t1").unwrap()), Err(Fail::Why(StatusCode::CONFLICT, _))), "restoring into a full folder");
    }
}
