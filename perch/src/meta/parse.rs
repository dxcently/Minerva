//! What eidolon's three read-only commands print, read into what the page is sent. Pure functions over bytes and text, no I/O.
//!
//! The text is eidolon's own and is shown as it came ([`text`] only drops control characters and bounds the length), because it is the
//! answer to "what did my eidolon load" and a perch that re-worded it would be a second opinion. The only things read *out* of it are the
//! ones a row of the page needs:
//!
//! ```text
//!   eidolon tools          a stream of pretty-printed JSON objects, one per tool (name, description, input_schema, approval, ...)
//!   eidolon plugins        `<name> <version>` at column 0 starts a plugin; every other line of it is indented or a sentence
//!   eidolon policy-rows    `yours:   <path> — table-version N (date)`, then `shipped:` and `rows:` lines
//! ```
//!
//! None of these has a field for "which plugin ships this tool": eidolon namespaces a plugin's tool as `<plugin>_<file>` and prints
//! nothing more, so [`tools`] reads the attribution back from the plugin names the listing gave ([`plugin_names`]).

use serde::Serialize;

/// More tools than this in one listing is not a tool list the tab can show; the rest are dropped.
pub const MAX_TOOLS: usize = 2000;
/// A tool's name is `[a-z][a-z0-9_]*` in eidolon; this is the room for a Melete or `mcp__` name, which is longer.
const MAX_NAME: usize = 200;
/// `read_only`, `mutating`, ... Anything longer is not an approval class.
const MAX_APPROVAL: usize = 40;
/// The first line of a description, in characters. The full text is in the tool's own manifest, which the tab does not need.
const MAX_DESC: usize = 200;

/// One row of `GET /hub/meta/tools`. Fields that eidolon did not give are absent, not empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Tool {
    pub name: String,
    /// eidolon's own word for the class: `read_only`, `mutating`, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    /// The plugin that ships it, when its name is `<a plugin the listing named>_<file>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// The first non-empty line of the description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
}

/// `eidolon tools`' stdout, read as a stream of JSON values (eidolon prints one pretty-printed object per tool, with nothing between).
/// A value that is not an object with a plain `name` is skipped; a stream that stops being JSON, or has no tool in it, is an error
/// that says where, because a listing this perch half understood would be a list that looks complete and is not.
pub fn tools(stdout: &[u8], plugins: &[String]) -> Result<Vec<Tool>, String> {
    let mut out = Vec::new();
    for (at, value) in serde_json::Deserializer::from_slice(stdout).into_iter::<serde_json::Value>().enumerate() {
        let value = value.map_err(|e| format!("value {} of the tool listing: {e}", at + 1))?;
        let Some(name) = value.get("name").and_then(serde_json::Value::as_str).filter(|name| plain(name, MAX_NAME)) else { continue };
        if out.len() >= MAX_TOOLS {
            break;
        }
        let approval = value.get("approval").and_then(serde_json::Value::as_str).filter(|word| plain(word, MAX_APPROVAL)).map(str::to_string);
        let desc = value.get("description").and_then(serde_json::Value::as_str).and_then(first_line);
        out.push(Tool { name: name.to_string(), approval, plugin: plugin_of(name, plugins).map(str::to_string), desc });
    }
    if out.is_empty() {
        return Err("the tool listing has no tools in it".to_string());
    }
    Ok(out)
}

/// Non-empty, within `max` bytes, and no control character: what a name or a class word is.
fn plain(word: &str, max: usize) -> bool {
    !word.is_empty() && word.len() <= max && !word.chars().any(char::is_control)
}

/// The first line of `text` that has anything on it, trimmed and cut at [`MAX_DESC`] characters.
fn first_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let line: String = line.chars().filter(|c| !c.is_control()).collect();
    if line.chars().count() <= MAX_DESC {
        return Some(line);
    }
    let mut cut: String = line.chars().take(MAX_DESC).collect();
    cut.push('…');
    Some(cut)
}

/// The plugin a tool belongs to: the longest of `plugins` for which `name` is `<plugin>_<something>`. Longest, because a plugin name may
/// itself hold an underscore (`a` and `a_b` can both exist, and `a_b_run` is `a_b`'s).
pub fn plugin_of<'a>(name: &str, plugins: &'a [String]) -> Option<&'a str> {
    plugins
        .iter()
        .filter(|plugin| name.len() > plugin.len() + 1 && name.starts_with(plugin.as_str()) && name.as_bytes()[plugin.len()] == b'_')
        .max_by_key(|plugin| plugin.len())
        .map(String::as_str)
}

/// The plugin names in `eidolon plugins`' text: the lines at column 0 that are exactly `<name> <version>` with `<name>` the grammar
/// eidolon gives a plugin (`[a-z][a-z0-9_]*`). A broken plugin (`name: why`), a missing one (`name: missing — ...`), `warning: ...` and
/// `no plugins installed at ...` are at column 0 too and are none of that shape.
pub fn plugin_names(listing: &str) -> Vec<String> {
    let valid = |name: &str| {
        name.starts_with(|c: char| c.is_ascii_lowercase()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    listing
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (Some(name), Some(_version), None) if valid(name) => Some(name.to_string()),
                _ => None,
            }
        })
        .collect()
}

/// The policy file `eidolon policy-rows` says it compared, from its `yours:   <path> — table-version ...` line. `None` when there is no
/// such line, or the path is not an absolute one (a copy that says `(none)` names no file), which the page then shows as unknown.
pub fn policy_file(rows: &str) -> Option<String> {
    let line = rows.lines().find_map(|line| line.strip_prefix("yours:"))?.trim();
    let path = line.split(" — ").next()?.trim();
    (path.starts_with('/') && !path.chars().any(char::is_control)).then(|| path.to_string())
}

/// `path` with the home directory written `~`, as the page shows every path. `home` is `$HOME`; one that is not absolute, or is `/`,
/// would turn every path into `~/...` and is ignored.
pub fn tilde(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.map(|home| home.trim_end_matches('/')).filter(|home| home.starts_with('/')) else { return path.to_string() };
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home).and_then(|rest| rest.strip_prefix('/')) {
        Some(rest) => format!("~/{rest}"),
        None => path.to_string(),
    }
}

/// A command's text for the page: lossy UTF-8, control characters other than newline and tab dropped (an escape sequence cannot reach a
/// `<pre>` as anything but its printable remainder), and at most `cap` bytes, cut at a character with a line that says it was cut.
pub fn text(raw: &[u8], cap: usize) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let mut out: String = lossy.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
    if out.len() > cap {
        let mut end = cap;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("\n[cut: the text is longer than the perch sends]\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the real `eidolon tools` (checked against the master build): pretty-printed objects, back to back.
    const STREAM: &str = "{\n  \"name\": \"read\",\n  \"description\": \"Read a file.\\nSecond line.\",\n  \"input_schema\": {\"type\": \"object\"},\n  \"approval\": \"read_only\"\n}\n\
                          {\n  \"name\": \"jev_graph\",\n  \"description\": \"\\n  Run a graph\\n\",\n  \"input_schema\": {},\n  \"approval\": \"read_only\",\n  \"prompt\": \"x\"\n}\n\
                          {\n  \"name\": \"browser_click\",\n  \"description\": \"\",\n  \"input_schema\": {},\n  \"approval\": \"mutating\"\n}\n";

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_stream_of_pretty_objects_is_a_list_with_the_first_description_line() {
        let list = tools(STREAM.as_bytes(), &names(&["browser", "jev"])).unwrap();
        let got: Vec<_> = list.iter().map(|t| (t.name.as_str(), t.approval.as_deref(), t.plugin.as_deref(), t.desc.as_deref())).collect();
        assert_eq!(
            got,
            [
                ("read", Some("read_only"), None, Some("Read a file.")),
                ("jev_graph", Some("read_only"), Some("jev"), Some("Run a graph")),
                ("browser_click", Some("mutating"), Some("browser"), None),
            ]
        );
        let sent = serde_json::to_value(&list[2]).unwrap();
        assert!(sent.get("desc").is_none() && sent.get("plugin").is_some(), "an absent field is absent, not empty: {sent}");
    }

    #[test]
    fn what_is_not_a_tool_is_skipped_and_what_is_not_json_is_an_error() {
        let mixed = "[1,2]\n{\"description\": \"no name\"}\n{\"name\": \"a\\u0007b\"}\n{\"name\": \"ok\", \"approval\": 3}\n";
        let list = tools(mixed.as_bytes(), &[]).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].approval.as_deref()), ("ok", None), "a name with a control character and a non-string class");
        assert!(tools(b"[system] seeded tools/read.rn\n{\"name\":\"a\"}", &[]).unwrap_err().contains("value 1"), "text before the stream is not a tool");
        assert!(tools(b"{\"name\":\"a\"}\n{\"name\":", &[]).is_err(), "a stream cut short is not a complete list");
        assert!(tools(b"", &[]).is_err(), "no tools at all is not an answer");
        assert!(tools(b"{\"x\":1}", &[]).is_err());
    }

    #[test]
    fn a_long_description_line_is_cut_and_the_list_is_bounded() {
        let long = format!("{{\"name\":\"a\",\"description\":\"{}\"}}", "é".repeat(MAX_DESC + 50));
        let list = tools(long.as_bytes(), &[]).unwrap();
        let desc = list[0].desc.as_deref().unwrap();
        assert_eq!(desc.chars().count(), MAX_DESC + 1);
        assert!(desc.ends_with('…'));
        let many: String = (0..MAX_TOOLS + 5).map(|i| format!("{{\"name\":\"t{i}\"}}")).collect();
        assert_eq!(tools(many.as_bytes(), &[]).unwrap().len(), MAX_TOOLS);
    }

    #[test]
    fn a_tool_belongs_to_the_longest_plugin_whose_name_and_an_underscore_begin_it() {
        let plugins = names(&["a", "a_b", "jev"]);
        assert_eq!(plugin_of("a_b_run", &plugins), Some("a_b"));
        assert_eq!(plugin_of("a_run", &plugins), Some("a"));
        assert_eq!(plugin_of("jev_graph", &plugins), Some("jev"));
        assert_eq!(plugin_of("jev", &plugins), None, "the bare plugin name is not one of its tools");
        assert_eq!(plugin_of("jev_", &plugins), None, "and nothing after the underscore is not a tool");
        assert_eq!(plugin_of("jevx_graph", &plugins), None, "a prefix that is not followed by an underscore");
        assert_eq!(plugin_of("read", &plugins), None);
    }

    #[test]
    fn the_plugin_names_are_the_two_word_lines_at_column_zero() {
        let listing = "browser 0.1.0\n    one headless Chromium\n    at       /x/browser\n    claims   tools: open, snapshot\n    browser_back   unvouched — asks\n\
                       jev 0.2.0\n    jev automation graphs\n\
                       odd -\n\
                       old_one: not adopted — a manifest that does not parse\n\
                       gone: missing — the record outlives the directory; reinstall it, or drop the entry from /x\n\
                       warning: plugin record is bad — listing as if the record were absent\n\
                       no plugins installed at /x/plugins\n\
                       Capital 1.0\n\
                       three words here\n";
        assert_eq!(plugin_names(listing), names(&["browser", "jev", "odd"]));
        assert_eq!(plugin_names("no plugins installed at /x/plugins\n"), Vec::<String>::new());
    }

    #[test]
    fn the_policy_file_is_the_path_on_the_yours_line() {
        let rows = "yours:   /home/u/.config/eidolon/policy.rn — table-version 1 (2026-09-28)\nshipped: built into this binary — table-version 1 (2026-09-28)\n\
                    rows:    identical (30 rows)\n";
        assert_eq!(policy_file(rows).as_deref(), Some("/home/u/.config/eidolon/policy.rn"));
        assert_eq!(policy_file("yours:   /a b/p.rn\n").as_deref(), Some("/a b/p.rn"), "no marker: the whole line, spaces and all");
        assert_eq!(policy_file("yours:   (none) — no policy.rn\n"), None);
        assert_eq!(policy_file("shipped: built in\n"), None);
        assert_eq!(policy_file(""), None);
    }

    #[test]
    fn a_path_under_home_is_written_with_a_tilde() {
        assert_eq!(tilde("/home/u/.config/x", Some("/home/u")), "~/.config/x");
        assert_eq!(tilde("/home/u", Some("/home/u/")), "~");
        assert_eq!(tilde("/home/user2/x", Some("/home/u")), "/home/user2/x", "a longer name is not under home");
        assert_eq!(tilde("/etc/x", Some("/home/u")), "/etc/x");
        assert_eq!(tilde("/etc/x", Some("/")), "/etc/x", "home of / would tilde everything");
        assert_eq!(tilde("/etc/x", Some("relative")), "/etc/x");
        assert_eq!(tilde("/etc/x", None), "/etc/x");
    }

    #[test]
    fn text_loses_its_control_characters_and_is_cut_on_a_boundary() {
        assert_eq!(text(b"a\x1b[31mb\r\nc\td\x00e", 100), "a[31mb\nc\tde");
        assert_eq!(text("\u{fffd}".as_bytes(), 100), "\u{fffd}");
        assert_eq!(text(b"a\xffb", 100), "a\u{fffd}b", "bytes that are not UTF-8 are replaced, not refused");
        let cut = text("éééé".as_bytes(), 3);
        assert!(cut.starts_with('é') && !cut.starts_with("éé") && cut.contains("[cut"), "{cut:?}");
        assert!(!text(b"short", 5).contains("[cut"));
    }
}
