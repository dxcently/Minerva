//! Small text helpers shared by the error messages.

use serde_json::Value;

/// A value quoted the way the service's messages quote what a caller sent:
/// strings in single quotes, lists in brackets, `None` for null.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quote(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", quote(k), repr(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// One string in single quotes.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// A list of strings, quoted: `['https://a.com', 'https://b.com']`.
pub fn quote_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// The first `max` characters (code points, not bytes).
pub fn truncate_chars(s: &str, max: usize) -> (&str, bool) {
    match s.char_indices().nth(max) {
        Some((i, _)) => (&s[..i], true),
        None => (s, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_quotes_like_the_messages_expect() {
        assert_eq!(repr(&json!("e7")), "'e7'");
        assert_eq!(repr(&json!(["https://a.com", 7])), "['https://a.com', 7]");
        assert_eq!(repr(&json!(null)), "None");
        assert_eq!(repr(&json!(true)), "True");
        assert_eq!(quote("it's"), "'it\\'s'");
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("héllo", 2), ("hé", true));
        assert_eq!(truncate_chars("hi", 5), ("hi", false));
        assert_eq!(truncate_chars("hi", 2), ("hi", false));
    }
}
