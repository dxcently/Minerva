//! What a subscription has left: the reading a provider script's
//! `quota()` hook makes.
//!
//! A metered endpoint is priced per token and [`crate::def::Cost`] is the
//! whole of what there is to say about it. A *plan* is the other kind of
//! account: a flat fee for an allowance that refills on a clock — z.ai's
//! GLM Coding Plan is so much every five hours and so much a week — and
//! on such an account the dollar figure the session meter shows is what
//! the turns *would* have cost, not a bill (see `builtin/zai.rn`). The
//! number that actually governs the next turn is how much of the window
//! is gone, and that is a question only the account can answer, so it is
//! asked of the provider rather than derived from the log.
//!
//! The reading is a script's to make because the endpoint that answers
//! it is undocumented and provider-shaped: z.ai's is one GET whose reply
//! encodes a window as a `(unit, number)` pair. Rust knows none of that;
//! it knows the shape a reading has once made — a plan's name if the
//! endpoint gives one, and a window per allowance with a label, a share
//! used and a reset time. `used` and `limit` come along when the endpoint
//! counts in something, but the *unit* is deliberately not named: it is
//! the endpoint's own — z.ai's Lite plan counts to 2000 per window in a
//! quantity its documentation does not name — and a harness that called
//! it "tokens" would be guessing.
//!
//! The types are here and not in `def.rs` because a reading is not a
//! definition: it is what the account says *now*, taken live and shown
//! with its age.

use serde::{Deserialize, Serialize};

/// One reading of a subscription's allowances.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    /// The plan as the endpoint names it (`lite`, `pro`), when it does.
    #[serde(default)]
    pub plan: Option<String>,
    /// Every window the plan meters, in the order the endpoint lists
    /// them — shortest first is the convention, and z.ai's reply already
    /// comes that way.
    #[serde(default)]
    pub windows: Vec<QuotaWindow>,
}

/// One allowance window: how much of it is used and when it refills.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuotaWindow {
    /// What the window is called: `5h`, `week`. A script chooses the
    /// word, since only it knows what the endpoint's encoding means.
    pub label: String,
    /// The share used, in percent. Clamped to `0..=100` on the way in:
    /// an endpoint reporting `103%` is reporting a window it has closed,
    /// and a gauge cannot draw more than full.
    #[serde(default)]
    pub percent: f64,
    /// Used and allowed, in the endpoint's own unnamed unit, when it
    /// counts in one.
    #[serde(default)]
    pub used: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    /// When the window refills, as milliseconds since the epoch.
    #[serde(default)]
    pub resets_ms: Option<u64>,
}

impl Quota {
    /// A reading as a script returned it, checked: the percentages are
    /// brought into range and a window with no label is dropped, since
    /// a gauge with nothing to call it is a bar with no meaning.
    pub fn normalized(mut self) -> Quota {
        self.windows.retain(|w| !w.label.trim().is_empty());
        for w in &mut self.windows {
            w.percent = if w.percent.is_finite() {
                w.percent.clamp(0.0, 100.0)
            } else {
                0.0
            };
        }
        self
    }
}

/// The envelope a script's `quota()` returns: the reading, or a sentence
/// about why there is none.
///
/// One shape rather than a Rune `Result`, because a `Result` is not data
/// the host can read back (rune's `Value` serializes options and objects
/// and refuses results), and because a script that cannot use `?` — a
/// hook is not itself a `Result`-returning function — has to say what
/// went wrong somehow. `#{ error: "…" }` is that somehow, and a reading
/// with an `error` field is the error whatever else it carries.
#[derive(Clone, Debug, Deserialize)]
pub struct QuotaReply {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(flatten)]
    pub quota: Quota,
}

impl QuotaReply {
    /// The reading, or the script's sentence as an error.
    pub fn into_result(self) -> anyhow::Result<Quota> {
        match self.error {
            Some(e) => anyhow::bail!("{e}"),
            None => Ok(self.quota.normalized()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_a_reading_or_a_sentence() {
        let good: QuotaReply = serde_json::from_value(serde_json::json!({
            "plan": "lite",
            "windows": [
                { "label": "5h", "percent": 7, "used": 158, "limit": 2000, "resets_ms": 1788693968953u64 },
                { "label": "week", "percent": 1 },
            ]
        }))
        .unwrap();
        let q = good.into_result().unwrap();
        assert_eq!(q.plan.as_deref(), Some("lite"));
        assert_eq!(q.windows.len(), 2);
        assert_eq!(
            q.windows[0].percent, 7.0,
            "an integer percentage reads as a float"
        );
        assert_eq!(q.windows[0].used, Some(158));
        assert_eq!(q.windows[0].resets_ms, Some(1788693968953));
        assert_eq!(
            q.windows[1].used, None,
            "what the endpoint does not count is absent, not zero"
        );

        let bad: QuotaReply =
            serde_json::from_value(serde_json::json!({ "error": "GET x: HTTP 401" })).unwrap();
        assert_eq!(
            bad.into_result().unwrap_err().to_string(),
            "GET x: HTTP 401"
        );
    }

    #[test]
    fn a_reading_is_brought_into_range() {
        let q = Quota {
            plan: None,
            windows: vec![
                QuotaWindow {
                    label: "5h".into(),
                    percent: 103.0,
                    ..Default::default()
                },
                QuotaWindow {
                    label: "".into(),
                    percent: 50.0,
                    ..Default::default()
                },
                QuotaWindow {
                    label: "week".into(),
                    percent: f64::NAN,
                    ..Default::default()
                },
            ],
        }
        .normalized();
        assert_eq!(q.windows.len(), 2, "a window with no name is dropped");
        assert_eq!(q.windows[0].percent, 100.0);
        assert_eq!(q.windows[1].percent, 0.0);
    }
}
