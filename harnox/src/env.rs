//! Environment-variable lookup with deprecated aliases.

/// Read `primary` from the environment, falling back to each name in `legacy`
/// in order. An empty or whitespace-only value counts as unset. A hit on a
/// legacy name is honoured but logged as deprecated, so a rename can roll out
/// without breaking an existing deployment's env file.
pub fn env_var(primary: &str, legacy: &[&str]) -> Option<String> {
    if let Ok(v) = std::env::var(primary)
        && !v.trim().is_empty()
    {
        return Some(v);
    }
    for name in legacy {
        if let Ok(v) = std::env::var(name)
            && !v.trim().is_empty()
        {
            tracing::warn!("{name} is deprecated; rename it to {primary}");
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // The process environment is global, so every test here uses names no
    // other test touches.

    #[test]
    fn primary_wins_and_blank_counts_as_unset() {
        // SAFETY: single-threaded access to names only this test uses.
        unsafe {
            std::env::set_var("RUST_LLM_TEST_PRIMARY", "  ");
            std::env::set_var("RUST_LLM_TEST_LEGACY", "old");
        }
        assert_eq!(
            env_var("RUST_LLM_TEST_PRIMARY", &["RUST_LLM_TEST_LEGACY"]).as_deref(),
            Some("old")
        );
        unsafe { std::env::set_var("RUST_LLM_TEST_PRIMARY", "new") };
        assert_eq!(
            env_var("RUST_LLM_TEST_PRIMARY", &["RUST_LLM_TEST_LEGACY"]).as_deref(),
            Some("new")
        );
    }

    #[test]
    fn unset_everywhere_is_none() {
        assert_eq!(env_var("RUST_LLM_TEST_NOPE", &["RUST_LLM_TEST_NOPE_EITHER"]), None);
    }
}
