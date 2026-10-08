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
                                      202 {msgid, disposition, delivery, status, to, mesh?, error?, warning?}
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
       503  no aoide (no --aoide and no ~/.local/bin/aoide); aoide's schema does not list a command or flag the perch sends (the sentence
            names it, and mail is off until aoide is one that does); a relative --aoide-home (`AOIDE_ROOT must be an absolute path`; the
            perch starts anyway, and every mail call is refused before aoide runs); another perch holds this home's mail (its lock), or the
            mail folder cannot be used. The schema is asked at most once a minute, good or bad (`aoide schema`, never `--version`: aoide has
            no such flag). The `mesh --json` entry of the schema is in that list, because a send's node check reads it
       429  two sends are running already (`SENDS_AT_ONCE` = 2): the third is refused at once, and nothing is run or written
       409  the node is not in the registry, or not paired; the mesh is not one the node is in (when `mesh --json` puts it in any);
            aoide refused the send before it filed anything (`accepted` false). The same sentences when aoide itself says unknown-node /
            unpaired-node; a node in several meshes is {error, why, meshes}. Certain refusals write no row.
       502  aoide could not be started (the spawn failed): no row, nothing was spooled.
            Every other 502 comes after the spawn, and writes a row with status `unknown`: a timeout (`SEND_TIMEOUT`, 45 s: longer than
            aoide's own 30 s, and `mail_send_ms` in a test), a signal, more output than the cap, an answer that is not JSON, a failure that
            names no certain reason (only `unknown-node`, `unpaired-node`, `ambiguous-mesh` or a `meshes` list are certain, and they are
            409), or an answer with no msgid for the recipient. It answers 502 with "may or may not have been sent: check `aoide mail outbox`
            for <node>/minerva before sending it again". There is no 504: a 504 invites a resend.
            An answer that says the send is not complete is a 202 with a `warning`, and the row is kept.
place  400  the body is not {chat}; 404 no such card, or a chat this perch does not know (an id outside the grammar included); 409 dismissed
dismiss, save   400 a body that is not empty or {}; 404 no such card; save: 404 when the text of the card is not on file
every route     401 no credential (a scoped agent token included); 404 for a method or a path that is not a route; 400 for a query string
```

## Files

All under `<home>/mail/` (a directory made `0700`, never a link), each opened `O_NOFOLLOW`, written `0600` (a looser one is tightened):

```text
sent.jsonl      one line per send, appended under a lock: {ts, origin, from, note, subject, sha256, to, mesh?, msgid, disposition, status}.
                origin is `operator`, or `agent:<chat>` (M3). The text is not kept, only its sha256. A row that cannot be written does not
                undo the send: the answer is the 202 with a `warning` sentence. The msgid is the recipient's copy (`--subject` shape).
                `disposition` is the recipient's `status` word (`filed`, `spooled`, `held`) as the perch kept it; aoide has no such key.
received.jsonl  one line per letter read from the mailbox, appended, text at most 64 KiB (cut at a character). A message id already on file
                is not written again, whatever cards.json holds. A torn last line (no newline) is never read as a record.
cards.json      message id -> {state, chat?, at}, and sent message id -> when its receipt was read. Rewritten whole, atomically. One that
                does not parse is NOT set aside: the mail routes say so and read nothing until the operator moves it aside by hand.
dismissed.jsonl the tombstones: one message id per dismissed card, appended. A dismissed card stays dismissed however cards.json came to be.
                The task keeps the newest 10,000 of them (`TOMBSTONES_KEEP`), read from the file's last 1 MiB (`TOMBSTONES_WINDOW`): a card
                dismissed before that window does not stay dismissed if cards.json is gone too.
.lock           this home's lock (an exclusive flock, held for as long as the perch runs). A second perch on the same home cannot take it,
                and its mail routes answer with a sentence.
```

## The receive task

`mail::start` spawns one task. It does nothing until a page has polled `GET /hub/mail` in the last 60 s (`--knob mail_watch_ms`), and then
every 5 s (`mail_tick_ms`) it **stats** `<aoide-home>/state/mail/base.jsonl` (inode, size, mtime) and runs `mail read --for=minerva --json`
(as `AOIDE_SESSION_ID=minerva-perch`, so only Minerva's own cursor moves) **only when that changed**. An unchanged mailbox, a missing one,
a page that stopped looking, and a `GET` itself cost no aoide call.

- The first look runs `--reread` once. It prints the whole history, so it recovers letters aoide handed over and the perch never wrote down.
  A plain read that **fails** (a timeout, too much output, output that is not JSON, a write that failed) may have moved the cursor past
  letters it did not keep, so a `--reread` is due after a pause of 30 s (`mail_pause_ms`), and no plain read runs until it has gone
  through. A reread that fails for another reason is tried again after a minute (`mail_reread_ms`). A reread **too big to print** (over
  16 MiB) can never succeed later, because the history only grows: it is not tried again. The page's `error` then says that letters for
  minerva could not be read here, and that `aoide mail read --for=minerva --reread`, run in a terminal, reads them. The perch keeps none.
- Every entry a reread or a plain read prints is kept, deduplicated by message id against what is on file. Nothing is cut by position:
  2001 letters in one read are 2001 cards (25 shown, `more` 1976), and a read that fits its cap is kept whole.
- `mail export` is not used: it is read-only, but it returns Markdown threads for every mailbox and no receipts.
- A **receipt** (an entry of type `receipt` whose text is the message id of a row of `sent.jsonl`) marks that row `delivered`, but only when
  it came from the node the row went to: a third node that learned an id cannot claim a delivery. If Aoide names a node differently in a
  receipt than the registry does, rows stay `accepted` and never reach `delivered`; the check is one `if` in `recv::apply`.
- Any other status comes from `mail outbox --json`, asked at most once a minute (`mail_ttl_ms`) and only while a row waits on it: a row in
  the outbox shows aoide's status and reason; a row that is not in an outbox fetched after it was sent has left it (`accepted`).
- The store is bounded: the task reads nothing while there are 5000 live cards (dismissed ones do not count) or `received.jsonl` is 32 MiB
  or more. It says so in `error`, and receipts wait with it. There are two ways on, both in the same run and without a restart: dismiss
  cards until fewer than 5000 are live, or move `received.jsonl` aside (the next tick finds it short). Moving `cards.json` aside does not
  free the store, because the cards are read into memory once, so that needs a restart. Aoide keeps the letters the perch has not read, so
  nothing is lost while the store is full.
  `GET` shows the 25 newest cards that are not dismissed (`more` counts the rest) and the 50 newest sent rows, and reads only the end of
  each file (2 MiB of letters, 256 KiB of rows).
- A card's sender is what the letter claims: the card shows `mailbox on node` beside it (`from_label`) and says so (`sender_claimed`).
- The outbox is read by `mail outbox --json` at most once a minute while a row waits on it, and the answer is cut at 2000 entries. A cut
  outbox is marked incomplete, so a row it does not list is `unknown`, never `accepted`.

## Caps

`aoide mail read --json` prints each letter **twice**: in the envelope's `message` text and in `data.entries[i].envelope.text`, both
JSON-escaped. So a read's stdout is about

    stdout ≈ 2 × (letter bytes, escaped) + about 1 KiB per entry

and a control byte costs `\u00XX` (six bytes) in each copy, so up to 12 × for a letter of control bytes. A plain read is capped at
`PLAIN_CAP` = 4 MiB and a `--reread` at `REREAD_CAP` = 16 MiB (and 30 s, `REREAD_TIMEOUT`); the runner's default is 1 MiB and 10 s. For 1 KiB
letters that is about 3 KiB of stdout per letter: about 1,400 letters fit a plain read and about 5,400 a reread.

A plain read over its cap has **already consumed** its letters (aoide marks a batch read as it prints it), so the reread due after it is
what brings them back, for as long as the whole history fits 16 MiB. Past that the loss is said on the page (see above).

## Risks that are accepted, and why

- **The letter's text is in argv while aoide runs**, so it can be read from `/proc/<pid>/cmdline` for that moment by any local account that can
  read that file (on a default Linux mount, every account; `hidepid` narrows it). aoide reads no body on stdin, and the schema lists `text`
  as the one argument, so there is no other shape to send it in. The cap (64 KiB) keeps it one argument; it does not hide the text.
- **The perch's environment is not handed on.** The runner clears it and passes only the allowlist (`aoide::ENV_KEEP`: PATH, HOME, LANG, TZ,
  and every `LC_*`) and the AOIDE_* variables it sets itself. A proxy and a provider key of the perch's do not reach aoide, and a test
  checks that. The XDG directories (`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`) and the CA bundle variables
  (`SSL_CERT_FILE`, `CURL_CA_BUNDLE`, ...) are dropped as well, and the loss is the operator's to weigh: a node that needs a private CA
  bundle is not reachable from here until its name is added to `ENV_KEEP` (one line, a decision). aoide's own use of those variables is
  not tested here; the fake does not read them. See `src/aoide/README.md`, "The child".
- **A child dies with the perch.** The runner sets `PR_SET_PDEATHSIG` (SIGKILL) on aoide in `pre_exec`, so a perch that dies takes a running
  aoide with it. The signal is tied to the thread that started the child, and the perch's runtime keeps its worker threads for its whole
  life. `the_child_dies_with_the_perch_that_started_it` (`tests/aoide.rs`) checks it. A timeout kills its process group as well.
- **A send to `self` rings aoided** (it is a filed letter in this machine's own mailbox). If aoided's petname fallback resolves the
  `minerva` mailbox to a running session named `minerva`, that session is nudged. Nothing here names a session.
- **Every mail read takes aoide's mailbox lock** (`.stage.lock`). A plain read's wait is bounded by the runner's 10 s timeout, and a reread's
  by `REREAD_TIMEOUT`, 30 s. The task never runs two reads at once.
- **The page's listing reads the end of `received.jsonl` only** (2 MiB, see "The receive task"): a card whose letter is older than that window is
  not shown, even though it is stored and not dismissed. Open (the review's `receive-card-window-hides`); not fixed in the M2 fix round.
- **The perch says it is listening before it takes the mail lock** (`lib.rs` prints first, and `mail::start` runs after the bind). A client that
  reaches into the mail folder in that moment can find it not yet made. The tests wait for the lock; the product does not, and the order is
  `lib.rs`'s (never edited in this round).
- **A held letter (`hold: true`) leaves only when the receiving node polls.** A Minerva perch never runs `mail poll`, so a hold is a letter
  the other node has to come and get; the row says `held`.
- **A card's text is another machine's.** It is stored and sent as a JSON string, clamped (64 KiB, headers to their grammars), and never
  interpreted by the perch; the page draws it as text.
- **Sender attribution is a choice the sender made** (`--from` is not signed as a role). `sender: "minerva-agent"` means the letter says so.
