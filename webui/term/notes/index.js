// The notepad, plugged into the page (core/registry.js), so app.js lists none of it: the ✎ tab in the left panel, the `note` pane kind, the
// `:note [title]` command, a few key rows, and the focus name `notepad` (app.js:58 already uses `note` for an ask's note line).
// What N2b (send, cards, hand-off) and M3 (agent mail) add lives in the four modules imported below; each registers itself.
import { leftTabs, paneKinds, commands, keyRows, whereOwners, focusOwners, host } from '../core/registry.js';
import { NotesTab } from './tab.js';
import { NotePane, paneTitle, paneStatus, paneClose } from './pane.js';
import { newNote, draftFor } from './store.js';
import { badge } from './cards.js';
import './send.js';
import './handoff.js';
import './agentmail.js';

leftTabs.add({ name: 'notes', icon: '✎', component: NotesTab, badge });

paneKinds.add({ kind: 'note', Pane: NotePane, title: paneTitle, status: paneStatus, close: paneClose });

commands.add({ name: 'note', help: 'a new note, opened in a pane: :note [title]', run: (arg) => { newNote(arg); } });

// Inside a note pane the status line says `notepad`, and an ask that arrives never takes focus from the box.
const inPad = (el) => !!el && typeof el.closest === 'function' && !!el.closest('.notepad');
whereOwners.add((el) => (inPad(el) ? 'notepad' : null));
focusOwners.add(inPad);

// The focused note's draft, for the key rows below.
const focusedDraft = () => { const x = host.focused && host.focused(); return x && x.kind === 'note' ? draftFor(x.id) : null; };
const KEYS = 'notes';
keyRows.add({ section: KEYS, keys: 'i ⏎', what: 'write in the focused note pane (esc leaves the box)', run: () => { const el = document.querySelector('.notepad.focus textarea'); if (el) el.focus(); } });
keyRows.add({ section: KEYS, keys: '^s', what: 'save the note now (it saves by itself 0.8 s after the last key)', run: () => { const d = focusedDraft(); if (d) d.flush(); else if (host.toast) host.toast('no note pane is focused'); } });
