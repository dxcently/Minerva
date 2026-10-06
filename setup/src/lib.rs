//! The logic behind `minerva-setup`: what the wizard reads, checks and writes. The prompts live in
//! `main.rs`; everything here is plain functions over paths and strings, so the tests can drive it.
//!
//! The one file it owns is `<home>/minerva.toml`, Minerva's own choices (the perch reads `default_model`
//! from it; `bin/perch-open.sh` reads `roots` through `minerva-setup --print-roots`). It never touches
//! `~/.config/eidolon`: the member's own config stays the member's.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// What a fresh home starts with. The same default the perch uses (`perch::config::FALLBACK_MODEL`).
pub const DEFAULT_MODEL: &str = "chatgpt:gpt-5.6-luna";

/// The models the wizard always offers: (id, hint). The first is the default.
pub const STATIC_MODELS: [(&str, &str); 3] = [
    ("chatgpt:gpt-5.6-luna", "default"),
    ("ollama:deepseek-v4.1-flash", "Ollama plan"),
    ("mock", "free, for trying the UI"),
];

/// `$MINERVA_HOME`, else `$HOME/Minerva`. The two environment values are passed in so tests need no global state.
pub fn default_home(minerva_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    if let Some(path) = minerva_home.filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            bail!("MINERVA_HOME must be an absolute path: {}", path.display());
        }
        return Ok(path);
    }
    let home = home.filter(|p| !p.is_empty()).context("$HOME is not set, so there is no default Minerva home")?;
    Ok(PathBuf::from(home).join("Minerva"))
}

/// `~` and `~/x` become `home` and `home/x`; anything else is returned as typed (trimmed, trailing `/` dropped).
pub fn expand(input: &str, home: &Path) -> PathBuf {
    let text = input.trim();
    let text = if text.len() > 1 { text.trim_end_matches('/') } else { text };
    if text == "~" {
        return home.to_path_buf();
    }
    match text.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(text),
    }
}

/// A Minerva home must be absolute and must not sit on a Windows drive: drvfs cannot hold `0600` files, and
/// the perch refuses to keep its secrets anywhere that cannot.
pub fn check_home(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("give an absolute path (it may start with ~/)".into());
    }
    if path.starts_with("/mnt") {
        return Err("not under /mnt/: a Windows drive (drvfs) cannot hold 0600 files; pick a folder in your WSL home".into());
    }
    Ok(())
}

/// A project root must be absolute, exist and be a directory. Returns the path as given (no symlink resolution).
pub fn check_root(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("give an absolute path (it may start with ~/)".into());
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} is not a folder", path.display())),
        Err(_) => Err(format!("{} does not exist", path.display())),
    }
}

/// Minerva's own choices, read from and written to `<home>/minerva.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub default_model: String,
    pub roots: Vec<PathBuf>,
}

/// `<home>/minerva.toml`.
pub fn settings_path(home: &Path) -> PathBuf {
    home.join("minerva.toml")
}

fn read_table(path: &Path) -> Result<Option<toml::Table>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let table = text.parse::<toml::Table>().with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(table))
}

impl Settings {
    /// The file's values; `None` when there is no file. A missing `default_model` reads as the default and
    /// missing `roots` as none (the perch-seeded file has only the model). A wrongly typed key is an error
    /// that names the file.
    pub fn load(home: &Path) -> Result<Option<Settings>> {
        let path = settings_path(home);
        let Some(table) = read_table(&path)? else { return Ok(None) };
        let default_model = match table.get("default_model") {
            None => DEFAULT_MODEL.to_string(),
            Some(value) => value.as_str().with_context(|| format!("{}: default_model must be a string", path.display()))?.to_string(),
        };
        let mut roots = Vec::new();
        if let Some(value) = table.get("roots") {
            let list = value.as_array().with_context(|| format!("{}: roots must be a list of paths", path.display()))?;
            for item in list {
                let text = item.as_str().with_context(|| format!("{}: roots must be a list of paths", path.display()))?;
                roots.push(PathBuf::from(text));
            }
        }
        Ok(Some(Settings { default_model, roots }))
    }

    /// Writes `default_model` and `roots` into `<home>/minerva.toml` atomically (temp file `0600`, fsync, rename).
    /// Any other key already in the file is kept.
    pub fn save(&self, home: &Path) -> Result<()> {
        let path = settings_path(home);
        let mut table = read_table(&path)?.unwrap_or_default();
        table.insert("default_model".into(), toml::Value::String(self.default_model.clone()));
        let mut roots = Vec::new();
        for root in &self.roots {
            let text = root.to_str().with_context(|| format!("root is not valid UTF-8: {}", root.display()))?;
            roots.push(toml::Value::String(text.to_string()));
        }
        table.insert("roots".into(), toml::Value::Array(roots));
        let text = toml::to_string(&table).context("rendering minerva.toml")?;
        write_atomic_0600(&path, text.as_bytes())
    }
}

/// The folders that exist and are directories, in file order, for `--print-roots`. The rest are named in
/// the second list so the caller can warn: a perch started with a missing `--root` would not start.
pub fn usable_roots(settings: &Settings) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for root in &settings.roots {
        if check_root(root).is_ok() { good.push(root.clone()) } else { bad.push(root.clone()) }
    }
    (good, bad)
}

/// `path` written atomically: a `0600` temp file beside it, fsynced, then renamed over it.
pub fn write_atomic_0600(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent folder")?;
    let name = path.file_name().context("path has no file name")?.to_string_lossy().into_owned();
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming {} over {}", tmp.display(), path.display()));
    }
    Ok(())
}

/// Creates `<home>`, `<home>/sessions` and `<home>/scratch` (`0700`) and `<home>/projects.json` (`[]`, `0600`),
/// each only when missing. A folder that already exists keeps its mode.
pub fn prepare_home(home: &Path) -> Result<()> {
    for dir in [home.to_path_buf(), home.join("sessions"), home.join("scratch")] {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let projects = home.join("projects.json");
    if !projects.exists() {
        write_atomic_0600(&projects, b"[]\n")?;
    }
    Ok(())
}

/// The mode of a path, for the tests and the summary.
pub fn mode_of(path: &Path) -> Result<u32> {
    Ok(std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?.permissions().mode() & 0o777)
}

/// Runs `cmd` and returns its stdout when it exits 0 within `limit`; `None` for any failure or a timeout
/// (the child is killed). Stderr is discarded and stdin closed.
pub fn run_timeout(mut cmd: Command, limit: Duration) -> Option<String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    match rx.recv_timeout(limit) {
        Ok(text) => {
            let status = child.wait().ok()?;
            status.success().then_some(text)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// Model ids out of `eidolon models`: a row is a first word that holds a `:` (`provider:model`) followed by
/// more text. The provider table and the notices above it have no such first word. Duplicates are dropped.
pub fn parse_models(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(id), Some(_)) = (words.next(), words.next()) else { continue };
        let ok = id.contains(':')
            && !id.starts_with(':')
            && !id.ends_with(':')
            && !id.contains("//")
            && id.chars().all(|c| c.is_ascii_alphanumeric() || ":._-/".contains(c));
        if ok && seen.insert(id.to_string()) {
            out.push(id.to_string());
        }
    }
    out
}

/// The select's rows: (id, label, hint). The static three, then the current model when it is none of them,
/// then up to `extra` of the discovered ones. `current` is first only as a pre-selection (see `main.rs`).
pub fn model_choices(discovered: &[String], current: Option<&str>, extra: usize) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = STATIC_MODELS.iter().map(|(id, hint)| (id.to_string(), hint.to_string())).collect();
    if let Some(current) = current
        && !rows.iter().any(|(id, _)| id == current)
    {
        rows.push((current.to_string(), "current".into()));
    }
    let mut added = 0;
    for id in discovered {
        if added == extra {
            break;
        }
        if !rows.iter().any(|(have, _)| have == id) {
            rows.push((id.clone(), "from eidolon models".into()));
            added += 1;
        }
    }
    rows
}

/// The Windows profile folder as a WSL path, or `None` when there is none (not WSL, or `cmd.exe` is absent).
/// Asked of `cmd.exe` from `/mnt/c` (so it does not complain about a UNC working directory) and mapped by
/// `wslpath`, so a renamed account or a custom automount root still resolves.
pub fn windows_profile() -> Option<PathBuf> {
    let mut ask = Command::new("cmd.exe");
    ask.args(["/c", "echo %USERPROFILE%"]).current_dir("/mnt/c");
    let raw = run_timeout(ask, Duration::from_secs(5))?;
    let raw = raw.trim_matches(|c| c == '\r' || c == '\n');
    if raw.is_empty() || raw == "%USERPROFILE%" {
        return None;
    }
    let mut map = Command::new("wslpath");
    map.args(["-u", raw]);
    let mapped = run_timeout(map, Duration::from_secs(5))?;
    let path = PathBuf::from(mapped.trim());
    (path.is_absolute() && path.is_dir()).then_some(path)
}

/// `eidolon models` stdout, within a few seconds, or `None`.
pub fn eidolon_models() -> Option<String> {
    let mut cmd = Command::new("eidolon");
    cmd.arg("models");
    run_timeout(cmd, Duration::from_secs(6))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn settings(model: &str, roots: &[&str]) -> Settings {
        Settings { default_model: model.into(), roots: roots.iter().map(PathBuf::from).collect() }
    }

    #[test]
    fn round_trip_keeps_values_and_mode() {
        let dir = tempdir().unwrap();
        let want = settings("mock", &["/home/a", "/srv/b c"]);
        want.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), Some(want));
        assert_eq!(mode_of(&settings_path(dir.path())).unwrap(), 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers, vec![OsString::from("minerva.toml")], "no temp file left behind");
    }

    #[test]
    fn save_keeps_unknown_keys_and_replaces_known_ones() {
        let dir = tempdir().unwrap();
        std::fs::write(settings_path(dir.path()), "default_model = \"old\"\nextra = 7\n").unwrap();
        settings("new", &["/x"]).save(dir.path()).unwrap();
        let text = std::fs::read_to_string(settings_path(dir.path())).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(table["extra"].as_integer(), Some(7));
        assert_eq!(table["default_model"].as_str(), Some("new"));
    }

    #[test]
    fn missing_file_is_none_and_a_model_only_file_has_no_roots() {
        let dir = tempdir().unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), None);
        std::fs::write(settings_path(dir.path()), "default_model = \"mock\"\n").unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), Some(settings("mock", &[])));
        std::fs::write(settings_path(dir.path()), "roots = 3\n").unwrap();
        assert!(Settings::load(dir.path()).is_err());
    }

    #[test]
    fn defaults_come_from_an_existing_file() {
        let dir = tempdir().unwrap();
        settings("ollama:deepseek-v4.1-flash", &["/tmp"]).save(dir.path()).unwrap();
        let have = Settings::load(dir.path()).unwrap().unwrap();
        let rows = model_choices(&[], Some(&have.default_model), 0);
        assert!(rows.iter().any(|(id, _)| id == "ollama:deepseek-v4.1-flash"));
        let odd = model_choices(&[], Some("zai:glm-5.3"), 0);
        assert_eq!(odd.last().unwrap().0, "zai:glm-5.3", "a current model outside the list is kept as a row");
        assert_eq!(have.roots, vec![PathBuf::from("/tmp")]);
    }

    #[test]
    fn home_under_mnt_is_refused() {
        assert!(check_home(Path::new("/mnt/c/Users/me/Minerva")).unwrap_err().contains("/mnt/"));
        assert!(check_home(Path::new("/mnt")).is_err());
        assert!(check_home(Path::new("/mntx/ok")).is_ok(), "only the /mnt folder itself, not a name that starts with it");
        assert!(check_home(Path::new("/home/me/Minerva")).is_ok());
        assert!(check_home(Path::new("relative/dir")).is_err());
    }

    #[test]
    fn roots_must_exist_and_be_folders() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(check_root(dir.path()).is_ok());
        assert!(check_root(&file).unwrap_err().contains("not a folder"));
        assert!(check_root(&dir.path().join("nope")).unwrap_err().contains("does not exist"));
        assert!(check_root(Path::new("rel")).is_err());
    }

    #[test]
    fn print_roots_lists_only_folders_that_exist() {
        let dir = tempdir().unwrap();
        let there = dir.path().join("there");
        std::fs::create_dir(&there).unwrap();
        let s = Settings { default_model: "mock".into(), roots: vec![there.clone(), dir.path().join("gone")] };
        let (good, bad) = usable_roots(&s);
        assert_eq!(good, vec![there]);
        assert_eq!(bad, vec![dir.path().join("gone")]);
    }

    #[test]
    fn expand_handles_tilde_and_trailing_slash() {
        let home = Path::new("/home/me");
        assert_eq!(expand("~", home), PathBuf::from("/home/me"));
        assert_eq!(expand(" ~/Projects/ ", home), PathBuf::from("/home/me/Projects"));
        assert_eq!(expand("/srv/x/", home), PathBuf::from("/srv/x"));
        assert_eq!(expand("/", home), PathBuf::from("/"));
    }

    #[test]
    fn home_default_prefers_minerva_home() {
        let got = default_home(Some("/srv/m".into()), Some("/home/me".into())).unwrap();
        assert_eq!(got, PathBuf::from("/srv/m"));
        let got = default_home(None, Some("/home/me".into())).unwrap();
        assert_eq!(got, PathBuf::from("/home/me/Minerva"));
        assert!(default_home(Some("rel".into()), None).is_err());
        assert!(default_home(None, None).is_err());
    }

    #[test]
    fn prepare_home_makes_the_pieces_once() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("Minerva");
        prepare_home(&home).unwrap();
        assert_eq!(mode_of(&home.join("sessions")).unwrap(), 0o700);
        assert_eq!(mode_of(&home.join("scratch")).unwrap(), 0o700);
        assert_eq!(std::fs::read_to_string(home.join("projects.json")).unwrap(), "[]\n");
        std::fs::write(home.join("projects.json"), "[1]").unwrap();
        prepare_home(&home).unwrap();
        assert_eq!(std::fs::read_to_string(home.join("projects.json")).unwrap(), "[1]", "an existing file is left alone");
    }

    #[test]
    fn models_are_parsed_out_of_the_listing() {
        let text = "deepseek\tAnthropic\thttps://api.deepseek.com/anthropic\t(built-in)\n\
                    nothing cached for deepseek, openrouter -- declared floor only\n\
                    \n\
                    deepseek:deepseek-flash                  DeepSeek Flash   deepseek\n\
                    zai:glm-5.3                              GLM 5.3          zai\n\
                    zai:glm-5.3                              GLM 5.3          zai\n\
                    https://x.y/z  a b\n";
        assert_eq!(parse_models(text), vec!["deepseek:deepseek-flash".to_string(), "zai:glm-5.3".to_string()]);
    }

    #[test]
    fn model_choices_add_discovered_ones_after_the_static_three() {
        let found = vec!["chatgpt:gpt-5.6-luna".to_string(), "a:b".to_string(), "c:d".to_string(), "e:f".to_string()];
        let rows = model_choices(&found, None, 2);
        let ids: Vec<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["chatgpt:gpt-5.6-luna", "ollama:deepseek-v4.1-flash", "mock", "a:b", "c:d"]);
    }

    #[test]
    fn a_slow_command_is_killed() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let t = std::time::Instant::now();
        assert_eq!(run_timeout(cmd, Duration::from_millis(200)), None);
        assert!(t.elapsed() < Duration::from_secs(3));
        let mut ok = Command::new("echo");
        ok.arg("hi");
        assert_eq!(run_timeout(ok, Duration::from_secs(3)).as_deref(), Some("hi\n"));
    }
}
