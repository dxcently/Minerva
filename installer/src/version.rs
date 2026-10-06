//! Version order. A version is `MAJOR.MINOR.PATCH` with an optional `-g<sha>` build suffix; only the numbers decide
//! whether something is newer, so a rebuild of the same version is never offered as an update.

use anyhow::{Result, anyhow, bail};

/// `0.1.0-gabc1234` -> (0, 1, 0).
pub fn parse_base(v: &str) -> Result<(u64, u64, u64)> {
    let core = v.trim().split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        bail!("not a MAJOR.MINOR.PATCH version: {v:?}");
    }
    let n = |s: &str| s.parse::<u64>().map_err(|_| anyhow!("not a MAJOR.MINOR.PATCH version: {v:?}"));
    Ok((n(parts[0])?, n(parts[1])?, n(parts[2])?))
}

/// True when `candidate` is strictly newer than `current` by its numbers.
pub fn is_newer(candidate: &str, current: &str) -> Result<bool> {
    Ok(parse_base(candidate)? > parse_base(current)?)
}

/// A version string safe to use as a directory name: `[0-9A-Za-z._-]`, 1..=64 characters, no leading dot.
pub fn check_dir_name(v: &str) -> Result<()> {
    let ok = !v.is_empty()
        && v.len() <= 64
        && !v.starts_with('.')
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        bail!("not a usable version string: {v:?}");
    }
    Ok(())
}

/// `YYYY-MM-DD` from Unix seconds (civil-from-days).
pub fn date_of(secs: u64) -> String {
    let z = (secs / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_ignores_the_sha() {
        assert!(is_newer("0.2.0", "0.1.9-gabc").unwrap());
        assert!(is_newer("0.10.0", "0.9.0").unwrap());
        assert!(!is_newer("0.1.0-gaaa", "0.1.0-gbbb").unwrap());
        assert!(!is_newer("0.1.0", "0.2.0").unwrap());
    }

    #[test]
    fn rejects_junk() {
        assert!(parse_base("1.2").is_err());
        assert!(parse_base("a.b.c").is_err());
        assert!(check_dir_name("../x").is_err());
        assert!(check_dir_name(".hidden").is_err());
        assert!(check_dir_name("0.1.0-gabc1234").is_ok());
    }
}
