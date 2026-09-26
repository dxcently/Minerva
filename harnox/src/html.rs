//! One HTML escape, one place to audit.

/// Minimal HTML escape for values interpolated into a served page — the five
/// XML entities, so an attacker-influenced value can't break out of either a
/// text node or a (single- or double-quoted) attribute.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_all_five_entities_and_nothing_else() {
        assert_eq!(html_escape(r#"a<b>&"c'd"#), "a&lt;b&gt;&amp;&quot;c&#39;d");
        assert_eq!(html_escape("plain text"), "plain text");
    }
}
