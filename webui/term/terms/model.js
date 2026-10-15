// The terminals' model, the page's side: the roster of the perch's terminals (metadata only, what GET /hub/term lists), the one stream
// that carries every terminal's output (open while a view shows one, closed STOP_AFTER after the last view), each terminal's input (one
// POST in flight, the rest joined, never retried), and the diff the activity folder reads. The screen (terms/view.js) is reached through
// `hooks`, so this file never touches the DOM and node can test the pure parts (tests/terms.test.mjs).
//
// The perch's frames (perch/src/term/README.md, T1c): term-list, term-reset, term-out, term-open, term-size, term-title, term-hold,
// term-exit, term-closed, term-input, hub-stream-end. A frame for a terminal the page does not hold is dropped, never guessed at.
import { signal } from '../core/ui.js';
import { auth, mode } from '../core/api.js';
import { stream } from '../core/sse.js';
import { extraStreams, streamRoom, host } from '../core/registry.js';
import { hubPoll } from '../core/lane.js';

export const TERM_ID = /^t[0-9]{1,6}$/;
export const MAX_TERMS = 16;                 // the perch's table
export const MAX_CHUNK = 64 * 1024;          // one input call: the perch's bound
export const BACKLOG_MAX = 2 * 1024 * 1024;  // bytes handed to a screen and not yet parsed: past it the terminal is resynced
export const STOP_AFTER = 10000;             // the stream closes this long after the last view
export const POLL_MS = 5000;                 // the roster poll while the stream is closed
export const POLL_TIMEOUT = 5000;
export const INPUT_MS = 10000;               // an input call with no answer is a refusal (status 0), never a retry
export const ROW_MS = 1000;                  // a row's last_ms (its place in the stack) moves at most this often
export const TITLE_MAX = 80;
export const LINE_MAX = 200;
export const FOLD_KEY = 'minerva.termfold';

// What the page shows. `status`: closed | connecting | live | reconnecting | refused | door | slot.
export const roster = signal([]);
export const status = signal('closed');
export const typedAt = signal({});   // id -> ms of the last agent input (the 'agent typing' line)
export const agentLine = signal({}); // id -> the agent's last input, as the perch pictures it
export const bellAt = signal({});    // id -> ms of the last bell
// id -> { id, term, fit, host, pend, seq, queue, backlog, loading, resyncing, follow }: the screen (view.js: term, fit, host) and the
// page's bookkeeping for one terminal (pend: its input queue; seq: the last byte the page has, which is what a resume asks from).
export const live = new Map();

// The screen (view.js) and the activity folder (activity.js) plug in here (setHooks); until they do, every hook does nothing.
export const hooks = { made() {}, out() {}, reset() {}, size() {}, gone() {}, event() {} };
export function setHooks(h) { Object.assign(hooks, h || {}); }

// ---------------------------------------------------------------- pure
const int = (v, lo, hi, d) => (Number.isInteger(v) ? Math.min(hi, Math.max(lo, v)) : d);
const str = (v) => (typeof v === 'string' ? v : '');
const ms = (v) => (Number.isFinite(v) && v >= 0 ? Math.floor(v) : undefined);

// A perch row, checked and cut to what the page reads. Null for anything without a terminal id.
export function vet(r) {
  if (!r || typeof r !== 'object' || typeof r.id !== 'string' || !TERM_ID.test(r.id)) return null;
  const exit = r.exit && typeof r.exit === 'object'
    ? { code: Number.isInteger(r.exit.code) ? r.exit.code : null, signal: Number.isInteger(r.exit.signal) ? r.exit.signal : null } : null;
  const v = {
    id: r.id, title: str(r.title), fg: str(r.fg), by: r.by === 'agent' ? 'agent' : 'operator', cwd: str(r.cwd),
    cols: int(r.cols, 20, 400, 120), rows: int(r.rows, 5, 200, 32), alive: r.alive === true, exit,
    seq: Number.isFinite(r.seq) ? r.seq : 0, started_ms: ms(r.started_ms) || 0, last_ms: ms(r.last_ms) || 0,
    hold: r.hold === true, ended: r.ended === true,
  };
  if (typeof r.chat === 'string' && r.chat) v.chat = r.chat;
  if (ms(r.agent_last_ms) !== undefined) v.agent_last_ms = ms(r.agent_last_ms);
  return v;
}

// A frame's base64 bytes, as bytes: the screen takes bytes, so a UTF-8 character split across two frames is parsed whole there.
export function decodeB64(s) {
  const bin = atob(typeof s === 'string' ? s : '');
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// Text a person reads (a title, a line from an agent): control and bidi characters become spaces or nothing, cut to n characters.
const CTRL = /[\u0000-\u001f\u007f-\u009f]/g;
const FORMAT = /[؜​-‏‪-‮⁠-⁯﻿]/g;
export function clipText(s, n) {
  return Array.from(String(s ?? '').replace(CTRL, ' ').replace(FORMAT, '')).slice(0, n).join('');
}

// How long ago, in the units a stack header has room for: 45s, 3m, 2h, 1d. '' with no start.
export function ageText(sinceMs, now) {
  if (!sinceMs) return '';
  const s = Math.max(0, Math.floor((now - sinceMs) / 1000));
  return s < 60 ? s + 's' : s < 3600 ? Math.floor(s / 60) + 'm' : s < 86400 ? Math.floor(s / 3600) + 'h' : Math.floor(s / 86400) + 'd';
}

// Whether the stack's order is recomputed now: at most every REORDER_MS, and never while the pointer or the focus is in the stack.
export const REORDER_MS = 2000;
export const reorderDue = (now, last, frozen) => !frozen && now - last >= REORDER_MS;

// The order of the stack: linked to the inspected chat first (0 before 1), then the most recent output first. Ids, in that order.
export function rankRows(rows, isLinked) {
  return rows.map((r, i) => ({ id: r.id, k: isLinked(r) ? 0 : 1, t: -(r.last_ms || 0), i }))
    .sort((a, b) => a.k - b.k || a.t - b.t || a.i - b.i)
    .map((x) => x.id);
}

// Which terminals are open before anyone touches the fold: the inspected chat's, and the two started last.
export function foldDefault(rows, isLinked) {
  const open = new Set(rows.filter(isLinked).map((r) => r.id));
  [...rows].sort((a, b) => (b.started_ms || 0) - (a.started_ms || 0)).slice(0, 2).forEach((r) => open.add(r.id));
  return open;
}

// Whether one terminal is open: what the person set ('o' or 'c') when they did, else the default.
export const foldOpen = (saved, id, dflt) => (saved[id] === 'o' ? true : saved[id] === 'c' ? false : dflt.has(id));

// The fold kept in localStorage. A broken or absent value is no choices; only the words 'o' and 'c' under a terminal id are kept.
export function cleanFold(v) {
  const out = {};
  if (v && typeof v === 'object' && !Array.isArray(v)) {
    for (const [id, w] of Object.entries(v)) if (TERM_ID.test(id) && (w === 'o' || w === 'c')) out[id] = w;
  }
  return out;
}
export function loadFold() {
  try { return cleanFold(JSON.parse(localStorage.getItem(FOLD_KEY))); } catch { return {}; }
}
export function saveFold(v) {
  try { localStorage.setItem(FOLD_KEY, JSON.stringify(cleanFold(v))); } catch { /* no storage: this tab only */ }
}

// The agent-side changes between two rosters: a terminal that is new and the agent's (`open`), or one whose agent input moved (`type`).
// A null `prev` is a first look: nothing has happened yet, so nothing is reported.
export function diffRoster(prev, next) {
  if (!prev) return [];
  const was = new Map(prev.map((r) => [r.id, r]));
  const out = [];
  for (const r of next) {
    const p = was.get(r.id);
    if (!p) { if (r.by === 'agent') out.push({ kind: 'open', id: r.id, chat: r.chat || null }); }
    else if ((r.agent_last_ms || 0) > (p.agent_last_ms || 0)) out.push({ kind: 'type', id: r.id, chat: r.chat || null });
  }
  return out;
}

// The `since` of GET /hub/term/events: `t1:4096,t3:0` for the terminals whose seq the page has (a missing one joins with a reset).
export function sinceOf(pairs) {
  return pairs.filter(([id, seq]) => TERM_ID.test(id) && Number.isSafeInteger(seq) && seq >= 0).map(([id, seq]) => id + ':' + seq).join(',');
}

// The words a refused input gets in the hint line. The perch's own answer is never shown.
export function refusalWords(st) {
  const words = {
    0: 'no answer from the perch', 404: 'no such terminal, it may have closed', 409: 'the terminal has ended', 413: 'too much input at once',
    415: 'the input was not sent as bytes', 503: "the terminal's input queue is full: type again in a moment",
  };
  return words[st] || 'the perch refused the input (' + st + ')';
}

// The input of one terminal. `send(bytes)` resolves to the status of one POST; `refused(status)` is said once when a POST is refused.
// Typed chunks wait in order and join into one call of up to MAX_CHUNK bytes; one call is in flight at a time. A refused call drops
// everything waiting (it is never retried, so the rest of a line cannot be sent out of order); a call that threw counts as status 0.
export function inputQueue(send, refused) {
  const pend = [];
  let size = 0, flying = false;
  function take() {
    const parts = [];
    let n = 0;
    while (pend.length && n < MAX_CHUNK) {
      const b = pend[0], room = MAX_CHUNK - n;
      if (b.length <= room) { parts.push(b); n += b.length; pend.shift(); }
      else { parts.push(b.subarray(0, room)); pend[0] = b.subarray(room); n += room; }
    }
    size -= n;
    const out = new Uint8Array(n);
    let o = 0;
    for (const p of parts) { out.set(p, o); o += p.length; }
    return out;
  }
  async function go() {
    if (flying || !pend.length) return;
    flying = true;
    const bytes = take();
    let st;
    try { st = await send(bytes); } catch { st = 0; }
    flying = false;
    if (st !== 204) { pend.length = 0; size = 0; refused(st); return; }
    go();
  }
  return {
    get size() { return size; },
    get busy() { return flying; },
    push(bytes) {
      if (!bytes || !bytes.length) return;
      pend.push(bytes);
      size += bytes.length;
      go();
    },
  };
}

// ---------------------------------------------------------------- the roster
const touched = new Map(); // id -> the last ms its row's last_ms moved

function patchRow(id, patch) {
  const cur = roster.peek();
  if (!cur.some((r) => r.id === id)) return;
  roster.value = cur.map((r) => (r.id === id ? { ...r, ...patch } : r));
}
function upsertRow(row) {
  const cur = roster.peek();
  const at = cur.findIndex((r) => r.id === row.id);
  if (at >= 0) roster.value = cur.map((r, i) => (i === at ? row : r));
  else if (cur.length < MAX_TERMS) roster.value = [...cur, row];
}
function dropLive(id) {
  live.delete(id);
  touched.delete(id);
  for (const s of [typedAt, agentLine, bellAt]) if (id in s.peek()) { const next = { ...s.peek() }; delete next[id]; s.value = next; }
  hooks.gone(id);
}
function removeRow(id) {
  roster.value = roster.peek().filter((r) => r.id !== id);
  if (live.has(id)) dropLive(id);
}
function entryOf(id) {
  let e = live.get(id);
  if (!e) {
    e = { id, term: null, fit: null, host: null, pend: null, seq: undefined, queue: [], backlog: 0, loading: false, resyncing: false, follow: false };
    live.set(id, e);
    hooks.made(id);
  }
  return e;
}

// The whole roster as the perch lists it (GET /hub/term or the stream's term-list): the rows are replaced, a terminal that is gone is
// dropped, and each listed one has its page-side entry. A poll answered while the stream runs is older than the stream and is ignored.
export function applyList(rows) {
  const next = [], seen = new Set();
  for (const r of Array.isArray(rows) ? rows : []) {
    const v = vet(r);
    if (!v || seen.has(v.id) || next.length >= MAX_TERMS) continue;
    seen.add(v.id);
    next.push(v);
  }
  roster.value = next;
  for (const id of [...live.keys()]) if (!seen.has(id)) dropLive(id);
  for (const v of next) entryOf(v.id);
}

// Output moves a row's last_ms at most once a second: the stack's order does not churn with every byte.
function touch(id, now) {
  if (now - (touched.get(id) || 0) < ROW_MS) return;
  touched.set(id, now);
  patchRow(id, { last_ms: now });
}

// One frame of the stream. `now` is the clock (a test passes its own).
export function onFrame(f, now = Date.now()) {
  if (!f || typeof f !== 'object') return;
  if (f.type === 'term-list') { applyList(f.terms); status.value = 'live'; return; }
  if (f.type === 'hub-stream-end' || typeof f.id !== 'string' || !TERM_ID.test(f.id)) return;
  const id = f.id;
  switch (f.type) {
    case 'term-reset':
    case 'term-out': {
      const e = live.get(id);
      if (!e) return;
      let bytes;
      try { bytes = decodeB64(f.data); } catch { return; }
      if (Number.isSafeInteger(f.seq)) e.seq = f.seq;
      if (f.type === 'term-reset') hooks.reset(id, bytes);
      else { hooks.out(id, bytes); touch(id, now); }
      return;
    }
    case 'term-open': {
      const row = vet(f.term);
      if (!row || row.id !== id) return;
      upsertRow(row);
      entryOf(id);
      if (row.by === 'agent') hooks.event({ kind: 'open', id, chat: row.chat || null });
      return;
    }
    case 'term-size': {
      const cols = int(f.cols, 20, 400, 120), rows = int(f.rows, 5, 200, 32);
      patchRow(id, { cols, rows });
      const e = live.get(id);
      if (e) hooks.size(id, cols, rows);
      return;
    }
    case 'term-title': patchRow(id, { title: str(f.title), fg: str(f.fg) }); return;
    case 'term-hold': patchRow(id, { hold: f.hold === true }); return;
    case 'term-exit': {
      // The perch sends the code and the signal flat, beside the id (its frame table); the roster row keeps them nested under `exit`.
      const code = Number.isInteger(f.code) ? f.code : null, signal = Number.isInteger(f.signal) ? f.signal : null;
      patchRow(id, { alive: false, fg: '', exit: { code, signal } });
      return;
    }
    case 'term-closed': removeRow(id); return;
    case 'term-input': {
      if (f.by !== 'agent') return;
      typedAt.value = { ...typedAt.peek(), [id]: now };
      agentLine.value = { ...agentLine.peek(), [id]: clipText(f.text, LINE_MAX) };
      patchRow(id, { agent_last_ms: now });
      hooks.event({ kind: 'type', id, chat: typeof f.chat === 'string' && f.chat ? f.chat : null });
      return;
    }
    default: return;
  }
}

// The bell (BEL from a program): the header flashes. The time is kept; the view decides how long that shows.
export function bell(id, now = Date.now()) {
  if (!live.has(id)) return;
  bellAt.value = { ...bellAt.peek(), [id]: now };
}

// Changes a row the way the operator asked for it (a hold, answered by the perch). The stream says the same again, which is harmless.
export function setHold(id, hold) { patchRow(id, { hold: hold === true }); }

// ---------------------------------------------------------------- the stream
let epoch = 0, running = false, controller = null, views = 0, stopTimer = 0, fails = 0, quick = false;

export const streamUp = () => running;

// Opens the stream (if it may) for one view and returns the function that gives the view back. The stream closes STOP_AFTER after the
// last view gives back.
export function watch() {
  views++;
  clearTimeout(stopTimer);
  stopTimer = 0;
  startStream();
  let given = false;
  return () => {
    if (given) return;
    given = true;
    views = Math.max(0, views - 1);
    if (!views && running) stopTimer = setTimeout(stopStream, STOP_AFTER);
  };
}

// The stream may open only while the browser has a connection to spare: the chats hold up to four between them, the terminal stream takes
// one, so it opens when no more than three chats hold a stream. Otherwise the status says so and retry() tries again.
export function startStream() {
  if (running) return true;
  if (mode() === 'door') { status.value = 'door'; return false; }
  const open = typeof host.chatStreams === 'function' ? host.chatStreams() : 0;
  if (streamRoom(open, 0) < 1) { status.value = 'slot'; return false; }
  running = true;
  extraStreams.value = 1;
  fails = 0;
  quick = false;
  loop(++epoch);
  return true;
}

export function retry() { if (status.peek() === 'slot') startStream(); }

export function stopStream() {
  epoch++;
  running = false;
  if (controller) controller.abort();
  controller = null;
  clearTimeout(stopTimer);
  stopTimer = 0;
  extraStreams.value = 0;
  status.value = 'closed';
}

// A screen that fell too far behind: its seq is forgotten, so the next connection sends it a reset (the screen clears and is rebuilt).
export function resync(id) {
  const e = live.get(id);
  if (!e) return;
  e.seq = undefined;
  quick = true;
  if (controller) controller.abort();
}

function streamUrl() {
  const pairs = [];
  for (const e of live.values()) if (e.seq !== undefined) pairs.push([e.id, e.seq]);
  const s = sinceOf(pairs);
  return '/hub/term/events' + (s ? '?since=' + s : '');
}

function refuse(st) {
  epoch++;
  running = false;
  controller = null;
  extraStreams.value = 0;
  status.value = st;
}

const pause = (t) => new Promise((r) => setTimeout(r, t));

// The connection, kept open while the stream runs; a stream that ends or fails is opened again with a growing wait.
async function loop(ep) {
  while (ep === epoch) {
    status.value = fails ? 'reconnecting' : 'connecting';
    const ctl = new AbortController();
    controller = ctl;
    let res;
    try { res = await stream(streamUrl(), auth(), (f) => { if (ep === epoch) onFrame(f); }, ctl.signal); }
    catch { res = { status: 0 }; }
    if (ep !== epoch) return;
    controller = null;
    if (res.status === 401 || res.status === 403) { refuse('refused'); return; }
    if (res.status === 404) { refuse('door'); return; }
    fails = res.status === 200 ? 0 : fails + 1;
    const wait = quick ? 0 : Math.min(10000, 1000 * 2 ** Math.max(0, fails - 1));
    quick = false;
    await pause(wait);
    if (ep !== epoch) return;
  }
}

// ---------------------------------------------------------------- what the page asks of the perch
// The roster, polled while the stream is closed (the activity folder's 5 s): through the lane, so it never holds a connection the stream or
// a typed key needs. Resolves to the status of the call.
export async function pollNow() {
  const r = await hubPoll('term', POLL_TIMEOUT);
  if (r.status === 200 && Array.isArray(r.data) && !running) applyList(r.data);
  return r.status;
}

// One terminal's input. The POST is raw bytes (the perch refuses any other Content-Type with 415), with the same credentials as every call.
async function postInput(id, bytes) {
  if (!TERM_ID.test(id)) return 0;
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), INPUT_MS);
  try {
    const r = await fetch(`/hub/term/${id}/input`, {
      method: 'POST', cache: 'no-store', headers: auth({ 'content-type': 'application/octet-stream' }), body: bytes, signal: ctl.signal,
    });
    if (r.status !== 204) await r.text().catch(() => {});
    return r.status;
  } catch {
    return 0;
  } finally {
    clearTimeout(timer);
  }
}

// What a person typed into a terminal: bytes, queued for that terminal's one input call at a time.
export function typed(id, bytes) {
  const e = live.get(id);
  if (!e || !bytes || !bytes.length) return;
  if (!e.pend) {
    e.pend = inputQueue((b) => postInput(id, b), (st) => {
      if (typeof host.toast === 'function') host.toast(`input not sent: ${refusalWords(st)}`, true);
    });
  }
  e.pend.push(bytes);
}

// For tests: the page as it is when a file loads. Stops the stream, empties the roster and the entries, and puts the hooks back.
export function _reset() {
  stopStream();
  roster.value = [];
  typedAt.value = {};
  agentLine.value = {};
  bellAt.value = {};
  live.clear();
  touched.clear();
  Object.assign(hooks, { made() {}, out() {}, reset() {}, size() {}, gone() {}, event() {} });
}
