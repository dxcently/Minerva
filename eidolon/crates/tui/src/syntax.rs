//! Colour for the inside of a code block.
//!
//! Not a parser: a line-local scanner that finds the four things nearly
//! every language spells the same way — a comment, a quoted string, a
//! number, and a word from a fixed list — and leaves the rest alone. It
//! is deliberately wrong at the edges (a string that spans lines, a
//! nested comment, a regex that looks like division) and cheap enough
//! to run on every frame of a streaming reply, which is the trade a
//! transcript wants over a correct highlighter that costs a frame.
//!
//! Every piece it finds is named by a [`Role`], never by a colour: the
//! theme decides what a string looks like, so a scheme is a table of
//! roles and not a patch to this file. A language the table does not
//! know gets nothing: a file that is not code is not guessed at, since
//! an apostrophe in prose is not a string and a Markdown heading is not
//! a comment, and a diff of a poem should read as a poem.

use std::ops::Range;

/// What a run of a line is, as far as this scanner can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Comment,
    Str,
    Number,
    Keyword,
}

impl Role {
    /// The theme role that paints it.
    pub fn name(self) -> &'static str {
        match self {
            Role::Comment => "comment",
            Role::Str => "string",
            Role::Number => "number",
            Role::Keyword => "keyword",
        }
    }

    /// Its place in a palette's table, in the order declared above.
    pub fn index(self) -> usize {
        match self {
            Role::Comment => 0,
            Role::Str => 1,
            Role::Number => 2,
            Role::Keyword => 3,
        }
    }
}

/// How one language spells the three things worth finding, and the
/// words it reserves. Adding a language is a row here.
pub struct Lang {
    /// What opens a comment that runs to the end of the line.
    pub comment: &'static [&'static str],
    /// What quotes a string.
    pub quotes: &'static [char],
    /// Words painted as keywords.
    pub words: &'static [&'static str],
}

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "None", "not", "or", "pass", "raise", "return", "self", "True", "False", "try", "while",
    "with", "yield",
];
const JS: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete",
    "else", "export", "extends", "false", "finally", "for", "from", "function", "if", "implements",
    "import", "in", "instanceof", "interface", "let", "new", "null", "return", "switch", "this",
    "throw", "true", "try", "type", "typeof", "undefined", "var", "while", "yield",
];
const GO: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for",
    "func", "go", "goto", "if", "import", "interface", "map", "nil", "package", "range", "return",
    "select", "struct", "switch", "true", "false", "type", "var",
];
const SHELL: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "read", "return", "set", "then", "until", "while",
];
const C: &[&str] = &[
    "break", "case", "char", "class", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "false", "float", "for", "if", "include", "int", "long", "namespace", "new",
    "nullptr", "private", "public", "return", "short", "sizeof", "static", "struct", "switch",
    "template", "true", "typedef", "union", "unsigned", "using", "void", "while",
];
const NIX: &[&str] = &[
    "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with", "true", "false",
    "null", "import",
];
const DATA: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];
const SQL: &[&str] = &[
    "and", "as", "asc", "by", "create", "delete", "desc", "drop", "from", "group", "having",
    "insert", "into", "join", "left", "limit", "not", "null", "on", "or", "order", "select", "set",
    "table", "update", "values", "where",
];

/// The fallback: nothing at all. A file the table does not know is not
/// assumed to be code — an apostrophe in a line of prose is not a
/// string opening, and a `#` at the head of a Markdown heading is not a
/// comment. Both were painted as such, and a poem came out green.
/// Guessing is only worth it where the guess is certain.
const TEXT: Lang = Lang {
    comment: &[],
    quotes: &[],
    words: &[],
};

/// What the fence's language, or a file's extension, means here. Prose
/// and anything unknown get [`TEXT`] and are left alone.
pub fn lang(name: &str) -> Lang {
    let n = name.trim().to_ascii_lowercase();
    let (comment, quotes, words): (&[&str], &[char], &[&str]) = match n.as_str() {
        "rust" | "rs" => (&["//"], &['"'], RUST),
        "python" | "py" => (&["#"], &['"', '\''], PYTHON),
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => {
            (&["//"], &['"', '\'', '`'], JS)
        }
        "go" => (&["//"], &['"', '`'], GO),
        "sh" | "bash" | "zsh" | "shell" | "console" => (&["#"], &['"', '\''], SHELL),
        "c" | "cpp" | "c++" | "h" | "hpp" | "java" | "cs" => (&["//"], &['"', '\''], C),
        "nix" => (&["#"], &['"'], NIX),
        "toml" | "yaml" | "yml" | "ini" | "conf" => (&["#"], &['"', '\''], DATA),
        "json" => (&[], &['"'], DATA),
        "sql" => (&["--"], &['"', '\''], SQL),
        // Named so the intent is on the page; they would fall through
        // to the same place anyway.
        "txt" | "text" | "md" | "markdown" | "rst" | "log" | "" => return TEXT,
        _ => return TEXT,
    };
    Lang {
        comment,
        quotes,
        words,
    }
}

/// The coloured runs of one line, in order and never overlapping. Byte
/// ranges into `line`; everything not covered is ordinary code.
///
/// Walked a character at a time, not a byte at a time: a line of prose
/// in a fence carries em-dashes and accents, and an index that lands
/// inside one is not a place a string can be cut.
pub fn scan(line: &str, lang: &Lang) -> Vec<(Range<usize>, Role)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        let Some(c) = rest.chars().next() else { break };
        let step = c.len_utf8();
        // A comment takes the rest of the line and ends the scan.
        if lang.comment.iter().any(|m| rest.starts_with(m)) {
            out.push((i..line.len(), Role::Comment));
            break;
        }
        if lang.quotes.contains(&c) {
            // An unterminated string ends where the line does.
            let mut end = line.len();
            let mut escaped = false;
            for (off, d) in rest[step..].char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if d == '\\' {
                    escaped = true;
                } else if d == c {
                    end = i + step + off + d.len_utf8();
                    break;
                }
            }
            out.push((i..end, Role::Str));
            i = end;
            continue;
        }
        // A number, but not the tail of an identifier like `utf8`.
        if c.is_ascii_digit() && !prev_is_word(line, i) {
            let n = rest
                .find(|d: char| !(d.is_ascii_alphanumeric() || d == '.' || d == '_'))
                .unwrap_or(rest.len());
            out.push((i..i + n, Role::Number));
            i += n;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let n = rest
                .find(|d: char| !(d.is_alphanumeric() || d == '_'))
                .unwrap_or(rest.len());
            if lang.words.contains(&&line[i..i + n]) {
                out.push((i..i + n, Role::Keyword));
            }
            i += n;
            continue;
        }
        i += step;
    }
    out
}

fn prev_is_word(line: &str, i: usize) -> bool {
    line[..i]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(line: &str, name: &str) -> Vec<(String, Role)> {
        scan(line, &lang(name))
            .into_iter()
            .map(|(r, k)| (line[r].to_string(), k))
            .collect()
    }

    #[test]
    fn a_line_of_rust_finds_its_words_and_its_string() {
        assert_eq!(
            roles(r#"let name = "eidolon"; // a note"#, "rust"),
            [
                ("let".into(), Role::Keyword),
                (r#""eidolon""#.into(), Role::Str),
                ("// a note".into(), Role::Comment),
            ]
        );
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        assert_eq!(
            roles(r#"url = "https://x/#frag""#, "toml"),
            [(r#""https://x/#frag""#.into(), Role::Str)]
        );
    }

    #[test]
    fn a_number_is_a_number_and_a_suffix_is_not() {
        assert_eq!(roles("x = 42", "rust"), [("42".into(), Role::Number)]);
        assert!(roles("let utf8 = 1", "rust").iter().all(|(t, _)| t != "8"));
    }

    /// A diff of a poem should read as a poem: nothing in a file the
    /// table does not know is painted as though it were code.
    #[test]
    fn prose_and_unknown_languages_are_left_alone() {
        for name in ["txt", "md", "", "brainfuck"] {
            assert!(roles(r#"thing "quoted" other"#, name).is_empty(), "{name}");
            assert!(roles("it's a quiet hour and the lamp keeps", name).is_empty(), "{name}");
            assert!(roles("# a heading, not a comment", name).is_empty(), "{name}");
        }
    }

    /// The crash this guards: a byte index that lands inside a
    /// multi-byte character is not a place to cut a line.
    #[test]
    fn a_line_with_characters_wider_than_a_byte_does_not_panic() {
        for lang in ["rust", "python", "toml", "sh", "brainfuck"] {
            for line in [
                "let x = 1; // a note — an aside",
                "# — a dash opening a comment",
                "let s = \"héllo — wörld\"; // ünicode",
                "—",
                "\"—",
                "ünicode_identifier = 42",
                "let 界 = \"界\";",
            ] {
                let runs = roles(line, lang);
                for (text, _) in &runs {
                    assert!(line.contains(text.as_str()), "{lang}: {line:?} produced {text:?}");
                }
            }
        }
    }

    #[test]
    fn an_unterminated_string_ends_at_the_line_and_does_not_run_away() {
        assert_eq!(
            roles(r#"s = "open"#, "rust"),
            [(r#""open"#.into(), Role::Str)]
        );
    }
}
