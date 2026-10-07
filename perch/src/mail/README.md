# src/mail/ — Aoide mail (`/hub/mail`)

A note is sent as Aoide mail, and a letter that comes back is a card in the notes tab. Aoide is reached only as a subprocess
(`src/aoide/`: an explicit binary and a closed set of argvs), never a transport of the perch's own. Every route here is the operator's
(the master token); the agent's own are `agent.rs`'s (M3). A perch started without `--home` has no mail: every route is the empty 404, and
nothing is run.

```text
mod.rs    State, pick() (the dispatch), the mail folder helpers (ensure_dir, append_lines, read_tail, line_safe), route, send_note, optin, start
send.rs   GET /hub/mail/nodes; the body of POST /hub/notes/<n>/send; send() and Letter, which M3 reuses; sent.jsonl           (M2)
recv.rs   GET /hub/mail, POST /hub/mail/<msgid>/place|dismiss|save; the receive task; received.jsonl and cards.json         (M2)
agent.rs  POST /hub/mail/a/<verb> behind the agent gate; POST /hub/sessions/<id>/mailsend                                    (M3)
```

`mail::pick` is the whole dispatch below `/hub/mail`; `a` and `a/...` are the agent's and nobody else's, by the same predicate as the agent
gate, and a message id is 64 hex digits, so `nodes` and `a` are never ids. Two entry points belong to other routes: `notes` owns `POST
/hub/notes/<n>/send` and calls `mail::send_note`; `POST /hub/sessions/<id>/mailsend` is a session verb that `serve` hands to `mail::optin`.

## Routes

```text
GET  /hub/mail/nodes                  [{name, verified, self, meshes}]   this machine (`self`, first), then the registry's nodes
POST /hub/notes/<n>/send {node, mesh?, hold?}
                                      202 {msgid, disposition, delivery, status, to, mesh?, warning?}
                                      409 {error, why, meshes}           a node in several meshes: ask which, send again with `mesh`
GET  /hub/mail                        {available, received:[card], more, sent:[row], error}      also: "a page is looking"
POST /hub/mail/<msgid>/place {chat}   200 {msgid, state:"placed", chat}  404 no such card or chat, 409 dismissed
POST /hub/mail/<msgid>/dismiss        200 {msgid, state:"dismissed"}     body empty or {}
POST /hub/mail/<msgid>/save           201 {name, sha, mtime}             a note made from the card (notes::create)
```

A **card** is `{msgid, seq, at, seen, node, mailbox, sender, via, subject, text, format, broken, clipped, thread, reply_to, state, chat}`:
`sender` is `minerva`, `minerva-agent` or `other` (from the sender's mailbox name, which the sender chose: attribution, not proof);
`format` is `letter` (an `AOIDE-LETTER/1` that parsed) or `raw`; `state` is `new`, `placed` (with `chat`) or `dismissed` (not listed). A
**sent row** is what `sent.jsonl` holds plus `status` (`queued`, `held`, `retrying`, `accepted`, `delivered`, `refused`, `unknown`) and
`reason`. **Nothing here ever posts a card to a door**: `place` records which chat the page shows a card with, and the page puts the
quoted text in a composer for a person to send or not. `tests/mail.rs` holds the fake door to that.

## Every refusal (each is a test in `tests/mail.rs`)

```text
send   400  the body is not {node, mesh?, hold?} (an unknown field is refused: there is no field to name a sender with); a node or mesh
            outside ^[a-z0-9][a-z0-9-]*$; a mesh or hold with `self`; a subject that is empty; no text            -> nothing is run
       404  no --home; no such note (the name grammar, a link or FIFO under the name)                             -> nothing is run
       413  the note is over 64 KiB (it sits in argv while aoide runs); the body is over 4 KiB                     -> nothing is run
       503  no aoide (no --aoide and no ~/.local/bin/aoide)
       409  the node is not in the registry, or not paired; the mesh is not one the node is in (when `mesh --json` puts it in any)
            -> the same sentences when aoide itself says unknown-node / unpaired-node; a node in several meshes is {error, why, meshes}
       502  aoide could not start, printed too much, printed what is not JSON, or failed with no envelope
       504  aoide did not answer in 10 s
            (502/504 after the letter may have been spooled also write a row with disposition `unknown`, and say "may or may not have been sent")
place  400  the body is not {chat}; 404 no such card, or a chat this perch does not know (an id outside the grammar included); 409 dismissed
dismiss, save   400 a body that is not empty or {}; 404 no such card; save: 404 when the text of the card is not on file
every route     401 no credential (a scoped agent token included); 404 for a method or a path that is not a route; 400 for a query string
```

## Files

All under `<home>/mail/` (a directory made `0700`, never a link), each opened `O_NOFOLLOW`, written `0600` (a looser one is tightened):

```text
sent.jsonl      one line per send, appended under a lock: {ts, origin, from, note, subject, sha256, to, mesh?, msgid, disposition, status}.
                origin is `operator`, or `agent:<chat>` (M3). The text is not kept, only its sha256. A row that cannot be written does not
                undo the send: the answer is the 202 with a `warning` sentence.
received.jsonl  one line per letter read from the mailbox, appended, deduped by message id, text at most 64 KiB (cut at a character)
cards.json      message id -> {state, chat?, at}, and sent message id -> when its receipt was read. Rewritten whole, atomically.
```

## The receive task

`mail::start` spawns one task. It does nothing until a page has polled `GET /hub/mail` in the last 60 s (`--knob mail_watch_ms`), and then
every 5 s (`mail_tick_ms`) it **stats** `<aoide-home>/state/mail/base.jsonl` (inode, size, mtime) and runs `mail read --for=minerva --json`
(as `AOIDE_SESSION_ID=minerva-perch`, so only Minerva's own cursor moves) **only when that changed**. An unchanged mailbox, a missing one,
a page that stopped looking, and a `GET` itself cost no aoide call.

- The first tick runs `--reread` once (4 MiB at most), which recovers a letter aoide handed over and advanced its cursor past and that the
  perch never wrote down. A plain read that may have advanced the cursor without the letters being kept (a timeout, too much output,
  output that is not JSON, a write that failed) is followed by a `--reread` after a pause of 30 s (`mail_pause_ms`).
- `mail export` is not used: it is read-only, but it returns Markdown threads for every mailbox and no receipts.
- A **receipt** (an entry of type `receipt` whose text is the message id of a row of `sent.jsonl`) marks that row `delivered`, but only when
  it came from the node the row went to: a third node that learned an id cannot claim a delivery. If Aoide names a node differently in a
  receipt than the registry does, rows stay `accepted` and never reach `delivered`; the check is one `if` in `recv::apply`.
- Any other status comes from `mail outbox --json`, asked at most once a minute (`mail_ttl_ms`) and only while a row waits on it: a row in
  the outbox shows aoide's status and reason; a row that is not in an outbox fetched after it was sent has left it (`accepted`).
- The store is bounded: the task stops reading at 5000 cards or 32 MiB of letters and says so in `error`. Aoide keeps the letters it was
  not asked for, so nothing is lost; moving `received.jsonl` and `cards.json` aside starts a fresh store. `GET` shows the 25 newest cards
  that are not dismissed (`more` counts the rest) and the 50 newest sent rows, and reads only the end of each file.

## Risks that are accepted, and why

- **The note's text is in argv while aoide runs**, so any process of this user can read it in `/proc/<pid>/cmdline` for that moment. The
  runner has no stdin shape for a letter; the cap (64 KiB) is what keeps it a single argument.
- **A send to `self` rings aoided** (it is a filed letter in this machine's own mailbox). If aoided's petname fallback resolves the
  `minerva` mailbox to a running session named `minerva`, that session is nudged. Nothing here names a session.
- **Every mail read takes aoide's mailbox lock**; the runner's 10 s timeout bounds the wait. The task never runs two at once.
- **A held letter (`hold: true`) leaves only when the receiving node polls.** A Minerva perch never runs `mail poll`, so a hold is a letter
  the other node has to come and get; the row says `held`.
- **A card's text is another machine's.** It is stored and sent as a JSON string, clamped (64 KiB, headers to their grammars), and never
  interpreted by the perch; the page draws it as text.
- **Sender attribution is a choice the sender made** (`--from` is not signed as a role). `sender: "minerva-agent"` means the letter says so.
