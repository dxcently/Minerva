// What an agent does in a terminal, seen from the page when the ⌨ tab is not showing: a badge on its rail icon, a toast with [ show ], and,
// for the focused chat's agent opening a terminal, the right rail switched to ⌨ once for that terminal (D11 item 5). The keyboard never moves.
// Two sources: the open chat streams (a frame tap sees term_open and term_send as the agent calls them, and a term_send names its terminal),
// and the roster, which the page reads through the lane every 5 s while the terminal stream is closed (a new agent terminal, or an agent's
// input that moved). A terminal's event is said at most once per 10 s; a replayed frame says nothing.
import { signal } from '../core/ui.js';
import { isXPane, host } from '../core/registry.js';
import { mode } from '../core/api.js';
import { list, label } from '../core/hub.js';
import { roster, streamUp, pollNow, diffRoster, setHooks, clipText, TERM_ID, POLL_MS } from './model.js';

export const GAP_MS = 10000;   // one event per terminal (or per chat, for an open not yet named) in this window
const TAP = { term_open: 'open', term_send: 'type' };

export const badge = signal(false);     // the ⌨ rail icon's dot
export const tabShown = signal(false);  // the ⌨ tab is drawn now (the tab sets it, and clears the badge when it shows)

// The frame tap's part, pure: a tool call the agent started. term_open names no terminal yet (its id is in the answer), so it has no id;
// term_send names its terminal in `id`, and one without a valid id is no event.
export function tapEvent(sid, frame) {
  if (!sid || !frame || frame.type !== 'tool-call-started' || typeof frame.name !== 'string' || !Object.hasOwn(TAP, frame.name)) return null;
  const kind = TAP[frame.name];
  if (kind === 'open') return { kind, id: null, chat: sid };
  const id = frame.input && typeof frame.input === 'object' ? frame.input.id : undefined;
  return typeof id === 'string' && TERM_ID.test(id) ? { kind, id, chat: sid } : null;
}

// A rate limit by key: `allow(key, now)` is true once per `gap` ms for each key. Old keys are dropped as the map grows.
export function makeGate(gap) {
  const last = new Map();
  return {
    allow(key, now) {
      const t = last.get(key);
      if (t !== undefined && now - t < gap) return false;
      last.set(key, now);
      if (last.size > 256) for (const [k, v] of last) if (now - v >= gap) last.delete(k);
      return true;
    },
  };
}
const gate = makeGate(GAP_MS);

// The chat's name for a toast: its title (the perch's list row), else its id; text, never markup, cut short.
const chatName = (chat) => {
  const row = chat ? list.value.find((r) => r.id === chat) : null;
  return clipText(row ? label(row) : chat || 'a chat', 40) || 'a chat';
};
// The chat in the focused pane, if a chat is focused (an xpane, or none, is no chat).
const focusedChat = () => {
  const f = typeof host.focused === 'function' ? host.focused() : null;
  return f && !isXPane(f) && f.sid ? f.sid : null;
};

const switched = new Set(); // terminals whose open already switched the rail to ⌨ (once each)
const showTerms = () => { if (typeof host.showTab === 'function') host.showTab('r', 'terms'); };

// One event, said: the rail switched (an open by the focused chat, once per terminal), the badge set (unless the tab shows), a toast.
// An open with no id yet (the tool call's start) raises the badge only: the terminal's id comes with the open the stream or the next poll
// sees, and that one is said, so one terminal is one toast.
export function report(ev, now = Date.now()) {
  if (!ev || (ev.kind !== 'open' && ev.kind !== 'type')) return;
  if (ev.kind === 'open' && ev.id && ev.chat && focusedChat() === ev.chat && !switched.has(ev.id)) { switched.add(ev.id); showTerms(); }
  if (ev.kind === 'open' && !ev.id) { if (!tabShown.peek()) badge.value = true; return; }
  if (!gate.allow(ev.kind + ' ' + ev.id, now)) return;
  if (!tabShown.peek()) badge.value = true;
  const who = 'agent in ' + chatName(ev.chat);
  const text = ev.kind === 'type' ? `${who} is typing in ${ev.id}` : `${who} opened ${ev.id}`;
  if (typeof host.toast === 'function') host.toast(text, false, { label: '[ show ]', on: showTerms });
}

// The frame tap: every frame of every open chat stream, read only; a replay's frames are history, not news.
export function tap(sid, frame, live) {
  if (!live) return;
  const ev = tapEvent(sid, frame);
  if (ev) report(ev);
}

// The roster poll, while the terminal stream is closed. The first answer is a baseline: what was there already is not news.
let polling = 0, baseline = false;
export async function poll() {
  if (mode() !== 'perch' || streamUp()) return;
  const before = roster.peek();
  const st = await pollNow();
  if (st !== 200 || streamUp()) return;
  if (baseline) for (const ev of diffRoster(before, roster.peek())) report(ev);
  baseline = true;
}
export function _resetPoll() { baseline = false; }   // for tests: a page's first answer is a baseline again

// Starts the poll and hooks the stream's own events in. Once; the page calls it at boot (terms/index.js). With no document (node's tests
// import this folder) there is no page to poll for, and a live interval would keep the test process from ending.
export function startActivity() {
  if (polling || typeof document === 'undefined') return;
  setHooks({ event: (ev) => report(ev) });
  polling = setInterval(poll, POLL_MS);
}
