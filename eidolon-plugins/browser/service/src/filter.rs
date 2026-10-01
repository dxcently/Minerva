//! What happens to snapshot text after it is rendered: the head block, the
//! first-heading lookup, and the role filter. All pure functions over the
//! text, so the tree's grammar is the only contract between them and the
//! renderer (`snapshot.rs`).
//!
//! The grammar: one node per line, `<2n spaces>- <content>`, content being
//! `role "name" [k=v]... [ref=R]`, with a trailing `:` when children follow.
//! A head containing a colon is wrapped in single quotes with `'` doubled.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::text::quote;

/// The roles a line's "nearest landmark" is looked for among.
pub const LANDMARK_ROLES: [&str; 7] = [
    "banner",
    "navigation",
    "main",
    "complementary",
    "contentinfo",
    "search",
    "region",
];

static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^'((?:''|[^'])*)'\s*:").unwrap());
static ROLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9_-]*").unwrap());
static ROLE_AND_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^(?P<role>[A-Za-z][A-Za-z0-9_-]*)(?:\s+"(?P<name>(?:[^"\\]|\\.)*)")?(?P<attrs>(?:\s*\[[A-Za-z]+=[^\]]*\])*)"#,
    )
    .unwrap()
});
static ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([A-Za-z]+)=([^\]]*)\]").unwrap());
static REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[ref=([^\]]+)\]").unwrap());

/// A line's content with the quoted wrap undone, or `None` when the wrap is
/// broken.
fn unwrap_quoted(content: &str) -> Option<String> {
    if content.starts_with('\'') {
        let caps = QUOTED.captures(content)?;
        Some(caps[1].replace("''", "'"))
    } else {
        Some(content.to_string())
    }
}

/// The role a line's content starts with, if it starts with one.
pub fn line_role(content: &str) -> Option<String> {
    let content = unwrap_quoted(content)?;
    ROLE.find(&content).map(|m| m.as_str().to_string())
}

/// Role and raw (still escaped) name.
fn line_role_and_name(content: &str) -> Option<(String, Option<String>, String)> {
    let content = unwrap_quoted(content)?;
    let caps = ROLE_AND_NAME.captures(&content)?;
    Some((
        caps["role"].to_string(),
        caps.name("name").map(|m| m.as_str().to_string()),
        caps.name("attrs")
            .map_or(String::new(), |m| m.as_str().to_string()),
    ))
}

/// Every ref a snapshot's text hands out.
pub fn refs_in(tree: &str) -> Vec<String> {
    REF.captures_iter(tree).map(|c| c[1].to_string()).collect()
}

/// The name of the first level-1 heading, if the tree has one.
pub fn first_heading(tree: &str) -> Option<String> {
    for raw in tree.lines() {
        let stripped = raw.trim_start_matches(' ');
        let Some(content) = stripped.strip_prefix("- ") else {
            continue;
        };
        let Some((role, name, attrs)) = line_role_and_name(content) else {
            continue;
        };
        if role != "heading" {
            continue;
        }
        let level_one = ATTR
            .captures_iter(&attrs)
            .any(|c| &c[1] == "level" && &c[2] == "1");
        match name {
            Some(n) if level_one && !n.is_empty() => return Some(n),
            _ => {}
        }
    }
    None
}

/// The head block every snapshot starts with. `chars` is the tree's length
/// in characters, so a reader can check it got the whole thing.
#[allow(clippy::too_many_arguments)]
pub fn head(
    url: &str,
    title: &str,
    heading: Option<&str>,
    scope: Option<&str>,
    roles: Option<&[String]>,
    section: Option<&str>,
    chars: usize,
) -> String {
    let mut out = format!("url: {url}\ntitle: {title}\n");
    if let Some(h) = heading {
        out.push_str(&format!("heading: {h}\n"));
    }
    if let Some(s) = scope {
        out.push_str(&format!("scope: {s}\n"));
    }
    if let Some(r) = roles {
        out.push_str(&format!("roles: {}\n", r.join(", ")));
    }
    if let Some(s) = section {
        out.push_str(&format!("section: {s}\n"));
    }
    out.push_str(&format!("chars: {chars}\n\n"));
    out
}

struct Frame {
    indent: usize,
    role: Option<String>,
    pending: bool,
    section_root: bool,
}

/// Keep only the lines whose role is in `roles`.
///
/// With `within`, the first line is the scope root: it is kept, and a line
/// passes only when its nearest landmark is the scope itself (a link inside
/// a `navigation` inside `main` belongs to the navigation). With `section`,
/// a line passes only inside the one landmark whose first child is a heading
/// named `section`. `max` caps how many lines are kept.
pub fn filter_roles(
    tree: &str,
    roles: &HashSet<String>,
    within: Option<&str>,
    max: Option<usize>,
    section: Option<&str>,
    url: &str,
) -> Result<String, String> {
    let lines: Vec<&str> = tree.lines().collect();
    if lines.is_empty() {
        return Ok(tree.to_string());
    }
    let (scope_line, body) = match within {
        Some(_) => (Some(lines[0]), &lines[1..]),
        None => (None, &lines[..]),
    };
    let mut stack: Vec<Frame> = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    let mut section_roots = 0usize;

    for raw in body {
        let stripped = raw.trim_start_matches(' ');
        let Some(content) = stripped.strip_prefix("- ") else {
            continue;
        };
        let indent = raw.len() - stripped.len();
        while stack.last().is_some_and(|f| f.indent >= indent) {
            stack.pop();
        }
        let role = line_role(content);

        if let Some(section) = section
            && let Some(top) = stack.last_mut()
            && top.pending
        {
            top.pending = false;
            if let Some((r, Some(name), _)) = line_role_and_name(content)
                && r == "heading"
                && name == section
            {
                top.section_root = true;
                section_roots += 1;
            }
        }

        let landmark = stack.iter().rev().find(|f| {
            f.role
                .as_deref()
                .is_some_and(|r| LANDMARK_ROLES.contains(&r))
        });
        let passes = if section.is_some() {
            landmark.is_some_and(|f| f.section_root)
        } else {
            let nearest = landmark.and_then(|f| f.role.as_deref()).or(within);
            within.is_none() || nearest == within
        };

        if let Some(r) = role.as_deref()
            && roles.contains(r)
            && passes
        {
            kept.push(stripped);
            if section.is_none() && max.is_some_and(|m| kept.len() >= m) {
                break;
            }
        }

        let is_landmark = role.as_deref().is_some_and(|r| LANDMARK_ROLES.contains(&r));
        stack.push(Frame {
            indent,
            role,
            pending: section.is_some() && is_landmark,
            section_root: false,
        });
    }

    if let Some(section) = section {
        if section_roots == 0 {
            return Err(format!(
                "section {}: no landmark whose first heading is that on {url}",
                quote(section)
            ));
        }
        if section_roots > 1 {
            return Err(format!(
                "section {}: {section_roots} landmarks whose first heading is that on {url}; a section names exactly one",
                quote(section)
            ));
        }
        if let Some(m) = max {
            kept.truncate(m);
        }
    }

    Ok(match scope_line {
        None => {
            let mut out = kept.join("\n");
            if !kept.is_empty() {
                out.push('\n');
            }
            out
        }
        Some(scope) => {
            let mut out = format!("{scope}\n");
            for k in kept {
                out.push_str("  ");
                out.push_str(k);
                out.push('\n');
            }
            out
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(roles: &[&str]) -> HashSet<String> {
        roles.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn line_role_reads_plain_quoted_and_property_lines() {
        assert_eq!(
            line_role(r#"link "Home" [ref=e7]"#).as_deref(),
            Some("link")
        );
        assert_eq!(line_role("generic [ref=e5]:").as_deref(), Some("generic"));
        assert_eq!(line_role("/url: https://x"), None);
        assert_eq!(line_role(": not a role at all"), None);
        assert_eq!(line_role("'unterminated"), None);
        assert_eq!(
            line_role(r#"'link "Toolbox: Ship''s Cat on the Kalmar Nyckel" [ref=e5081] [cursor=pointer]':"#)
                .as_deref(),
            Some("link")
        );
    }

    #[test]
    fn a_link_belongs_to_its_nearest_landmark() {
        let tree = "- main [ref=e1]:\n  - link \"Direct\" [ref=e2]\n  - navigation [ref=e3]:\n    - link \"Nested\" [ref=e4]\n";
        let out = filter_roles(tree, &set(&["link"]), Some("main"), None, None, "u").unwrap();
        assert!(out.starts_with("- main [ref=e1]:\n"));
        assert!(out.contains("Direct"));
        assert!(!out.contains("Nested"));
    }

    #[test]
    fn sibling_landmarks_pop_off_the_stack() {
        let tree = "- main [ref=e1]:\n  - navigation [ref=e2]:\n    - link \"InNav\" [ref=e3]\n  - region [ref=e4]:\n    - link \"InRegion\" [ref=e5]\n  - link \"Direct\" [ref=e6]\n";
        let out = filter_roles(tree, &set(&["link"]), Some("main"), None, None, "u").unwrap();
        assert!(out.contains("Direct"));
        assert!(!out.contains("InNav"));
        assert!(!out.contains("InRegion"));
    }

    #[test]
    fn without_a_scope_the_output_is_flush_left() {
        let tree = "- banner [ref=e1]:\n  - link \"Chrome\" [ref=e2]\n- main [ref=e3]:\n  - link \"Body\" [ref=e4]\n";
        let out = filter_roles(tree, &set(&["link"]), None, None, None, "u").unwrap();
        assert_eq!(
            out,
            "- link \"Chrome\" [ref=e2]\n- link \"Body\" [ref=e4]\n"
        );
    }

    #[test]
    fn several_roles_and_a_cap() {
        let tree = "- main [ref=e1]:\n  - link \"L\" [ref=e2]\n  - heading \"H\" [level=1] [ref=e3]\n  - button \"B\" [ref=e4]\n";
        let out = filter_roles(
            tree,
            &set(&["link", "button"]),
            Some("main"),
            None,
            None,
            "u",
        )
        .unwrap();
        assert!(out.contains("\"L\"") && out.contains("\"B\"") && !out.contains("\"H\""));

        let mut many = String::from("- main [ref=e1]:\n");
        for i in 0..5 {
            many.push_str(&format!("  - link \"L{i}\" [ref=e{}]\n", i + 2));
        }
        let out = filter_roles(&many, &set(&["link"]), Some("main"), Some(2), None, "u").unwrap();
        assert!(out.contains("L0") && out.contains("L1") && !out.contains("L2"));
    }

    #[test]
    fn max_is_a_cap_not_a_floor_and_lines_stay_byte_for_byte() {
        let tree = "- main [ref=e1]:\n  - link \"Only\" [ref=e2]\n";
        assert_eq!(
            filter_roles(tree, &set(&["link"]), Some("main"), Some(64), None, "u").unwrap(),
            tree
        );
        let weird = "- main [ref=e1]:\n  - link \"Weird  spacing\" [ref=e2] [cursor=pointer]\n";
        assert_eq!(
            filter_roles(weird, &set(&["link"]), Some("main"), None, None, "u").unwrap(),
            weird
        );
    }

    #[test]
    fn no_matches_leaves_the_scope_line_or_nothing() {
        let tree = "- main [ref=e1]:\n  - heading \"T\" [level=1]\n";
        assert_eq!(
            filter_roles(tree, &set(&["link"]), Some("main"), None, None, "u").unwrap(),
            "- main [ref=e1]:\n"
        );
        assert_eq!(
            filter_roles(
                "- heading \"T\" [level=1]\n",
                &set(&["link"]),
                None,
                None,
                None,
                "u"
            )
            .unwrap(),
            ""
        );
        assert_eq!(
            filter_roles("", &set(&["link"]), None, None, None, "u").unwrap(),
            ""
        );
    }

    const SECTIONS: &str = "- main [ref=e1]:\n  - link \"Lead\" [ref=e10]\n  - region [ref=e2]:\n    - heading \"Etymology\" [level=2] [ref=e3]\n    - link \"Cat word\" [ref=e4]\n  - region [ref=e5]:\n    - heading \"See also\" [level=2] [ref=e6]\n    - link \"Domestication\" [ref=e7]\n    - link \"Felidae\" [ref=e8]\n    - navigation [ref=e11]:\n      - link \"Portal\" [ref=e12]\n";

    #[test]
    fn a_section_keeps_only_its_own_links() {
        let out = filter_roles(
            SECTIONS,
            &set(&["link"]),
            Some("main"),
            None,
            Some("See also"),
            "u",
        )
        .unwrap();
        assert!(out.contains("Domestication") && out.contains("Felidae"));
        assert!(!out.contains("Cat word") && !out.contains("Lead") && !out.contains("Portal"));
        let two = filter_roles(
            SECTIONS,
            &set(&["link"]),
            Some("main"),
            Some(1),
            Some("See also"),
            "u",
        )
        .unwrap();
        assert!(two.contains("Domestication") && !two.contains("Felidae"));
    }

    #[test]
    fn a_section_must_name_exactly_one_landmark() {
        let err = filter_roles(
            SECTIONS,
            &set(&["link"]),
            Some("main"),
            None,
            Some("Nope"),
            "u",
        )
        .unwrap_err();
        assert!(err.contains("'Nope'") && err.contains("no landmark"));

        let dup = "- main [ref=e1]:\n  - region [ref=e2]:\n    - heading \"Notes\" [ref=e3]\n    - link \"A\" [ref=e4]\n  - region [ref=e5]:\n    - heading \"Notes\" [ref=e6]\n    - link \"B\" [ref=e7]\n";
        let err = filter_roles(
            dup,
            &set(&["link"]),
            Some("main"),
            Some(1),
            Some("Notes"),
            "u",
        )
        .unwrap_err();
        assert!(err.contains('2') && err.contains("exactly one"));
    }

    #[test]
    fn only_a_first_child_heading_makes_a_section_root() {
        let tree = "- main [ref=e1]:\n  - region [ref=e2]:\n    - link \"First\" [ref=e3]\n    - heading \"See also\" [ref=e4]\n    - link \"Wrong\" [ref=e5]\n  - region [ref=e6]:\n    - heading \"See also\" [ref=e7]\n    - link \"Right\" [ref=e8]\n";
        let out = filter_roles(
            tree,
            &set(&["link"]),
            Some("main"),
            None,
            Some("See also"),
            "u",
        )
        .unwrap();
        assert!(out.contains("Right") && !out.contains("Wrong") && !out.contains("First"));
    }

    #[test]
    fn first_heading_finds_level_one_only() {
        assert_eq!(
            first_heading("- heading \"Cat\" [level=1] [ref=e2]\n").as_deref(),
            Some("Cat")
        );
        assert_eq!(
            first_heading("- heading \"Cat\" [level=2] [ref=e2]\n"),
            None
        );
        assert_eq!(first_heading("- link \"x\" [ref=e1]\n"), None);
        assert_eq!(
            first_heading("- 'heading \"Toolbox: Overview\" [level=1]': [ref=e9]\n").as_deref(),
            Some("Toolbox: Overview")
        );
    }

    #[test]
    fn the_head_has_a_fixed_shape() {
        let roles = vec!["link".to_string()];
        assert_eq!(
            head(
                "https://example.com",
                "Example",
                None,
                Some("main"),
                Some(&roles),
                Some("See also"),
                10
            ),
            "url: https://example.com\ntitle: Example\nscope: main\nroles: link\nsection: See also\nchars: 10\n\n"
        );
        let key = Regex::new("^[a-z][a-z_]*$").unwrap();
        for k in [
            "url", "title", "heading", "scope", "roles", "section", "chars",
        ] {
            assert!(key.is_match(k));
        }
    }

    #[test]
    fn refs_are_read_off_the_text() {
        assert_eq!(
            refs_in("- a [ref=e1]\n  - b [ref=e22]\n"),
            vec!["e1", "e22"]
        );
    }
}
