//! `/hub/notes`: the notepad's storage and routes (slice N1).
//!
//! Every test starts its own perch with a Minerva home of its own (a temp dir), so nothing reads or writes the operator's `~/Minerva`.
//! The name and path rules, the caps, the etag, rename, trash and restore, the modes of what is written, and the files that are not
//! notes (a link, a FIFO, a dotfile, a folder) are tested here; `src/notes/mod.rs` has the same rules as unit tests.

mod common;

use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::*;
use serde_json::{json, Value};

const MAX_TEXT: usize = 256 * 1024;

/// A perch with a Minerva home: `(perch, home)`. The temp dir around the home lives as long as the perch.
fn with_home() -> (Perch, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let root = base.path().canonicalize().unwrap();
    let args: Vec<std::ffi::OsString> = vec!["--root".into(), root.into(), "--home".into(), home.clone().into()];
    let perch = Perch::builder().args(args).hold(base).start();
    (perch, home)
}

fn body(answer: &Answer) -> Value {
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON body ({e}): {}{}", answer.head, answer.body))
}

fn sha_of(bytes: &[u8]) -> String {
    harnox::crypto::hex(&harnox::crypto::sha256(bytes))
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

/// `POST /hub/notes`, expected to make one: the whole 201 answer.
fn make(perch: &Perch, request: Value) -> Value {
    let answer = perch.post_json("/hub/notes", request);
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    body(&answer)
}

fn name_of(made: &Value) -> String {
    made["name"].as_str().unwrap().to_string()
}

fn save(perch: &Perch, name: &str, text: &str, base: &str) -> Answer {
    perch.post_json(&format!("/hub/notes/{name}"), json!({ "text": text, "base": base }))
}

fn read(perch: &Perch, name: &str) -> Value {
    let answer = perch.get(&format!("/hub/notes/{name}"));
    assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
    body(&answer)
}

fn names(list: &Answer) -> Vec<String> {
    assert_eq!(list.status, 200, "{}{}", list.head, list.body);
    body(list).as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap().to_string()).collect()
}

fn set_mtime(path: &Path, secs_ago: u64) {
    let when = SystemTime::now() - Duration::from_secs(secs_ago);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
}

fn mkfifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
}

#[test]
fn notes_are_behind_the_token() {
    let (perch, home) = with_home();
    let routes = [
        ("GET", "/hub/notes"),
        ("POST", "/hub/notes"),
        ("GET", "/hub/notes/x"),
        ("POST", "/hub/notes/x"),
        ("POST", "/hub/notes/x/rename"),
        ("POST", "/hub/notes/x/trash"),
        ("POST", "/hub/notes/x/send"),
        ("GET", "/hub/notes/trash"),
        ("POST", "/hub/notes/trash/x/restore"),
        ("POST", "/hub/notes/trash/empty"),
    ];
    for (method, path) in routes {
        let bare = Request::new(&perch.addr, method, path).body(br#"{"text":"x","base":"y","title":"t","confirm":true}"#.to_vec()).send(&perch.addr);
        assert_eq!(bare.status, 401, "{method} {path} with no credential: {}", bare.head);
        assert_eq!(bare.body, "", "{method} {path}: the 401 has no body");
        let wrong = Request::new(&perch.addr, method, path).bearer("Bearer not-the-token").send(&perch.addr);
        assert_eq!(wrong.status, 401, "{method} {path} with a wrong token: {}", wrong.head);
    }
    assert!(!home.join("notes").exists() && !home.join("trash").join("notes").exists(), "an unauthenticated request touched the home");
    // And with the token the same routes are the module's.
    assert_eq!(perch.get("/hub/notes").status, 200);
}

#[test]
fn without_a_home_there_are_no_notes_and_every_route_is_the_empty_404() {
    let perch = Perch::start();
    for (method, path) in
        [("GET", "/hub/notes"), ("POST", "/hub/notes"), ("GET", "/hub/notes/x"), ("POST", "/hub/notes/x"), ("GET", "/hub/notes/trash"), ("POST", "/hub/notes/trash/empty")]
    {
        let answer = perch.apireq(method, path).body(br#"{"text":"x","base":"y","confirm":true}"#.to_vec()).send(&perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{method} {path}: {}", answer.head);
    }
    // And the gate is still in front.
    assert_eq!(Request::new(&perch.addr, "GET", "/hub/notes").send(&perch.addr).status, 401);
}

#[test]
fn a_note_is_made_listed_read_and_saved_by_its_etag() {
    let (perch, home) = with_home();
    let (notes, file) = (home.join("notes"), |name: &str| home.join("notes").join(format!("{name}.md")));
    assert_eq!(body(&perch.get("/hub/notes")), json!([]), "no notes, and no folder made by looking");
    assert!(!notes.exists());

    let made = make(&perch, json!({ "text": "hello\nworld" }));
    let name = name_of(&made);
    let parts: Vec<&str> = name.split('-').collect();
    assert!(parts.len() == 3 && name.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "a birth name: {name}");
    assert_eq!(std::fs::read_to_string(file(&name)).unwrap(), "hello\nworld");
    assert_eq!(made["sha"], sha_of(b"hello\nworld"), "the answer names the sha the first save is based on");
    assert!(made["mtime"].is_u64());
    assert_eq!((mode(&notes), mode(&file(&name))), (0o700, 0o600), "owner-only, folder and file");

    let list = body(&perch.get("/hub/notes"));
    assert_eq!(list.as_array().unwrap().len(), 1);
    let row = &list[0];
    assert_eq!((row["name"].as_str(), row["title"].as_str(), row["size"].as_u64()), (Some(name.as_str()), Some("hello"), Some(11)));
    assert!(row["mtime"].is_u64());

    let whole = read(&perch, &name);
    assert_eq!((whole["name"].as_str(), whole["text"].as_str()), (Some(name.as_str()), Some("hello\nworld")));
    assert_eq!(whole["sha"], sha_of(b"hello\nworld"));
    assert_eq!(whole["sha"], sha_of(&std::fs::read(file(&name)).unwrap()), "the etag is the sha256 of the file");

    let saved = save(&perch, &name, "hello\nworld\nmore", whole["sha"].as_str().unwrap());
    assert_eq!(saved.status, 200, "{}{}", saved.head, saved.body);
    let saved = body(&saved);
    assert_eq!(saved["sha"], sha_of(b"hello\nworld\nmore"));
    assert!(saved["mtime"].is_u64());
    assert_eq!(std::fs::read_to_string(file(&name)).unwrap(), "hello\nworld\nmore");
    assert_eq!(mode(&file(&name)), 0o600, "a save keeps it owner-only");
    assert_eq!(read(&perch, &name)["sha"], saved["sha"]);

    // Saving what is already there is fine and writes nothing.
    let again = save(&perch, &name, "hello\nworld\nmore", saved["sha"].as_str().unwrap());
    assert_eq!((again.status, body(&again)["sha"].clone()), (200, saved["sha"].clone()));

    // A title and a text: the title is the first line, so it is the listing's title too.
    let titled = name_of(&make(&perch, json!({ "title": "  groceries  ", "text": "milk" })));
    assert_eq!(std::fs::read_to_string(file(&titled)).unwrap(), "groceries\n\nmilk");
    let only_title = name_of(&make(&perch, json!({ "title": "plans" })));
    assert_eq!(std::fs::read_to_string(file(&only_title)).unwrap(), "plans\n");
    let blank = name_of(&make(&perch, json!({})));
    assert_eq!(std::fs::read_to_string(file(&blank)).unwrap(), "");
    let empty_body = perch.post("/hub/notes");
    assert_eq!(empty_body.status, 201, "a POST with no body is a blank note: {}{}", empty_body.head, empty_body.body);
    let titles: std::collections::BTreeMap<String, String> = body(&perch.get("/hub/notes"))
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["name"].as_str().unwrap().to_string(), row["title"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(titles.len(), 5);
    assert_eq!((titles[&titled].as_str(), titles[&only_title].as_str(), titles[&blank].as_str()), ("groceries", "plans", ""));

    // Nothing but notes is left in the folder: no temp file from the atomic writes.
    for entry in std::fs::read_dir(&notes).unwrap() {
        let entry = entry.unwrap();
        assert!(entry.file_name().to_str().unwrap().ends_with(".md"), "{:?}", entry.file_name());
        assert_eq!(mode(&entry.path()), 0o600);
    }
    // Sixteen at once get sixteen different names.
    let threads: Vec<_> = (0..16)
        .map(|n| {
            let (addr, auth) = (perch.addr.clone(), perch.auth());
            std::thread::spawn(move || {
                let request = Request::new(&addr, "POST", "/hub/notes").bearer(auth).body(serde_json::to_vec(&json!({ "text": format!("note {n}") })).unwrap());
                let answer = request.send(&addr);
                assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
                body(&answer)["name"].as_str().unwrap().to_string()
            })
        })
        .collect();
    let mut made: Vec<String> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    made.sort();
    made.dedup();
    assert_eq!(made.len(), 16);
    assert_eq!(names(&perch.get("/hub/notes")).len(), 5 + 16);
}

#[test]
fn a_request_names_a_note_by_its_grammar_and_nothing_decoded_reaches_a_path() {
    let (perch, home) = with_home();
    // What a path that escapes would reach: a note-shaped file above the notes folder, and one beside the home.
    std::fs::write(home.join("secret.md"), "above the notes").unwrap();
    std::fs::write(home.parent().unwrap().join("secret.md"), "beside the home").unwrap();
    let long = "x".repeat(49);
    let hostile = [
        "..", ".", "%2e%2e", "%2e%2e%2fsecret", "..%2fsecret", "%2e%2e/secret", "../secret", "a%2fb", "a%2Fb", "%2fsecret", "%252e%252e", "a%00b", "%00",
        "A", "-a", "a_b", ".hidden", "secret.md", "x.md", "x.", "a%20b", "%C3%A9", "a/b/c", "//secret", "/secret", "..;/secret", "x/..", "x/./",
        long.as_str(),
    ];
    for name in hostile {
        for (method, path, request) in [
            ("GET", format!("/hub/notes/{name}"), json!(null)),
            ("POST", format!("/hub/notes/{name}"), json!({ "text": "pwned", "base": "x" })),
            ("POST", format!("/hub/notes/{name}/rename"), json!({ "title": "pwned" })),
            ("POST", format!("/hub/notes/{name}/trash"), json!(null)),
            ("POST", format!("/hub/notes/{name}/send"), json!({ "node": "self" })),
            ("POST", format!("/hub/notes/trash/{name}/restore"), json!(null)),
        ] {
            let mut sent = perch.apireq(method, &path);
            if !request.is_null() {
                sent = sent.body(serde_json::to_vec(&request).unwrap());
            }
            let answer = sent.send(&perch.addr);
            assert_eq!(answer.status, 404, "{method} {path}: {}{}", answer.head, answer.body);
            assert_eq!(answer.body, "", "{method} {path}: a name that is no name has no body to leak");
        }
    }
    assert_eq!(std::fs::read_to_string(home.join("secret.md")).unwrap(), "above the notes");
    assert_eq!(std::fs::read_to_string(home.parent().unwrap().join("secret.md")).unwrap(), "beside the home");
    assert!(!home.join("notes").exists() && !home.join("trash").exists(), "a refused name made nothing: {:?}", std::fs::read_dir(&home).unwrap().count());

    // `trash` is a route and never a note: GET is the trash list, POST has no meaning.
    assert_eq!(body(&perch.get("/hub/notes/trash")), json!([]));
    assert_eq!(perch.post_json("/hub/notes/trash", json!({ "text": "x", "base": "y" })).status, 405);
    // A name of the right shape that is not there is a plain 404.
    for (method, path) in [("GET", "/hub/notes/ghost"), ("POST", "/hub/notes/ghost/rename"), ("POST", "/hub/notes/ghost/trash")] {
        let answer = perch.apireq(method, path).body(br#"{"title":"x"}"#.to_vec()).send(&perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{method} {path}");
    }
    let ghost = save(&perch, "ghost", "text", "base");
    assert_eq!((ghost.status, ghost.body.as_str()), (404, ""), "a save to a note that is not there makes none");
    assert_eq!(perch.post("/hub/notes/trash/ghost/restore").status, 404);
    assert!(!home.join("notes").join("ghost.md").exists());
}

#[test]
fn wrong_methods_are_405_and_the_route_table_has_no_other_verbs() {
    let (perch, _home) = with_home();
    let name = name_of(&make(&perch, json!({ "text": "x" })));
    for (method, path) in [
        ("PUT", "/hub/notes".to_string()),
        ("DELETE", "/hub/notes".to_string()),
        ("PUT", format!("/hub/notes/{name}")),
        ("DELETE", format!("/hub/notes/{name}")),
        ("GET", format!("/hub/notes/{name}/rename")),
        ("GET", format!("/hub/notes/{name}/trash")),
        ("GET", format!("/hub/notes/{name}/send")),
        ("POST", "/hub/notes/trash".to_string()),
        ("GET", "/hub/notes/trash/empty".to_string()),
        ("GET", "/hub/notes/trash/x/restore".to_string()),
        ("DELETE", "/hub/notes/trash/x/restore".to_string()),
    ] {
        let answer = perch.apireq(method, &path).send(&perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (405, ""), "{method} {path}");
    }
    for path in ["/hub/notes/x/y", "/hub/notes/x/rename/y", "/hub/notes/x/trash/y", "/hub/notes/trash/x", "/hub/notes/trash/x/restore/y"] {
        assert_eq!(perch.get(path).status, 404, "{path}");
        assert_eq!(perch.post(path).status, 404, "{path}");
    }
    assert!(perch.get(&format!("/hub/notes/{name}")).status == 200, "and the note is still there");
}

#[test]
fn a_link_a_fifo_a_dotfile_and_a_folder_are_not_notes() {
    let (perch, home) = with_home();
    let real = name_of(&make(&perch, json!({ "text": "real" })));
    let notes = home.join("notes");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target");
    std::fs::write(&target, "outside secret").unwrap();
    std::os::unix::fs::symlink(&target, notes.join("link.md")).unwrap();
    std::os::unix::fs::symlink(outside.path(), notes.join("linked-dir.md")).unwrap();
    std::fs::write(notes.join(".dot.md"), "dot").unwrap();
    std::fs::write(notes.join("plain.txt"), "txt").unwrap();
    std::fs::write(notes.join("Upper.md"), "up").unwrap();
    std::fs::write(notes.join("trash.md"), "reserved").unwrap();
    std::fs::create_dir(notes.join("dir.md")).unwrap();
    mkfifo(&notes.join("pipe.md"));

    assert_eq!(names(&perch.get("/hub/notes")), std::slice::from_ref(&real), "only the regular note is listed");
    for name in ["link", "linked-dir", "dir", "pipe"] {
        let answer = perch.get(&format!("/hub/notes/{name}"));
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "GET {name}: a refused file is not read, and not named in the answer");
        let answer = save(&perch, name, "overwritten", &sha_of(b"outside secret"));
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "a save to {name}");
        let answer = perch.post_json(&format!("/hub/notes/{name}/rename"), json!({ "title": "moved" }));
        assert_eq!(answer.status, 404, "rename {name}: {}{}", answer.head, answer.body);
        assert_eq!(perch.post(&format!("/hub/notes/{name}/trash")).status, 404, "trash {name}");
    }
    assert!(std::fs::symlink_metadata(notes.join("link.md")).unwrap().file_type().is_symlink(), "the link was not replaced or moved");
    assert!(std::fs::symlink_metadata(notes.join("linked-dir.md")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside secret", "nothing went through the link");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1, "nothing was made beside the link's target");
    assert!(!notes.join("moved.md").exists() && !home.join("trash").join("notes").exists());
    assert!(std::fs::symlink_metadata(notes.join("pipe.md")).unwrap().file_type().is_fifo(), "the FIFO is still a FIFO");

    // The real note still works, and a create is not fooled by what is lying around.
    let whole = read(&perch, &real);
    assert_eq!(save(&perch, &real, "real, edited", whole["sha"].as_str().unwrap()).status, 200);
    make(&perch, json!({ "text": "another" }));
    assert_eq!(names(&perch.get("/hub/notes")).len(), 2);

    // The same in the trash: a link there is not listed, not restored, and not removed through.
    assert_eq!(perch.post(&format!("/hub/notes/{real}/trash")).status, 204);
    let trash = home.join("trash").join("notes");
    std::os::unix::fs::symlink(&target, trash.join("tlink.md")).unwrap();
    mkfifo(&trash.join("tpipe.md"));
    assert_eq!(names(&perch.get("/hub/notes/trash")), std::slice::from_ref(&real));
    for name in ["tlink", "tpipe"] {
        let answer = perch.post(&format!("/hub/notes/trash/{name}/restore"));
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "restore {name}");
    }
    assert!(!notes.join("tlink.md").exists() && !notes.join("tpipe.md").exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside secret");
}

#[test]
fn the_notes_folder_must_be_a_plain_directory() {
    let (perch, home) = with_home();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("a.md"), "not yours").unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), home.join("notes")).unwrap();
    for (method, path) in [("GET", "/hub/notes"), ("GET", "/hub/notes/a"), ("POST", "/hub/notes/a")] {
        let answer = perch.apireq(method, path).body(br#"{"text":"x","base":"y"}"#.to_vec()).send(&perch.addr);
        assert_ne!(answer.status, 200, "{method} {path} went through a linked folder: {}", answer.body);
        assert!(!answer.body.contains("not yours"), "{method} {path}: {}", answer.body);
    }
    assert_eq!(perch.get("/hub/notes").status, 503);
    assert_eq!(perch.post_json("/hub/notes", json!({ "text": "x" })).status, 503);
    assert_eq!(perch.post("/hub/notes/a/trash").status, 503, "a note is not trashed out of a linked folder");
    assert_eq!(std::fs::read_to_string(elsewhere.path().join("a.md")).unwrap(), "not yours");
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 1, "nothing was written through the link");
    assert!(!home.join("trash").join("notes").exists());

    // A folder that exists with loose modes is closed on the first write.
    std::fs::remove_file(home.join("notes")).unwrap();
    std::fs::create_dir(home.join("notes")).unwrap();
    std::fs::set_permissions(home.join("notes"), std::fs::Permissions::from_mode(0o755)).unwrap();
    make(&perch, json!({ "text": "x" }));
    assert_eq!(mode(&home.join("notes")), 0o700);
}

#[test]
fn a_note_is_256_kib_at_most_and_holds_no_nul() {
    let (perch, home) = with_home();
    let file = |name: &str| home.join("notes").join(format!("{name}.md"));
    let at_cap = "a".repeat(MAX_TEXT);
    let made = make(&perch, json!({ "text": at_cap }));
    let name = name_of(&made);
    assert_eq!(std::fs::metadata(file(&name)).unwrap().len(), MAX_TEXT as u64);
    let saved = save(&perch, &name, &"b".repeat(MAX_TEXT), made["sha"].as_str().unwrap());
    assert_eq!(saved.status, 200, "256 KiB exactly is allowed: {}{}", saved.head, saved.body);

    let over = "c".repeat(MAX_TEXT + 1);
    let before = std::fs::read(file(&name)).unwrap();
    let sha = sha_of(&before);
    let answer = save(&perch, &name, &over, &sha);
    assert_eq!(answer.status, 413, "{}{}", answer.head, answer.body);
    assert_eq!(std::fs::read(file(&name)).unwrap(), before, "a refused save wrote nothing");
    let answer = perch.post_json("/hub/notes", json!({ "text": over }));
    assert_eq!(answer.status, 413, "{}{}", answer.head, answer.body);
    assert_eq!(names(&perch.get("/hub/notes")).len(), 1, "and a refused create made no note");
    // The cap is bytes, not characters, and a title is part of the text.
    assert_eq!(perch.post_json("/hub/notes", json!({ "text": "é".repeat(MAX_TEXT / 2) })).status, 201);
    assert_eq!(perch.post_json("/hub/notes", json!({ "text": "é".repeat(MAX_TEXT / 2 + 1) })).status, 413);
    assert_eq!(perch.post_json("/hub/notes", json!({ "title": "t", "text": "x".repeat(MAX_TEXT - 1) })).status, 413);
    // A body past anything a note could need is refused before it is held.
    let auth = perch.auth();
    let huge = send_oversize(&perch.addr, "POST", "/hub/notes", Some(auth.as_str()), (MAX_TEXT + 1024) * 6 + 1);
    assert_eq!(huge.status, 413, "{}", huge.head);

    // NUL.
    for text in ["a\u{0}b", "\u{0}"] {
        let answer = save(&perch, &name, text, &sha);
        assert_eq!(answer.status, 400, "{}{}", answer.head, answer.body);
        assert!(body(&answer)["error"].as_str().unwrap().contains("NUL"), "{}", answer.body);
        assert_eq!(perch.post_json("/hub/notes", json!({ "text": text })).status, 400);
    }
    assert_eq!(std::fs::read(file(&name)).unwrap(), before);
    assert_eq!(names(&perch.get("/hub/notes")).len(), 2, "the 413s and the NULs made nothing: the first note and the two-byte-character one");

    // Not JSON, or not the right fields.
    // An array is not an object, though serde would read `["text", "sha"]` as the fields of a struct.
    let bad_saves: [&[u8]; 7] = [
        b"not json", b"[]", b"[\"text\", \"base\"]", b"{\"text\": 5, \"base\": \"x\"}", b"{\"base\": \"x\"}", b"{\"text\": \"x\"}",
        b"{\"text\": \"\\ud800\", \"base\": \"x\"}",
    ];
    for raw in bad_saves {
        let answer = perch.apireq("POST", &format!("/hub/notes/{name}")).body(raw.to_vec()).send(&perch.addr);
        assert_eq!(answer.status, 400, "{:?}: {}{}", String::from_utf8_lossy(raw), answer.head, answer.body);
    }
    let bad_creates: [&[u8]; 6] = [b"not json", b"[]", b"[\"text\"]", b"[null, null]", b"{\"text\": 5}", b"{\"title\": []}"];
    for raw in bad_creates {
        let answer = perch.apireq("POST", "/hub/notes").body(raw.to_vec()).send(&perch.addr);
        assert_eq!(answer.status, 400, "{:?}: {}{}", String::from_utf8_lossy(raw), answer.head, answer.body);
    }
    // A title is one line of 120 characters at most.
    let too_long = "t".repeat(121);
    for title in ["a\nb", "a\u{7}b", too_long.as_str()] {
        assert_eq!(perch.post_json("/hub/notes", json!({ "title": title })).status, 400, "{title:?}");
    }
    assert_eq!(perch.post_json("/hub/notes", json!({ "title": "t".repeat(120) })).status, 201);
}

#[test]
fn a_file_put_there_by_hand_that_is_not_text_is_listed_but_not_opened() {
    let (perch, home) = with_home();
    make(&perch, json!({ "text": "first, to make the folder" }));
    let notes = home.join("notes");
    std::fs::write(notes.join("latin.md"), b"caf\xe9 au lait\n").unwrap();
    std::fs::write(notes.join("nul.md"), b"a\0b").unwrap();
    std::fs::write(notes.join("big.md"), vec![b'x'; MAX_TEXT + 1]).unwrap();
    std::fs::write(notes.join("exact.md"), vec![b'y'; MAX_TEXT]).unwrap();
    for (name, status) in [("latin", 422), ("nul", 422), ("big", 413), ("exact", 200)] {
        let answer = perch.get(&format!("/hub/notes/{name}"));
        assert_eq!(answer.status, status, "{name}: {}", answer.head);
    }
    // The big one cannot be saved over from a copy the page never had.
    assert_eq!(save(&perch, "big", "small", &sha_of(&vec![b'x'; MAX_TEXT + 1])).status, 413);
    assert_eq!(std::fs::metadata(notes.join("big.md")).unwrap().len(), MAX_TEXT as u64 + 1);
    let list = body(&perch.get("/hub/notes"));
    let row = |name: &str| list.as_array().unwrap().iter().find(|row| row["name"] == name).unwrap().clone();
    assert_eq!(row("big")["size"], MAX_TEXT + 1);
    assert_eq!(row("latin")["title"], "caf", "the title is the text that is valid up to the first bad byte");
    assert_eq!(row("nul")["title"], "ab", "control characters are dropped from a title");
}

#[test]
fn the_list_is_newest_first_titled_by_its_first_line_and_stops_at_2000() {
    let (perch, home) = with_home();
    make(&perch, json!({ "text": "x" }));
    let notes = home.join("notes");
    std::fs::remove_file(std::fs::read_dir(&notes).unwrap().next().unwrap().unwrap().path()).unwrap();
    let long_line = "é".repeat(300);
    std::fs::write(notes.join("old.md"), "\n\n   \n  Old note  \nsecond line").unwrap();
    std::fs::write(notes.join("mid.md"), format!("\r\n\u{7}{long_line}\nrest")).unwrap();
    std::fs::write(notes.join("new.md"), "# new\n").unwrap();
    std::fs::write(notes.join("blank.md"), "\n \n\t\n").unwrap();
    for (name, ago) in [("old", 3000), ("mid", 2000), ("new", 1000), ("blank", 4000)] {
        set_mtime(&notes.join(format!("{name}.md")), ago);
    }
    let list = body(&perch.get("/hub/notes"));
    let rows = list.as_array().unwrap();
    assert_eq!(rows.iter().map(|r| r["name"].as_str().unwrap()).collect::<Vec<_>>(), ["new", "mid", "old", "blank"], "newest first");
    assert_eq!(rows[0]["title"], "# new");
    assert_eq!(rows[1]["title"].as_str().unwrap().chars().count(), 120, "a title is at most 120 characters, control characters dropped");
    assert_eq!(rows[2]["title"], "Old note");
    assert_eq!(rows[3]["title"], "");
    assert_eq!(rows[2]["size"], "\n\n   \n  Old note  \nsecond line".len());

    // 2000 are listed and no more are made.
    for n in 0..2003 {
        std::fs::write(notes.join(format!("bulk-{n}.md")), format!("bulk {n}")).unwrap();
    }
    let all = names(&perch.get("/hub/notes"));
    assert_eq!(all.len(), 2000);
    assert!(all.iter().all(|name| name.starts_with("bulk-")), "the 2000 newest are the ones listed, and the four old ones are cut");
    let refused = perch.post_json("/hub/notes", json!({ "text": "the 2001st" }));
    assert_eq!(refused.status, 409, "{}{}", refused.head, refused.body);
    assert_eq!(std::fs::read_dir(&notes).unwrap().count(), 4 + 2003, "and it made no file");
}

#[test]
fn a_stale_base_is_a_409_with_the_current_sha_and_two_tabs_never_overwrite_each_other() {
    let (perch, home) = with_home();
    let made = make(&perch, json!({ "text": "start" }));
    let (name, first) = (name_of(&made), made["sha"].as_str().unwrap().to_string());
    let file = home.join("notes").join(format!("{name}.md"));

    // Two tabs read the same copy.
    assert_eq!(read(&perch, &name)["sha"], first.as_str());
    let a = save(&perch, &name, "tab A was here", &first);
    assert_eq!(a.status, 200, "{}{}", a.head, a.body);
    let after_a = body(&a)["sha"].as_str().unwrap().to_string();
    let b = save(&perch, &name, "tab B was here", &first);
    assert_eq!(b.status, 409, "{}{}", b.head, b.body);
    assert_eq!(body(&b)["sha"], after_a.as_str(), "the 409 carries the sha now, so the page can offer reload");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "tab A was here", "the loser wrote nothing");
    // Tab B reloads, and its next save goes through.
    let reloaded = read(&perch, &name);
    assert_eq!((reloaded["text"].as_str(), reloaded["sha"].as_str()), (Some("tab A was here"), Some(after_a.as_str())));
    assert_eq!(save(&perch, &name, "tab B, after reading", &after_a).status, 200);
    // "Keep mine" is a save based on the sha from the 409.
    let mine = save(&perch, &name, "tab A again", &after_a);
    assert_eq!(mine.status, 409);
    let kept = save(&perch, &name, "tab A again", body(&mine)["sha"].as_str().unwrap());
    assert_eq!(kept.status, 200);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "tab A again");

    // A base that is not a sha at all is just a stale one; a missing one is a malformed request.
    let current = body(&kept)["sha"].as_str().unwrap().to_string();
    let (upper, zeros) = (current.to_uppercase(), "0".repeat(64));
    for base in ["", "x", upper.as_str(), &current[..63], zeros.as_str()] {
        let answer = save(&perch, &name, "nope", base);
        assert_eq!(answer.status, 409, "{base:?}: {}{}", answer.head, answer.body);
        assert_eq!(body(&answer)["sha"], current.as_str());
    }
    assert_eq!(perch.post_json(&format!("/hub/notes/{name}"), json!({ "text": "nope" })).status, 400);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "tab A again");
}

/// POSTs every `(path, body)` at the same moment: each connection is made first and all of them write on one signal. The answers come
/// back in the order given.
fn at_once(perch: &Perch, requests: Vec<(String, Value)>) -> Vec<Answer> {
    use std::io::{Read as _, Write as _};
    let go = std::sync::Arc::new(std::sync::Barrier::new(requests.len()));
    let threads: Vec<_> = requests
        .into_iter()
        .map(|(path, request)| {
            let (addr, auth, go) = (perch.addr.clone(), perch.auth(), go.clone());
            std::thread::spawn(move || {
                let wire = serde_json::to_vec(&request).unwrap();
                let mut bytes = Request::new(&addr, "POST", &path).bearer(auth).body(wire.clone()).head().into_bytes();
                bytes.extend_from_slice(&wire);
                let mut sock = std::net::TcpStream::connect(&addr).unwrap();
                sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
                go.wait();
                sock.write_all(&bytes).unwrap();
                let mut raw = Vec::new();
                sock.read_to_end(&mut raw).unwrap();
                Answer::parse(&raw)
            })
        })
        .collect();
    threads.into_iter().map(|t| t.join().unwrap()).collect()
}

#[test]
fn many_saves_from_one_copy_at_once_have_one_winner() {
    let (perch, home) = with_home();
    let made = make(&perch, json!({ "text": "start" }));
    let (name, sha) = (name_of(&made), made["sha"].as_str().unwrap().to_string());
    // A big text, so the write after the check is slow: without the lock more than one writer gets past the check before any of them
    // has replaced the file.
    let texts: Vec<String> = (0..24).map(|n| format!("writer {n:02} {}", "x".repeat(200 * 1024))).collect();
    let requests = texts.iter().map(|text| (format!("/hub/notes/{name}"), json!({ "text": text, "base": sha }))).collect();
    let statuses: Vec<u16> = at_once(&perch, requests).iter().map(|answer| answer.status).collect();
    let winners: Vec<usize> = (0..texts.len()).filter(|n| statuses[*n] == 200).collect();
    assert_eq!(winners.len(), 1, "one copy, one winner: {statuses:?}");
    assert!(statuses.iter().all(|status| *status == 200 || *status == 409), "{statuses:?}");
    let text = std::fs::read_to_string(home.join("notes").join(format!("{name}.md"))).unwrap();
    assert_eq!(text, texts[winners[0]], "and the file is the winner's, whole");
    assert_eq!(read(&perch, &name)["sha"], sha_of(texts[winners[0]].as_bytes()));
}

#[test]
fn many_renames_to_one_title_at_once_never_overwrite_each_other() {
    let (perch, home) = with_home();
    let texts: Vec<String> = (0..16).map(|n| format!("note {n:02}")).collect();
    let made: Vec<String> = texts.iter().map(|text| name_of(&make(&perch, json!({ "text": text })))).collect();
    let requests = made.iter().map(|name| (format!("/hub/notes/{name}/rename"), json!({ "title": "same title" }))).collect();
    let answers = at_once(&perch, requests);
    let mut renamed: Vec<String> = answers
        .iter()
        .map(|answer| {
            assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
            body(answer)["name"].as_str().unwrap().to_string()
        })
        .collect();
    renamed.sort();
    renamed.dedup();
    assert_eq!(renamed.len(), 16, "sixteen notes, sixteen names: {renamed:?}");
    assert!(renamed.iter().all(|name| name.starts_with("same-title")));
    let mut kept: Vec<String> = renamed.iter().map(|name| std::fs::read_to_string(home.join("notes").join(format!("{name}.md"))).unwrap()).collect();
    kept.sort();
    assert_eq!(kept, texts, "and every text survived");
    // Trash the sixteen, make sixteen more that take the same sixteen names, and trash those at once into a trash that holds those names:
    // each waits under a name of its own.
    let trash_all = |names: &[String]| {
        let requests = names.iter().map(|name| (format!("/hub/notes/{name}/trash"), json!(null))).collect();
        for answer in at_once(&perch, requests) {
            assert_eq!(answer.status, 204, "{}{}", answer.head, answer.body);
        }
    };
    trash_all(&renamed);
    let again: Vec<String> = (0..16).map(|n| name_of(&make(&perch, json!({ "text": format!("again {n:02}") })))).collect();
    for name in &again {
        assert_eq!(perch.post_json(&format!("/hub/notes/{name}/rename"), json!({ "title": "same title" })).status, 200);
    }
    let mut second = names(&perch.get("/hub/notes"));
    second.sort();
    assert_eq!(second, renamed, "the same sixteen names are free again");
    trash_all(&second);
    let mut waiting = names(&perch.get("/hub/notes/trash"));
    waiting.sort();
    waiting.dedup();
    assert_eq!(waiting.len(), 32, "every note found a name in the trash");
    let mut all_texts: Vec<String> =
        waiting.iter().map(|name| std::fs::read_to_string(home.join("trash").join("notes").join(format!("{name}.md"))).unwrap()).collect();
    all_texts.sort();
    let mut expected: Vec<String> = texts.iter().cloned().chain((0..16).map(|n| format!("again {n:02}"))).collect();
    expected.sort();
    assert_eq!(all_texts, expected, "and not one text was lost to a clash");
}

#[test]
fn a_rename_takes_the_slug_of_a_title_and_a_clash_gets_2() {
    let (perch, home) = with_home();
    let file = |name: &str| home.join("notes").join(format!("{name}.md"));
    let (a, b) = (name_of(&make(&perch, json!({ "text": "text a" }))), name_of(&make(&perch, json!({ "text": "text b" }))));
    let rename = |name: &str, title: &str| perch.post_json(&format!("/hub/notes/{name}/rename"), json!({ "title": title }));

    let done = rename(&a, "Build Checklist");
    assert_eq!(done.status, 200, "{}{}", done.head, done.body);
    assert_eq!(body(&done), json!({ "name": "build-checklist" }));
    assert!(!file(&a).exists() && file("build-checklist").exists(), "the file moved");
    assert_eq!(std::fs::read_to_string(file("build-checklist")).unwrap(), "text a", "and its text came along");
    assert_eq!(mode(&file("build-checklist")), 0o600);
    assert_eq!(perch.get(&format!("/hub/notes/{a}")).status, 404);
    assert_eq!(read(&perch, "build-checklist")["name"], "build-checklist");

    // The same title for another note: -2, and again -3.
    assert_eq!(body(&rename(&b, "build checklist!!"))["name"], "build-checklist-2");
    let c = name_of(&make(&perch, json!({ "text": "text c" })));
    assert_eq!(body(&rename(&c, "BUILD CHECKLIST"))["name"], "build-checklist-3");
    // Its own name is free to it: nothing moves.
    assert_eq!(body(&rename("build-checklist-2", "build  checklist"))["name"], "build-checklist-2", "a note is not its own clash");
    assert_eq!(body(&rename("build-checklist", "Build Checklist"))["name"], "build-checklist");
    // `trash` is a route, so it is never a name.
    let d = name_of(&make(&perch, json!({ "text": "text d" })));
    assert_eq!(body(&rename(&d, "Trash"))["name"], "trash-2");
    // The suffix stays inside 48 characters.
    let long = "word ".repeat(20);
    let (e, f) = (name_of(&make(&perch, json!({ "text": "e" }))), name_of(&make(&perch, json!({ "text": "f" }))));
    let (first, second) = (body(&rename(&e, &long))["name"].as_str().unwrap().to_string(), body(&rename(&f, &long))["name"].as_str().unwrap().to_string());
    assert!(first.len() <= 48 && second.len() <= 48 && first != second && second.ends_with("-2"), "{first} {second}");

    // Titles that make no name, and ones that are not titles.
    let too_long = "t".repeat(121);
    for title in ["", "   ", "!!!", "日本語", "a\nb", "a\u{7}b", too_long.as_str()] {
        let answer = rename("build-checklist", title);
        assert_eq!(answer.status, 400, "{title:?}: {}{}", answer.head, answer.body);
    }
    assert_eq!(perch.post_json("/hub/notes/build-checklist/rename", json!({ "name": "x" })).status, 400);
    assert_eq!(perch.post("/hub/notes/build-checklist/rename").status, 400);
    let array = perch.apireq("POST", "/hub/notes/build-checklist/rename").body(br#"["anything"]"#.to_vec()).send(&perch.addr);
    assert_eq!(array.status, 400, "{}{}", array.head, array.body);
    assert!(file("build-checklist").exists(), "and the note is where it was");
    assert_eq!(rename("ghost", "anything").status, 404);
    // The list shows the new names.
    let listed = names(&perch.get("/hub/notes"));
    for name in ["build-checklist", "build-checklist-2", "build-checklist-3", "trash-2", first.as_str(), second.as_str()] {
        assert!(listed.iter().any(|n| n == name), "{name} in {listed:?}");
    }
}

#[test]
fn a_note_goes_to_the_trash_and_comes_back_to_a_free_name_only() {
    let (perch, home) = with_home();
    let trash = home.join("trash").join("notes");
    let a = name_of(&make(&perch, json!({ "text": "first note\nbody" })));
    let moved = perch.post(&format!("/hub/notes/{a}/trash"));
    assert_eq!((moved.status, moved.body.as_str()), (204, ""));
    assert!(!home.join("notes").join(format!("{a}.md")).exists());
    assert_eq!(std::fs::read_to_string(trash.join(format!("{a}.md"))).unwrap(), "first note\nbody", "a rename, so the text is whole");
    assert_eq!((mode(&trash), mode(&trash.join(format!("{a}.md"))), mode(&home.join("trash"))), (0o700, 0o600, 0o700));
    assert_eq!(names(&perch.get("/hub/notes")), Vec::<String>::new());
    let listed = body(&perch.get("/hub/notes/trash"));
    assert_eq!((listed[0]["name"].as_str(), listed[0]["title"].as_str(), listed[0]["size"].as_u64()), (Some(a.as_str()), Some("first note"), Some(15)));
    assert_eq!(perch.get(&format!("/hub/notes/{a}")).status, 404, "a trashed note is not read as a note");
    assert_eq!(save(&perch, &a, "x", "y").status, 404);
    assert_eq!(perch.post(&format!("/hub/notes/{a}/trash")).status, 404, "and not trashed twice");

    let back = perch.post(&format!("/hub/notes/trash/{a}/restore"));
    assert_eq!((back.status, body(&back)), (200, json!({ "name": a })));
    assert_eq!(read(&perch, &a)["text"], "first note\nbody");
    assert_eq!(body(&perch.get("/hub/notes/trash")), json!([]));
    assert_eq!(perch.post(&format!("/hub/notes/trash/{a}/restore")).status, 404, "it is not in the trash any more");

    // A restore onto a name a note has taken is a 409, and neither is touched.
    assert_eq!(perch.post(&format!("/hub/notes/{a}/trash")).status, 204);
    let squatter = name_of(&make(&perch, json!({ "text": "squatter" })));
    let words = a.replace('-', " ");
    assert_eq!(body(&perch.post_json(&format!("/hub/notes/{squatter}/rename"), json!({ "title": words })))["name"], a.as_str(), "its title is the trashed one's name");
    let clash = perch.post(&format!("/hub/notes/trash/{a}/restore"));
    assert_eq!(clash.status, 409, "{}{}", clash.head, clash.body);
    assert!(body(&clash)["error"].as_str().unwrap().contains("already exists"));
    assert_eq!(std::fs::read_to_string(home.join("notes").join(format!("{a}.md"))).unwrap(), "squatter", "the squatter was not overwritten");
    assert_eq!(names(&perch.get("/hub/notes/trash")), std::slice::from_ref(&a), "and the trashed one is still there");
    // Trashing the squatter puts a second note of the same name in the trash beside the first: both are kept.
    assert_eq!(perch.post(&format!("/hub/notes/{a}/trash")).status, 204);
    let mut waiting = names(&perch.get("/hub/notes/trash"));
    waiting.sort();
    assert_eq!(waiting, [a.clone(), format!("{a}-2")]);
    assert_eq!(std::fs::read_to_string(trash.join(format!("{a}.md"))).unwrap(), "first note\nbody");
    assert_eq!(std::fs::read_to_string(trash.join(format!("{a}-2.md"))).unwrap(), "squatter");
    // Both can come back, one under each name.
    assert_eq!(perch.post(&format!("/hub/notes/trash/{a}/restore")).status, 200);
    assert_eq!(perch.post(&format!("/hub/notes/trash/{a}-2/restore")).status, 200);
    assert_eq!(names(&perch.get("/hub/notes")).len(), 2);
}

#[test]
fn the_notes_trash_and_the_chats_trash_do_not_see_each_other() {
    let (perch, home) = with_home();
    let note = name_of(&make(&perch, json!({ "text": "a note" })));
    assert_eq!(perch.post(&format!("/hub/notes/{note}/trash")).status, 204);
    // A chat log in the chats' trash, as `POST /hub/sessions/<id>/trash` leaves it.
    std::fs::write(home.join("trash").join("chat-one.eid"), "").unwrap();
    // The chats' trash lists the chat and not the notes folder; the notes' trash lists the note and not the chat.
    let chats = body(&perch.get("/hub/trash"));
    assert_eq!(chats.as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap()).collect::<Vec<_>>(), ["chat-one"], "{chats}");
    assert_eq!(names(&perch.get("/hub/notes/trash")), std::slice::from_ref(&note));
    // Emptying one does not touch the other.
    let emptied = perch.post_json("/hub/trash/empty", json!({ "confirm": true }));
    assert_eq!((emptied.status, body(&emptied)), (200, json!({ "removed": 1 })));
    assert!(home.join("trash").join("notes").join(format!("{note}.md")).exists(), "the chats' empty left the notes' trash alone");
    std::fs::write(home.join("trash").join("chat-two.eid"), "").unwrap();
    std::fs::write(home.join("trash").join("notes").join("chat-three.eid"), "").unwrap();
    let emptied = perch.post_json("/hub/notes/trash/empty", json!({ "confirm": true }));
    assert_eq!((emptied.status, body(&emptied)), (200, json!({ "removed": 1 })));
    assert!(home.join("trash").join("chat-two.eid").exists(), "the notes' empty left the chats' trash alone");
    assert!(home.join("trash").join("notes").join("chat-three.eid").exists(), "and a log is not a note, wherever it lies");
}

#[test]
fn emptying_the_trash_needs_confirm_and_removes_only_note_files() {
    let (perch, home) = with_home();
    let (a, b, c) = (
        name_of(&make(&perch, json!({ "text": "a" }))),
        name_of(&make(&perch, json!({ "text": "b" }))),
        name_of(&make(&perch, json!({ "text": "c, stays in the notes" }))),
    );
    for name in [&a, &b] {
        assert_eq!(perch.post(&format!("/hub/notes/{name}/trash")).status, 204);
    }
    let trash = home.join("trash").join("notes");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("target.md"), "keep me").unwrap();
    std::fs::write(trash.join("keep.txt"), "x").unwrap();
    std::fs::write(trash.join("Odd.md"), "x").unwrap();
    std::fs::write(trash.join(".dot.md"), "x").unwrap();
    std::fs::create_dir(trash.join("sub.md")).unwrap();
    std::fs::write(trash.join("sub.md").join("in.md"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path().join("target.md"), trash.join("ln.md")).unwrap();
    mkfifo(&trash.join("fifo.md"));
    let count = || std::fs::read_dir(&trash).unwrap().count();
    let before = count();

    // Nothing but `{"confirm": true}` empties it.
    let refusals: [&[u8]; 8] = [b"", b"{}", b"{\"confirm\": false}", b"{\"confirm\": \"true\"}", b"{\"confirm\": 1}", b"[true]", b"true", b"not json"];
    for raw in refusals {
        let answer = perch.apireq("POST", "/hub/notes/trash/empty").body(raw.to_vec()).send(&perch.addr);
        assert_eq!(answer.status, 400, "{:?}: {}{}", String::from_utf8_lossy(raw), answer.head, answer.body);
    }
    assert_eq!(count(), before, "no refusal removed anything");
    assert_eq!(names(&perch.get("/hub/notes/trash")).len(), 2);

    let emptied = perch.post_json("/hub/notes/trash/empty", json!({ "confirm": true }));
    assert_eq!((emptied.status, body(&emptied)), (200, json!({ "removed": 2 })));
    let mut left: Vec<String> = std::fs::read_dir(&trash).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    left.sort();
    assert_eq!(left, [".dot.md", "Odd.md", "fifo.md", "keep.txt", "ln.md", "sub.md"], "only the two notes went");
    assert_eq!(std::fs::read_to_string(outside.path().join("target.md")).unwrap(), "keep me", "and nothing through a link");
    assert!(trash.join("sub.md").join("in.md").exists());
    assert_eq!(body(&perch.get("/hub/notes/trash")), json!([]));
    assert_eq!(names(&perch.get("/hub/notes")), [c], "the notes themselves were never in the way");
    let again = perch.post_json("/hub/notes/trash/empty", json!({ "confirm": true }));
    assert_eq!((again.status, body(&again)), (200, json!({ "removed": 0 })));
}

#[test]
fn an_empty_trash_that_was_never_made_empties_to_zero() {
    let (perch, home) = with_home();
    let emptied = perch.post_json("/hub/notes/trash/empty", json!({ "confirm": true }));
    assert_eq!((emptied.status, body(&emptied)), (200, json!({ "removed": 0 })));
    assert_eq!(body(&perch.get("/hub/notes/trash")), json!([]));
    assert!(!home.join("trash").join("notes").exists(), "looking at the trash does not make it");
}

#[test]
fn the_notes_survive_a_restart_of_the_perch_with_the_same_home() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let args = || -> Vec<std::ffi::OsString> { vec!["--root".into(), base.path().canonicalize().unwrap().into(), "--home".into(), home.clone().into()] };
    let (name, sha) = {
        let mut perch = Perch::builder().args(args()).start();
        let made = make(&perch, json!({ "text": "kept across a restart" }));
        perch.stop();
        (name_of(&made), made["sha"].as_str().unwrap().to_string())
    };
    let perch = Perch::builder().args(args()).start();
    let whole = read(&perch, &name);
    assert_eq!((whole["text"].as_str(), whole["sha"].as_str()), (Some("kept across a restart"), Some(sha.as_str())), "the same etag after a restart");
    assert_eq!(names(&perch.get("/hub/notes")), [name]);
}
