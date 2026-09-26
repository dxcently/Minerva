//! Search primitive: regex grep over a tree, honouring `.gitignore`, with a
//! glob filter and a result cap. Uses the `ignore` walker (ripgrep's) so the
//! set of files searched matches what a developer expects.

use std::path::Path;

use anyhow::Context;
use ignore::WalkBuilder;
use regex::RegexBuilder;

pub const DEFAULT_MAX: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

pub struct GrepOptions<'a> {
    pub pattern: &'a str,
    pub path: Option<&'a str>,
    pub glob: Option<&'a str>,
    pub case_insensitive: bool,
    pub max: Option<usize>,
}

pub fn grep(cwd: &Path, opts: GrepOptions<'_>) -> anyhow::Result<(Vec<Hit>, bool)> {
    let re = RegexBuilder::new(opts.pattern)
        .case_insensitive(opts.case_insensitive)
        .build()
        .with_context(|| format!("invalid regex {:?}", opts.pattern))?;
    let root = super::resolve(cwd, opts.path.unwrap_or("."));
    let glob = match opts.glob {
        Some(g) => Some(globset(g)?),
        None => None,
    };
    let max = opts.max.unwrap_or(DEFAULT_MAX).max(1);
    let mut hits = Vec::new();
    let mut truncated = false;

    'walk: for entry in WalkBuilder::new(&root)
        .hidden(true)
        .git_ignore(true)
        .build()
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(g) = &glob {
            let rel = path.strip_prefix(&root).unwrap_or(path);
            if !g.is_match(rel) && !g.is_match(path.file_name().unwrap_or_default()) {
                continue;
            }
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                if hits.len() >= max {
                    truncated = true;
                    break 'walk;
                }
                let shown = path.strip_prefix(cwd).unwrap_or(path).display().to_string();
                let text = if line.len() > 500 {
                    format!("{}…", &line[..line.floor_char_boundary(500)])
                } else {
                    line.to_string()
                };
                hits.push(Hit {
                    path: shown,
                    line: i + 1,
                    text,
                });
            }
        }
    }
    Ok((hits, truncated))
}

fn globset(g: &str) -> anyhow::Result<globset::GlobSet> {
    let mut b = globset::GlobSetBuilder::new();
    for part in g.split(',') {
        let part = part.trim();
        if !part.is_empty() {
            b.add(globset::Glob::new(part).with_context(|| format!("invalid glob {part:?}"))?);
        }
    }
    Ok(b.build()?)
}

/// Render hits as `path:line: text`, the shape every editor understands.
pub fn render(hits: &[Hit], truncated: bool) -> String {
    if hits.is_empty() {
        return "[no matches]".into();
    }
    let mut s = String::new();
    for h in hits {
        s.push_str(&format!("{}:{}: {}\n", h.path, h.line, h.text));
    }
    if truncated {
        s.push_str("[more matches not shown; narrow the pattern or path]\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_with_glob() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(d.path().join("b.txt"), "fn main() {}\n").unwrap();
        let (hits, _) = grep(
            d.path(),
            GrepOptions {
                pattern: "main",
                path: None,
                glob: Some("*.rs"),
                case_insensitive: false,
                max: None,
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a.rs");
    }
}
