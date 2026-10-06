//! What a chat is called: the names a person gives (`<home>/names.json`), the three-word birth name of a new inbox
//! chat, and the folder slug a title becomes.
//!
//! `names.json` is `{ "<id>": "<title>" }`, written atomically with mode `0600`. A name given by hand beats the title eidolon
//! derives from the first message, everywhere the perch shows one (the list, the trash, a fork's `(fork)` title). Nothing here
//! touches a log or a folder.
//!
//! **Birth names.** A new chat with no folder works in `<home>/scratch/<adjective>-<animal>-<thing>` (`quiet-owl-lantern`),
//! lowercase ascii and never a digit, so the sidebar can call it something before it has a title. When the chat settles with a
//! title, the folder is renamed to [`slug`] of that title (relocate.rs).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

/// A chat's name is a label, not a path: short, one line.
pub const MAX_TITLE: usize = 120;
/// A folder slug is at most this long.
pub const MAX_SLUG: usize = 48;

/// A title as a person gave it, trimmed; `Err` says why not. The empty title is not an error here: the caller reads it as "no custom name".
pub fn clean_title(raw: &str) -> std::result::Result<String, &'static str> {
    let title = raw.trim();
    if title.chars().count() > MAX_TITLE {
        return Err("that name is too long (120 characters at most)");
    }
    if title.chars().any(|c| c.is_control()) {
        return Err("a name cannot contain control characters");
    }
    Ok(title.to_string())
}

pub struct Names {
    file: Option<PathBuf>,
    map: Mutex<BTreeMap<String, String>>,
}

impl Names {
    /// Reads `file` (absent is empty; a corrupt one is set aside as `names.json.bad-<ts>`, see [`crate::statefile`]).
    pub fn open(file: Option<PathBuf>) -> Self {
        let map = crate::statefile::load(file.as_deref());
        Self { file, map: Mutex::new(map) }
    }

    pub fn get(&self, id: &str) -> Option<String> {
        self.map.lock().unwrap().get(id).cloned()
    }

    /// Sets (or, with `None`, clears) the name of one chat. The memory changes only if the write succeeded.
    pub fn set(&self, id: &str, title: Option<&str>) -> Result<()> {
        let Some(file) = &self.file else { anyhow::bail!("no Minerva home") };
        let mut map = self.map.lock().unwrap();
        let mut next = map.clone();
        match title.map(str::trim).filter(|t| !t.is_empty()) {
            Some(title) => next.insert(id.to_string(), title.to_string()),
            None => next.remove(id),
        };
        if next == *map {
            return Ok(());
        }
        write(file, &next)?;
        *map = next;
        Ok(())
    }

    /// Drops the name of a chat that is gone for good (the trash was emptied).
    pub fn forget(&self, id: &str) {
        if self.get(id).is_some()
            && let Err(e) = self.set(id, None)
        {
            tracing::warn!(error = %format!("{e:#}"), "perch: names.json could not be written");
        }
    }
}

fn write(file: &Path, map: &BTreeMap<String, String>) -> Result<()> {
    let body = serde_json::to_vec_pretty(map).context("serializing the names")?;
    harnox::fs::write_atomic_0600(file, &body).with_context(|| format!("writing {}", file.display()))
}

/// A folder name for a title: lowercase ascii letters and digits, runs of anything else become one `-`, at most [`MAX_SLUG`]
/// characters, no leading or trailing `-`. `None` when nothing is left (a title of only symbols or non-ascii letters).
pub fn slug(title: &str) -> Option<String> {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    // A word the tree refuses (`secret`, `credentials`: tree.rs `denied`) is left out of the folder name: a folder the files tab will
    // not open is no use as a chat's own folder. "my secret plan" is `my-plan`; a title of only such words has no slug.
    let kept: Vec<&str> = out.split('-').filter(|word| !crate::tree::refused(word)).collect();
    let mut out = kept.join("-");
    out.truncate(MAX_SLUG);
    let out = out.trim_end_matches('-').to_string();
    (!out.is_empty() && !crate::tree::refused(&out)).then_some(out)
}

/// Whether `name` is `slug` or `slug-<n>` (a collision suffix): the folder already follows the title.
pub fn follows(name: &str, slug: &str) -> bool {
    match name.strip_prefix(slug) {
        Some("") => true,
        Some(rest) => rest.strip_prefix('-').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())),
        None => false,
    }
}

const ADJECTIVES: &[&str] = &[
    "quiet", "cozy", "sleepy", "gentle", "amber", "silver", "velvet", "misty", "hushed", "mellow", "drowsy", "soft", "dusky", "warm", "starry",
    "moonlit", "snug", "calm", "twilight", "woolly", "silent", "golden", "plush", "dim", "faded", "tender", "patient", "wandering", "midnight", "brindle",
    "frosted", "cloudy", "downy", "lonely", "nimble", "honeyed", "tawny", "rustling", "peaceful", "curious", "humble", "wistful", "shy", "sunny",
    "hollow", "lazy", "bright", "dappled",
];

const ANIMALS: &[&str] = &[
    "owl", "moth", "fox", "hedgehog", "badger", "otter", "heron", "wren", "dormouse", "hare", "newt", "finch", "bat", "mole", "lynx", "stag",
    "robin", "swift", "toad", "cricket", "firefly", "kestrel", "marten", "vole", "sparrow", "lark", "gecko", "shrew", "beetle", "magpie",
    "raven", "pika", "ferret", "dove", "crane", "quail", "tern", "weasel", "lemur", "panda", "koala", "puffin", "seal", "gull", "stoat", "ibis",
    "orca", "mouse",
];

const THINGS: &[&str] = &[
    "lantern", "teacup", "blanket", "candle", "kettle", "lamp", "quilt", "pillow", "satchel", "compass", "lighthouse", "orchard", "meadow", "hearth",
    "attic", "window", "harbor", "bridge", "garden", "cabin", "chimney", "ladder", "button", "ribbon", "thimble", "teapot", "parlor", "porch",
    "stairwell", "bookshelf", "armchair", "fireplace", "thicket", "lagoon", "clearing", "burrow", "nest", "hollow", "willow", "pebble", "acorn",
    "mitten", "scarf", "lullaby", "almanac", "hammock", "inkwell", "bonfire",
];

/// A birth name from a number: `<adjective>-<animal>-<thing>`. The same number is the same name, so a test can pin one; the caller
/// draws a fresh number per try.
pub fn birth_name(n: u64) -> String {
    let (a, b, c) = (ADJECTIVES.len() as u64, ANIMALS.len() as u64, THINGS.len() as u64);
    // A multiplicative scramble so near numbers do not share a first word.
    let n = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11;
    format!("{}-{}-{}", ADJECTIVES[(n % a) as usize], ANIMALS[(n / a % b) as usize], THINGS[(n / (a * b) % c) as usize])
}

/// A fresh random number for [`birth_name`]: the std hasher is seeded from the OS per instance, and the clock and a counter go in too.
pub fn random_u64() -> u64 {
    use std::hash::{BuildHasher as _, Hasher as _};
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    hasher.write_u128(nanos);
    hasher.write_u64(COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    hasher.finish()
}

/// A new directory for one folderless chat: `<scratch>/<birth name>`, a new name per try when that one is taken, and after
/// [`TRIES`] a `-2`, `-3`, ... on the last name. `create_dir` (not `create_dir_all`) is what makes the name this chat's alone.
pub fn fresh_scratch_with(scratch: &Path, next: &mut dyn FnMut() -> u64) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(scratch)?;
    let mut last = String::new();
    for _ in 0..TRIES {
        last = birth_name(next());
        match std::fs::create_dir(scratch.join(&last)) {
            Ok(()) => return Ok(scratch.join(&last)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    for n in 2..1000 {
        let dir = scratch.join(format!("{last}-{n}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free scratch name"))
}

/// How many fresh birth names are tried before the `-2` suffix.
const TRIES: usize = 16;

/// [`fresh_scratch_with`] with random numbers.
pub fn fresh_scratch(scratch: &Path) -> std::io::Result<PathBuf> {
    fresh_scratch_with(scratch, &mut random_u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_birth_name_is_three_lowercase_words_and_never_a_digit() {
        for list in [ADJECTIVES, ANIMALS, THINGS] {
            assert!(list.iter().all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase())), "{list:?}");
        }
        let mut seen = std::collections::HashSet::new();
        for _ in 0..500 {
            let name = birth_name(random_u64());
            let parts: Vec<&str> = name.split('-').collect();
            assert_eq!(parts.len(), 3, "{name}");
            assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '-'), "{name}");
            seen.insert(name);
        }
        assert!(seen.len() > 400, "names are drawn from a wide space, got {} distinct of 500", seen.len());
        assert_eq!(birth_name(7), birth_name(7), "a number is a name");
        assert_ne!(birth_name(7), birth_name(8));
    }

    #[test]
    fn a_scratch_name_is_unique_among_existing_folders_and_falls_back_to_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        // The generator keeps proposing a taken name, then a free one: the free one wins.
        let taken = birth_name(1);
        std::fs::create_dir(dir.path().join(&taken)).unwrap();
        let mut calls = 0;
        let got = fresh_scratch_with(dir.path(), &mut || {
            calls += 1;
            if calls < 3 { 1 } else { 2 }
        })
        .unwrap();
        assert_eq!(got, dir.path().join(birth_name(2)));
        assert_eq!(calls, 3);
        // A generator that never varies runs out of tries and takes `-2`, then `-3`.
        let (a, b) = (fresh_scratch_with(dir.path(), &mut || 1).unwrap(), fresh_scratch_with(dir.path(), &mut || 1).unwrap());
        assert_eq!(a, dir.path().join(format!("{taken}-2")));
        assert_eq!(b, dir.path().join(format!("{taken}-3")));
        // And two random ones never collide.
        let (x, y) = (fresh_scratch(dir.path()).unwrap(), fresh_scratch(dir.path()).unwrap());
        assert_ne!(x, y);
        assert!(x.file_name().unwrap().to_str().unwrap().chars().all(|c| c.is_ascii_lowercase() || c == '-'));
    }

    #[test]
    fn a_slug_never_names_a_folder_the_tree_refuses() {
        assert_eq!(slug("my secret plan").as_deref(), Some("my-plan"));
        assert_eq!(slug("Credentials rotation").as_deref(), Some("rotation"));
        assert_eq!(slug("the secretary").as_deref(), Some("the"));
        assert_eq!(slug("secrets"), None, "nothing left is no slug (the birth name stays)");
        for title in ["db secret key dump", "aws credentials leak", "Secret", "the-credentials-file"] {
            if let Some(s) = slug(title) {
                assert!(!crate::tree::refused(&s), "{title:?} -> {s}");
            }
        }
    }

    #[test]
    fn a_title_becomes_a_folder_slug() {
        assert_eq!(slug("juice shop recon").as_deref(), Some("juice-shop-recon"));
        assert_eq!(slug("  Fix the  LOGIN bug!! ").as_deref(), Some("fix-the-login-bug"));
        assert_eq!(slug("a/b\\c..d").as_deref(), Some("a-b-c-d"), "no separator survives");
        assert_eq!(slug("日本語"), None);
        assert_eq!(slug("---"), None);
        let long = slug(&"word ".repeat(40)).unwrap();
        assert!(long.len() <= MAX_SLUG && !long.ends_with('-'), "{long}");
        assert!(follows("juice-shop-recon", "juice-shop-recon") && follows("juice-shop-recon-2", "juice-shop-recon"));
        assert!(!follows("juice-shop-recon-x", "juice-shop-recon") && !follows("juice-shop-recon-", "juice-shop-recon") && !follows("quiet-owl-lantern", "juice"));
    }

    #[test]
    fn names_are_labels_stored_0600_and_cleared_by_an_empty_one() {
        for bad in ["a\nb", "a\u{7}b"] {
            assert!(clean_title(bad).is_err(), "{bad:?}");
        }
        assert!(clean_title(&"n".repeat(121)).is_err());
        assert_eq!(clean_title("  ok  "), Ok("ok".into()));
        assert_eq!(clean_title("   "), Ok(String::new()));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("names.json");
        let names = Names::open(Some(file.clone()));
        names.set("a", Some(" Hello ")).unwrap();
        assert_eq!(names.get("a").as_deref(), Some("Hello"));
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(Names::open(Some(file.clone())).get("a").as_deref(), Some("Hello"));
        names.set("a", Some("")).unwrap();
        assert_eq!(names.get("a"), None, "an empty name is no name");
        assert!(Names::open(None).set("a", Some("x")).is_err(), "no home keeps nothing");
    }
}
