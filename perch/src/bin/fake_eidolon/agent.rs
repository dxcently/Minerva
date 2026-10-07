//! `FAKE_AGENT_CALL`: the fake door calling the perch as an agent's tool would. **A stub until slice AG.**
//!
//! AG fills [`after_say`]: with `FAKE_AGENT_CALL=<verb>:<json>` the door process itself POSTs `/hub/term/a/<verb>` (or `/hub/mail/a/...`)
//! to the perch, from a socket this process owns, with the scoped token the test's flags wrote to a file, and records the status and body
//! to `$FAKE_LOG`. That is the case the perch's attribution must accept (the peer socket belongs to a live door's pid). It can also spawn
//! `sh -c curl` so the same call arrives from a *child* of the door, which must be refused. `fake_eidolon.rs` only calls the hook, after
//! the 202 for a `say` has gone out; every knob it reads is an environment variable of its own choosing.

use std::sync::Arc;

use super::Fake;

/// `POST /api/say` was accepted and answered 202. Make the call `FAKE_AGENT_CALL` names, if it is set; spawn rather than block, because
/// this runs on the connection that carried the `say`, and the perch's answer to the agent call may depend on that connection being free.
pub fn after_say(_fake: &Arc<Fake>) {}
