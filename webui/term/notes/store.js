// The notepad's state and its calls to the perch's /hub/notes (perch/src/notes/README.md). No view and no DOM at import: node tests it
// (tests/notes.test.mjs), and tab.js, pane.js and the stubs N2b and M3 fill all go through here.
//
//   Draft            one open note: the text in its box, the sha it was read at, and the autosave. A pane makes one (draftOf) and closes it
//   titleOf(text)    the first non-empty line, as the perch's listing names it (tab.js draws the live title of an open note with it)
//   createNote / renameNote / trashNote / restoreNote / emptyTrash    one call each, answering { ok, ... } or { ok: false, status, why }
//   openNote(name)   the note's pane: focuses the one that is open, else a new pane beside the focused one
//   newNote(title)   `:note [title]` and `[ + new note ]`: make it, open it, say so
//   rows, trashRows, off, stale, refresh(), watch()    the lists the tab draws
//
// The autosave is the point of this file. A save names the sha it was read at (`base`); the perch answers 409 with the sha now when the file
// moved on, so two tabs on one note become a visible conflict, never a silent overwrite. Text is never dropped: whatever has not reached
// the perch is also in localStorage (when it will take it) from the moment a save starts until one lands.
import { signal, batch } from '../core/ui.js';
import { hubGet, hubPost, mode } from '../core/api.js';
import { hubPoll } from '../core/lane.js';
import { host } from '../core/registry.js';

export const SAVE_MS = 800;                 // after the last key
export const MAX_BYTES = 256 * 1024;        // a note, as UTF-8 bytes (the perch answers 413 past it)
export const NAME = /^[a-z0-9][a-z0-9-]{0,47}$/;   // a note's file name (not `trash`: that is a route, and the perch never makes it)
export const POLL_MS = 10000;
export const STASH = 'minerva.note.';       // + the note's name: { text, base, at }
const STASH_DAYS = 30;
const RETRY_MS = [3000, 6000, 12000, 30000]; // after a save that could not reach the perch
export const OFF = 'notes are off: this server was started without a home folder (--home)';
const LOGIN = 'the server refused the login for this page (it restarted?). Run `hoot` again.';

const enc = new TextEncoder();
export const bytesOf = (text) => enc.encode(String(text)).length;
const nl = (s) => String(s).replace(/\r\n?/g, '\n');   // a box hands back LF; a file from elsewhere may be CRLF
const same = (a, b) => a === b || nl(a) === nl(b);

// perch/src/notes/mod.rs title_of: the first non-empty line, control characters dropped (not replaced), 120 characters at most.
// Read line by line from the front, so a 256 KiB note costs what its first lines cost (it runs on every key).
export function titleOf(text) {
  const s = String(text);
  for (let i = 0; i <= s.length;) {
    let j = s.indexOf('\n', i);
    if (j < 0) j = s.length;
    const clean = s.slice(i, j).replace(/\p{Cc}/gu, '').trim();
    if (clean) return [...clean].slice(0, 120).join('').trimEnd();
    i = j + 1;
  }
  return '';
}

// What a refusal says, in words. `gone` is what a 404 means for the call (a list or a make: the server has no notes; a named note: it is not there).
export function whyOf(status, data, gone = OFF) {
  if (status === 0) return 'the server is not answering';
  if (status === 401) return LOGIN;
  if (status === 404) return gone;
  if (status === 413) return 'a note is 256 KiB at most';
  const said = data && typeof data.error === 'string' ? data.error : '';
  return said || `the server answered ${status}`;
}
const fail = (r, gone) => ({ ok: false, status: r.status, why: whyOf(r.status, r.data, gone) });
const GONE = 'that note is gone: it was renamed or trashed somewhere else';
const say = (text, err, act) => { if (host.toast) host.toast(text, err, act); };

// ---------------------------------------------------------------- a note's text, kept
const browserStorage = () => { try { return globalThis.localStorage || null; } catch { return null; } };

// The word the status line and the pane's bottom border use for each state.
export const WORD = { loading: 'loading…', saved: 'saved', dirty: 'unsaved', saving: 'saving…', conflict: 'conflict', failed: 'not saved', gone: 'gone', error: 'unreadable' };

// One open note. States: loading, saved, dirty (typed, a save is waiting), saving (one POST in flight), conflict (409: nothing is sent until
// the person picks), failed (the POST did not land: the text stays, and is tried again unless the perch refused it), gone (404 on a save),
// error (the note could not be read). `text` is what the box holds; `saved` what the perch has; `base` the sha the next save names.
//
// Injected for tests: `net` ({ get(name), save(name, text, base) } -> { status, data }), `clock` ({ now, set, clear }), `storage`
// (localStorage's shape, or null), `say` (a toast).
export class Draft {
  constructor(name, o = {}) {
    this.name = signal(name);
    this.paneId = o.paneId || null;
    this.net = o.net || { get: (n) => hubGet('notes/' + n), save: (n, text, base) => hubPost('notes/' + n, { text, base }) };
    this.clock = o.clock || { now: () => Date.now(), set: (f, ms) => setTimeout(f, ms), clear: (t) => clearTimeout(t) };
    this.storage = o.storage === undefined ? browserStorage() : o.storage;
    this.say = o.say || say;
    this.state = signal('loading');
    this.why = signal('');
    this.retry = false;             // a failed save is tried again by itself (the pane says so)
    this.title = signal('');        // the first line of the box, kept as it is typed
    this.savedAt = signal(0);       // ms: the last save, or the file's own time when it was read
    this.rev = signal(0);           // counts the times the text is set from outside the box (a read, a reload): the box re-reads it
    this.text = '';
    this.saved = '';
    this.base = '';
    this.theirs = '';               // the perch's sha when a save was refused with 409
    this.timer = 0;
    this.keyAt = 0;
    this.inflight = null;
    this.paused = false;            // exclusive(): a rename or a trash is under way, nothing is written to the old name
    this.owed = false;
    this.retries = 0;
    this.started = false;
    this.closed = false;
    this.shown = false;             // the pane has put the first read into its box
    drafts.add(this);
    hook();
  }

  dirty() { return !same(this.text, this.saved); }
  // Whether the perch has everything in the box.
  clean() { const s = this.state.peek(); return (s === 'saved' || s === 'dirty' || s === 'saving' || s === 'failed') && !this.dirty(); }
  set(state, why = '') { batch(() => { this.state.value = state; this.why.value = why; }); }
  get path() { return 'notes/' + this.name.peek(); }

  // ---- the box
  edit(text) {
    if (this.closed) return;
    this.text = text;
    this.title.value = titleOf(text);
    const s = this.state.peek();
    if (s === 'loading' || s === 'gone' || s === 'error' || s === 'conflict') return;  // nothing is sent until the person picks a way out
    this.keyAt = this.clock.now();
    if (s === 'saving') return;                       // when it lands, what was typed meanwhile is armed
    if (!this.dirty()) { this.disarm(); this.set('saved'); return; }   // typed back to what is saved
    if (s !== 'failed') this.set('dirty');
    this.arm(SAVE_MS);
  }
  arm(ms) { this.disarm(); this.timer = this.clock.set(() => { this.timer = 0; this.flush(); }, ms); }
  disarm() { if (this.timer) { this.clock.clear(this.timer); this.timer = 0; } }
  // The wait that is left of the debounce: 800 ms after the last key, counted from the key and not from now.
  rest() { return Math.max(0, this.keyAt + SAVE_MS - this.clock.now()); }

  // ---- saving
  // Everything typed so far to the perch, now (blur, close, a hidden tab, ctrl+s, before a rename). Waits for a save in flight, then
  // sends the box once more if it still differs. true: that save landed, or there was nothing to save.
  async flush() {
    this.disarm();
    if (this.paused) { this.owed = true; return false; }
    while (this.inflight) await this.inflight;
    const s = this.state.peek();
    if (s === 'loading' || s === 'gone' || s === 'error' || s === 'conflict') return !this.dirty();
    if (!this.dirty()) { if (s !== 'saved') this.set('saved'); return true; }
    return this.write();
  }

  // One POST. Never two at once: flush() waits for `inflight`, and a key typed meanwhile only marks the box dirty.
  write() {
    const text = this.text, base = this.base, name = this.name.peek(), bytes = bytesOf(text);
    if (bytes > MAX_BYTES) return Promise.resolve(this.refuse(`too big: ${bytes} bytes, and a note is ${MAX_BYTES} (256 KiB) at most`, false));
    this.set('saving');
    this.stash(text, base);
    const run = Promise.resolve().then(() => this.net.save(name, text, base)).catch(() => ({ status: 0, data: null }))
      .then((r) => { this.inflight = null; return this.landed(text, r); });
    this.inflight = run;
    return run;
  }

  landed(sent, r) {
    const st = r.status, data = r.data || {};
    if (st === 200 && typeof data.sha === 'string') {
      this.saved = sent;
      this.base = data.sha;
      this.retries = 0;
      this.retry = false;
      this.savedAt.value = this.clock.now();
      touched(this.name.peek(), sent, this.savedAt.peek());
      if (this.dirty()) {                              // typed while it was in flight
        this.set('dirty');
        if (!this.closed && !this.paused) this.arm(this.rest()); else this.owed = true;
        this.stash(this.text, this.base);
      } else { this.set('saved'); this.unstash(); }
      return true;
    }
    if (st === 409 && typeof data.sha === 'string') {
      this.theirs = data.sha;
      this.stash(this.text, this.base);
      this.set('conflict', 'this note changed somewhere else since you opened it');
      return false;
    }
    if (st === 404) { this.stash(this.text, this.base); this.set('gone', GONE); return false; }
    if (st === 413) return this.refuse('too big: a note is 256 KiB at most', false);
    if (st === 400) return this.refuse(data.error || 'the server will not take that text', false);
    if (st === 401) return this.refuse(LOGIN, false);
    return this.refuse(st === 0 ? 'the server is not answering' : data.error || `the server answered ${st}`, true);
  }

  // A save that did not land. The text stays in the box and in the stash; `retry` tries again with a growing wait (a typed key tries sooner).
  refuse(why, retry) {
    this.stash(this.text, this.base);
    this.retry = retry && !this.closed;
    this.set('failed', why);
    if (retry && !this.closed) this.arm(RETRY_MS[Math.min(this.retries++, RETRY_MS.length - 1)]);
    return false;
  }

  // The conflict's two ways out. keepMine: the next save names the perch's sha, so it writes over theirs. reload: theirs replaces the box.
  async keepMine() {
    if (this.state.peek() !== 'conflict') return false;
    this.base = this.theirs;
    this.set('dirty');
    return this.flush();
  }
  async reload() {
    const r = await this.read();
    if (r.status === 200 && r.data && typeof r.data.text === 'string' && typeof r.data.sha === 'string') {
      this.disarm();
      this.take(r.data);
      this.set('saved');
      this.unstash();
      return true;
    }
    if (r.status === 404) { this.set('gone', GONE); return false; }
    this.say(`could not reload: ${whyOf(r.status, r.data, GONE)}`, true);
    return false;
  }
  // The perch's copy becomes the box's.
  take(data) {
    this.text = this.saved = data.text;
    this.title.value = titleOf(data.text);
    this.base = data.sha;
    this.theirs = '';
    this.savedAt.value = data.mtime ? data.mtime * 1000 : this.clock.now();
    this.rev.value++;
  }
  async read() {
    try { return await this.net.get(this.name.peek()); } catch { return { status: 0, data: null }; }
  }

  // A gone note's text, saved as a new note under a fresh name; the pane follows it.
  async rescue() {
    if (this.state.peek() !== 'gone') return false;
    const r = await createNote({ text: this.text });
    if (!r.ok) { this.say(`could not save it as a new note: ${r.why}`, true); return false; }
    this.unstash();
    this.name.value = r.name;
    this.saved = this.text;
    this.base = r.sha;
    this.savedAt.value = this.clock.now();
    this.set('saved');
    this.say(`saved as a new note: ${r.name}`);
    refresh();
    return true;
  }

  // ---- reading
  start() {
    if (this.started) return Promise.resolve();
    this.started = true;
    return this.load();
  }
  async load() {
    if (!NAME.test(this.name.peek())) { this.set('error', 'that is not a note name'); return; }
    this.set('loading');
    const r = await this.read();
    if (this.closed) return;
    if (r.status === 200 && r.data && typeof r.data.text === 'string' && typeof r.data.sha === 'string') {
      this.take(r.data);
      const kept = this.unkept();
      if (kept && !same(kept.text, r.data.text)) {
        this.text = kept.text;
        this.title.value = titleOf(kept.text);
        this.rev.value++;
        if (kept.base === r.data.sha) {                // the perch's copy is the one the lost text was typed on: it just was not sent
          this.set('dirty');
          this.arm(0);
          this.say(`${this.name.peek()}: text that was not saved last time is back; saving it now`);
        } else {                                       // the perch's copy moved on meanwhile: the two ways out are the conflict's
          this.theirs = r.data.sha;
          this.set('conflict', 'text from an earlier session is not the perch\'s copy now');
        }
        return;
      }
      this.unstash();
      this.set('saved');
      return;
    }
    if (r.status === 404) this.set('gone', GONE);
    else if (r.status === 422) this.set('error', 'that file is not plain UTF-8 text');
    else this.set('error', `could not read this note: ${whyOf(r.status, r.data, GONE)}`);
  }

  // ---- the stash: localStorage, when it will take the text
  stash(text, base) {
    if (!this.storage) return;
    try { this.storage.setItem(STASH + this.name.peek(), JSON.stringify({ text, base, at: this.clock.now() })); } catch { /* full, or private mode: the text is still in the box */ }
  }
  unstash() {
    if (!this.storage) return;
    try { this.storage.removeItem(STASH + this.name.peek()); } catch { /* nothing to remove it from */ }
  }
  // What an earlier session left: { text, base }, or null (nothing, unreadable, or older than STASH_DAYS).
  unkept() {
    if (!this.storage) return null;
    try {
      const v = JSON.parse(this.storage.getItem(STASH + this.name.peek()));
      if (!v || typeof v.text !== 'string' || typeof v.base !== 'string') return null;
      if (this.clock.now() - (Number(v.at) || 0) > STASH_DAYS * 864e5) { this.unstash(); return null; }
      return v;
    } catch { return null; }
  }
  // The box is about to go out of sight: whatever the perch does not have is in the stash first.
  keep() {
    const s = this.state.peek();
    if (this.dirty() || s === 'failed' || s === 'conflict' || s === 'gone') this.stash(this.text, this.base);
  }

  // ---- a rename or a trash is a change of the file, not of its text: nothing is written while it runs, the box is saved first
  async exclusive(fn) {
    if (this.paused) return { ok: false, why: 'that note is being changed already' };
    const ok = await this.flush();
    if (!ok && this.dirty()) {
      const s = this.state.peek();
      return { ok: false, why: `${s === 'conflict' ? 'settle the conflict' : 'save the note'} first (${this.why.peek() || WORD[s]})` };
    }
    this.paused = true;
    this.disarm();
    try { return await fn(); } finally {
      this.paused = false;
      const s = this.state.peek();
      if ((this.owed || this.dirty()) && s !== 'conflict' && s !== 'gone' && s !== 'error' && s !== 'loading') {
        this.owed = false;
        if (this.dirty()) { if (s !== 'failed') this.set('dirty'); this.arm(0); }
      }
    }
  }
  // The file has another name; the stash goes with it.
  setName(name) {
    const old = this.name.peek();
    if (old === name) return;
    let kept = null;
    if (this.storage) { try { kept = this.storage.getItem(STASH + old); this.storage.removeItem(STASH + old); } catch { /* nothing to move */ } }
    this.name.value = name;
    if (kept && this.storage) { try { this.storage.setItem(STASH + name, kept); } catch { /* full */ } }
  }

  // ---- the end
  // The pane left the tiles: save what is unsaved, and say so when that did not work (the text is in the stash then).
  close() {
    if (this.closed) return this.closing;
    this.closed = true;
    this.disarm();
    this.keep();
    this.closing = this.flush().then((ok) => {
      if (!ok && this.dirty()) this.say(`${this.name.peek()} was not saved (${this.why.peek() || WORD[this.state.peek()]}); its text is kept in this browser`, true);
      this.forget();
    });
    return this.closing;
  }
  // The note is trashed: nothing more to save.
  drop() {
    this.closed = true;
    this.disarm();
    this.unstash();
    this.forget();
  }
  forget() { drafts.delete(this); if (this.paneId) byPane.delete(this.paneId); }
}

// ---------------------------------------------------------------- every open note
export const drafts = new Set();
const byPane = new Map();
export const draftOf = (p) => {
  let d = byPane.get(p.id);
  if (!d) { d = new Draft(p.name, { paneId: p.id }); byPane.set(p.id, d); }
  return d;
};
export const draftFor = (id) => byPane.get(id) || null;
const draftNamed = (name) => [...drafts].find((d) => d.name.peek() === name && !d.closed) || null;

// A hidden tab or a closing page: stash, then send. Registered once, at the first Draft.
export function flushAll() { for (const d of drafts) { d.keep(); d.flush(); } }
let hooked = false;
function hook() {
  if (hooked || typeof document === 'undefined') return;
  hooked = true;
  document.addEventListener('visibilitychange', () => { if (document.visibilityState === 'hidden') flushAll(); });
  addEventListener('pagehide', flushAll);
}

// ---------------------------------------------------------------- the lists
export const rows = signal([]);        // [{ name, title, size, mtime }], newest first
export const trashRows = signal([]);
export const off = signal('');         // why there are no notes, or ''
export const stale = signal(false);    // the last poll failed; the lists are the last good ones
export const loaded = signal(false);
const rowOk = (r) => !!r && typeof r.name === 'string' && NAME.test(r.name);
const clean = (r) => ({ name: r.name, title: typeof r.title === 'string' ? r.title : '', size: Number(r.size) || 0, mtime: Number(r.mtime) || 0 });

// A save landed: the listing's row for that note follows at once (newest first), instead of waiting for the next poll.
function touched(name, text, ms) {
  if (!loaded.peek() || !NAME.test(name)) return;
  const row = { name, title: titleOf(text), size: bytesOf(text), mtime: Math.floor(ms / 1000) };
  rows.value = [row, ...rows.peek().filter((r) => r.name !== name)];
}

let busy = null;
// `lane`: a periodic poll (waits its turn behind the page's other polls); a click's refresh does not.
export function refresh(lane = false) {
  if (mode() !== 'perch') return Promise.resolve();
  if (busy) return busy;
  const get = lane ? (p) => hubPoll(p) : (p) => hubGet(p);
  busy = (async () => {
    const a = await get('notes');
    if (a.status === 200 && Array.isArray(a.data)) { batch(() => { rows.value = a.data.filter(rowOk).map(clean); off.value = ''; stale.value = false; loaded.value = true; }); }
    else if (a.status === 404) { batch(() => { off.value = OFF; rows.value = []; trashRows.value = []; loaded.value = true; }); return; }
    else { stale.value = true; return; }
    const t = await get('notes/trash');
    if (t.status === 200 && Array.isArray(t.data)) trashRows.value = t.data.filter(rowOk).map(clean);
  })().finally(() => { busy = null; });
  return busy;
}

let watchers = 0, ticker = 0;
// The tab is on screen: read the lists now and every POLL_MS while the page is visible. The returned function stops it.
export function watch() {
  watchers++;
  refresh(true);
  if (watchers === 1 && typeof document !== 'undefined') {
    ticker = setInterval(() => { if (document.visibilityState === 'visible' && !off.peek()) refresh(true); }, POLL_MS);
  }
  return () => { if (--watchers === 0) { clearInterval(ticker); ticker = 0; } };
}

// ---------------------------------------------------------------- one call each
// A name is checked against the grammar before it is joined to a path (the perch checks again).
const badName = { ok: false, status: 0, why: 'that is not a note name' };

export async function createNote({ title = '', text = '' } = {}) {
  const body = {};
  if (title) body.title = title;
  if (text) body.text = text;
  const r = await hubPost('notes', body);
  if (r.status === 201 && r.data && typeof r.data.name === 'string' && NAME.test(r.data.name)) {
    return { ok: true, name: r.data.name, sha: String(r.data.sha || ''), mtime: Number(r.data.mtime) || 0 };
  }
  return fail(r);
}

// The file gets the name the title makes. An open note is saved first and writes nothing while it moves; its box follows the new name.
export async function renameNote(name, title) {
  if (!NAME.test(name)) return badName;
  const d = draftNamed(name);
  const call = async () => {
    const r = await hubPost(`notes/${name}/rename`, { title });
    if (r.status === 200 && r.data && typeof r.data.name === 'string' && NAME.test(r.data.name)) {
      if (d) d.setName(r.data.name);
      return { ok: true, name: r.data.name };
    }
    return fail(r, GONE);
  };
  const out = d ? await d.exclusive(call) : await call();
  refresh();
  return out;
}

// To the trash (a rename the perch makes; `restoreNote` undoes it). An open note is saved first, and its pane closes.
export async function trashNote(name) {
  if (!NAME.test(name)) return badName;
  const d = draftNamed(name);
  const call = async () => {
    const r = await hubPost(`notes/${name}/trash`);
    return r.status === 204 ? { ok: true } : fail(r, GONE);
  };
  const out = d ? await d.exclusive(call) : await call();
  if (out.ok && d) { const id = d.paneId; d.drop(); if (id && host.closePane) host.closePane(id); }
  refresh();
  return out;
}

// `name` is the trash's own name for the note (a second note of one name waits there as `-2`).
export async function restoreNote(name) {
  if (!NAME.test(name)) return badName;
  const r = await hubPost(`notes/trash/${name}/restore`);
  const out = r.status === 200 && r.data && typeof r.data.name === 'string' ? { ok: true, name: r.data.name } : fail(r, 'that note is not in the trash any more');
  refresh();
  return out;
}

export async function emptyTrash() {
  const r = await hubPost('notes/trash/empty', { confirm: true });
  const out = r.status === 200 && r.data ? { ok: true, removed: Number(r.data.removed) || 0 } : fail(r);
  refresh();
  return out;
}

// Before a note leaves the page as mail: what is in the box must be what the perch reads from the file.
export async function settled(name) {
  const d = draftNamed(name);
  if (!d) return true;
  const ok = await d.flush();
  if (ok && d.clean()) return true;
  say(`${name} is not saved yet (${d.why.peek() || WORD[d.state.peek()]}): fix that first`, true);
  return false;
}

// ---------------------------------------------------------------- panes
const noteAt = (name) => host.xpanes && host.xpanes().find((x) => x.kind === 'note' && ((draftFor(x.id) && draftFor(x.id).name.peek()) || x.name) === name);

// The note's pane. One note, one pane: an open one is focused instead of doubled (two boxes on one file would only fight each other).
// `fresh`: just made, so the caret goes to the end.
export function openNote(name, { fresh = false } = {}) {
  if (!NAME.test(name) || !host.openXPane) return null;
  const open = noteAt(name);
  if (open) {
    if (host.focusPane) host.focusPane(open.id);
    if (typeof document !== 'undefined') {   // the box too: a click on a note in the list means to write in it
      requestAnimationFrame(() => { const el = document.querySelector(`.notepad[data-pane="${open.id}"] textarea`); if (el) el.focus({ preventScroll: true }); });
    }
    return open;
  }
  return host.openXPane('note', { name, fresh });
}

// `:note [title]` and `[ + new note ]`.
export async function newNote(raw = '') {
  if (mode() !== 'perch') { say('notes need the hub: open Minerva with hoot', true); return null; }
  const title = String(raw || '').trim();
  const r = await createNote({ title });
  if (!r.ok) { say(r.why, true); return null; }
  refresh();
  const x = openNote(r.name, { fresh: true });
  say(title ? `new note ${r.name}: ${title}` : `new note ${r.name}`);
  return x;
}
