//! Projects: a folder with a name, remembered in `<home>/projects.json` as `[{name, path, created}]`; and the
//! small `<home>/chat-cwds.json` map (`{id: cwd}`) that lets a saved chat be put under its project after the perch
//! restarts (the cwd is inside the journal, which the perch never decodes, so it remembers the one it started the
//! chat in).
//!
//! A project is only a name and a path. Removing one forgets the entry; **nothing in the folder is touched, ever**.
//! Both files are written atomically with mode `0600`. The path of a project is judged by the caller against the
//! allowed roots (see `serve.rs`); this file holds no policy on where a folder may be.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// A project name is a label, not a path: short, one line.
pub const MAX_NAME: usize = 100;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Project {
    pub name: String,
    pub path: String,
    /// Unix seconds.
    pub created: u64,
}

#[derive(Debug, PartialEq)]
pub enum AddError {
    /// No Minerva home: nowhere to keep the list.
    NoHome,
    BadName(&'static str),
    Write(String),
}

/// A name as a person gave it, trimmed; `Err` says why not.
pub fn clean_name(raw: &str) -> std::result::Result<String, &'static str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("a project needs a name");
    }
    if name.chars().count() > MAX_NAME {
        return Err("that name is too long (100 characters at most)");
    }
    if name.chars().any(|c| c.is_control()) {
        return Err("a name cannot contain control characters");
    }
    Ok(name.to_string())
}

pub struct Projects {
    file: Option<PathBuf>,
    items: Mutex<Vec<Project>>,
}

impl Projects {
    /// Reads `file` (absent is empty). A file that does not parse is left alone on disk and read as empty: the next
    /// successful change replaces it, and the warning says so.
    pub fn open(file: Option<PathBuf>) -> Self {
        let items = file.as_deref().map(read_list).unwrap_or_default();
        Self { file, items: Mutex::new(items) }
    }

    pub fn enabled(&self) -> bool {
        self.file.is_some()
    }

    pub fn list(&self) -> Vec<Project> {
        self.items.lock().unwrap().clone()
    }

    /// Adds the folder `path` (canonical, already checked) as `name`, or the folder's own name. A path that is already
    /// a project is not added twice: the existing entry comes back, and `false` says nothing was created.
    pub fn add(&self, path: &Path, name: Option<&str>) -> std::result::Result<(Project, bool), AddError> {
        let Some(file) = &self.file else { return Err(AddError::NoHome) };
        let text = path.to_string_lossy().into_owned();
        let mut items = self.items.lock().unwrap();
        if let Some(found) = items.iter().find(|p| p.path == text) {
            return Ok((found.clone(), false));
        }
        let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(name) => clean_name(name).map_err(AddError::BadName)?,
            None => path.file_name().and_then(|n| n.to_str()).map(str::to_string).filter(|n| !n.is_empty()).unwrap_or_else(|| "project".into()),
        };
        let created = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let project = Project { name, path: text, created };
        let mut next = items.clone();
        next.push(project.clone());
        write(file, &next).map_err(|e| AddError::Write(format!("{e:#}")))?;
        *items = next;
        Ok((project, true))
    }

    /// Forgets entry `n` (its place in the list). `Ok(None)` is no such entry.
    pub fn remove(&self, n: usize) -> std::result::Result<Option<Project>, AddError> {
        let Some(file) = &self.file else { return Err(AddError::NoHome) };
        let mut items = self.items.lock().unwrap();
        if n >= items.len() {
            return Ok(None);
        }
        let mut next = items.clone();
        let gone = next.remove(n);
        write(file, &next).map_err(|e| AddError::Write(format!("{e:#}")))?;
        *items = next;
        Ok(Some(gone))
    }

    /// The name of the project `cwd` is in (its folder or one inside it); the deepest wins.
    pub fn name_of(&self, cwd: &Path) -> Option<String> {
        self.items
            .lock()
            .unwrap()
            .iter()
            .filter(|p| cwd.starts_with(&p.path))
            .max_by_key(|p| Path::new(&p.path).components().count())
            .map(|p| p.name.clone())
    }
}

fn read_list(file: &Path) -> Vec<Project> {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, path = %file.display(), "perch: projects.json could not be read; starting with none");
            return Vec::new();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %file.display(), "perch: projects.json does not parse; starting with none");
        Vec::new()
    })
}

fn write(file: &Path, items: &[Project]) -> Result<()> {
    let body = serde_json::to_vec_pretty(items).context("serializing the projects")?;
    harnox::fs::write_atomic_0600(file, &body).with_context(|| format!("writing {}", file.display()))
}

/// `{id: cwd}`, remembered across restarts.
pub struct Cwds {
    file: Option<PathBuf>,
    map: Mutex<HashMap<String, String>>,
}

impl Cwds {
    pub fn open(file: Option<PathBuf>) -> Self {
        let map = file
            .as_deref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { file, map: Mutex::new(map) }
    }

    pub fn get(&self, id: &str) -> Option<PathBuf> {
        self.map.lock().unwrap().get(id).map(PathBuf::from)
    }

    /// Remembers `cwd` for `id`; a write that fails is logged and the memory still has it.
    pub fn remember(&self, id: &str, cwd: &Path) {
        let text = cwd.to_string_lossy().into_owned();
        let mut map = self.map.lock().unwrap();
        if map.get(id) == Some(&text) {
            return;
        }
        map.insert(id.to_string(), text);
        if let Some(file) = &self.file {
            let written = serde_json::to_vec(&*map).context("serializing the chat folders").and_then(|body| {
                harnox::fs::write_atomic_0600(file, &body).with_context(|| format!("writing {}", file.display()))
            });
            if let Err(e) = written {
                tracing::warn!(error = %format!("{e:#}"), "perch: the chat folders could not be saved");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_added_once_named_and_forgotten_without_touching_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("projects.json");
        let folder = dir.path().join("work");
        std::fs::create_dir(&folder).unwrap();
        let projects = Projects::open(Some(file.clone()));
        let (first, made) = projects.add(&folder, None).unwrap();
        assert!(made);
        assert_eq!((first.name.as_str(), first.path.as_str()), ("work", folder.to_str().unwrap()), "the name defaults to the folder's");
        let (again, made) = projects.add(&folder, Some("other")).unwrap();
        assert!(!made && again == first, "one entry per path");
        assert_eq!(projects.list().len(), 1);
        let named = dir.path().join("two");
        let (second, _) = projects.add(&named, Some("  The Two  ")).unwrap();
        assert_eq!(second.name, "The Two");
        // It is on disk, 0600, and a second open reads it back.
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(Projects::open(Some(file.clone())).list(), projects.list());
        assert_eq!(projects.remove(5), Ok(None));
        assert_eq!(projects.remove(0).unwrap().unwrap().name, "work");
        assert!(folder.is_dir(), "forgetting a project never deletes the folder");
        assert_eq!(Projects::open(Some(file)).list().len(), 1);
    }

    #[test]
    fn names_are_labels_and_no_home_keeps_nothing() {
        for bad in ["", "   ", "a\nb", "a\u{7}b"] {
            assert!(clean_name(bad).is_err(), "{bad:?}");
        }
        assert!(clean_name(&"n".repeat(101)).is_err());
        assert_eq!(clean_name(" ok "), Ok("ok".into()));
        let dir = tempfile::tempdir().unwrap();
        let none = Projects::open(None);
        assert_eq!(none.add(dir.path(), None), Err(AddError::NoHome));
        assert!(none.list().is_empty() && !none.enabled());
        let some = Projects::open(Some(dir.path().join("p.json")));
        assert!(matches!(some.add(dir.path(), Some("a\nb")), Err(AddError::BadName(_))));
        assert!(some.list().is_empty(), "a refused add changes nothing");
    }

    #[test]
    fn a_chat_belongs_to_the_deepest_project_that_holds_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let projects = Projects::open(Some(dir.path().join("p.json")));
        projects.add(Path::new("/p/outer"), Some("outer")).unwrap();
        projects.add(Path::new("/p/outer/inner"), Some("inner")).unwrap();
        assert_eq!(projects.name_of(Path::new("/p/outer/x")).as_deref(), Some("outer"));
        assert_eq!(projects.name_of(Path::new("/p/outer/inner/x/y")).as_deref(), Some("inner"));
        assert_eq!(projects.name_of(Path::new("/p/outer")).as_deref(), Some("outer"));
        assert_eq!(projects.name_of(Path::new("/p/outerx")), None, "a prefix of the name is not inside");
        assert_eq!(projects.name_of(Path::new("/q")), None);
    }

    #[test]
    fn a_corrupt_list_reads_as_empty_and_the_next_change_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("projects.json");
        std::fs::write(&file, "not json").unwrap();
        let projects = Projects::open(Some(file.clone()));
        assert!(projects.list().is_empty());
        projects.add(Path::new("/p/x"), None).unwrap();
        assert_eq!(Projects::open(Some(file)).list().len(), 1);
    }

    #[test]
    fn chat_folders_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chat-cwds.json");
        let cwds = Cwds::open(Some(file.clone()));
        cwds.remember("a", Path::new("/p/x"));
        cwds.remember("a", Path::new("/p/x"));
        cwds.remember("b", Path::new("/p/y"));
        let back = Cwds::open(Some(file));
        assert_eq!(back.get("a"), Some(PathBuf::from("/p/x")));
        assert_eq!(back.get("b"), Some(PathBuf::from("/p/y")));
        assert_eq!(back.get("c"), None);
    }
}
