//! The scoped agent tokens, their files, and the gate that checks them (slice AG fills this; today it is the fail-closed stub).

use anyhow::Result;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use super::{Caller, Scope};
use crate::serve::{status, BoxBody, Ctx};

/// The scoped token for `scope` in constant time, then the chat the socket belongs to ([`super::attrib`]). The stub refuses everything
/// with 401, which is also what a call with no credential gets, so a perch whose agent gate is not built opens nothing.
pub fn authenticate(_ctx: &Ctx, _req: &Request<Incoming>, _scope: Scope) -> Result<Caller, Box<Response<BoxBody>>> {
    Err(Box::new(status(StatusCode::UNAUTHORIZED)))
}

/// Mint a term token and a mail token and write each `0600` to the file its flag names, when the flags are given. Stub: writes nothing.
pub fn start(_ctx: &Ctx) -> Result<()> {
    Ok(())
}

/// Remove the token files and the url file, each only if it still holds what this perch wrote (compare before unlink: see
/// [`super::on_exit`], and `sessions::drop_token_file` for the pattern). Stub: nothing was written, so nothing is removed.
pub fn on_exit(_ctx: &Ctx) {}
