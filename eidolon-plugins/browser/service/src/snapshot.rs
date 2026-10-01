//! Chrome's accessibility tree, printed as snapshot text.
//!
//! Input is the flat node list `Accessibility.getFullAXTree` returns, already
//! reduced to [`AxItem`]s. Output is the grammar `filter.rs` reads:
//!
//! ```text
//! - main [ref=e4]:
//!   - heading "Welcome" [level=1] [ref=e5]
//!   - link "the next link" [ref=e7]:
//!     - /url: https://example.com/next
//!   - paragraph [ref=e6]: some text
//! ```
//!
//! What is left out: nodes Chrome marks ignored (hidden, `aria-hidden`), and
//! the wrappers that only hold other nodes: Chrome's internal roles
//! (`RootWebArea`, `LabelText`, ...) and unnamed `generic`/`none`. Their
//! children move up a level instead.

use serde_json::Value;

/// One node of the accessibility tree.
#[derive(Debug, Clone, Default)]
pub struct AxItem {
    pub role: String,
    pub name: String,
    pub ignored: bool,
    pub children: Vec<usize>,
    pub backend: Option<i64>,
    /// `(property name, value)`, e.g. `("level", 1)`, `("url", "https://…")`.
    pub props: Vec<(String, Value)>,
}

impl AxItem {
    fn prop(&self, name: &str) -> Option<&Value> {
        self.props.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn flag(&self, name: &str) -> Option<String> {
        self.prop(name).map(|v| match v {
            Value::String(s) => s.to_ascii_lowercase(),
            other => other.to_string(),
        })
    }
}

enum Out {
    Element {
        head: String,
        url: Option<String>,
        backend: Option<i64>,
        children: Vec<Out>,
    },
    Text(String),
}

/// Render the tree under `root`. With `include_root`, the root is the first
/// line and everything else sits under it (a `within` snapshot); without, the
/// root's contents are the top level. `depth` limits how many levels below
/// the top are shown. `mint` turns a node's backend id into a ref.
pub fn render(
    items: &[AxItem],
    root: usize,
    include_root: bool,
    depth: Option<usize>,
    mint: &mut dyn FnMut(i64) -> String,
) -> String {
    let outs = if include_root {
        vec![element(items, root)]
    } else {
        let mut outs = Vec::new();
        build(items, root, &mut outs);
        outs
    };
    let mut buf = String::new();
    write(&outs, 0, depth, mint, &mut buf);
    buf
}

/// The nodes a `within` names: not ignored, with exactly that role.
pub fn with_role(items: &[AxItem], role: &str) -> Vec<usize> {
    (0..items.len())
        .filter(|&i| !items[i].ignored && items[i].role == role)
        .collect()
}

fn build(items: &[AxItem], i: usize, out: &mut Vec<Out>) {
    let n = &items[i];
    let splice = |out: &mut Vec<Out>| {
        for &c in &n.children {
            build(items, c, out);
        }
    };
    if n.ignored {
        return splice(out);
    }
    match n.role.as_str() {
        "InlineTextBox" | "ListMarker" | "LineBreak" => {}
        "StaticText" => {
            let text = n.name.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                return;
            }
            match out.last_mut() {
                Some(Out::Text(prev)) => {
                    prev.push(' ');
                    prev.push_str(&text);
                }
                _ => out.push(Out::Text(text)),
            }
        }
        r if r.starts_with(|c: char| c.is_ascii_uppercase()) => splice(out),
        "generic" | "none" | "presentation" if n.name.trim().is_empty() => splice(out),
        _ => out.push(element(items, i)),
    }
}

fn element(items: &[AxItem], i: usize) -> Out {
    let n = &items[i];
    let mut children = Vec::new();
    for &c in &n.children {
        build(items, c, &mut children);
    }
    let name = n.name.trim();
    // A link's own text repeats its name; say it once.
    if !name.is_empty() && children.iter().all(|c| matches!(c, Out::Text(_))) {
        let joined: Vec<&str> = children
            .iter()
            .filter_map(|c| match c {
                Out::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        if joined.join(" ") == name.split_whitespace().collect::<Vec<_>>().join(" ") {
            children.clear();
        }
    }

    let mut head = n.role.clone();
    if !name.is_empty() {
        head.push_str(&format!(" \"{}\"", escape(name)));
    }
    match n.flag("checked").as_deref() {
        Some("true") => head.push_str(" [checked]"),
        Some("mixed") => head.push_str(" [checked=mixed]"),
        _ => {}
    }
    for flag in ["disabled", "expanded"] {
        if n.flag(flag).as_deref() == Some("true") {
            head.push_str(&format!(" [{flag}]"));
        }
    }
    if n.role == "heading"
        && let Some(level) = n.prop("level").and_then(Value::as_i64)
    {
        head.push_str(&format!(" [level={level}]"));
    }
    match n.flag("pressed").as_deref() {
        Some("true") => head.push_str(" [pressed]"),
        Some("mixed") => head.push_str(" [pressed=mixed]"),
        _ => {}
    }
    if n.flag("selected").as_deref() == Some("true") {
        head.push_str(" [selected]");
    }
    let url = if n.role == "link" {
        n.prop("url").and_then(Value::as_str).map(str::to_string)
    } else {
        None
    };
    Out::Element {
        head,
        url,
        backend: n.backend,
        children,
    }
}

fn write(
    outs: &[Out],
    level: usize,
    depth: Option<usize>,
    mint: &mut dyn FnMut(i64) -> String,
    buf: &mut String,
) {
    let indent = "  ".repeat(level);
    for o in outs {
        match o {
            Out::Text(t) => buf.push_str(&format!("{indent}- text: {t}\n")),
            Out::Element {
                head,
                url,
                backend,
                children,
            } => {
                let mut full = head.clone();
                if let Some(b) = backend {
                    full.push_str(&format!(" [ref={}]", mint(*b)));
                }
                if full.contains(':') {
                    full = format!("'{}'", full.replace('\'', "''"));
                }
                let open = depth.is_none_or(|d| level < d);
                let url = url.as_deref().filter(|_| open);
                let kids: &[Out] = if open { children } else { &[] };
                match (kids, url) {
                    ([], None) => buf.push_str(&format!("{indent}- {full}\n")),
                    ([Out::Text(t)], None) => buf.push_str(&format!("{indent}- {full}: {t}\n")),
                    _ => {
                        buf.push_str(&format!("{indent}- {full}:\n"));
                        if let Some(u) = url {
                            buf.push_str(&format!("{indent}  - /url: {u}\n"));
                        }
                        write(kids, level + 1, depth, mint, buf);
                    }
                }
            }
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(role: &str, name: &str, children: &[usize], backend: Option<i64>) -> AxItem {
        AxItem {
            role: role.into(),
            name: name.into(),
            children: children.to_vec(),
            backend,
            ..Default::default()
        }
    }

    /// root > main > [heading, link > text, div(generic) > button, hidden]
    fn page() -> Vec<AxItem> {
        let mut heading = node("heading", "Welcome", &[], Some(3));
        heading.props.push(("level".into(), json!(1)));
        let mut link = node("link", "Next", &[5], Some(4));
        link.props
            .push(("url".into(), json!("https://example.com/next")));
        let mut hidden = node("link", "Hidden", &[], Some(9));
        hidden.ignored = true;
        vec![
            node("RootWebArea", "T", &[1], Some(1)),
            node("main", "", &[2, 3, 6, 8], Some(2)),
            heading,
            link,
            node("StaticText", "unused", &[], None),
            node("StaticText", "Next", &[], None),
            node("generic", "", &[7], Some(6)),
            node("button", "Press me", &[], Some(7)),
            hidden,
        ]
    }

    fn mint() -> impl FnMut(i64) -> String {
        let mut n = 0;
        move |_| {
            n += 1;
            format!("e{n}")
        }
    }

    #[test]
    fn a_page_renders_in_the_snapshot_grammar() {
        let items = page();
        let out = render(&items, 0, false, None, &mut mint());
        assert_eq!(
            out,
            "- main [ref=e1]:\n  - heading \"Welcome\" [level=1] [ref=e2]\n  - link \"Next\" [ref=e3]:\n    - /url: https://example.com/next\n  - button \"Press me\" [ref=e4]\n"
        );
    }

    #[test]
    fn within_puts_the_root_first_and_depth_cuts_below_it() {
        let items = page();
        let out = render(&items, 1, true, Some(0), &mut mint());
        assert_eq!(out, "- main [ref=e1]\n");
        assert_eq!(with_role(&items, "main"), vec![1]);
        assert_eq!(with_role(&items, "link"), vec![3]);
    }

    #[test]
    fn unnamed_text_is_inlined_and_quotes_are_escaped() {
        let items = vec![
            node("RootWebArea", "", &[1], None),
            node("paragraph", "", &[2], Some(1)),
            node("StaticText", "say \"hi\"", &[], None),
        ];
        let out = render(&items, 0, false, None, &mut mint());
        assert_eq!(out, "- paragraph [ref=e1]: say \"hi\"\n");
        let items = vec![
            node("RootWebArea", "", &[1], None),
            node("button", "a \"b\"", &[], Some(1)),
        ];
        assert_eq!(
            render(&items, 0, false, None, &mut mint()),
            "- button \"a \\\"b\\\"\" [ref=e1]\n"
        );
    }

    #[test]
    fn rendered_text_passes_through_the_filter() {
        let items = page();
        let out = render(&items, 1, true, None, &mut mint());
        let roles = ["link".to_string()].into_iter().collect();
        let kept =
            crate::filter::filter_roles(&out, &roles, Some("main"), None, None, "u").unwrap();
        assert!(kept.contains("Next"));
        assert_eq!(
            crate::filter::first_heading(&out).as_deref(),
            Some("Welcome")
        );
    }
}
