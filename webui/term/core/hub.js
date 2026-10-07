// The perch's session list, polled. `GET /hub/sessions` is the source (PLAN D3:
// no /hub/events stream, which would take one of the browser's six connections
// to the perch). Perch mode only: in door mode nothing here runs.
//
// Polled at boot, every POLL_MS while the tab is visible, at once when it
// becomes visible again, and after every start/resume/stop this page makes. A
// failed poll keeps the last list and marks it stale; it never toasts. Both GETs
// wait their turn in the request lane (lane.js), so the poll never holds more
// than the one connection the lane uses.
import { signal } from './ui.js';
import { rows, mode, POLL_TIMEOUT_MS } from './api.js';
import { hubPoll } from './lane.js';

export const trash = signal([]); // chats deleted into the perch's trash: [{ id, title, mtime }], newest first

export const POLL_MS = 5000;
export const list = signal([]);   // rows as the perch sent them
export const stale = signal(false); // the last poll failed; `list` is the last good one
export const known = signal(false); // detect() has said perch: views that branch on the mode redraw

let busy = null, timer = 0, started = false;

export function refresh() {
  if (mode() !== 'perch') return Promise.resolve();
  if (busy) return busy;
  busy = (async () => {
    // Timed out (status 0) counts as a failed poll: stale shows and the next tick polls again.
    const { status, data } = await hubPoll('sessions', POLL_TIMEOUT_MS);
    if (status === 200 && Array.isArray(data)) { list.value = data; stale.value = false; }
    else stale.value = true;
    const t = await hubPoll('trash', POLL_TIMEOUT_MS);
    if (t.status === 200 && Array.isArray(t.data)) trash.value = t.data;
  })().finally(() => { busy = null; });
  return busy;
}

// The approval mode of a chat: 'yolo' when its door says so (the door's own word), else what the perch reports for it ('manual' or
// 'auto': the perch chose the config the door runs with), else '' (a door another process started: nobody here knows).
export function modeOf(sid, hello) {
  if (hello && hello.yolo) return 'yolo';
  const row = list.value.find((r) => r.id === sid);
  return row && (row.mode === 'manual' || row.mode === 'auto') ? row.mode : '';
}

export function startPolling() {
  if (started || mode() !== 'perch') return;
  started = true;
  list.value = rows();
  known.value = true;
  stale.value = false;
  const tick = () => { if (document.visibilityState === 'visible') refresh(); };
  timer = setInterval(tick, POLL_MS);
  document.addEventListener('visibilitychange', tick);
}

// ---------------------------------------------------------------- views of the list
export const live = (rs) => rs.filter((r) => r.state === 'live');
// `other`: eidolon's own shared sessions folder (read-only), plus the chats of this app that were archived by hand or sat idle
// for 7 days (the perch decides both: `archived`, `auto_other`). A live chat is never in it.
export const isOther = (r) => r.state !== 'live' && (r.origin === 'other' || !!r.archived || !!r.auto_other);
// `archived` (by hand), `idle` (aged out), or '' (eidolon's own folder, which has its own words).
export const otherTag = (r) => (r.origin ? '' : r.archived ? 'archived' : r.auto_other ? 'idle' : '');
// Saved chats of this app, and doors of ours that exited, newest first, not in `other`.
export const keptRows = (rs) => rs.filter((r) => ((r.state === 'kept' && !r.origin) || r.state === 'dead') && !isOther(r))
  .sort((a, b) => (b.mtime || 0) - (a.mtime || 0));
export const elsewhere = (rs) => rs.filter((r) => r.state === 'live-elsewhere');
// The `other` group, newest first.
export const older = (rs) => rs.filter((r) => (r.state === 'kept' || r.state === 'dead') && isOther(r)).sort((a, b) => (b.mtime || 0) - (a.mtime || 0));

// 5m, 3h, 2d from a unix mtime in seconds.
export function age(mtime, now = Date.now() / 1000) {
  if (!mtime) return '?';
  const s = Math.max(0, now - mtime);
  if (s < 60) return Math.floor(s) + 's';
  if (s < 3600) return Math.floor(s / 60) + 'm';
  if (s < 86400) return Math.floor(s / 3600) + 'h';
  return Math.floor(s / 86400) + 'd';
}
export const label = (r) => r.title || r.id;

// A title as a folder name, the way the perch makes one (names.rs `slug`): lowercase letters and digits, runs of anything else a single
// `-`, at most 48 characters; '' when nothing is left.
export const slugOf = (title) => String(title || '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+/, '').slice(0, 48).replace(/-+$/, '');
