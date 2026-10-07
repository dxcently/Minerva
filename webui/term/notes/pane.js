// The note pane (kind 'note', registered in index.js): a frame with the note's name in the top border, a plain box to write in, and in the
// bottom border the verbs `[ send to node ] [ rename ] [ trash ]` and the save state. All the saving is store.js's Draft; this only draws it
// and hands it the text. The pane is a leaf of the page's tile tree like a chat (core/registry.js paneKinds).
import { html, K, signal, useRef, useLayoutEffect, useEffect, useState } from '../core/ui.js';
import { paneStyle } from '../core/registry.js';
import { under } from '../core/menu.js';
import { draftOf, draftFor, titleOf, bytesOf, renameNote, trashNote, settled, WORD } from './store.js';
import { sendOff, sendNote } from './send.js';

// ---------------------------------------------------------------- the clock that ages `saved · 2s`
const now = signal(Date.now());
let holders = 0, tick = 0;
// Counts while a save state is on screen: one timer for all the panes, none when there are none.
function hold() {
  if (!holders++) { now.value = Date.now(); tick = setInterval(() => { now.value = Date.now(); }, 1000); }
  return () => { if (!--holders) { clearInterval(tick); tick = 0; } };
}
// 'now', 2s, 4m, 3h, 5d.
export function ago(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  return s < 1 ? 'now' : s < 60 ? s + 's' : s < 3600 ? Math.floor(s / 60) + 'm' : s < 86400 ? Math.floor(s / 3600) + 'h' : Math.floor(s / 86400) + 'd';
}

// ---------------------------------------------------------------- what a kind says about its panes (core/registry.js paneKinds)
export const paneTitle = (p) => { const d = draftFor(p.id); return 'note ' + (d ? d.name.value : p.name); };
// The status line while the pane has focus: the name and the save state.
export const paneStatus = (p) => { const d = draftFor(p.id); return d ? `note ${d.name.value} · ${WORD[d.state.value]}` : 'note'; };
// The pane leaves the tiles: whatever is unsaved goes to the perch first.
export const paneClose = (p) => { const d = draftFor(p.id); if (d) d.close(); };

// ---------------------------------------------------------------- pieces
// Split and close, cut into the top border on the right (the same two glyphs a chat pane has: panes/session.js Glyphs).
function Glyphs({ p, act }) {
  const g = (ch, title, owner, on) => html`<span class="k glyph" role="button" tabindex="0" title=${title} data-owner=${owner}
    onClick=${(e) => { e.stopPropagation(); on(e.currentTarget); }}
    onKeyDown=${(e) => { if (!e.repeat && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); e.stopPropagation(); on(e.currentTarget); } }}>${ch}</span>`;
  return html`<div class="fglyphs">
    ${g('+', 'split right or down', `split-${p.id}`, (el) => act.splitMenu(p, under(el)))}
    ${g('×', 'close the pane (⌥w  :q)', `close-${p.id}`, () => act.close(p.id))}
  </div>`;
}

// The title in the top border becomes a field: the file gets the name the text makes (the perch slugs it). Enter does it, Esc or leaving cancels.
function RenameBox({ text, done }) {
  const ref = useRef(null), over = useRef(false);
  useLayoutEffect(() => { const el = ref.current; if (el) { el.value = text; el.focus(); el.select(); } }, []);
  const end = (commit) => {
    if (over.current) return;
    over.current = true;
    done(commit && ref.current ? ref.current.value : null);
  };
  return html`<input class="ren" type="text" ref=${ref} spellcheck="false" autocomplete="off" aria-label="the file name comes from this" maxlength="120"
    onKeyDown=${(e) => { e.stopPropagation(); if (e.key === 'Enter') { e.preventDefault(); end(true); } else if (e.key === 'Escape') { e.preventDefault(); end(false); } }}
    onBlur=${() => end(false)} />`;
}

// The save state: what the perch has, in a few words, with the way out of a conflict (or of a vanished note) where the words are.
function SaveState({ d }) {
  useEffect(hold, []);
  const s = d.state.value, why = d.why.value;
  const act = (label, title, on) => html`<${K} cls="act" title=${title} on=${on}>${label}</${K}>`;
  if (s === 'saved') return html`<span class="faint" title="the perch has all of it">saved · ${ago(now.value - d.savedAt.value)}</span>`;
  if (s === 'saving') return html`<span class="faint">saving…</span>`;
  if (s === 'dirty') return html`<span class="faint" title="it is saved 0.8 s after the last key">unsaved</span>`;
  if (s === 'conflict') {
    return html`<span class="r">conflict: </span>${act('[ reload ]', "take the perch's copy and drop what you typed", () => d.reload())} ${act('[ keep mine ]', "save what is here over the perch's copy", () => d.keepMine())}`;
  }
  if (s === 'failed') return html`<span class="r" title=${why}>not saved</span>`;
  if (s === 'gone') return html`<span class="r">gone: </span>${act('[ save as new note ]', 'keep this text as a note of its own', () => d.rescue())}`;
  if (s === 'error') return html`<span class="r" title=${why}>unreadable: </span>${act('[ retry ]', 'read it again', () => { d.started = false; d.start(); })}`;
  return html`<span class="faint">loading…</span>`;
}

// A sentence above the box when the state needs one. The actions are in the bottom border, where the state is.
function Notice({ d }) {
  const s = d.state.value, why = d.why.value;
  if (s === 'conflict') {
    return html`<div class="banner err">${why}. [ reload ] takes the perch's copy and drops what you typed since; [ keep mine ] saves yours over it. Nothing is sent until you choose.</div>`;
  }
  if (s === 'failed') return html`<div class="banner err">not saved: ${why}. What you typed is kept in this browser${d.retry ? '; trying again' : ''}.</div>`;
  if (s === 'gone') return html`<div class="banner err">${why}. What you typed is kept here until you save it as a new note or close this.</div>`;
  if (s === 'error') return html`<div class="banner err">${why}</div>`;
  return null;
}

// Whether the box may take focus by itself after it is read: not from under a field the person is typing in elsewhere.
const takesFocus = (el) => {
  const at = document.activeElement;
  return !at || at === document.body || at === el || !(at.tagName === 'TEXTAREA' || at.tagName === 'INPUT');
};

// ---------------------------------------------------------------- the pane
export function NotePane({ p, rect, focused, act }) {
  const d = draftOf(p);
  const box = useRef(null), root = useRef(null);
  const [renaming, setRenaming] = useState(false);
  useEffect(() => { d.start(); }, [d]);
  // The box holds what the person types; the draft hears every key. A read from the perch (the first, a reload) is put in the box here.
  const rev = d.rev.value;
  useLayoutEffect(() => {
    const el = box.current;
    if (!el || !rev) return;
    el.value = d.text;
    const first = !d.shown;
    d.shown = true;
    const at = first && !p.fresh ? 0 : el.value.length;
    el.setSelectionRange(at, at);
    if (first && focused && takesFocus(el)) { el.focus({ preventScroll: true }); if (p.fresh) el.scrollTop = el.scrollHeight; }
  }, [rev]);
  const s = d.state.value, name = d.name.value, title = d.title.value;
  const writeIn = () => { if (box.current) box.current.focus(); };
  const key = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); e.currentTarget.blur(); if (root.current) root.current.focus(); }
    else if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 's') { e.preventDefault(); d.flush(); }
  };
  // The pane itself has focus (Esc left the box): i or Enter goes back in. app.js ignores both for a pane that is not a chat.
  const paneKey = (e) => {
    if (e.target !== e.currentTarget || e.repeat || e.ctrlKey || e.altKey || e.metaKey) return;
    if (e.key === 'i' || e.key === 'Enter') { e.preventDefault(); writeIn(); }
  };
  const send = async (e) => {
    const at = under(e.currentTarget);
    if (!(await settled(d.name.peek()))) return;
    sendNote({ name: d.name.peek(), title: titleOf(d.text), size: bytesOf(d.text) }, at);
  };
  const renamed = async (text) => {
    setRenaming(false);
    writeIn();
    const t = text == null ? '' : text.trim();
    if (!t) return;
    const before = d.name.peek(), r = await renameNote(before, t);
    act.toast(r.ok ? (r.name === before ? `the file is already called ${r.name}` : `renamed to ${r.name}`) : r.why, !r.ok);
  };
  const trash = async () => {
    const before = d.name.peek(), r = await trashNote(before);
    act.toast(r.ok ? `${before} is in the trash: restore it from the ✎ tab` : r.why, !r.ok);
  };
  const bad = s === 'conflict' || s === 'gone' || s === 'error';
  const reason = sendOff({ name, title, size: 0 });
  return html`<section class=${'pane frame notepad' + (focused ? ' focus' : '')} style=${paneStyle(rect)} tabindex="-1" ref=${root}
      data-pane=${p.id} data-kind=${p.kind} onMouseDown=${() => act.focus(p.id)} onKeyDown=${paneKey}>
    <div class="ftitle nt">${renaming
      ? html`✎ <${RenameBox} text=${title} done=${renamed} /><span class="faint"> ⏎ rename · esc cancel</span>`
      : html`✎ <span class="b" title=${name + '.md'}>${name}</span>${title && title !== name && html` <span class="faint">· ${title}</span>`}`}</div>
    <${Glyphs} p=${p} act=${act} />
    <${Notice} d=${d} />
    <textarea class="np" ref=${box} spellcheck="false" aria-label=${'note ' + name} readOnly=${s === 'loading' || s === 'error'}
      placeholder=${s === 'loading' ? 'loading…' : 'write here: it saves by itself'}
      onInput=${(e) => d.edit(e.currentTarget.value)} onKeyDown=${key} onFocus=${() => act.focus(p.id)} onBlur=${() => { d.keep(); d.flush(); }}></textarea>
    <div class=${'np-bar' + (bad ? ' bad' : '')}>
      <span class="np-verbs"><${K} cls="act" off=${reason} title=${reason || 'send this note to another node over Aoide mail'} on=${send}>[ send to node ]</${K}>
        <${K} cls="act" title="name the file after a title" on=${() => setRenaming(true)}>[ rename ]</${K}>
        <${K} cls="act" title="move it to the trash: it can be restored" on=${trash}>[ trash ]</${K}></span>
      <span class="np-state"><${SaveState} d=${d} /></span>
    </div>
  </section>`;
}
