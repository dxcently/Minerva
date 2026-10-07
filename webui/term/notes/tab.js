// The ✎ tab of the left panel: `[ + new note ]`, the notes newest first, the slots N2b fills (received cards, sent rows), and a folded trash.
// It reads the lists in store.js (polled while the tab is on screen) and acts through store.js; nothing is kept here but what is folded.
import { html, K, signal, useEffect, useRef, useLayoutEffect } from '../core/ui.js';
import { mode } from '../core/api.js';
import { known, age } from '../core/hub.js';
import { openMenu, under, atPointer } from '../core/menu.js';
import { host } from '../core/registry.js';
import { rows, trashRows, off, stale, loaded, watch, refresh, newNote, openNote, renameNote, trashNote, restoreNote, emptyTrash, settled } from './store.js';
import { Received } from './cards.js';
import { Sent, sendOff, sendNote } from './send.js';

const SHOWN = 30;                       // notes drawn, then `more…` adds 50
const shown = signal(SHOWN);
const trashOpen = signal(false);
const renaming = signal(null);          // the name of the row being renamed in place
const say = (text, err) => { if (host.toast) host.toast(text, err); };

// The field that renames a row in place. Its text is set once, so the 10 s poll redrawing the row never overwrites what is being typed.
function EditName({ text, save }) {
  const ref = useRef(null), over = useRef(false);
  useLayoutEffect(() => { const el = ref.current; if (el) { el.value = text; el.focus(); el.select(); } }, []);
  const end = (commit) => {
    if (over.current) return;
    over.current = true;
    const value = ref.current ? ref.current.value : '';
    renaming.value = null;
    if (commit) save(value);
  };
  return html`<input class="ren" type="text" ref=${ref} spellcheck="false" autocomplete="off" aria-label="the file name comes from this" maxlength="120"
    onKeyDown=${(e) => { e.stopPropagation(); if (e.key === 'Enter') { e.preventDefault(); end(true); } else if (e.key === 'Escape') { e.preventDefault(); end(false); } }}
    onClick=${(e) => e.stopPropagation()} onBlur=${() => end(false)} />`;
}

async function rename(r, text) {
  const t = String(text || '').trim();
  if (!t) return;
  const out = await renameNote(r.name, t);
  say(out.ok ? (out.name === r.name ? `the file is already called ${out.name}` : `renamed to ${out.name}`) : out.why, !out.ok);
}
async function trash(r) {
  const out = await trashNote(r.name);
  say(out.ok ? `${r.title || r.name} is in the trash below: restore it from there` : out.why, !out.ok);
}
async function restore(r) {
  const out = await restoreNote(r.name);
  say(out.ok ? `${r.title || out.name} is back as ${out.name}` : out.why, !out.ok);
}
function emptyAsk(n, at) {
  openMenu([
    { label: `permanently delete ${n} note${n === 1 ? '' : 's'}? this can't be undone`, off: ' ' },
    { label: 'delete permanently', on: async () => {
      const out = await emptyTrash();
      say(out.ok ? `${out.removed} note${out.removed === 1 ? '' : 's'} deleted for good` : out.why, !out.ok);
    } },
    { label: 'keep them', on: () => {} },
  ], at, { title: 'empty the notes trash' });
}

// One note's menu: the same from a right-click and from the row's `⋯`.
function rowMenu(r, at) {
  const note = { name: r.name, title: r.title, size: r.size };
  openMenu([
    { label: 'open', on: () => openNote(r.name) },
    { label: 'rename…', hint: 'the file name', on: () => { renaming.value = r.name; } },
    { label: 'send to node…', off: sendOff(note), on: async () => { if (await settled(r.name)) sendNote(note, at); } },
    { sep: true },
    { label: 'trash', hint: 'it can be restored', on: () => trash(r) },
  ], at, { title: r.title || r.name });
}

function Row({ r }) {
  if (renaming.value === r.name) {
    return html`<div class="it note ed"><span class="faint">✎</span><${EditName} text=${r.title} save=${(t) => rename(r, t)} /></div>`;
  }
  return html`<div class="it note" onContextMenu=${(e) => { e.preventDefault(); rowMenu(r, atPointer(e)); }}>
    <${K} cls="nrow" title=${r.name + '.md\nright-click or ⋯: rename, send, trash'} on=${() => openNote(r.name)}>
      <span class="nt">${r.title || html`<span class="faint">(empty)</span>`}</span><span class="faint na">${age(r.mtime)}</span></${K}>
    <${K} cls="mo" title="rename, send, trash" on=${(e) => rowMenu(r, under(e.currentTarget))}>⋯</${K}>
  </div>`;
}

function Trash() {
  const t = trashRows.value, n = t.length;
  if (!n) return null;
  return html`<div class="grp trash">
    <div class="gh2"><${K} cls="gname" title=${trashOpen.value ? 'fold' : 'notes you trashed; they can be restored'} on=${() => { trashOpen.value = !trashOpen.value; }}>${trashOpen.value ? '▾' : '▸'} trash (${n})</${K}>
      <${K} cls="plus" title="delete them for good" on=${(e) => emptyAsk(n, under(e.currentTarget))}>[ empty… ]</${K}></div>
    ${trashOpen.value && html`<div class="list">${t.map((r) => html`<div class="it note" key=${r.name}>
      <span class="nrow faint"><span class="nt" title=${r.name}>${r.title || '(empty)'}</span><span class="na">${age(r.mtime)}</span></span>
      <${K} cls="mo on" title="put it back in the list" on=${() => restore(r)}>[ restore ]</${K}></div>`)}</div>`}</div>`;
}

export function NotesTab() {
  useEffect(() => watch(), []);
  known.value; // redraw once the mode is known
  const rs = rows.value;
  if (mode() !== 'perch') return html`<div class="notes"><div class="needs">notes need the hub: open Minerva with hoot</div></div>`;
  if (off.value) {
    return html`<div class="notes"><div class="needs">${off.value}</div>
      <div class="newrow"><${K} cls="act" on=${() => refresh()}>[ check again ]</${K}></div></div>`;
  }
  const n = shown.value, more = rs.length - n;
  return html`<div class="notes">
    <div class="newrow"><${K} cls="newnote" title="a new note, opened in a pane (:note [title] does the same from anywhere)" on=${() => newNote()}>[ + new note ]</${K}></div>
    ${stale.value && html`<div class="faint">list stale</div>`}
    ${rs.length ? html`<div class="list">${rs.slice(0, n).map((r) => html`<${Row} key=${r.name} r=${r} />`)}
        ${more > 0 && html`<div class="it"><${K} cls="faint" on=${() => { shown.value = n + 50; }}>more… (${more})</${K}></div>`}</div>`
      : html`<div class="faint pad">${loaded.value ? 'no notes yet' : 'reading…'}</div>`}
    <${Received} />
    <${Sent} />
    <${Trash} />
  </div>`;
}
