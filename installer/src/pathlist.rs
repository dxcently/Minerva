//! The user PATH as a `;`-separated string: add or remove one directory, nothing else touched.

fn same(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim().trim_end_matches(['\\', '/']))
}

/// `Some(new)` when `dir` was not in `existing` (appended); `None` when it already is.
pub fn add(existing: &str, dir: &str) -> Option<String> {
    if existing.split(';').any(|p| same(p, dir)) {
        return None;
    }
    let mut parts: Vec<&str> = existing.split(';').filter(|p| !p.trim().is_empty()).collect();
    parts.push(dir);
    Some(parts.join(";"))
}

/// `Some(new)` when `dir` was in `existing` (removed); `None` when it was not.
pub fn remove(existing: &str, dir: &str) -> Option<String> {
    if !existing.split(';').any(|p| same(p, dir)) {
        return None;
    }
    Some(existing.split(';').filter(|p| !p.trim().is_empty() && !same(p, dir)).collect::<Vec<_>>().join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_idempotent() {
        let p = r"C:\a;C:\b\";
        assert_eq!(add(p, r"C:\Minerva\bin").unwrap(), r"C:\a;C:\b\;C:\Minerva\bin");
        assert_eq!(add(r"C:\a;c:\minerva\BIN\", r"C:\Minerva\bin"), None);
        assert_eq!(remove(r"C:\a;C:\Minerva\bin;C:\b", r"C:\minerva\bin\").unwrap(), r"C:\a;C:\b");
        assert_eq!(remove(p, r"C:\Minerva\bin"), None);
        assert_eq!(add("", r"C:\x").unwrap(), r"C:\x");
    }
}
