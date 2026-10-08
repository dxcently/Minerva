//! Sending a note as mail (slice M2): `GET /hub/mail/nodes` and the body of `POST /hub/notes/<n>/send`.
//!
//! ```text
//!   GET  /hub/mail/nodes                    [{name, verified, self, meshes}], "this machine" (`self`) first, then the verified and the
//!                                           unverified nodes of Aoide's registry (the page greys the latter as "not paired")
//!   POST /hub/notes/<n>/send {node, mesh?, hold?}
//!        202 {msgid, disposition, delivery, status, to, mesh?, warning?}
//! ```
//!
//! **The send** is `aoide mail send --json --to=<node>/minerva --from=minerva [--mesh=<m>] --subject=<title> [--hold] -- <text>`
//! ([`crate::aoide::Shape::MailSend`]): the note's whole text, its title (the first non-empty line, cleaned to one printable line) as the
//! subject, and a sender that is a fixed value (`minerva`) and not a field of the request, so nothing a page sends can name another one.
//! The perch always sends a subject, so aoide answers in its `--subject` shape: one recipient, whose `msgid`, `status`, `delivery` and
//! `error` are the letter's, and the whole send's `complete` and `accepted`.
//!
//! **Every refusal, in the order it is made.** The first five cost nothing: no process is started, and the fake `aoide`'s log is empty.
//!
//! ```text
//!   400  the body is not {node, mesh?, hold?} with those types (an unknown field, `from` included, is refused)
//!   404  the note is not there (`notes::load`: the name grammar, a link or a FIFO under the name, a missing folder), or no --home
//!   413  the note's text is over 64 KiB (it sits in a command line while aoide runs, readable in /proc)
//!   400  the runner's own checks, run before anything: a node or mesh name outside ^[a-z0-9][a-z0-9-]*$; a mesh with `self`; hold
//!        with `self` ("hold needs another node"); a subject that is empty, longer than 120 characters or not one line; no text
//!   503  another perch keeps this home's mail folder (`mail/.lock`), or aoide is not there or does not list what the perch sends
//!   429  two sends are running already (see [`mail::SENDS_AT_ONCE`]): wait for one, then send again
//!   409  the node is not in Aoide's registry, or is registered but not paired ("mail needs a VERIFIED node"), or `mesh` is not one
//!        of the meshes `mesh --json` puts that node in (when it puts it in any: an empty list is "unknown", not "unreachable").
//!        This is the one refusal that reads aoide before the letter: a read-only `mesh --json`, which runs no send
//!   ---- the process runs (`mail send`, with the send's own timeout, [`aoide::SEND_TIMEOUT`], longer than aoide's own) ----
//!   409  aoide refused for a reason it names (unknown-node, unpaired-node, ambiguous-mesh): nothing was spooled, so no row. A
//!        refusal with `data.meshes` is `{error, why, meshes}`, so the page can ask which and send again with `mesh`
//!   502  any other answer that may come after the letter was spooled: a refusal without a reason, an envelope-less failure, a
//!        signal, a timeout, too much output, output that is not JSON, or a reply with no recipient the perch can read. Each writes a
//!        row with status `unknown` (msgid absent when none was read), and says "may or may not have been sent: check `aoide mail
//!        outbox` for <node>/minerva before sending it again". Never a 504 that invites a resend
//!   502  aoide could not be started (nothing ran, no row)
//! ```
//!
//! **The row.** A letter that aoide took is one line of `<home>/mail/sent.jsonl` (`0600`): `{ts, origin, from, note, subject, sha256,
//! to, mesh?, msgid, disposition, status}`. The text is not kept, only the sha256 of what was sent; the subject and the note's title are
//! kept, and a title is one line of at most 120 characters. `disposition` is the recipient's `status` word as aoide printed it (`filed`,
//! `spooled`, `held`); `status` is the recipient's `delivery.status` when there is one, else what the word means. `GET /hub/mail` works
//! out the current one. A row that cannot be written does not undo the send: the answer is still the 202, with a `warning` sentence.
//!
//! **For M3.** [`send`] is the whole of it for the agent too: [`Letter`] names the sender ([`Sender::MinervaAgent`]) and the origin
//! (`agent:<chat>`), and the checks, the node rules, the argv and the row are the same.

use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::aoide::{self, Aoide, Error, Reply, Sender, Shape, To};
use crate::serve::{body_of, json, problem, status, BoxBody, Ctx};
use crate::{mail, mesh, notes};

/// The body of a send is a node, a mesh and a flag.
const ASK_CAP: usize = 4 * 1024;
/// A sentence aoide printed, cut when it is passed on.
const WHY_MAX: usize = 300;
/// At most this many meshes are passed on from a refusal.
const MESHES_MAX: usize = 16;

// ---------------------------------------------------------------------------
// GET /hub/mail/nodes
// ---------------------------------------------------------------------------

/// One row of the node menu. `self` is this machine, and is not a name in the registry.
#[derive(Debug, PartialEq, Serialize)]
pub struct Target {
    pub name: String,
    pub verified: bool,
    #[serde(rename = "self")]
    pub this: bool,
    pub meshes: Vec<String>,
}

/// `GET /hub/mail/nodes`. `rest` is always `""`. Any other method is the 404 of a route that is not there, as the mesh's are.
pub async fn nodes(ctx: &Ctx, req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    let (method, query) = (req.method().clone(), req.uri().query().unwrap_or("").to_string());
    drop(req);
    if mail::mail_dir(ctx).is_none() || method != Method::GET {
        return status(StatusCode::NOT_FOUND);
    }
    if !query.is_empty() {
        return problem(StatusCode::BAD_REQUEST, "expected no query");
    }
    json(StatusCode::OK, &menu(ctx).await)
}

/// This machine, then the registry's nodes. A registry entry named `self`, or outside the name grammar, is left out: a name here ends up
/// in an argument, and `self` always means this machine. No registry is a menu with this machine in it and nothing else; no aoide still
/// lists the registry (it is a file) with no meshes, and a send to any of them is the 503 that says there is no aoide.
async fn menu(ctx: &Ctx) -> Vec<Target> {
    let mut list = vec![Target { name: "self".to_string(), verified: true, this: true, meshes: Vec::new() }];
    for node in mesh::nodes(ctx).await {
        if node.name != "self" && aoide::name_ok(&node.name) {
            list.push(Target { name: node.name, verified: node.verified, this: false, meshes: node.meshes });
        }
    }
    list
}

// ---------------------------------------------------------------------------
// POST /hub/notes/<n>/send
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ask {
    node: String,
    #[serde(default)]
    mesh: Option<String>,
    #[serde(default)]
    hold: Option<bool>,
}

const ASK_SHAPE: &str = "expected {\"node\": \"self\" or a node's name, \"mesh\"?: a mesh's name, \"hold\"?: true or false}";

/// The body: a JSON object with exactly those fields. An unknown one is refused, so there is no field to name a sender with.
async fn asked(req: Request<Incoming>) -> Result<Ask, Box<Response<BoxBody>>> {
    let (_, bytes) = body_of(req, ASK_CAP).await?;
    let refused = || Box::new(problem(StatusCode::BAD_REQUEST, ASK_SHAPE));
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if !value.is_object() {
        return Err(refused());
    }
    serde_json::from_value(value).map_err(|_| refused())
}

/// `POST /hub/notes/<n>/send {node, mesh?, hold?}`, called by `notes` through `mail::send_note`. `rest` is the note's name, which
/// `notes` has already parsed. The note is read again here: what is sent is the file as it is now.
pub async fn send_note(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    if mail::mail_dir(ctx).is_none() {
        return status(StatusCode::NOT_FOUND);
    }
    let ask = match asked(req).await {
        Ok(ask) => ask,
        Err(refusal) => return *refusal,
    };
    let note = match notes::load(ctx, rest).await {
        Ok(note) => note,
        Err(refusal) => return *refusal,
    };
    let letter = Letter {
        from: Sender::Minerva,
        origin: "operator".to_string(),
        note: Some(note.name),
        node: ask.node,
        mesh: ask.mesh,
        hold: ask.hold.unwrap_or(false),
        // The title is a note's, and a note can hold a line separator or a bidi control that `title_of` keeps: the subject is one printable line.
        subject: mail::line_safe(&note.title, mail::SUBJECT_MAX),
        text: note.text,
    };
    match send(ctx, letter).await {
        Ok(sent) => json(StatusCode::ACCEPTED, &sent.answer()),
        Err(refusal) => refusal.response(),
    }
}

// ---------------------------------------------------------------------------
// The letter, the refusals, the send
// ---------------------------------------------------------------------------

/// What is to be sent, from whom and why. Nothing in it is taken from a request body but `node`, `mesh` and `hold`.
pub(crate) struct Letter {
    /// The sender aoide is told (`--from`), a fixed choice.
    pub from: Sender,
    /// What the row calls the cause: `operator`, or `agent:<chat>` (M3). Never sent anywhere.
    pub origin: String,
    /// The note it was made from, when it was.
    pub note: Option<String>,
    /// `self` or a node's name.
    pub node: String,
    pub mesh: Option<String>,
    pub hold: bool,
    pub subject: String,
    pub text: String,
}

/// The letter's text and subject are the operator's words and an agent's: a `{:?}` in a log line is not a place for them, so it prints
/// the sizes and the names (the same rule as `aoide::Shape`).
impl std::fmt::Debug for Letter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Letter")
            .field("from", &self.from.name())
            .field("origin", &self.origin)
            .field("note", &self.note)
            .field("node", &self.node)
            .field("mesh", &self.mesh)
            .field("hold", &self.hold)
            .field("subject_chars", &self.subject.chars().count())
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

/// Why a send did not happen (or, for a 502 that says "may or may not have been sent", it may have). A sentence for a person, and the
/// status it travels with.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub code: StatusCode,
    pub why: String,
    /// The meshes a node is trusted in, when aoide refused to choose one.
    pub meshes: Option<Vec<String>>,
}

impl Refusal {
    fn new(code: StatusCode, why: impl Into<String>) -> Self {
        Self { code, why: why.into(), meshes: None }
    }

    fn bad(why: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, why)
    }

    fn conflict(why: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, why)
    }

    /// `{error}`, and for a node in several meshes `{error, why, meshes}` (the same sentence twice, so a handler of `error` and a handler
    /// of `why` each have it).
    pub(crate) fn response(self) -> Response<BoxBody> {
        match self.meshes {
            Some(meshes) => json(self.code, &json!({ "error": self.why, "why": self.why, "meshes": meshes })),
            None => problem(self.code, &self.why),
        }
    }
}

/// What a send made.
#[derive(Debug)]
pub(crate) struct Sent {
    pub msgid: Option<String>,
    pub disposition: String,
    pub delivery: Option<Value>,
    pub status: String,
    pub to: String,
    pub mesh: Option<String>,
    /// The recipient's `error` as aoide printed it, cleaned to one line, when it printed one.
    pub error: Option<String>,
    /// The row could not be written, or the send was not complete; the letter went anyway.
    pub warning: Option<String>,
}

impl Sent {
    pub(crate) fn answer(&self) -> Value {
        let mut answer = json!({
            "msgid": self.msgid,
            "disposition": self.disposition,
            "delivery": self.delivery,
            "status": self.status,
            "to": self.to,
        });
        if let Some(mesh) = &self.mesh {
            answer["mesh"] = json!(mesh);
        }
        if let Some(error) = &self.error {
            answer["error"] = json!(error);
        }
        if let Some(warning) = &self.warning {
            answer["warning"] = json!(warning);
        }
        answer
    }
}

fn unknown_node(name: &str) -> String {
    format!("no node named {name} is registered with Aoide on this machine")
}

fn unpaired_node(name: &str) -> String {
    format!("node {name} is registered but not paired: mail needs a paired node, so pair it first (aoide pair), then send again")
}

fn to_of(node: &str) -> Result<To, Refusal> {
    if node == "self" {
        Ok(To::SelfNode)
    } else if aoide::name_ok(node) {
        Ok(To::Node(node.to_string()))
    } else {
        Err(Refusal::bad("a node is \"self\" or a name of lower case letters, digits and -, starting with a letter or digit, at most 64 characters"))
    }
}

fn shape_of(letter: &Letter, to: To) -> Shape {
    Shape::MailSend {
        to,
        from: letter.from,
        mesh: letter.mesh.clone(),
        subject: letter.subject.clone(),
        hold: letter.hold,
        text: letter.text.clone(),
    }
}

/// Sends `letter`: the checks that cost nothing, a place among the sends, aoide, the node and the mesh, the run, and the row. `Ok` is a
/// letter aoide took.
pub(crate) async fn send(ctx: &Ctx, letter: Letter) -> Result<Sent, Refusal> {
    let Some(dir) = mail::mail_dir(ctx) else { return Err(Refusal::new(StatusCode::NOT_FOUND, "this perch has no home to keep mail in")) };
    // 1. What is refused without asking anyone. The runner's own checks are the ones used: they run before a process exists.
    if letter.text.len() > mail::TEXT_MAX {
        let kib = letter.text.len().div_ceil(1024);
        return Err(Refusal::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("that note is {kib} KiB and a letter is 64 KiB at most (it sits in a command line while aoide runs)"),
        ));
    }
    let to = to_of(&letter.node)?;
    if let Err(Error::Refused(why)) = shape_of(&letter, to.clone()).argv() {
        return Err(Refusal::bad(why));
    }
    // 2. Another perch on this home keeps the mail folder: this one sends nothing.
    if let Some(why) = mail::refused(&ctx.mail) {
        return Err(Refusal::new(StatusCode::SERVICE_UNAVAILABLE, why));
    }
    // 3. A place among the sends: a third one at once is 429, and nothing is started for it.
    let Some(_slot) = mail::take_send_slot(&ctx.mail) else {
        return Err(Refusal::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("{} sends are running already: wait for one to finish, then send again", mail::SENDS_AT_ONCE),
        ));
    };
    // 4. Aoide, and that it lists what the perch sends.
    let aoide = mail::usable(ctx).await.map_err(|why| Refusal::new(StatusCode::SERVICE_UNAVAILABLE, why))?;
    // 5. The node and the mesh.
    resolve(ctx, &letter).await?;
    // 6. The run.
    deliver(ctx, &aoide, &dir, letter, to).await
}

/// A node other than this machine is a verified one of Aoide's registry, and a mesh, when it is named and the node's meshes are known,
/// is one of them. `mesh --json` lists the rows that drift and not a clean bill per node, so a healthy node can have no meshes here:
/// then the name is the grammar's to check and aoide's to judge.
async fn resolve(ctx: &Ctx, letter: &Letter) -> Result<(), Refusal> {
    if letter.node == "self" {
        return Ok(());
    }
    let known = mesh::nodes(ctx).await;
    let Some(node) = known.iter().find(|node| node.name == letter.node) else { return Err(Refusal::conflict(unknown_node(&letter.node))) };
    if !node.verified {
        return Err(Refusal::conflict(unpaired_node(&node.name)));
    }
    if let Some(mesh) = &letter.mesh
        && !node.meshes.is_empty()
        && !node.meshes.contains(mesh)
    {
        return Err(Refusal::conflict(format!("node {} is not in mesh {mesh}; it is in: {}", node.name, node.meshes.join(", "))));
    }
    Ok(())
}

async fn deliver(ctx: &Ctx, aoide: &Aoide, dir: &std::path::Path, letter: Letter, to: To) -> Result<Sent, Refusal> {
    let sha = harnox::crypto::hex(&harnox::crypto::sha256(letter.text.as_bytes()));
    let address = format!("{}/{}", letter.node, aoide::MAILBOX);
    // Once aoide has been spawned the letter may be spooled, whatever came back: the attempt is on record as `unknown`, with the msgid
    // when one was read, and the page is told to look at the outbox before it sends again.
    let unknown = |why: String| {
        Refusal::new(StatusCode::BAD_GATEWAY, format!("{why}; the letter may or may not have been sent: check `aoide mail outbox` for {address} before sending it again"))
    };
    let timeout = mail::knob_ms(ctx, "mail_send_ms", aoide::SEND_TIMEOUT);
    match aoide.for_send(timeout).run(&shape_of(&letter, to)).await {
        Ok(reply) => {
            let took = parse_took(&reply);
            if !took.accepted {
                // Aoide took the letter's shape and said no to this recipient: nothing was spooled.
                return Err(Refusal::conflict(took.error.unwrap_or_else(|| format!("aoide did not accept the letter for {address}"))));
            }
            if took.msgid.is_none() {
                let _ = record(ctx, dir, row_of(&letter, &sha, &address, None, "unknown", "unknown")).await;
                return Err(unknown("aoide answered in a shape the perch does not read".to_string()));
            }
            let row = row_of(&letter, &sha, &address, took.msgid.as_deref(), &took.disposition, &took.status);
            let mut warning = record(ctx, dir, row).await.err();
            if !took.complete && warning.is_none() {
                warning = Some(format!("aoide did not complete the send to {address}: check `aoide mail outbox` before sending it again"));
            }
            Ok(Sent {
                msgid: took.msgid,
                disposition: took.disposition,
                delivery: took.delivery,
                status: took.status,
                to: address,
                mesh: letter.mesh,
                error: took.error,
                warning,
            })
        }
        Err(Error::Failed(reply)) if certain(&reply) => Err(refused_by_aoide(&reply, &letter.node)),
        Err(Error::Failed(reply)) => {
            let why = refused_by_aoide(&reply, &letter.node).why;
            let _ = record(ctx, dir, row_of(&letter, &sha, &address, None, "unknown", "unknown")).await;
            Err(unknown(why))
        }
        Err(Error::Refused(why)) => Err(Refusal::bad(why)),
        Err(e @ Error::NoBinary) => Err(Refusal::new(StatusCode::SERVICE_UNAVAILABLE, e.to_string())),
        // Not started: nothing was spooled, so there is no row and nothing to check.
        Err(e @ Error::Spawn(_)) => Err(Refusal::new(StatusCode::BAD_GATEWAY, e.to_string())),
        Err(e @ (Error::Timeout(_) | Error::TooBig(_) | Error::NotJson)) => {
            let _ = record(ctx, dir, row_of(&letter, &sha, &address, None, "unknown", "unknown")).await;
            Err(unknown(e.to_string()))
        }
    }
}

/// Whether an aoide that ran and said no said so before anything was spooled: it named a reason that is certain (`unknown-node`,
/// `unpaired-node`, `ambiguous-mesh`) or listed the meshes it would choose from. Any other failure, or a signal, may have spooled.
fn certain(reply: &Reply) -> bool {
    if reply.code < 0 || reply.json.is_none() {
        return false;
    }
    let data = reply.data();
    let named = data.and_then(|data| data.get("reason")).and_then(Value::as_str);
    let meshes = data.and_then(|data| data.get("meshes")).and_then(Value::as_array).is_some_and(|list| !list.is_empty());
    matches!(named, Some("unknown-node" | "unpaired-node" | "ambiguous-mesh")) || meshes
}

/// What aoide said it did with the letter, from its `--subject` shape: the one recipient's `msgid`, `status` (the word `disposition` is
/// kept as), `delivery` and `error`, and the whole send's `accepted` and `complete`.
struct Took {
    msgid: Option<String>,
    disposition: String,
    delivery: Option<Value>,
    status: String,
    /// The recipient was accepted (`false` for a letter aoide would not take for this node).
    accepted: bool,
    /// The send was complete (every copy filed or spooled).
    complete: bool,
    error: Option<String>,
}

/// `data.recipients[0]`, the one recipient the perch sends to. Read defensively: aoide's `--json` shapes are not in its schema, and a
/// field that is not what it should be is left out. A reply with other than one recipient has no msgid this perch can tell is its own.
fn parse_took(reply: &Reply) -> Took {
    let data = reply.data();
    let recipients = data.and_then(|data| data.get("recipients")).and_then(Value::as_array);
    let recipient = recipients.filter(|list| list.len() == 1).and_then(|list| list.first());
    let msgid = recipient
        .and_then(|recipient| recipient.get("msgid"))
        .and_then(Value::as_str)
        .filter(|msgid| mail::hex64(msgid))
        .map(str::to_string);
    let disposition = recipient
        .and_then(|recipient| recipient.get("status"))
        .and_then(Value::as_str)
        .filter(|word| mail::word_ok(word))
        .unwrap_or("unknown")
        .to_string();
    let delivery = recipient.and_then(|recipient| recipient.get("delivery")).and_then(clean_delivery);
    let error = recipient.and_then(|recipient| recipient.get("error")).and_then(clean_error);
    let accepted = recipient.and_then(|recipient| recipient.get("accepted")).and_then(Value::as_bool).unwrap_or(true);
    let complete = data.and_then(|data| data.get("complete")).and_then(Value::as_bool).unwrap_or(false);
    let reported = delivery.as_ref().and_then(|delivery| delivery.get("status")).and_then(Value::as_str).map(str::to_string);
    let status = reported.unwrap_or_else(|| match disposition.as_str() {
        "filed" => "delivered",
        "held" => "held",
        "spooled" => "queued",
        _ => "unknown",
    }
    .to_string());
    Took { msgid, disposition, delivery, status, accepted, complete, error }
}

/// The recipient's `error` as one line: a string, or an object's `reason` or `message`. Anything else is nothing.
fn clean_error(error: &Value) -> Option<String> {
    let text = match error {
        Value::String(text) => text.as_str(),
        Value::Object(object) => object.get("reason").or_else(|| object.get("message"))?.as_str()?,
        _ => return None,
    };
    let line = mail::line_safe(text, WHY_MAX);
    (!line.is_empty()).then_some(line)
}

/// The fields of aoide's `delivery` that the page shows, each checked: a status word, a reason sentence, a scope word, a time, a flag.
fn clean_delivery(delivery: &Value) -> Option<Value> {
    let object = delivery.as_object()?;
    let word = |key: &str| object.get(key).and_then(Value::as_str).filter(|word| mail::word_ok(word)).map(str::to_string);
    let text = |key: &str, max: usize| object.get(key).and_then(Value::as_str).map(|text| mail::line_safe(text, max));
    Some(json!({
        "status": word("status")?,
        "reason": text("reason", 200),
        "reasonScope": word("reasonScope"),
        "nextAttemptAt": text("nextAttemptAt", 40),
        "ackPending": object.get("ackPending").and_then(Value::as_bool).unwrap_or(false),
    }))
}

/// An aoide that ran and said no. A refusal it understood (an envelope) is a 409; one it did not (no envelope) is a 502.
fn refused_by_aoide(reply: &Reply, node: &str) -> Refusal {
    let data = reply.data();
    let meshes: Vec<String> = data
        .and_then(|data| data.get("meshes"))
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).filter(|mesh| aoide::name_ok(mesh)).take(MESHES_MAX).map(str::to_string).collect())
        .unwrap_or_default();
    let why = mail::line_safe(&reply.sentence(), WHY_MAX);
    if !meshes.is_empty() {
        return Refusal { code: StatusCode::CONFLICT, why, meshes: Some(meshes) };
    }
    match data.and_then(|data| data.get("reason")).and_then(Value::as_str) {
        Some("unknown-node") => Refusal::conflict(unknown_node(node)),
        Some("unpaired-node") => Refusal::conflict(unpaired_node(node)),
        _ if reply.json.is_some() => Refusal::conflict(why),
        _ => Refusal::new(StatusCode::BAD_GATEWAY, why),
    }
}

// ---------------------------------------------------------------------------
// sent.jsonl
// ---------------------------------------------------------------------------

fn row_of(letter: &Letter, sha: &str, to: &str, msgid: Option<&str>, disposition: &str, status: &str) -> Value {
    let mut row = json!({
        "ts": mail::now(),
        "origin": letter.origin,
        "from": letter.from.name(),
        "note": letter.note,
        "subject": letter.subject,
        "sha256": sha,
        "to": to,
        "msgid": msgid,
        "disposition": disposition,
        "status": status,
    });
    if let Some(mesh) = &letter.mesh {
        row["mesh"] = json!(mesh);
    }
    row
}

/// Appends the row to `<home>/mail/sent.jsonl`, under the lock, on a blocking thread. `Err` is the sentence for the `warning`.
async fn record(ctx: &Ctx, dir: &std::path::Path, row: Value) -> Result<(), String> {
    let (ctx, dir, line) = (ctx.clone(), dir.to_path_buf(), row.to_string());
    let done = tokio::task::spawn_blocking(move || {
        let _held = ctx.mail.sent.lock().unwrap_or_else(|e| e.into_inner());
        mail::ensure_dir(&dir)?;
        mail::append_lines(&dir.join("sent.jsonl"), &[line])
    })
    .await;
    match done {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "perch: a mail row was not written");
            Err("the letter was sent, but the row for it could not be written to sent.jsonl".to_string())
        }
        Err(e) => {
            tracing::warn!(error = %e, "perch: a mail row was not written");
            Err("the letter was sent, but the row for it was not written".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(code: i32, json: Value) -> Reply {
        Reply { code, stdout: json.to_string(), json: Some(json), stderr: String::new() }
    }

    fn letter(node: &str, mesh: Option<&str>, hold: bool, subject: &str, text: &str) -> Letter {
        Letter {
            from: Sender::Minerva,
            origin: "operator".to_string(),
            note: Some("a-note".to_string()),
            node: node.to_string(),
            mesh: mesh.map(str::to_string),
            hold,
            subject: subject.to_string(),
            text: text.to_string(),
        }
    }

    /// The `--subject` shape of a send to one recipient, as the probe observed it (made-up values).
    fn subject_reply(recipient: Value) -> Value {
        json!({"status": "ok", "command": "mail.send", "message": "1/1 recipient copies filed or spooled; inspect recipient delivery status before retrying",
               "gated": false, "data": {"accepted": 1, "complete": true, "recipients": [recipient], "replyTo": null, "subject": "s", "threadId": "e".repeat(64)}})
    }

    #[test]
    fn a_letter_aoide_took_is_read_from_the_recipient_of_its_subject_shape() {
        let spooled = json!({"accepted": true, "address": {"name": "minerva", "node": "osaka"}, "delivery":
            {"status": "queued", "reason": null, "reasonScope": null, "nextAttemptAt": null, "ackPending": false},
            "error": null, "msgid": "6".repeat(64), "ring": "no-daemon", "role": "to", "status": "spooled"});
        let took = parse_took(&reply(0, subject_reply(spooled)));
        assert_eq!((took.msgid.as_deref(), took.disposition.as_str(), took.status.as_str()), (Some("6".repeat(64).as_str()), "spooled", "queued"));
        assert_eq!(took.delivery.as_ref().unwrap()["ackPending"], json!(false));
        assert!(took.accepted && took.complete && took.error.is_none());
        let filed = json!({"accepted": true, "address": {"name": "minerva", "node": "self"}, "delivery": null, "error": null,
            "msgid": "5".repeat(64), "ring": "no-daemon", "role": "to", "status": "filed"});
        let took = parse_took(&reply(0, subject_reply(filed)));
        assert_eq!((took.msgid.as_deref(), took.disposition.as_str(), took.status.as_str()), (Some("5".repeat(64).as_str()), "filed", "delivered"));
        assert!(took.delivery.is_none());
        let held = json!({"accepted": true, "delivery": null, "error": {"reason": "held\u{1b}[0m\nfor a while"}, "msgid": "7".repeat(64), "status": "held"});
        let took = parse_took(&reply(0, subject_reply(held)));
        assert_eq!((took.status.as_str(), took.error.as_deref()), ("held", Some("held[0mfor a while")));
        // The envelope with no subject is not a shape the perch reads: no msgid comes from it.
        let no_subject = reply(0, json!({"status": "ok", "data": {"envelope": {"msgid": "5".repeat(64)}, "disposition": "filed", "ring": "no-daemon"}}));
        let took = parse_took(&no_subject);
        assert_eq!((took.msgid, took.disposition.as_str(), took.status.as_str()), (None, "unknown", "unknown"));
        // Two recipients is not the one the perch sent to.
        let two = json!({"status": "ok", "data": {"complete": true, "recipients": [{"msgid": "6".repeat(64), "status": "filed"}, {"msgid": "7".repeat(64), "status": "filed"}]}});
        assert_eq!(parse_took(&reply(0, two)).msgid, None);
    }

    #[test]
    fn what_aoide_printed_that_is_not_what_it_should_be_is_left_out() {
        for recipient in [
            json!({"msgid": "short", "status": "Spooled; rm -rf", "delivery": "queued"}),
            json!({"msgid": "G".repeat(64), "status": ["spooled"], "delivery": [1, 2]}),
            json!({"msgid": 7, "status": 7, "delivery": {"status": "a b c"}}),
            json!("recipient is a string"),
            json!(null),
        ] {
            let took = parse_took(&reply(0, subject_reply(recipient.clone())));
            assert_eq!(took.msgid, None, "{recipient}");
            assert_eq!((took.disposition.as_str(), took.status.as_str()), ("unknown", "unknown"), "{recipient}");
            assert!(took.delivery.is_none(), "{recipient}");
        }
        for data in [json!("data is a string"), json!(null)] {
            let took = parse_took(&reply(0, json!({"status": "ok", "data": data})));
            assert_eq!(took.msgid, None, "{data}");
            assert_eq!((took.disposition.as_str(), took.status.as_str()), ("unknown", "unknown"), "{data}");
        }
        let delivery =
            clean_delivery(&json!({"status": "retrying", "reason": "bad\u{1b}[31m\nline", "reasonScope": "remote", "nextAttemptAt": "t", "extra": 1}))
                .unwrap();
        assert_eq!(delivery["reason"], json!("bad[31mline"), "a reason is one printable line");
        assert!(delivery.get("extra").is_none());
    }

    #[test]
    fn aoides_refusals_become_sentences_and_the_meshes_to_ask_about() {
        let multi = reply(
            1,
            json!({"status": "error", "message": "node `multi` is trusted in more than one mesh (alpha, beta): name one",
                   "data": {"reason": "ambiguous-mesh", "meshes": ["alpha", "beta", "Bad Mesh", 3]}}),
        );
        let refusal = refused_by_aoide(&multi, "multi");
        assert_eq!(refusal.code, StatusCode::CONFLICT);
        assert_eq!(refusal.meshes, Some(vec!["alpha".to_string(), "beta".to_string()]));
        assert!(refusal.why.contains("more than one mesh"), "{}", refusal.why);
        let unknown = refused_by_aoide(&reply(1, json!({"status": "error", "message": "x", "data": {"reason": "unknown-node"}})), "osaka");
        assert_eq!((unknown.code, unknown.why.as_str()), (StatusCode::CONFLICT, "no node named osaka is registered with Aoide on this machine"));
        let unpaired = refused_by_aoide(&reply(1, json!({"status": "error", "message": "x", "data": {"reason": "unpaired-node"}})), "bridge");
        assert!(unpaired.why.contains("bridge is registered but not paired"), "{}", unpaired.why);
        let other = refused_by_aoide(&reply(1, json!({"status": "error", "message": "the outbox is full"})), "osaka");
        assert_eq!((other.code, other.why.as_str()), (StatusCode::CONFLICT, "the outbox is full"));
        let none = refused_by_aoide(&Reply { code: 2, stdout: String::new(), json: None, stderr: "boom\nmore".to_string() }, "osaka");
        assert_eq!((none.code, none.why.as_str()), (StatusCode::BAD_GATEWAY, "boom"));
        let many: Vec<String> = (0..40).map(|n| format!("m{n}")).collect();
        let flood = refused_by_aoide(&reply(1, json!({"status": "error", "message": "m", "data": {"meshes": many}})), "multi");
        assert_eq!(flood.meshes.unwrap().len(), MESHES_MAX);
    }

    #[test]
    fn the_checks_that_cost_nothing_are_the_runners() {
        let refused = |letter: Letter| {
            let to = to_of(&letter.node)?;
            match shape_of(&letter, to).argv() {
                Err(Error::Refused(why)) => Err(Refusal::bad(why)),
                Err(e) => panic!("{e}"),
                Ok(argv) => Ok(argv),
            }
        };
        assert_eq!(refused(letter("self", None, true, "s", "t")).unwrap_err().why, "hold needs another node");
        assert!(refused(letter("self", Some("lab"), false, "s", "t")).unwrap_err().why.contains("no mesh"));
        assert!(refused(letter("osaka", Some("Lab"), false, "s", "t")).is_err());
        assert!(refused(letter("Osaka", None, false, "s", "t")).is_err());
        assert!(refused(letter("osaka", None, false, "two\nlines", "t")).unwrap_err().why.contains("one line"));
        assert!(refused(letter("osaka", None, false, "sep\u{2028}x", "t")).unwrap_err().why.contains("one line"));
        assert!(refused(letter("osaka", None, false, "", "t")).unwrap_err().why.contains("needs a subject"));
        assert!(refused(letter("osaka", None, false, "s", "")).unwrap_err().why.contains("needs text"));
        let argv = refused(letter("osaka", Some("alpha"), true, "a note", "text")).unwrap();
        assert_eq!(argv.first().map(String::as_str), Some("mail"));
        assert!(argv.contains(&"--from=minerva".to_string()) && argv.contains(&"--mesh=alpha".to_string()), "{argv:?}");
    }

    #[test]
    fn a_row_is_one_line_with_the_text_left_out() {
        let mut one = letter("osaka", Some("alpha"), false, "build\u{2028}checklist \"v2\"", "SECRET-TEXT");
        one.note = None;
        one.origin = "agent:ctf-pwn-3".to_string();
        one.from = Sender::MinervaAgent;
        let row = row_of(&one, &"a".repeat(64), "osaka/minerva", Some(&"6".repeat(64)), "spooled", "queued");
        let line = row.to_string();
        assert!(!line.contains('\n') && !line.contains("SECRET-TEXT"), "{line}");
        assert_eq!(row["from"], json!("minerva-agent"));
        assert_eq!(row["origin"], json!("agent:ctf-pwn-3"));
        assert_eq!(row["note"], Value::Null);
        assert_eq!(row["mesh"], json!("alpha"));
        let nameless = row_of(&letter("self", None, false, "s", "t"), "b", "self/minerva", None, "unknown", "unknown");
        assert!(nameless.get("mesh").is_none());
    }
}
