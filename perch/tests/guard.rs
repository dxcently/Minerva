//! The guard rows (slice G1): the deny rows in the generated policies, `perch::guard::status()` and `refuses_term_text`.
//!
//! A stub until G1. Nothing is guarded and the status says so; G1 replaces this pin with the Rune-compiled tables run through the real
//! `eidolon policy`, the fallback cases and the term-text word check.

#[test]
fn nothing_is_guarded_and_the_status_says_so_until_g1() {
    let status = perch::guard::status();
    assert!(!status.on, "{status:?}");
    assert_eq!(status.file, None);
    assert_eq!(status.why, "not built yet");
    assert_eq!(perch::guard::refuses_term_text("aoide mail send x"), None);
}
