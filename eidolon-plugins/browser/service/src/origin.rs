//! Origins, and checking a `confine` list.

use serde_json::Value;

use crate::text::repr;

/// `scheme://host[:port]`, lowercased, with the scheme's default port
/// dropped. A URL with no `//` (`data:`, `about:`) has an empty host.
pub fn origin_of(url: &str) -> Result<String, String> {
    let (scheme, rest) = match url.find(':') {
        Some(i)
            if url[..i]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) =>
        {
            (url[..i].to_ascii_lowercase(), &url[i + 1..])
        }
        _ => (String::new(), url),
    };
    let Some(after) = rest.strip_prefix("//") else {
        return Ok(format!("{scheme}://"));
    };
    let authority = &after[..after.find(['/', '?', '#']).unwrap_or(after.len())];
    let hostport = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let end = v6.find(']').unwrap_or(v6.len());
        let port = v6[end..].strip_prefix("]:");
        (&v6[..end], port)
    } else {
        match hostport.rfind(':') {
            Some(i) => (&hostport[..i], Some(&hostport[i + 1..])),
            None => (hostport, None),
        }
    };
    let mut out = format!("{scheme}://{}", host.to_ascii_lowercase());
    if let Some(p) = port.filter(|p| !p.is_empty()) {
        let n: u32 = p
            .parse()
            .map_err(|_| format!("port could not be read as a number: {p:?}"))?;
        if n > 65535 {
            return Err(format!("port out of range 0-65535: {n}"));
        }
        let default = match scheme.as_str() {
            "http" | "ws" => Some(80),
            "https" | "wss" => Some(443),
            _ => None,
        };
        if default != Some(n) {
            out.push_str(&format!(":{n}"));
        }
    }
    Ok(out)
}

/// A `confine` argument as a list of normalised origins.
pub fn parse_confine(v: &Value) -> Result<Vec<String>, String> {
    let bad = || {
        format!(
            "confine must be a list of origins like https://en.wikipedia.org; got {}",
            repr(v)
        )
    };
    let items = v.as_array().filter(|a| !a.is_empty()).ok_or_else(bad)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let s = item.as_str().filter(|s| !s.is_empty()).ok_or_else(bad)?;
        if !is_bare_origin(&s.to_ascii_lowercase()) {
            return Err(bad());
        }
        let origin = origin_of(s).map_err(|_| bad())?;
        if !out.contains(&origin) {
            out.push(origin);
        }
    }
    Ok(out)
}

/// `http(s)://host[:port]` and nothing else: no path, no userinfo, no
/// trailing slash.
fn is_bare_origin(s: &str) -> bool {
    let Some(rest) = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
    else {
        return false;
    };
    let (host, port) = match rest.rfind(':') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        && port.is_none_or(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn origins_normalise() {
        let cases = [
            ("https://example.com:443/x", "https://example.com"),
            ("http://example.com:80/x", "http://example.com"),
            ("http://127.0.0.1:5173/x", "http://127.0.0.1:5173"),
            (
                "https://EN.Wikipedia.ORG/wiki/Cat",
                "https://en.wikipedia.org",
            ),
            ("https://example.com/a/b?x=1#y", "https://example.com"),
            ("https://user:pw@example.com/", "https://example.com"),
            ("http://[::1]:8080/", "http://::1:8080"),
            ("data:text/html,hi", "data://"),
            ("about:blank", "about://"),
        ];
        for (url, want) in cases {
            assert_eq!(origin_of(url).unwrap(), want, "{url}");
        }
        assert_ne!(
            origin_of("http://x.com").unwrap(),
            origin_of("https://x.com").unwrap()
        );
        assert!(origin_of("http://x:abc/").is_err());
        assert!(origin_of("http://x:70000/").is_err());
    }

    #[test]
    fn confine_takes_bare_origins_only() {
        for bad in [
            json!("main"),
            json!([]),
            json!(["https://example.com", 7]),
            json!(["ftp://x"]),
            json!(["file:///etc/passwd"]),
            json!(["https://x/path"]),
            json!(["https://x/"]),
            json!(["https://u@x"]),
        ] {
            let err = parse_confine(&bad).unwrap_err();
            assert!(
                err.starts_with("confine must be a list of origins"),
                "{bad}"
            );
        }
    }

    #[test]
    fn confine_normalises_default_ports() {
        assert_eq!(
            parse_confine(&json!(["https://Example.com:443", "http://127.0.0.1:5000"])).unwrap(),
            vec!["https://example.com", "http://127.0.0.1:5000"]
        );
    }
}
