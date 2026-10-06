//! The browser's way in: a one-time code that is traded for a session cookie.
//!
//! `POST /hub/code` (behind the token) mints a code; `POST /hub/redeem` (open, no token)
//! trades a valid one for a `Set-Cookie`. The launcher mints the code from the token file and
//! puts it in a URL, so the token itself never reaches a browser, an argv or a history entry.
//!
//! **The cookie is not the token.** Each redeem mints a fresh session secret and keeps it in
//! memory only; the token file stays the one master credential, and a perch restart (or
//! [`Logins::new`]) invalidates every cookie without touching a file.
//!
//! A code lives [`CODE_TTL`] seconds, works once, and at most [`MAX_CODES`] are outstanding
//! (the oldest is dropped for a newer). A redeem consumes the code it matched before it answers.
//! Comparison is constant time over every outstanding code, never `==` and never an early exit.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq as _;

/// How long a minted code can be redeemed.
pub const CODE_TTL: Duration = Duration::from_secs(30);
/// Outstanding codes kept at once.
pub const MAX_CODES: usize = 4;
/// Session secrets kept at once; the oldest is dropped for a newer.
pub const MAX_SESSIONS: usize = 32;

pub struct Logins {
    ttl: Duration,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    codes: Vec<(String, Instant)>,
    sessions: Vec<String>,
}

fn same(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

impl Logins {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, state: Mutex::new(State::default()) }
    }

    /// A fresh single-use code. Expired ones are swept first, then the oldest is dropped if the
    /// cap is still reached.
    pub fn mint_code(&self) -> String {
        let code = harnox::crypto::random_token();
        let mut state = self.state.lock().unwrap();
        let ttl = self.ttl;
        state.codes.retain(|(_, at)| at.elapsed() < ttl);
        while state.codes.len() >= MAX_CODES {
            state.codes.remove(0);
        }
        state.codes.push((code.clone(), Instant::now()));
        code
    }

    /// The session secret for a valid, unexpired, unused `code`, which is consumed either way
    /// it matched. `None` for anything else.
    pub fn redeem(&self, code: &str) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        let ttl = self.ttl;
        state.codes.retain(|(_, at)| at.elapsed() < ttl);
        // Every entry is compared, so the time taken does not say which slot matched.
        let mut hit = None;
        for (i, (held, _)) in state.codes.iter().enumerate() {
            if same(code, held) {
                hit = Some(i);
            }
        }
        state.codes.remove(hit?);
        let secret = harnox::crypto::random_token();
        while state.sessions.len() >= MAX_SESSIONS {
            state.sessions.remove(0);
        }
        state.sessions.push(secret.clone());
        Some(secret)
    }

    /// Whether `presented` is a session secret this perch issued.
    pub fn session_ok(&self, presented: &str) -> bool {
        let state = self.state.lock().unwrap();
        let mut ok = false;
        for held in &state.sessions {
            ok |= same(presented, held);
        }
        ok
    }
}

/// The value of the cookie named `name` in a `Cookie` header, when it is there.
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').filter_map(|pair| pair.trim().split_once('=')).find(|(n, _)| *n == name).map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_works_once() {
        let logins = Logins::new(CODE_TTL);
        let code = logins.mint_code();
        let secret = logins.redeem(&code).expect("first use");
        assert!(logins.session_ok(&secret));
        assert!(logins.redeem(&code).is_none(), "second use");
        assert!(!logins.session_ok("nope"));
    }

    #[test]
    fn an_expired_code_is_refused() {
        let logins = Logins::new(Duration::from_millis(20));
        let code = logins.mint_code();
        std::thread::sleep(Duration::from_millis(40));
        assert!(logins.redeem(&code).is_none());
    }

    #[test]
    fn only_a_few_codes_are_outstanding() {
        let logins = Logins::new(CODE_TTL);
        let first = logins.mint_code();
        let rest: Vec<_> = (0..MAX_CODES).map(|_| logins.mint_code()).collect();
        assert!(logins.redeem(&first).is_none(), "the oldest was dropped");
        assert!(logins.redeem(&rest[MAX_CODES - 1]).is_some());
    }

    #[test]
    fn a_cookie_is_found_by_exact_name() {
        assert_eq!(cookie_value("a=1; minerva_5=xyz; b=2", "minerva_5"), Some("xyz"));
        assert_eq!(cookie_value("xminerva_5=xyz", "minerva_5"), None);
        assert_eq!(cookie_value("minerva_55=xyz", "minerva_5"), None);
        assert_eq!(cookie_value("", "minerva_5"), None);
    }
}
