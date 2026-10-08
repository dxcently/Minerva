//! `fake_minerva` — a stand-in for the installer's `minerva` binary, for the perch's tests (slice R4).
//!
//! It answers exactly the five argv the perch's install routes run, in the shape installer/README.md's "The perch contract" gives, over a
//! fixture directory, and **refuses everything else with exit 2** (so a route that ever ran `update`, `keys remove-all` or `jev-setup` would
//! be logged as refused and fail a test). Every call, refused or not, is first appended to `<dir>/log` as one JSON line:
//! `{"argv": [...], "stdin": "<text>" | null, "env": [<names only>], "refused": bool, "pid": N}`.
//!
//! ```text
//!   status --json                              <dir>/status.json verbatim when it exists, else a fixed "installed" answer
//!   keys list --json                           <dir>/list.json verbatim when it exists, else [{id, fingerprint, state, source, expires}],
//!                                              one per <dir>/keys/<ID>.pub
//!   keys inspect - --json                      {id, fingerprint, comment}                    the key on stdin
//!   keys add - --confirm <FP> --json           {"added": ID} | {"error": {kind, sentence}} (exit 1)
//!   keys remove <ID> --confirm <FP> --json     {"removed": ID} | {"error": ...} (exit 1)
//! ```
//!
//! The fixture dir is `$MINERVA_FAKE_DIR` (the perch passes every `MINERVA_*` variable on, which is what makes this reach the child). A key
//! is the text `untrusted comment: <c>` then `FAKE <ID> <FP>` (16 hex digits each), not a minisign key: the fake does no cryptography. Its
//! files are `<ID>.pub` (the text) and `<ID>.source` (`compiled in`, `added by hand`, `added by a signed rotation`; absent means by hand);
//! `<dir>/revoked` lists revoked ids, `<dir>/unsafe`, when it exists, makes `add` and `remove` answer kind `unsafe`, and `<dir>/sentence`,
//! when it exists, is the sentence of every refusal. The real program
//! removes a rotation key too (it refuses only the compiled-in one), so the perch's own source check is the only guard for ruling 6; the
//! fake is permissive in the same way on purpose, so a perch that stopped refusing one would be seen to.
//!
//! `<dir>/behave`, read on every call, makes it misbehave: `hang` (sleep 30 s, then answer), `slow` (sleep 700 ms, then answer), `linger`
//! (sleep 1500 ms, then answer), `big` (100 KiB of `x`), `garbage`, `empty`, `lie-ok` (exit 0 with an error answer) and `lie-error` (exit 1
//! with a success answer). `write:<word>` does the same for `keys add` and `keys remove` only, so the reads before a write behave. The
//! sleeps come after the call is logged and before the key is written, which is what a test of a write that must not be killed needs.
//!
//! Built only with `--features test-bins` (`required-features` in Cargo.toml), so a normal `cargo build` ships no test double.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

const COMPILED: &str = "compiled in";
const BY_HAND: &str = "added by hand";

enum Verb {
    Status,
    List,
    Inspect,
    Add(String),
    Remove(String, String),
}

impl Verb {
    /// The five shapes the perch runs, and nothing else.
    fn parse(argv: &[String]) -> Option<Verb> {
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["status", "--json"] => Some(Verb::Status),
            ["keys", "list", "--json"] => Some(Verb::List),
            ["keys", "inspect", "-", "--json"] => Some(Verb::Inspect),
            ["keys", "add", "-", "--confirm", fp, "--json"] => Some(Verb::Add(fp.to_string())),
            ["keys", "remove", id, "--confirm", fp, "--json"] => Some(Verb::Remove(id.to_string(), fp.to_string())),
            _ => None,
        }
    }

    fn reads_stdin(&self) -> bool {
        matches!(self, Verb::Inspect | Verb::Add(_))
    }

    fn writes(&self) -> bool {
        matches!(self, Verb::Add(_) | Verb::Remove(..))
    }
}

/// A key as the fake reads one: lower-case digits, no spaces.
struct Key {
    id: String,
    fp: String,
    comment: String,
}

fn parse_key(text: &str) -> Option<Key> {
    let mut comment = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(c) = line.strip_prefix("untrusted comment:") {
            comment = c.trim().to_string();
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let hex16 = |s: &str| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
        return match words.as_slice() {
            ["FAKE", id, fp] if hex16(id) && hex16(fp) => Some(Key { id: id.to_ascii_uppercase(), fp: fp.to_ascii_lowercase(), comment }),
            _ => None,
        };
    }
    None
}

/// The way minerva prints a fingerprint: four groups of four, lower case.
fn grouped(fp: &str) -> String {
    fp.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

fn norm(fp: &str) -> String {
    fp.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase()
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = std::env::var_os("MINERVA_FAKE_DIR").map(PathBuf::from) else {
        eprintln!("fake_minerva: MINERVA_FAKE_DIR names the fixture directory");
        return 99;
    };
    let verb = Verb::parse(&argv);
    let stdin = verb.as_ref().filter(|v| v.reads_stdin()).map(|_| {
        let mut bytes = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    log(&dir, &argv, stdin.as_deref(), verb.is_none());
    let Some(verb) = verb else {
        eprintln!("fake_minerva: refused argv {argv:?}");
        return 2;
    };
    let behave = std::fs::read_to_string(dir.join("behave")).unwrap_or_default();
    let word = match behave.trim().strip_prefix("write:") {
        Some(word) if verb.writes() => word,
        Some(_) => "",
        None => behave.trim(),
    };
    match word {
        "hang" => std::thread::sleep(Duration::from_secs(30)),
        "slow" => std::thread::sleep(Duration::from_millis(700)),
        "linger" => std::thread::sleep(Duration::from_millis(1500)),
        "big" => {
            let _ = std::io::stdout().write_all(&vec![b'x'; 100 * 1024]);
            return 0;
        }
        "garbage" => {
            println!("this is not json");
            return 0;
        }
        "empty" => return 0,
        "lie-ok" => {
            println!(r#"{{"error":{{"kind":"mismatch","sentence":"a refusal that exits 0"}}}}"#);
            return 0;
        }
        "lie-error" => {
            println!(r#"{{"added":"AAAAAAAAAAAAAAAB","removed":"AAAAAAAAAAAAAAAB"}}"#);
            return 1;
        }
        _ => {}
    }
    match verb {
        Verb::Status => {
            match std::fs::read(dir.join("status.json")) {
                Ok(bytes) => {
                    let _ = std::io::stdout().write_all(&bytes);
                }
                Err(_) => println!(
                    "{}",
                    json!({"version": "0.1.0-gabc1234", "notice": null, "plugins": [{"name": "browser", "version": "0.3.0", "how": "stock"}],
                           "jev": {"set_up": true, "running": true, "pid": 4242}})
                ),
            }
            0
        }
        Verb::List => {
            if let Ok(bytes) = std::fs::read(dir.join("list.json")) {
                let _ = std::io::stdout().write_all(&bytes);
                return 0;
            }
            let rows: Vec<_> = keys(&dir)
                .into_iter()
                .map(|(key, source)| {
                    let state = if revoked(&dir).contains(&key.id) { "revoked" } else { "trusted" };
                    json!({"id": key.id, "fingerprint": grouped(&key.fp), "state": state, "source": source, "expires": null})
                })
                .collect();
            println!("{}", serde_json::Value::Array(rows));
            0
        }
        Verb::Inspect => match parse_key(stdin.as_deref().unwrap_or("")) {
            Some(key) => {
                println!("{}", json!({"id": key.id, "fingerprint": grouped(&key.fp), "comment": key.comment}));
                0
            }
            None => {
                eprintln!("Error: that is not a minisign public key (bad base64)");
                1
            }
        },
        Verb::Add(confirm) => add(&dir, stdin.as_deref().unwrap_or(""), &confirm),
        Verb::Remove(id, confirm) => remove(&dir, &id, &confirm),
    }
}

/// A refusal. `<dir>/sentence`, when it exists, is what every refusal says (so a test can send a long one).
fn refuse(kind: &str, sentence: &str) -> i32 {
    let said = std::env::var_os("MINERVA_FAKE_DIR").and_then(|dir| std::fs::read_to_string(PathBuf::from(dir).join("sentence")).ok());
    println!("{}", json!({"error": {"kind": kind, "sentence": said.as_deref().unwrap_or(sentence)}}));
    1
}

fn add(dir: &Path, text: &str, confirm: &str) -> i32 {
    let Some(key) = parse_key(text) else { return refuse("failed", "that is not a minisign public key (bad base64)") };
    if norm(confirm) != key.fp {
        return refuse("mismatch", "the confirmation does not match the key's fingerprint; nothing changed");
    }
    if dir.join("unsafe").exists() {
        return refuse("unsafe", "the keys directory is not a private directory of yours");
    }
    if revoked(dir).contains(&key.id) {
        return refuse("revoked", "that key was revoked, and a revoked key is never accepted again");
    }
    match keys(dir).into_iter().find(|(held, _)| held.id == key.id) {
        Some((held, _)) if held.fp != key.fp => return refuse("collision", "that key id is in use by a DIFFERENT key"),
        Some((_, source)) if source == COMPILED => return refuse("compiled", "that is the compiled-in key: it is already trusted"),
        _ => {}
    }
    let keys_dir = dir.join("keys");
    let _ = std::fs::create_dir_all(&keys_dir);
    let _ = std::fs::write(keys_dir.join(format!("{}.pub", key.id)), text);
    let _ = std::fs::write(keys_dir.join(format!("{}.source", key.id)), BY_HAND);
    println!("{}", json!({"added": key.id}));
    0
}

fn remove(dir: &Path, id: &str, confirm: &str) -> i32 {
    let id = id.to_ascii_uppercase();
    if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return refuse("not-found", "no such key: a key id is 16 hex digits (minerva keys list)");
    }
    if dir.join("unsafe").exists() {
        return refuse("unsafe", "the keys directory is not a private directory of yours");
    }
    let Some((key, source)) = keys(dir).into_iter().find(|(key, _)| key.id == id) else {
        return refuse("not-found", "no key with that id (minerva keys list)");
    };
    if norm(confirm) != key.fp {
        return refuse("mismatch", "the confirmation does not match the key's fingerprint; nothing changed");
    }
    if source == COMPILED {
        return refuse("compiled", "the compiled-in key cannot be removed by hand");
    }
    let _ = std::fs::remove_file(dir.join("keys").join(format!("{id}.pub")));
    let _ = std::fs::remove_file(dir.join("keys").join(format!("{id}.source")));
    println!("{}", json!({"removed": id}));
    0
}

/// Every key in `<dir>/keys`, by id, with its source label.
fn keys(dir: &Path) -> Vec<(Key, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir.join("keys")).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pub") {
            continue;
        }
        let Some(key) = std::fs::read_to_string(&path).ok().and_then(|text| parse_key(&text)) else { continue };
        let source = std::fs::read_to_string(path.with_extension("source")).map(|s| s.trim().to_string()).unwrap_or_else(|_| BY_HAND.to_string());
        found.push((key, source));
    }
    found.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    found
}

fn revoked(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("revoked")).unwrap_or_default().lines().map(|l| l.trim().to_ascii_uppercase()).collect()
}

/// One JSON line per call, written before the call is answered. The environment is logged by name only.
fn log(dir: &Path, argv: &[String], stdin: Option<&str>, refused: bool) {
    let mut names: Vec<String> = std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()).collect();
    names.sort();
    let line = json!({"argv": argv, "stdin": stdin, "env": names, "refused": refused, "pid": std::process::id()});
    // one write of the whole line and its newline: a reader never sees half of a call (writeln! is several writes)
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log")) {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}
