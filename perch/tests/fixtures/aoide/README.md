# tests/fixtures/aoide/ — what `fake_aoide` replays

Scripted `aoide --json` output, in the envelope aoide prints (`status`, `command`, `message`, `gated`, `changed?`, `data?`). `fake_aoide`
reads this directory through `FAKE_AOIDE_DIR`; `src/bin/fake_aoide.rs` says which shape reads which file. The runner (R2a), the mesh view
(R2b) and mail (M2) are tested against it, and nothing here is a copy of anything real.

| File | Replayed for | Holds |
|---|---|---|
| `version.txt` | `--version` | `aoide 0.0.26` |
| `mesh.json` | `mesh --json` | two meshes (`alpha`, `beta`) with drift rows, `osaka` unverified in both, and two undeclared nodes |
| `outbox.json` | `mail outbox --json` | two entries (one retrying toward `osaka`, one refused toward `bridge`) and the per-node summary |
| `mail-read.json` | `mail read --for=minerva --json` | eight entries, below |
| `mail-read-reread.json` | the same with `--reread` | the same eight and one older letter that only a reread prints |
| `send.json` | `mail send ...` | a table by the node in `--to`: `self`, `osaka` (`with_hold`), `bridge` (unpaired), `multi` (the multi-mesh refusal; `with_mesh` succeeds); any other node is `unknown-node` |

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

**The shapes are modelled, not observed.** They follow the vendored `aoide-core` (0.0.23) source and the field names in the plan
(`disposition`, `data.reason`); the real 0.0.26's output was never run for this slice. Two differences between a self send and a remote one
are kept on purpose because the 0.0.23 source has them: a self send answers with the mailbase entry (`data.envelope.msgid`, `ring`), a remote
one with the envelope (`data.msgid`, `data.delivery`). `X0` compares these against a real `aoide` on a temp root, and a fixture that disagrees
with it is a fixture to fix.

The files are generated once and edited by hand after that; ids are runs of one hex digit so a test can say which letter it means.
