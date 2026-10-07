# webui/term/notes

The notepad: the left `✎` tab, the `note` pane, `:note [title]`. A note is a plain text file the perch keeps under its `--home` (`perch/src/notes/README.md`); the page reads and writes it through `/hub/notes`, and saves by itself. Nothing in `app.js` knows about it: `index.js` plugs in through `core/registry.js`.

| file | does |
|---|---|
| `index.js` | registers the tab, the pane kind, `:note`, the `notepad` focus name and a few key rows; imports the four modules below |
| `store.js` | all the state and every call to the perch: `Draft` (one open note and its autosave), the lists, create, rename, trash, restore, empty, `openNote`, `newNote`. No DOM at import, so node tests it |
| `tab.js` | the tab: the list (newest first, a title for each), `[ + new note ]`, a row menu, the folded trash, `off` and `needs the hub` states |
| `pane.js` | the pane: a text box with the file name and save state cut into its borders, the conflict and failure banners, `[ rename ]` `[ send to node ]` `[ trash ]` |
| `notes.css` | the tab's and the pane's styles, linked from `index.html` |
| `cards.js` | stub for N2b: received cards, and the rail badge |
| `send.js` | stub for N2b: sending a note to a node, and the tab's sent rows |
| `handoff.js` | stub for N2b: the chat-menu line that drafts a hand-off note |
| `agentmail.js` | stub for M3: the loaded tab's `agent mail` section |

## What the page does

```
 typing --0.8 s after the last key--> POST /hub/notes/<n> { text, base }
                                         200 { sha }      saved; the next save names this sha
                                         409 { sha }      conflict: nothing more is sent;  [ reload ] takes theirs, [ keep mine ] writes over it
                                         404              gone: [ save as new note ] keeps the text
                                         413 | 400 | 401   said in words, not retried
                                         no answer        not saved: retried after 3, 6, 12, 30 s (a key tries sooner)
```

- A save names the sha the note was read at (`base`). Two tabs on one note become a visible conflict from the 409, never a silent overwrite.
- One save is in the air at a time. A key typed meanwhile marks the box unsaved; it goes out when the first lands, on the sha that came back.
- Text is never dropped. From the moment a save starts until one lands the text is also in `localStorage` (`minerva.note.<name>`). A pane that closes, a hidden tab and a closing page send what is unsaved first. A stash an earlier session left comes back: on the same sha it saves itself, on a note that moved it is a conflict. Stashes older than 30 days are ignored.
- A note is 256 KiB (UTF-8 bytes) at most; the page refuses more itself.
- A rename or a trash saves the box first and writes nothing to the old name while the file moves; the open box follows the new name, and a trashed note's pane closes. `[ rename ]` changes the file name only (the perch derives it from the title); the text is never touched.
- One note, one pane: opening a note that is open focuses its pane.
- A perch started without `--home` answers 404 on every notes route: the tab says notes are off.

## Routes used (all through `core/api.js`)

`GET /hub/notes` and `/hub/notes/trash` · `POST /hub/notes` `{ title?, text? }` · `GET|POST /hub/notes/<n>` · `POST /hub/notes/<n>/rename` `{ title }` · `POST /hub/notes/<n>/trash` · `POST /hub/notes/trash/<n>/restore` · `POST /hub/notes/trash/empty` `{ confirm: true }`. The lists are polled every 10 s through `core/lane.js` while the tab is on screen and the page is visible.

## For N2b and M3 (the seams)

`tab.js` and `pane.js` import exactly these names; fill the files, keep the names.

| module | exports | contract |
|---|---|---|
| `cards.js` | `badge`, `Received` | `badge`: a signal of the unread count, given to the rail icon. `Received`: a component with no props, drawn in the tab under the list, `null` when empty |
| `send.js` | `sendOff(note)`, `sendNote(note, at)`, `Sent` | `sendOff` is `''` when sending works, else the reason `[ send to node ]` is greyed. `sendNote` opens the send menu at the `core/menu.js` anchor `at`; `note` is `{ name, title, size }`. The pane and the tab call `settled(name)` first, so the file is what the box holds. `Sent`: a component with no props for the tab's sent rows |
| `handoff.js` | none | registers itself (`chatMenuItems.add`); wants `createNote({ title, text })` and `openNote(name, { fresh: true })` from `store.js` |
| `agentmail.js` | none | registers itself (`loadedSections.add`) |

From `store.js`: `createNote`, `openNote`, `newNote`, `settled(name)` (resolves true when the perch has everything in the box), `refresh()`, `rows`, `titleOf(text)`, `NAME` (the file-name grammar `^[a-z0-9][a-z0-9-]{0,47}$`), `draftFor(paneId)`.

The status line says `notepad` while focus is inside a note pane (`app.js` already uses `note` for an ask's note line).

Tests: `tests/notes.test.mjs` (the autosave, the conflict path, the stash, titles, `:note`, the registry wiring). What a pane draws is checked in a browser.
