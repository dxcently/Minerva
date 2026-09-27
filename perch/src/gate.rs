//! The gates every request passes, in the order [`crate::serve`] calls them.
//!
//! Gate 0 (the bind) is checked once at start and refuses to bring the process up
//! at all; the other three are per request. None of them reads the path, so a caller
//! without the token cannot learn the route table from status codes — that is what
//! makes the token gate (hub.md §10.2, gate 1) "before path parsing" in practice.
//!
//! Nothing here compares secrets with `==`: [`token_ok`] is constant time, because a
//! byte-by-byte search for the token is a byte-by-byte search *through this process*.

use std::net::SocketAddr;

use anyhow::Result;

/// Refuses anything but `127.0.0.0/8` and `::1`: the token is a file on this box,
/// not a credential for a network. There is deliberately no flag that widens this —
/// a perch reachable from a LAN means every door behind it is too.
pub fn loopback_only(bind: SocketAddr) -> Result<()> {
    if bind.ip().is_loopback() {
        return Ok(());
    }
    anyhow::bail!("refusing to bind {bind}: the perch serves loopback only (127.0.0.0/8 or ::1)")
}

/// Constant time, so the token cannot be searched for byte by byte. The length is
/// public: it is whatever [`harnox::crypto::random_token`] mints.
pub fn token_ok(presented: &str, want: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    presented.as_bytes().ct_eq(want.as_bytes()).into()
}

/// DNS rebinding, and only that: `Host` is the target's authority, so a hostile name
/// resolving to loopback is refused, but a cross-site form posting to `127.0.0.1` is
/// not. Not a CSRF defence — the token is.
///
/// Every request needs this, statics included: the static bundle is served without a
/// token, so a rebinding name would otherwise read it. Stricter than the door, which
/// skips the check on its event stream, where a cross-site page has no token anyway.
pub fn host_ok(host: Option<&str>, bound: SocketAddr) -> bool {
    host.is_some_and(|host| host_matches(host, bound))
}

/// The bound authority, or `localhost:PORT` — the single name hub.md Q8 admits (a Windows
/// browser reaching WSL2 loopback over forwarding may send it). No other name is allowed:
/// that is the whole point of the check.
///
/// Compared whole, against the two strings a `Host` can legitimately carry, both built from
/// `bound`: `SocketAddr` renders a v6 address bracketed the way a `Host` header does
/// (`[::1]:4477`). Compared in pieces instead, `[127.0.0.1]:4477`, `127.0.0.1]:4477` and an
/// unbracketed `::1:4477` all matched — none of them a name a page can be rebound to, but
/// all of them looser than "equals the bound `ip:port`".
///
/// Case-insensitive, because a `Host` is a case-insensitive name (and v6 hex digits are);
/// this is the one place a spelling is read as another spelling.
pub fn host_matches(host: &str, bound: SocketAddr) -> bool {
    host.eq_ignore_ascii_case(&bound.to_string()) || host.eq_ignore_ascii_case(&format!("localhost:{}", bound.port()))
}

/// `Sec-Fetch-Site`, when the browser sends one, must say the request came from this
/// origin. A cross-site page cannot forge the header (`Sec-*` is a forbidden header
/// name), so anything else is a page that should not be talking to us at all. Absent
/// passes: curl sends none, and a same-origin page always passes.
///
/// The two allowed values are matched exactly. No browser spells them otherwise, and
/// accepting a spelling with different case would only ever widen what is admitted.
pub fn sec_fetch_ok(site: Option<&str>) -> bool {
    match site {
        None => true,
        Some(site) => site == "same-origin" || site == "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_loopback_addresses_are_served() {
        for ok in ["127.0.0.1:4477", "127.5.5.5:1", "[::1]:4477"] {
            assert!(loopback_only(ok.parse().unwrap()).is_ok(), "{ok} is loopback");
        }
        for no in ["0.0.0.0:4477", "[::]:4477", "192.168.1.5:4477", "10.0.0.1:80"] {
            let addr: SocketAddr = no.parse().unwrap();
            let err = loopback_only(addr).unwrap_err().to_string();
            assert!(err.contains(&addr.to_string()), "the refusal names the address: {err}");
        }
    }

    #[test]
    fn the_token_comparison_answers_yes_and_no() {
        assert!(token_ok("abc", "abc"));
        assert!(!token_ok("abc", "abd"));
        assert!(!token_ok("abc", "abcd"), "a prefix of the token is not the token");
        assert!(!token_ok("", "abc"));
        assert!(!token_ok("abc", ""));
    }

    #[test]
    fn the_bound_authority_and_localhost_are_what_a_host_may_carry() {
        let bound = addr("127.0.0.1:4477");
        assert!(host_matches("127.0.0.1:4477", bound));
        assert!(host_matches("localhost:4477", bound), "the one name Q8 admits");
        assert!(host_matches("[::1]:4477", addr("[::1]:4477")), "`SocketAddr` brackets v6 in both strings");
        assert!(host_matches("localhost:4477", addr("[::1]:4477")));
        // A `Host` name is case-insensitive, so the comparison is too.
        assert!(host_matches("LOCALHOST:4477", bound));
        assert!(host_matches("LocalHost:4477", addr("[::1]:4477")));
    }

    #[test]
    fn host_header_mismatch_is_caught_by_the_same_comparison() {
        let bound = addr("127.0.0.1:4477");
        assert!(!host_matches("127.0.0.1:4478", bound), "the right name, the wrong port");
        assert!(!host_matches("127.0.0.1", bound), "no port is not the bound authority");
        assert!(!host_matches("localhost", bound), "…nor is the name without it");
        assert!(!host_matches("evil.example:4477", bound));
        assert!(!host_matches("localhorst:4477", bound), "a name that starts like localhost is another name");
        assert!(!host_ok(None, bound), "no `Host` at all is refused: HTTP/1.1 always has one");
        // The forms the piece-wise comparison used to admit: each is a different string
        // from the two the bound authority can be spelled with.
        assert!(!host_matches("[127.0.0.1]:4477", bound), "a bracketed v4 is not a `Host` the bound address renders");
        assert!(!host_matches("127.0.0.1]:4477", bound), "…nor is a stray `]`");
        assert!(!host_matches("::1:4477", addr("[::1]:4477")), "a v6 address is bracketed in a `Host`");
    }

    #[test]
    fn sec_fetch_site_admits_our_own_origin_and_the_typed_url() {
        assert!(sec_fetch_ok(None), "curl sends none");
        assert!(sec_fetch_ok(Some("same-origin")));
        assert!(sec_fetch_ok(Some("none")), "a typed URL or a bookmark is `none`");
        assert!(!sec_fetch_ok(Some("cross-site")));
        assert!(!sec_fetch_ok(Some("same-site")), "another origin on this host is not ours");
        assert!(!sec_fetch_ok(Some("SAME-ORIGIN")), "the value is exact");
        assert!(!sec_fetch_ok(Some("")));
    }
}
