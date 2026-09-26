//! File primitives: numbered read, whole-file write, exact-substring edit.

use std::path::Path;

use anyhow::{Context, bail};

/// Default cap on lines returned by [`read`] when the caller gives none.
pub const DEFAULT_LIMIT: usize = 2000;
/// A single line longer than this is truncated with a marker.
pub const MAX_LINE: usize = 2000;

/// Read `path`, returning `cat -n`-style numbered lines from `offset`
/// (1-based) for `limit` lines. Numbering makes the output citable
/// (`file:line`) and makes truncation visible.
pub fn read(
    cwd: &Path,
    path: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> anyhow::Result<String> {
    let full = super::resolve(cwd, path);
    let bytes = std::fs::read(&full).with_context(|| format!("reading {}", full.display()))?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        bail!(
            "{} looks binary ({} bytes); not rendering it",
            full.display(),
            bytes.len()
        );
    }
    let text = String::from_utf8_lossy(&bytes);
    let start = offset.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(DEFAULT_LIMIT).max(1);
    let total = text.lines().count();
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(start - 1).take(limit) {
        let n = i + 1;
        if line.chars().count() > MAX_LINE {
            let cut: String = line.chars().take(MAX_LINE).collect();
            out.push_str(&format!("{n:>6}\t{cut}… [line truncated]\n"));
        } else {
            out.push_str(&format!("{n:>6}\t{line}\n"));
        }
    }
    if total == 0 {
        out.push_str("[empty file]\n");
    } else if start + limit - 1 < total {
        out.push_str(&format!(
            "[showing lines {start}-{} of {total}]\n",
            start + limit - 1
        ));
    }
    Ok(out)
}

/// Write `content` to `path`, creating parent directories. Returns a short
/// receipt. Overwrites without asking — the *policy* decision was made
/// upstream; this is the effect.
pub fn write(cwd: &Path, path: &str, content: &str) -> anyhow::Result<String> {
    let full = super::resolve(cwd, path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let existed = full.exists();
    std::fs::write(&full, content).with_context(|| format!("writing {}", full.display()))?;
    Ok(format!(
        "{} {} ({} bytes)",
        if existed { "overwrote" } else { "created" },
        full.display(),
        content.len()
    ))
}

/// Replace `old` with `new` in `path`. Without `replace_all`, `old` must
/// occur exactly once — zero is "not found", more than one is "ambiguous",
/// and both are errors the model can act on by supplying more context.
pub fn edit(
    cwd: &Path,
    path: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> anyhow::Result<String> {
    let full = super::resolve(cwd, path);
    if old.is_empty() {
        bail!("old_str must not be empty");
    }
    if old == new {
        bail!("old_str and new_str are identical");
    }
    let text =
        std::fs::read_to_string(&full).with_context(|| format!("reading {}", full.display()))?;
    let count = text.matches(old).count();
    let updated = match (count, replace_all) {
        (0, _) => bail!("old_str not found in {}", full.display()),
        (1, _) => text.replacen(old, new, 1),
        (_, true) => text.replace(old, new),
        (n, false) => bail!(
            "old_str occurs {n} times in {}; add context to make it unique or set replace_all",
            full.display()
        ),
    };
    std::fs::write(&full, &updated).with_context(|| format!("writing {}", full.display()))?;
    let n = if replace_all { count } else { 1 };
    Ok(format!(
        "edited {} ({n} replacement{})",
        full.display(),
        if n == 1 { "" } else { "s" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_requires_uniqueness() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "f.txt", "a b a").unwrap();
        assert!(edit(d.path(), "f.txt", "a", "c", false).is_err());
        edit(d.path(), "f.txt", "a", "c", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.path().join("f.txt")).unwrap(),
            "c b c"
        );
    }

    #[test]
    fn read_numbers_and_windows() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "f.txt", "one\ntwo\nthree\n").unwrap();
        let out = read(d.path(), "f.txt", Some(2), Some(1)).unwrap();
        assert!(out.starts_with("     2\ttwo\n"));
        assert!(out.contains("of 3"));
    }
}
