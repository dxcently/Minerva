//! A chat's git head, read from `.git/HEAD` the way the tab needs it and no further. **git is never run**: a repository is
//! the operator's folder and git reads its config for filters, hooks and `core.fsmonitor`, any of which runs code.
//!
//! ```text
//!   <cwd>/.git            a directory   -> <cwd>/.git/HEAD
//!                         a file        -> `gitdir: <path>` (a linked worktree or a submodule) -> <path>/HEAD, the one hop git itself makes
//!                         a symlink, or anything else          -> nothing (a link is never followed: it could point anywhere)
//!   HEAD                  `ref: refs/heads/<name>`             -> the branch
//!                         40 or 64 hex digits                  -> a detached head, that sha
//!                         anything else                        -> nothing
//! ```
//!
//! Every read opens with `O_NOFOLLOW` and `O_NONBLOCK` (a `HEAD` that is a link is refused, and one that is a FIFO does not hold the
//! thread), checks the open file is a regular one, and reads at most [`MAX`] bytes. What comes out is a branch name or hex digits, so
//! the worst a hostile folder can make this say is a branch name it chose. A repository above the folder is not looked for: the folder a
//! chat runs in is the repository or it is not one here.

use std::io::Read as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// A `HEAD` or a `.git` file is one line. This is room for a long path and a long branch name.
const MAX: u64 = 4096;
/// Git's own limit on a ref name's length is a path component's; a branch name past this is not shown.
const MAX_BRANCH: usize = 255;

#[derive(Debug, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    /// Lower case hex, 40 digits (sha-1) or 64 (sha-256).
    Detached(String),
    /// No repository here, one that is not read (a link), or a `HEAD` that is not a branch or a sha.
    Nothing,
}

/// The head of the repository whose working tree is `root`, which the caller has already made canonical.
pub fn head_of(root: &Path) -> Head {
    let dotgit = root.join(".git");
    let Ok(meta) = std::fs::symlink_metadata(&dotgit) else { return Head::Nothing };
    let gitdir = if meta.is_dir() {
        dotgit
    } else if meta.is_file() {
        match read_small(&dotgit).as_deref().and_then(|text| gitdir_of(text, root)).and_then(|dir| dir.canonicalize().ok()) {
            Some(dir) => dir,
            None => return Head::Nothing,
        }
    } else {
        return Head::Nothing;
    };
    read_small(&gitdir.join("HEAD")).map_or(Head::Nothing, |text| parse_head(&text))
}

/// `path`'s text, when it is a regular file of at most [`MAX`] bytes, valid UTF-8, and not reached through a link at its last name.
fn read_small(path: &Path) -> Option<String> {
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX {
        return None;
    }
    let mut bytes = Vec::new();
    // One byte past the cap: a file that grew since the `fstat` is still bounded.
    file.take(MAX + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The directory a `.git` file names: its one line `gitdir: <path>`, a relative path being relative to the folder the file is in.
fn gitdir_of(text: &str, base: &Path) -> Option<PathBuf> {
    let line = text.trim();
    let path = line.strip_prefix("gitdir:")?.trim();
    if path.is_empty() || path.contains(['\0', '\n']) {
        return None;
    }
    Some(base.join(path))
}

fn parse_head(text: &str) -> Head {
    let line = text.trim();
    if let Some(name) = line.strip_prefix("ref: refs/heads/") {
        let ok = !name.is_empty() && name.len() <= MAX_BRANCH && !name.chars().any(char::is_control);
        return if ok { Head::Branch(name.to_string()) } else { Head::Nothing };
    }
    if matches!(line.len(), 40 | 64) && line.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Head::Detached(line.to_ascii_lowercase());
    }
    Head::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn repo(root: &Path, head: &str) {
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), head).unwrap();
    }

    #[test]
    fn head_text_is_a_branch_a_sha_or_nothing() {
        assert_eq!(parse_head("ref: refs/heads/main\n"), Head::Branch("main".into()));
        assert_eq!(parse_head("ref: refs/heads/feature/a b\n"), Head::Branch("feature/a b".into()), "a slash and a space are in a branch name");
        assert_eq!(parse_head(&format!("{SHA}\n")), Head::Detached(SHA.into()));
        assert_eq!(parse_head(&SHA.to_uppercase()), Head::Detached(SHA.into()), "the sha is lower cased");
        let long = "a".repeat(64);
        assert_eq!(parse_head(&long), Head::Detached(long.clone()));
        for bad in [
            "",
            "ref: refs/heads/\n",
            "ref: refs/tags/v1\n",
            "ref: refs/heads/a\x1b[31m\n",
            "ref:refs/heads/main",
            "0123456789abcdef",
            &SHA[..39],
            &format!("{SHA}0"),
            "ref: refs/heads/main\nref: refs/heads/other\n",
            "gitdir: /x",
        ] {
            assert_eq!(parse_head(bad), Head::Nothing, "{bad:?}");
        }
        assert_eq!(parse_head(&format!("ref: refs/heads/{}", "x".repeat(MAX_BRANCH + 1))), Head::Nothing);
    }

    #[test]
    fn a_gitdir_line_is_one_path_and_a_relative_one_is_beside_the_file() {
        let base = Path::new("/w/tree");
        assert_eq!(gitdir_of("gitdir: /r/.git/worktrees/w\n", base), Some(PathBuf::from("/r/.git/worktrees/w")));
        assert_eq!(gitdir_of("gitdir: ../r/.git/modules/s", base), Some(PathBuf::from("/w/tree/../r/.git/modules/s")));
        for bad in ["", "gitdir:", "gitdir:   \n", "/r/.git", "gitdir /r", "gitdir: /a\nb"] {
            assert_eq!(gitdir_of(bad, base), None, "{bad:?}");
        }
    }

    #[test]
    fn a_branch_a_detached_head_and_no_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert_eq!(head_of(&root), Head::Nothing, "no .git at all");
        repo(&root, "ref: refs/heads/main\n");
        assert_eq!(head_of(&root), Head::Branch("main".into()));
        repo(&root, &format!("{SHA}\n"));
        assert_eq!(head_of(&root), Head::Detached(SHA.into()));
        repo(&root, "this is not a head\n");
        assert_eq!(head_of(&root), Head::Nothing);
    }

    #[test]
    fn a_dot_git_file_is_followed_once_to_the_linked_worktrees_head() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let (main, wt, sub) = (base.join("main"), base.join("wt"), base.join("sub"));
        let linked = main.join(".git/worktrees/wt");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", linked.display())).unwrap();
        assert_eq!(head_of(&wt), Head::Branch("feature".into()), "an absolute gitdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        assert_eq!(head_of(&sub), Head::Branch("feature".into()), "a relative gitdir is relative to the folder of the file");
        // The hop is one: a gitdir whose own `.git`-ish thing is another pointer is a HEAD that is not one.
        std::fs::write(linked.join("HEAD"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(head_of(&wt), Head::Nothing);
        std::fs::write(wt.join(".git"), "gitdir: /nowhere/at/all\n").unwrap();
        assert_eq!(head_of(&wt), Head::Nothing, "a gitdir that is not there");
        std::fs::write(wt.join(".git"), "not a pointer\n").unwrap();
        assert_eq!(head_of(&wt), Head::Nothing);
    }

    /// Mutation: open without `O_NOFOLLOW`, or take `metadata` instead of `symlink_metadata` on `.git`, and a case here goes red.
    #[test]
    fn a_link_is_never_followed_at_the_dot_git_or_at_head() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let outside = base.join("outside");
        repo(&outside, "ref: refs/heads/stolen\n");
        let (a, b, c, d) = (base.join("a"), base.join("b"), base.join("c"), base.join("d"));
        for folder in [&a, &b, &c, &d] {
            std::fs::create_dir_all(folder).unwrap();
        }
        // `.git` is a link to a repository outside the folder.
        std::os::unix::fs::symlink(outside.join(".git"), a.join(".git")).unwrap();
        assert_eq!(head_of(&a), Head::Nothing);
        // `.git` is a real directory whose `HEAD` is a link to a file outside it.
        std::fs::create_dir_all(b.join(".git")).unwrap();
        std::os::unix::fs::symlink(outside.join(".git/HEAD"), b.join(".git/HEAD")).unwrap();
        assert_eq!(head_of(&b), Head::Nothing);
        // A `.git` file that is a link, and a `.git` file pointing at a gitdir whose `HEAD` is a link.
        std::os::unix::fs::symlink(outside.join(".git/HEAD"), c.join(".git")).unwrap();
        assert_eq!(head_of(&c), Head::Nothing);
        let linked = base.join("linked-gitdir");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(outside.join(".git/HEAD"), linked.join("HEAD")).unwrap();
        std::fs::write(d.join(".git"), format!("gitdir: {}\n", linked.display())).unwrap();
        assert_eq!(head_of(&d), Head::Nothing);
        // The control: the same outside repository, read as a real directory, is what the links would have said.
        assert_eq!(head_of(&outside), Head::Branch("stolen".into()));
    }

    #[test]
    fn a_head_that_is_big_or_a_fifo_is_refused_and_does_not_hold_the_thread() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        repo(&root, &format!("ref: refs/heads/{}\n", "x".repeat(MAX as usize)));
        assert_eq!(head_of(&root), Head::Nothing, "over the byte cap");
        std::fs::remove_file(root.join(".git/HEAD")).unwrap();
        let fifo = std::ffi::CString::new(root.join(".git/HEAD").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(head_of(&root), Head::Nothing, "a FIFO named HEAD: refused, and open() returned");
    }
}
