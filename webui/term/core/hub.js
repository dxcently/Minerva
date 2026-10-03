// The perch's session list, polled. `GET /hub/sessions` is the source (PLAN D3:
// no /hub/events stream, which would take one of the browser's six connections
// to the perch). Perch mode only: in door mode nothing here runs.
//
// Polled at boot, every POLL_MS while the tab is visible, at once when it
// becomes visible again, and after every start/resume/stop this page makes. A
// failed poll keeps the last list and marks it stale; it never toasts.
import { signal } from './ui.js';
import { hubGet, rows, mode, POLL_TIMEOUT_MS } from './api.js';

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
    const { status, data } = await hubGet('sessions', POLL_TIMEOUT_MS);
    if (status === 200 && Array.isArray(data)) { list.value = data; stale.value = false; }
    else stale.value = true;
  })().finally(() => { busy = null; });
  return busy;
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
// Kept logs, and doors of ours that exited, newest first.
export const keptRows = (rs) => rs.filter((r) => r.state === 'kept' || r.state === 'dead')
  .sort((a, b) => (b.mtime || 0) - (a.mtime || 0));
export const elsewhere = (rs) => rs.filter((r) => r.state === 'live-elsewhere');

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
