# tests/fixtures/aoide/ — what `fake_aoide` replays

Scripted `aoide --json` output, in the envelope aoide prints (`status`, `command`, `message`, `gated`, `changed?`, `data?`). `fake_aoide`
reads this directory through `FAKE_AOIDE_DIR`; `src/bin/fake_aoide.rs` says which shape reads which file. The runner (R2a), the mesh view
(R2b) and mail (M2) are tested against it, and nothing here is a copy of anything real.

| File | Replayed for | Holds |
|---|---|---|
| `version.txt` | nothing: `--version` is not a shape (aoide 0.0.26 exits 2 on it) | `aoide 0.0.26`. `tests/aoide.rs` checks it, and `tests/mesh.rs` copies it into the snapshot beside `schema.json` |
| `schema.json` | `schema` | the top-level `aoide` version and the `commands` the perch uses (`mail send`, `mail read`, `mail outbox`, `mesh`), with 0.0.26's flag names and made-up summaries |
| `mesh.json` | `mesh --json` | two meshes (`alpha`, `beta`) with drift rows, `osaka` unverified in both, and two undeclared nodes |
| `outbox.json` | `mail outbox --json` | two entries (one retrying toward `osaka`, one refused toward `bridge`) and the per-node summary |
| `mail-read.json` | `mail read --for=minerva --json` | eight entries, below. Each letter is in the envelope's `message` and in `data.entries[i].envelope.text`, both JSON-escaped, as aoide prints it |
| `mail-read-reread.json` | the same with `--reread` | the same eight and one older letter that only a reread prints |
| `send.json` | `mail send ...` | a table by the node in `--to`, answering in the `--subject` shape (`data.recipients[0]` holds the msgid, status, delivery and error): `self` (filed), `osaka` (spooled, queued; `with_hold` held), `bridge` (unpaired), `multi` (the multi-mesh refusal; `with_mesh` spooled). Any other node is `unknown-node`. Per recipient: `accepted` (a bool), `address`, `delivery`, `error`, `msgid`, `ring`, `role`, `status`; for the whole send `data.accepted` is a count and `data.complete` a bool |

The eight entries of `mail-read.json`, which are the cases M2 and the mesh view lean on:

```text
seq 1  letter   from minerva         a structured AOIDE-LETTER/1 (subject, to, cc, body with a newline, quotes and CJK)
seq 2  letter   from minerva-agent   a structured letter in a thread (threadId, replyTo)
seq 3  letter   from osaka/conductor plain text, no structure
seq 4  receipt  from osaka/minerva   its text is the msgid osaka's send returns (66...6): marks that sent row delivered
seq 5  letter   from osaka/minerva   an AOIDE-LETTER/1 cut off in the middle of its JSON
seq 6  letter   from osaka/minerva   hostile text: markup, a script close, ANSI colour, an OSC title, a template, a bidi override, a fence
seq 7  letter   from "<b>evil</b>"   hostile header fields: markup and escapes as node and mailbox, a time that is not a time, an id that is not hex
seq 8  letter   from minerva         a raw note with no letter marker
```

**What is observed and what is modelled.** The read, reread, `--subject` send to self, empty outbox and schema shapes were observed on
aoide 0.0.26 (a temp root, self-mail, `env -i`). The fixtures keep those structures and use made-up values (names, msgids, signatures,
node names); no raw output is copied. Not observed, because no node is paired: a remote send, `--hold`, a non-empty outbox entry, a
remote letter's delivery object, and a remote receipt. Those are modelled from the vendored `aoide-core` (0.0.23) source and the field
names in the plan, and the entries that depend on them say so. A self send and a remote one answer the same `--subject` shape (the
`self` and `osaka` entries of `send.json`), and the perch reads `data.recipients[0]` for both.

**The no-subject shape is not in this directory.** A send without `--subject` answers `data.envelope` (with its msgid), `receivedAt`, `ring`,
`seq`, `type` and `via`, with no `recipients`, and no `disposition` anywhere (both observed). The perch always sends `--subject`, and no
registered node could reach an entry for this shape, so it was removed from `send.json` rather than kept unreachable. The unit test
`a_letter_aoide_took_is_read_from_the_recipient_of_its_subject_shape` in `src/mail/send.rs` holds that shape and proves the perch reads no
msgid from it.

The files are generated once and edited by hand after that; ids are runs of one hex digit so a test can say which letter it means.
