//! Parsing the scorer's output into points.
//!
//! Aeacus shows earned points and a total. How the report is opened is not
//! stated on the practice-image page (triage-toolkit.md §7.1), so `parse_score`
//! reads defensively: it accepts the shapes a scorer command is likely to print
//! and takes the first earned/total pair it can trust.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Score {
    pub earned: i64,
    pub total: i64,
    /// Penalties the scorer reported, when the reading carried them (the
    /// `botforge-vm.sh score` line prints `penalties N`). `None` when the
    /// output had no penalties token, so the row can tell "zero" from "unknown".
    pub penalties: Option<i64>,
}

impl Score {
    pub fn solved(&self) -> bool {
        self.total > 0 && self.earned >= self.total
    }
}

/// Pull an `(earned, total)` pair out of a scorer's console output.
///
/// Recognised, in order of preference:
///   "12 / 256", "12/256", "12 of 256"      -> (12, 256)
///   "Score: 12"  / "points = 12"           -> (12, fallback_total)
///   "Points 12"                            -> (12, fallback_total)
/// Returns `None` when no earned figure is found, so the caller can retry
/// rather than record a fabricated zero.
pub fn parse_score(text: &str, fallback_total: i64) -> Option<Score> {
    let penalties = parse_penalties(text);
    if let Some((earned, total)) = parse_pair(text) {
        return Some(Score { earned, total, penalties });
    }
    parse_single(text).map(|earned| Score { earned, total: fallback_total, penalties })
}

/// The penalty count on a `... penalties N ...` reading, or `None` when the
/// output names no penalties (a bare pair, or an unreachable guest's silence).
pub fn parse_penalties(text: &str) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    let pos = lower.find("penalties")?;
    let after = &lower.as_bytes()[pos + "penalties".len()..];
    let mut j = 0;
    while j < after.len() && (after[j] == b' ' || after[j] == b':' || after[j] == b'=') {
        j += 1;
    }
    take_int(after, j).map(|(v, _)| v)
}

fn parse_pair(text: &str) -> Option<(i64, i64)> {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let (earned, next) = match take_int(bytes, i) {
            Some(v) => v,
            None => {
                i += 1;
                continue;
            }
        };
        let sep = skip_separator(bytes, next);
        if let Some((total, _)) = sep.and_then(|j| take_int(bytes, j)) {
            if total > 0 && earned <= total {
                return Some((earned, total));
            }
        }
        i = next;
    }
    None
}

/// Between the two numbers only "/", "of", and spaces may appear; anything else
/// means these are two unrelated numbers, not a score pair.
fn skip_separator(bytes: &[u8], mut i: usize) -> Option<usize> {
    let start = i;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'/' {
        i += 1;
    } else if bytes[i..].starts_with(b"of") {
        i += 2;
    } else {
        return None;
    }
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    if i == start {
        None
    } else {
        Some(i)
    }
}

fn parse_single(text: &str) -> Option<i64> {
    let lower = text.to_ascii_lowercase();
    for key in ["score", "points"] {
        if let Some(pos) = lower.find(key) {
            let after = &lower.as_bytes()[pos + key.len()..];
            let mut j = 0;
            while j < after.len() && (after[j] == b' ' || after[j] == b':' || after[j] == b'=') {
                j += 1;
            }
            if let Some((v, _)) = take_int(after, j) {
                return Some(v);
            }
        }
    }
    None
}

fn take_int(bytes: &[u8], mut i: usize) -> Option<(i64, usize)> {
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    let mut v: i64 = 0;
    for &b in &bytes[start..i] {
        v = v.checked_mul(10)?.checked_add((b - b'0') as i64)?;
    }
    Some((v, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pair reading with no penalties token (the common non-botforge shapes).
    fn pair(earned: i64, total: i64) -> Option<Score> {
        Some(Score { earned, total, penalties: None })
    }

    #[test]
    fn pair_slash() {
        assert_eq!(parse_score("Score 12 / 256 points", 256), pair(12, 256));
    }

    #[test]
    fn pair_no_spaces() {
        assert_eq!(parse_score("12/256", 256), pair(12, 256));
    }

    #[test]
    fn pair_of() {
        assert_eq!(parse_score("you have 40 of 256", 256), pair(40, 256));
    }

    #[test]
    fn single_with_fallback() {
        assert_eq!(parse_score("Score: 12", 256), pair(12, 256));
        assert_eq!(parse_score("points = 7", 256), pair(7, 256));
    }

    #[test]
    fn baseline_zero_is_real() {
        assert_eq!(parse_score("0 / 256", 256), pair(0, 256));
    }

    #[test]
    fn nothing_is_none_not_zero() {
        assert_eq!(parse_score("scoring engine ready", 256), None);
    }

    #[test]
    fn earned_over_total_is_not_a_pair() {
        // "report 5-7" style noise must not read as 5/7; falls through to single.
        assert_eq!(parse_score("300 / 256", 256), None);
    }

    #[test]
    fn botforge_vm_score_line() {
        // The exact line `botforge-vm.sh score` prints, read live off the
        // practice image on 2026-09-26. The penalties token rides along.
        let line = "score 0/256 penalties 0 generated 2026-09-26T08:13:03\n";
        assert_eq!(parse_score(line, 256), Some(Score { earned: 0, total: 256, penalties: Some(0) }));
        // Why the verb normalises instead of passing the report through: the
        // report's own words are not a pair, and its date would read as 9 of 26.
        assert_eq!(parse_score("0 out of 256 points received", 256), None);
        assert_eq!(parse_score("Generated At: 2026/09/26 08:11:19", 256), pair(9, 26));
        // An unreachable guest prints nothing on stdout: no reading, so the loop retries.
        assert_eq!(parse_score("", 256), None);
    }

    #[test]
    fn penalties_token_is_read_and_its_absence_is_unknown() {
        assert_eq!(parse_penalties("score 40/256 penalties 3 generated x"), Some(3));
        assert_eq!(parse_penalties("40 / 256"), None);
        assert_eq!(parse_penalties(""), None);
    }

    #[test]
    fn solved_predicate() {
        let s = |earned, total| Score { earned, total, penalties: None };
        assert!(s(256, 256).solved());
        assert!(!s(255, 256).solved());
        assert!(!s(0, 0).solved());
    }
}
