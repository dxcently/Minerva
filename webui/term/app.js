// The screen: sidebars, tiles, the hint line, the status line, keys, the `:`
// line. Sessions live in core/state.js and panes draw them (panes/session.js);
// this file arranges panes and routes every action. Every action is reachable
// by mouse; keys and the `:` line are shortcuts to the same functions.
import { html, render, K, HUB, signal, computed, effect, batch, useRef, useLayoutEffect } from './core/ui.js';
import { takeToken, takeCode, redeem, DOOR, detect, mode, rows, hubPost } from './core/api.js';
import { list, stale, known, refresh, startPolling, modeOf, live, keptRows, older, elsewhere, age, label, otherTag, trash, slugOf } from './core/hub.js';
import { Session, tildify, kfmt, stateOf, STATE_CLS, backoffMs, connWord } from './core/state.js';
import * as tile from './core/tile.js';
import { menu, openMenu, closeMenu, outside, atPointer, under, Menus } from './core/menu.js';
import { Pane } from './panes/session.js';
import { panel } from './panes/asks.js';
import { FilesTab } from './panes/files.js';
import { LoadedTab } from './panes/loaded.js';
import { DiffTab } from './panes/diff.js';
import { GraphTab } from './graph/index.js';
import { MeshTab } from './mesh/index.js';
import { Home, home } from './panes/home.js';
import { projects, projectFor, sorted, isScratch, loadProjects, addProject, forgetProject, renameProject } from './core/projects.js';
import { Picker, picker as pickerOpen, openPicker } from './panes/picker.js';
import { Overlay, overlay } from './core/overlay.js';
import { Needs, Kv } from './core/bits.js';
import { hubPoll } from './core/lane.js';
import { limitTable } from './core/ctxlimit.js';
import { FONTS, DEFAULT_FONT, theme, themes, font, cell, setTheme, setFont, resolve, startLook } from './core/look.js';
import {
  host, paneKinds, commands, keyRows, loadedSections, MAX_SIDS, chatCap, overCap,
  BASE_LEFT, BASE_RIGHT, mergeTabs, rightTabs, leftTabs, pickTab, cycleTab, badgeText,
  mergeCommands, mergeKeyRows, keyClaimed, focusClaimed, whereOf, splitExtraItems, chatMenuExtras,
  isXPane, anchorOf, addXPane, dropXPane, paneInfo,
} from './core/registry.js';
// Features that register themselves (tabs, panes, sections, commands, keys: core/registry.js); app.js lists none of what they add.
import './terms/index.js';
import './notes/index.js';
import './allow/list.js';

// The tabs of each side: the page's own (BASE_LEFT / BASE_RIGHT in core/registry.js, which also holds their rail icons, Unicode symbols
// and not emoji), then the ones features registered. Components: Left() and INSPECTOR below for the base ones, the registration for the rest.
const leftAll = computed(() => mergeTabs(BASE_LEFT, leftTabs.list));
const rightAll = computed(() => mergeTabs(BASE_RIGHT, rightTabs.list));
const tabsOn = (side) => (side === 'l' ? leftAll : rightAll).value;
const RAILS_KEY = 'minerva.rails', NARROW_CELLS = 110; // under this many cells wide, an open panel overlays the tiles
// Sidebar widths in cells: the defaults a double-click on the border goes back to, and the bounds.
const SIDES = { l: 30, r: 34 }, SIDE_MIN = 16, SIDE_MAX = 72, SIDES_KEY = 'minerva.sidebars';

// ---------------------------------------------------------------- screen state
// Per side: open or closed, and the chosen tab, kept in localStorage (no storage: defaults, this tab only).
const RAILS0 = (() => {
  let v = null;
  try { v = JSON.parse(localStorage.getItem(RAILS_KEY)); } catch { /* no storage: defaults */ }
  const closed = (x) => (typeof x === 'boolean' ? x : innerWidth < 900); // a small screen starts compact
  const names = (side) => tabsOn(side).map((t) => t.name); // a saved name stays if its tab is still there (a feature's tab registered before this runs)
  return {
    lc: closed(v && v.lc), rc: closed(v && v.rc),
    lt: pickTab(v && v.lt, names('l'), 'sessions'), rt: pickTab(v && v.rt, names('r'), 'rec'),
  };
})();
const ui = {
  panes: signal([]),            // [{ id, sid, s: Session }]; panes on one sid share s; a pane's sid can change (showIn)
  xpanes: signal([]),           // [{ id, kind, ...data }]: panes that are not chats (core/registry.js paneKinds); they and `panes` are leaves of `tree`
  lastChat: signal(null),       // the id of the chat pane that had focus last: what the inspector keeps showing while an xpane has focus
  tree: signal(null),           // core/tile.js split tree of pane ids (both kinds)
  focus: signal(null),          // pane id, of either kind
  left: signal(RAILS0.lt),
  right: signal(RAILS0.rt),
  lrail: signal(RAILS0.lc),     // true: the left panel is closed, only its rail shows
  rrail: signal(RAILS0.rc),
  narrow: signal(false),        // fewer than NARROW_CELLS cells wide: an open panel overlays the tiles
  hint: signal(null),           // { mode: 'toast' | 'menu', text?, err? }
  keys: signal(false),          // the key reference is open
  cmd: signal(null),            // the `:` line's text while open, else null
  held: signal(false),          // the oldest ask waits for a pause in typing before it takes focus
  box: signal({ w: 0, h: 0, ...cell.peek() }),
  sides: signal(loadSides()),   // { l, r }: sidebar widths in cells, kept in localStorage
  folded: signal(new Set()),    // sidebar project groups folded shut, by cwd
  where: signal('idle'),        // what holds focus: 'ask' | 'note' | 'compose' | 'idle'
  tab: signal(null),            // the ask panel's current tab ({ s, a }), while it is pending
  fit: signal(0),               // how much the status line dropped to fit (FIT below)
  token: false,                 // this page can authenticate: a #token= in sessionStorage, or a login cookie
  login: signal(''),            // the one-time link being traded for a cookie: '' | 'wait' | 'expired'
  gate: signal(null),           // why a page holding a token has no sessions (perch mode): 'refused' | 'offline'
};
function loadSides() {
  let v = null;
  try { v = JSON.parse(localStorage.getItem(SIDES_KEY)); } catch { /* no storage: defaults */ }
  const n = (x, d) => (Number.isFinite(x) ? Math.min(SIDE_MAX, Math.max(SIDE_MIN, Math.round(x))) : SIDES[d]);
  return { l: n(v && v.l, 'l'), r: n(v && v.r, 'r') };
}
function setSide(d, cells) {
  const most = Math.min(SIDE_MAX, Math.floor((innerWidth * 0.45) / cell.peek().cw));
  const n = Math.max(SIDE_MIN, Math.min(Math.max(most, SIDE_MIN), Math.round(cells)));
  if (n === ui.sides.peek()[d]) return;
  ui.sides.value = { ...ui.sides.peek(), [d]: n };
  try { localStorage.setItem(SIDES_KEY, JSON.stringify(ui.sides.peek())); } catch { /* this tab only */ }
}
const saveRails = () => {
  try { localStorage.setItem(RAILS_KEY, JSON.stringify({ lc: ui.lrail.peek(), rc: ui.rrail.peek(), lt: ui.left.peek(), rt: ui.right.peek() })); } catch { /* this tab only */ }
};
const byId = (id) => ui.panes.value.find((p) => p.id === id);
const xById = (id) => ui.xpanes.value.find((x) => x.id === id);
// `focused` is the focused CHAT pane (null while an xpane has focus): the ~30 sites that read p.s / p.sid stay on it. `focusedAny` is the
// focused pane of either kind; `inspected` the chat the inspector shows: the focused one, else the last one that was.
const focused = computed(() => byId(ui.focus.value) || null);
const focusedAny = computed(() => focused.value || xById(ui.focus.value) || null);
const inspected = computed(() => focused.value || byId(ui.lastChat.value) || null);
const isMirror = (p) => ui.panes.value.find((x) => x.s === p.s) !== p;
let paneSeq = 0, toastTimer = 0;

// ---------------------------------------------------------------- actions
// One Session, so one /api/events stream, per session id. Panes are views:
// a mirror reuses the Session its id already has, and closing a pane closes
// the Session only when no pane still shows it. (One stream per pane hit
// HTTP/1.1's six-connections-per-host limit at six mirrors and hung every
// request after it.)
const sessionFor = (sid) => {
  const p = ui.panes.value.find((x) => x.sid === sid);
  return p ? p.s : new Session(sid, { toast, cap: (id) => capped(id, null, true), seen: (s) => !!focused.peek() && focused.peek().s === s });
};

// The session a bare `:open` means. A door serves one session, DOOR. Under the
// perch there is no `door` session: it is the focused pane's session (while an xpane has focus, the chat the inspector shows), else none.
function defaultSid() {
  if (mode() !== 'perch') return DOOR;
  const p = inspected.peek();
  return p ? p.sid : null;
}

// The browser allows six connections per host (shared by every tab of the
// profile) and every session that follows a stream holds one, so under the
// perch at most MAX_SIDS distinct sessions hold a stream at once (PLAN G7),
// fewer by what extraStreams says other streams hold (the terminal stream
// takes one: core/registry.js). Only panes that hold a stream count: a stopped,
// kept, dead, gone, refused or goodbye'd pane holds none (Session.holdsStream).
// A mirror of such a session is always allowed. `except` is a pane that is about
// to stop showing its session.
const holdingSids = (except) => new Set(ui.panes.peek().filter((x) => x !== except && x.s.holdsStream()).map((x) => x.sid));
function capped(sid, except, opens = false) {
  if (mode() !== 'perch') return false;
  const panes = ui.panes.peek().filter((x) => x !== except);
  // `opens`: the session is about to open a stream of its own (a resume), so a pane already showing it is no excuse.
  if (!opens && panes.some((x) => x.sid === sid)) return false;
  if (!overCap(holdingSids(except), sid)) return false;
  const cap = chatCap();
  toast(`${cap} chat${cap > 1 ? 's' : ''} can be open at once${cap < MAX_SIDS ? ' while the terminal stream is open' : ''}: the browser allows 6 connections to the server across all its tabs; close or stop one first`, true);
  return true;
}

// A new pane on `sid`, splitting pane `at` (default: the focused one) to the
// right (`dir` 'row'), below ('col'), or along its longer side.
function openPane(sid = defaultSid(), { at = ui.focus.value, dir } = {}) {
  if (!sid) { toast('which session? :open <id>'); return null; }
  if (capped(sid)) return null;
  const id = 'p' + ++paneSeq;
  // queue: messages held here until the turn ends (panes/session.js
  // Outbox); kept by the pane, so a reconnect keeps them.
  const p = { id, sid, s: sessionFor(sid), queue: signal([]), editing: signal(null) };
  batch(() => {
    ui.panes.value = [...ui.panes.value, p];
    ui.tree.value = tile.add(ui.tree.value, at, id, ui.box.value, dir);
    ui.focus.value = id;
  });
  requestAnimationFrame(() => p.focusComposer && p.focusComposer());
  return p;
}

function mirrorPane(id = ui.focus.value, dir) {
  const p = byId(id);
  if (!p) { openPane(undefined, { dir }); return; } // no chat has focus (none, or an xpane): the chat the inspector shows, beside the focused pane
  openPane(p.sid, { at: p.id, dir });
}

// A pane of a registered kind (core/registry.js paneKinds), beside pane `at` (default the focused one, of either kind). It takes focus, as a
// new chat pane does. `data` is kept on the pane ({ id, kind, ...data }); `id` and `kind` are not data.
function openXPane(kind, data = {}, { at = ui.focus.value, dir } = {}) {
  if (!paneKinds.get(kind)) { toast(`no such pane kind: ${kind}`, true); return null; }
  const x = { ...data, id: 'p' + ++paneSeq, kind };
  const next = addXPane({ xpanes: ui.xpanes.peek(), tree: ui.tree.peek(), focus: ui.focus.peek() }, x, { at, box: ui.box.peek(), dir });
  batch(() => { ui.xpanes.value = next.xpanes; ui.tree.value = next.tree; ui.focus.value = next.focus; });
  return x;
}

// An xpane leaves the tiles: its kind's close(p) runs once (a terminal goes back to its stack), the tree promotes its sibling.
function closeXPane(x) {
  paneInfo(x, 'close');
  const next = dropXPane({ xpanes: ui.xpanes.peek(), tree: ui.tree.peek(), focus: ui.focus.peek() }, x.id);
  batch(() => { ui.xpanes.value = next.xpanes; ui.tree.value = next.tree; ui.focus.value = next.focus; });
}

// Open a session the way a click means it: show it in the focused pane (or focus
// the pane that already shows it), and open the first pane when there is none.
// Splitting is its own act: `beside`, the frame glyphs, `:vsplit` / `:split`.
function openSession(sid = defaultSid(), { beside = false } = {}) {
  if (!sid) { toast('which chat? :open <id>'); return null; }
  const f = focused.peek();
  if (beside || !f) return openPane(sid);
  const there = ui.panes.peek().find((x) => x.sid === sid);
  if (there) { focusPane(there.id); requestAnimationFrame(() => { const p = byId(there.id); if (p && p.focusComposer) p.focusComposer(); }); return there; }
  showIn(f.id, sid);
  requestAnimationFrame(() => { const p = byId(f.id); if (p && p.sid === sid && p.focusComposer) p.focusComposer(); });
  return null;
}

// Show session `sid` in pane `id` instead of the one it shows. The pane is
// drawn afresh (Tiles keys it by id and sid); the old Session closes when no
// pane still shows it. Messages queued in the pane are for the session it
// showed: switching drops them, so it asks first.
function showIn(id, sid, sure = false) {
  const p = byId(id);
  if (!p || p.sid === sid) return;
  if (capped(sid, p)) return;
  const n = p.queue.peek().length;
  if (n && !sure) {
    const el = document.querySelector(`.pane[data-pane="${id}"] .composer`);
    openMenu([
      { label: `drop ${n} queued and switch`, on: () => showIn(id, sid, true) },
      { label: 'keep them here', on: () => {} },
    ], el ? under(el) : { x: innerWidth / 2, y: innerHeight / 2, x2: innerWidth / 2, y2: innerHeight / 2 }, { title: `${n} queued message${n > 1 ? 's' : ''}` });
    return;
  }
  const next = { ...p, sid, s: sessionFor(sid), queue: signal([]), editing: signal(null) };
  ui.panes.value = ui.panes.value.map((x) => (x === p ? next : x));
  if (!ui.panes.value.some((x) => x.s === p.s)) p.s.close();
}

function closePane(id = ui.focus.value) {
  const x = xById(id);
  if (x) { closeXPane(x); return; }
  const p = byId(id);
  if (!p) return;
  const last = !ui.panes.value.some((x) => x !== p && x.s === p.s);
  if (last) p.s.close();
  batch(() => {
    ui.panes.value = ui.panes.value.filter((x) => x !== p);
    ui.tree.value = tile.remove(ui.tree.value, id);
    const rest = tile.leaves(ui.tree.value);
    if (ui.focus.value === id) ui.focus.value = rest[rest.length - 1] || null;
  });
  toast(last ? 'pane closed; the chat keeps running. :open shows it again'
    : 'pane closed; another pane still shows this chat');
}

const focusPane = (id) => { if (ui.focus.value !== id) ui.focus.value = id; };
const withPane = (fn) => () => { const p = focused.value; if (p) fn(p); else toast('no pane focused'); };
const cancel = withPane((p) => p.s.cancel());
// A resume may start a paid turn, so one bare `r` only asks: the same key again
// within R_CONFIRM_MS (on the same session) does it. The banner click and
// `:resume` are deliberate already and resume at once.
const R_CONFIRM_MS = 3000;
let rArmed = null; // { s, until }
function resumeKey() {
  const p = focused.peek();
  if (!p || !p.s.canResume()) return false;
  if (rArmed && rArmed.s === p.s && performance.now() < rArmed.until) { rArmed = null; resumeFocused(); return true; }
  rArmed = { s: p.s, until: performance.now() + R_CONFIRM_MS };
  ui.hint.value = { mode: 'toast', text: 'press r again to resume (it may continue the last turn), or click resume' };
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { rArmed = null; if (ui.hint.value && ui.hint.value.mode === 'toast') ui.hint.value = null; }, R_CONFIRM_MS);
  return true;
}
const resumeFocused = () => {
  const p = focused.value;
  if (!p) return toast('no pane focused', true);
  if (!p.s.canResume()) return toast(mode() === 'perch' ? 'this session is not resumable' : 'resume needs the perch', true);
  Promise.resolve(p.s.resume()).then(refresh);
};
// Resume session `sid` from a menu: open a pane on it if none shows it, wait for
// the pane to learn it is resumable (kept / dead), then resume. Only on this click.
async function resumeSid(sid) {
  let p = ui.panes.peek().find((x) => x.sid === sid);
  if (!p) { openSession(sid); p = ui.panes.peek().find((x) => x.sid === sid); }
  if (!p) return;
  for (let i = 0; i < 40 && !p.s.canResume(); i++) await new Promise((r) => setTimeout(r, 250));
  if (!p.s.canResume()) { toast('that session is not resumable now', true); return; }
  await p.s.resume();
  refresh();
}
const toComposer = withPane((p) => p.focusComposer && p.focusComposer());

// Every open session once, as [sid, Session].
const sessions = () => [...new Map(ui.panes.value.map((p) => [p.sid, p.s]))];
// A chat is named by the title the perch gives it (a name by hand, else its first message, else its birth name); the id is on
// hover. Before the list has the row it is its project's name or `chat`: never the log's stem, which is a stamp.
const sessionName = (s) => {
  const r = list.value.find((x) => x.id === s.sid);
  if (r && r.title) return r.title;
  const hl = s.hello.value;
  return (hl && project(hl.cwd)) || 'chat';
};
// A session's project is its working directory, named by its last part.
// Its project's name; an inbox folder has none (it is the chat's own); another folder is named by its last part.
const project = (cwd) => {
  const p = projectFor(cwd);
  if (p) return p.name;
  return isScratch(cwd) ? '' : String(cwd || '').split('/').filter(Boolean).pop() || cwd || '?';
};

// ---------------------------------------------------------------- the ask queue
// Every pending ask on the page is a tab of the one ask panel (panes/asks.js),
// oldest first, once however many panes show its session. The panel sits
// above the composer of the pane that shows the current tab: the focused
// pane if it shows that session, else the first that does.
// The oldest ask not yet answered takes focus by itself, once (web-ui.md 7.1),
// and only:
//   - when what has focus holds no text: a composer or the `:` line with
//     anything typed in it keeps focus for as long as it holds text, and the
//     status line says `▶ N waiting` (Tab, a click on it, its tab or its
//     marker go there);
//   - after PAUSE_MS with no typing in a composer or the `:` line, and not
//     while a menu is open or the panel holds focus on an unanswered ask.
// Answering the current tab moves on to the oldest unanswered one. When none
// is left, focus goes back to the composer it came from (a blurred textarea
// keeps its caret). Esc in the panel goes back to the composer and leaves the
// ask pending; it never takes focus by itself again, and the asks behind it
// wait too, until it is answered: `N waiting`, Tab, the tabs and the markers
// reach them.
const PAUSE_MS = 1000;
const tabs = computed(() => {
  const seen = new Set(), out = [];
  for (const p of ui.panes.value) {
    if (seen.has(p.s)) continue;
    seen.add(p.s);
    for (const a of p.s.asks.value) out.push({ s: p.s, a });
  }
  return out.sort((x, y) => x.a.n - y.a.n);
});
const queue = computed(() => tabs.value.filter((x) => !x.a.chosen.value)); // not yet answered
const unsent = computed(() => tabs.value.filter((x) => !x.a.busy.value).length);

// Send every recorded answer, back to back in queue order (the door answers
// one call at a time), once none is left unanswered. One batch at a time.
// Each ask is looked up by id when its turn comes (a reconnect replaces the
// objects); the first refusal stops the batch, so none is sent out of order.
let sending = false;
function submitAsks() {
  if (sending) return true;
  const todo = tabs.peek().filter((x) => x.a.chosen.peek() && !x.a.busy.peek()).map((x) => ({ s: x.s, id: x.a.id }));
  if (!todo.length || queue.peek().length) return false;
  sending = true;
  (async () => {
    let sent = 0;
    try {
      for (const [i, x] of todo.entries()) {
        const a = x.s.asks.peek().find((y) => y.id === x.id);
        if (!a) continue; // settled meanwhile
        const r = await x.s.send(a);
        if (r.status === 204 || r.status === 409 || r.status === 404 || r.status === 0) { sent += r.status === 204; continue; }
        toast(`sent ${sent} of ${todo.length}; ask ${i + 1} refused: ${r.why}`, true);
        const t = tabs.peek().find((y) => y.a === a);
        if (t) focusQueued(t);
        return;
      }
    } finally { sending = false; }
  })();
  return true;
}
const current = computed(() => {
  const t = ui.tab.value, all = tabs.value;
  return all.find((x) => t && x.a === t.a) || queue.value[0] || all[0] || null;
});
const panelPane = computed(() => {
  const c = current.value, f = focused.value;
  if (!c) return null;
  const p = f && f.s === c.s ? f : ui.panes.value.find((x) => x.s === c.s);
  return p ? p.id : null;
});
// Asks that had their one automatic focus, by session and ask id, so a
// reconnect's replayed copy of an ask does not take focus again.
const taken = new Set();
const tag = (s, a) => s.sid + ' ' + a.id;
const LAND_TRIES = 20; // steer's checks (150 ms apart) that focus landed on the head, before it gives up until the queue changes
let typedAt = 0, steerT = 0, tries = 0, returnTo = null, owed = false;

const inPanel = () => { const at = document.activeElement; return !!(at && at.closest && at.closest('.askpanel')); };
const holdsText = (el) => el && (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT') && !el.closest('.ask') && el.value !== '';

// Make `q` the current tab and give the panel focus (at once when it is
// drawn, else as soon as it is).
function focusQueued(q, auto = false) {
  const p = ui.panes.peek().find((x) => x.s === q.s);
  if (!p) return false;
  panel.armed.value = !auto;
  const at = document.activeElement;
  if (at && ((at.tagName === 'TEXTAREA' && at.closest('.composer')) || focusClaimed(at))) returnTo = at; // a composer, or a place a feature owns (a terminal)
  owed = true;
  batch(() => {
    ui.tab.value = q;
    const f = focused.peek();
    if (!f || f.s !== q.s) focusPane(p.id);
  });
  // The panel takes focus as soon as it draws this tab where it now belongs;
  // at once if it already does (it may be about to move to another pane).
  panel.want = true;
  const el = panel.el, host = el && el.isConnected && el.closest('.pane');
  if (host && host.dataset.pane === panelPane.peek() && current.peek() && current.peek().a === q.a) {
    panel.want = false;
    el.focus({ preventScroll: true });
  }
  return true;
}

function steer(poll) {
  clearTimeout(steerT);
  const head = queue.peek()[0], cur = current.peek(), here = inPanel();
  if (!head) { ui.held.value = false; if (!tabs.peek().length) giveBack(); return; } // answered but not settled: it may yet be refused
  if (here && cur && cur.a === head.a) { taken.add(tag(head.s, head.a)); ui.held.value = false; tries = 0; return; } // landed
  if (here && cur && cur.a.chosen.peek()) { ui.held.value = false; focusQueued(head); return; } // answered: on to the next
  if (taken.has(tag(head.s, head.a))) { ui.held.value = false; return; }
  const at = document.activeElement;
  const wait = typedAt + PAUSE_MS - performance.now();
  const busy = holdsText(at) || menu.peek() || ui.cmd.peek() != null || here || focusClaimed(at); // focus a feature owns (a terminal) is never taken
  if (wait > 0 || busy) {
    ui.held.value = true;
    steerT = setTimeout(() => steer(true), Math.max(wait, 150));
    return;
  }
  ui.held.value = false;
  if (!poll) tries = 0;
  if (tries++ >= LAND_TRIES) { taken.add(tag(head.s, head.a)); return; }
  focusQueued(head, true); // unarmed: the operator did not ask for it
  steerT = setTimeout(() => steer(true), 150); // did it land?
}
effect(() => { queue.value; tabs.value; steer(); });

function giveBack() {
  const back = returnTo, at = document.activeElement;
  returnTo = null;
  if (!owed) return;
  owed = false;
  const lost = !at || at === document.body || at.closest('.ask') || at.classList.contains('pane');
  if (!lost) return;
  if (back && back.isConnected) back.focus(); else toComposer();
}

// A key typed into an unarmed ask was meant for the composer: put it there,
// at the caret, and give focus back. The ask does not take focus again.
function bounce(p, key) {
  const cur = current.peek();
  if (cur) taken.add(tag(cur.s, cur.a));
  leaveAsk(p);
  const el = document.activeElement;
  if (!el || el.tagName !== 'TEXTAREA') return;
  const a = el.selectionStart, b = el.selectionEnd;
  if (key === 'Backspace') { if (a !== b) el.setRangeText('', a, b, 'end'); else if (a > 0) el.setRangeText('', a - 1, a, 'end'); }
  else el.setRangeText(key, a, b, 'end');
  el.dispatchEvent(new Event('input', { bubbles: true }));
  typedAt = performance.now();
}

// The asks session `s` holds now take no focus by themselves (their turn is being cancelled).
const letBe = (s) => { for (const a of s.asks.peek()) taken.add(tag(s, a)); };

function focusHead() {
  const head = queue.peek()[0];
  return head ? focusQueued(head) : false;
}

// Esc in the panel: back to the composer the ask took focus from, else the pane's own.
function leaveAsk(p) {
  const back = returnTo;
  owed = false;
  if (back && back.isConnected) back.focus(); else if (p.focusComposer) p.focusComposer();
}

// The next (previous) tab, from the current one when the panel has focus,
// else from the oldest unanswered.
function cycleAsk(dir = 1) {
  const t = tabs.peek();
  if (!t.length) return false;
  if (!inPanel()) return focusHead() || focusQueued(t[0]);
  const at = t.findIndex((x) => current.peek() && x.a === current.peek().a);
  return focusQueued(t[(at + dir + t.length) % t.length]);
}

// Open or close one side's panel (on `tab`, when given); on a narrow screen opening one closes the other.
function toggle(rail, tab) {
  const left = rail === 'lrail';
  batch(() => {
    if (tab) ui[left ? 'left' : 'right'].value = tab;
    ui[rail].value = !ui[rail].value;
    if (ui.narrow.value && !ui[rail].value) ui[left ? 'rrail' : 'lrail'].value = true;
  });
}
// A rail icon: a closed panel opens on that tab, the open tab's icon closes it, another tab's icon switches.
function railPick(side, name) {
  const rail = side === 'l' ? 'lrail' : 'rrail', cur = side === 'l' ? ui.left : ui.right;
  if (ui[rail].peek()) return toggle(rail, name);
  if (cur.peek() === name) return toggle(rail);
  cur.value = name;
}
function cycleRight(d) {
  ui.right.value = cycleTab(tabsOn('r').map((t) => t.name), ui.right.value, d);
}

// ---------------------------------------------------------------- hint line
// `act` ({ label, on }) puts one clickable word after the text, as `[ show ]`; a toast with one stays as long as an error does.
function toast(text, err, act) {
  if (ui.hint.value && ui.hint.value.mode === 'menu') return;
  ui.hint.value = { mode: 'toast', text, err, act };
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { if (ui.hint.value && ui.hint.value.mode === 'toast') ui.hint.value = null; }, err || act ? 7000 : 3500);
}

// The key reference (`?`), as the TUI's (`space ?`): every key by where it
// works, the commands last. The first section is what the keys do where
// focus is now. Each entry is clickable and does what its key does; an
// entry under `asks` takes you to the ask panel, where the key works.
const KEYREF = [
  ['anywhere', [
    ['i', 'write in the focused pane', toComposer],
    [':', 'a command (tab completes)', () => openCmd()],
    ['ctrl+⏎', 'new pane with a picker: new / resume / existing / mirror (perch)', () => picker()],
    ['tab ⇧tab', 'next / previous waiting ask', () => cycleAsk(1)],
    ['^c', 'stop the turn', cancel],
    ['r r', 'resume an ended session: press twice (perch; it may continue the last turn)', () => resumeFocused()],
    ['?', 'this reference', () => {}],
    ['esc', 'close a menu or this', () => {}],
  ]],
  ['composer', [
    ['⏎', 'send; steer while a turn runs', toComposer],
    ['⌥⏎', 'queue behind the turn (held here, editable)', toComposer],
    ['↑', 'edit the last queued (composer empty)', toComposer],
    ['⇧⏎', 'a newline', toComposer],
    ['tab', 'the oldest waiting ask', () => focusHead()],
    ['^c', 'stop (composer empty)', cancel],
    ['esc', 'leave the composer', () => document.activeElement && document.activeElement.blur()],
  ]],
  ['asks', [
    ['↑↓ j k', 'move between options', focusHead],
    ['⏎', 'take the highlighted option', focusHead],
    ['y n', 'yes / no', focusHead],
    ['1-9', "a question's option", focusHead],
    ['c', 'chat about it', focusHead],
    ['tab', 'a note with the answer', focusHead],
    ['s  :submit', 'send every answer (⏎ on the last one does too)', () => submitAsks()],
    ['←→ h l ⇧tab', 'previous / next ask', focusHead],
    ['esc', 'back to the composer, the ask waits', focusHead],
  ]],
  ['panes', [
    ['⌥w  :q', 'close the pane', () => closePane()],
    [':vsplit :split', 'mirror right / down', () => mirrorPane(undefined, 'row')],
    ['^b', 'the sessions sidebar (the left rail alone: compact)', () => toggle('lrail')],
    ['^i', 'the inspector (the right rail alone: compact)', () => toggle('rrail')],
    ['[ ]', 'inspector tab', () => cycleRight(1)],
    [':term :edit :files', "open a terminal / editor / file manager at the session's folder (or a path)", () => openCmd()],
    ['/', 'filter files (files tab)', () => { ui.rrail.value = false; ui.right.value = 'files'; requestAnimationFrame(() => { const f = document.querySelector('#right .filter input'); if (f) f.focus(); }); }],
  ]],
];

const keyRef = () => { ui.keys.value = !ui.keys.peek(); };
// The reference as drawn: the rows above, with what features registered added or put in place of a row (core/registry.js keyRows).
const keyGroups = computed(() => mergeKeyRows(KEYREF, keyRows.list));

// ---------------------------------------------------------------- the : line
// The page's own commands. What a feature registers (core/registry.js commands) is merged over them: a registered name REPLACES its entry
// here, and the boot log says which.
const BASE_COMMANDS = {
  q: ['close the pane', () => closePane()],
  close: ['close the pane', () => closePane()],
  open: ['show chat <id> in the focused pane (no id: this door / the focused chat)', (arg) => (arg ? openSession(arg) : openPane())],
  vsplit: ['mirror the focused pane to the right', () => mirrorPane(undefined, 'row')],
  split: ['mirror the focused pane below', () => mirrorPane(undefined, 'col')],
  stop: ['cancel the running turn', cancel],
  submit: ['send every answered ask (all must be answered)', () => submitAsks() || toast(queue.peek().length ? 'answer every ask to submit' : 'nothing to submit')],
  queue: ['queue text (or the composer) behind the turn', (arg) => withPane((p) => p.s.submit('queue', arg || ''))()],
  reconnect: ['drop the stream and replay', withPane((p) => p.s.reconnect())],
  new: ["start a chat: :new <folder> [model] [manual|auto|yolo] (no folder: a chat in the inbox; no mode: the home's default_mode, else yolo)", (arg) => newCmd(arg)],
  rename: ['rename this chat: :rename <name> (none: clear it)', (arg) => withPane((p) => renameChat(p.sid, arg))()],
  resume: ['resume a stopped, dead or kept session (perch); may continue its last turn', () => resumeFocused()],
  sidebar: ['toggle the left sidebar', () => toggle('lrail')],
  inspector: ['toggle the inspector', () => toggle('rrail')],
  keys: ['the key reference', () => keyRef()],
  theme: ['colours: :theme <name> (none: list them)', (arg) => pickLook('theme', arg)],
  term: ["open a terminal at the focused session's folder (on the door's machine)", withPane((p) => p.s.launch('terminal'))],
  edit: ["open an editor at the session's folder, or at <path>", (arg) => withPane((p) => p.s.launch('editor', arg || undefined))()],
  files: ["open a file manager at the session's folder, or at <path>", (arg) => withPane((p) => p.s.launch('files', arg || undefined))()],
  font: ['font: :font <name> (none: list them)', (arg) => pickLook('font', arg)],
};
const cmdTable = computed(() => mergeCommands(BASE_COMMANDS, commands.list).table); // { name: [help, run] }, base order then registered

function pickLook(what, arg) {
  const names = what === 'theme' ? themes.value : Object.keys(FONTS);
  const cur = (what === 'theme' ? theme : font).value;
  if (!arg) { toast(`${what}s: ${names.map((n) => (n === cur ? `[${n}]` : n)).join(' · ')}`); return; }
  const set = what === 'theme' ? setTheme : setFont;
  set(resolve(names, arg)).then((err) => err && toast(err, true));
}

function openCmd(prefill = '') {
  closeMenu();
  batch(() => { ui.cmd.value = prefill; ui.hint.value = { mode: 'menu' }; });
}
function closeCmd() {
  batch(() => { ui.cmd.value = null; if (ui.hint.value && ui.hint.value.mode === 'menu') ui.hint.value = null; });
}
// What an empty match says: the `:` line has the page's own commands (and the ones features register); the door has no list of its own.
const NO_CMD = "no such command; these are the page's own commands (eidolon's web door lists none)";
const matches = (text) => {
  const w = (text || '').trim().split(/\s+/)[0] || '';
  return Object.keys(cmdTable.value).filter((c) => c.startsWith(w));
};
function runCmd(text = ui.cmd.value || '') {
  const m = /^\s*(\S+)\s*([\s\S]*)$/.exec(text), w = m && m[1], rest = m ? m[2].trim() : ''; // the argument keeps its inner spacing (a quoted folder)
  closeCmd();
  if (!w) return;
  const c = cmdTable.value[w]; // no prototype: `:constructor` is no command
  if (c) c[1](rest);
  else toast(`:${w}: ${NO_CMD}`, true);
}

// ---------------------------------------------------------------- menus
// What a split opens. Door mode: only a mirror (and the sessions already open).
// Under the perch it also offers every live session and the newest kept ones;
// picking opens a pane and nothing more (a resume is the pane's own banner
// action, so a pick never spends by itself).
const KEPT_PICK = 15;
const ageOf = (r) => age(r.mtime);
function livePicks(open) {
  const rs = live(list.peek());
  return rs.length ? rs.map((r) => ({ label: label(r), tick: ui.panes.peek().some((x) => x.sid === r.id), on: () => open(r.id) })) : null;
}
function keptPicks(open) {
  const rs = keptRows(list.peek()).slice(0, KEPT_PICK);
  return rs.length ? rs.map((r) => ({ label: `${label(r)}  ${ageOf(r)}`, hint: r.state === 'dead' ? 'exited' : '', on: () => open(r.id) })) : null;
}

// ---------------------------------------------------------------- new sessions
// `POST /hub/sessions {cwd, model?}` (perch only). The perch waits for the
// door's hello (up to ~20 s), so a hint stays up meanwhile and a second start
// in the same folder does nothing.
const HOME_RE = /^(\/home\/[^/]+|\/root)(?=\/|$)/;
const starting = new Set();
function homeOf() {
  const cwds = [...ui.panes.peek().map((x) => { const h = x.s.hello.value; return h && h.cwd; }), ...list.peek().map((r) => r.cwd)];
  for (const c of cwds) { const m = HOME_RE.exec(c || ''); if (m) return m[1]; }
  return null;
}
// The folder as typed: absolute, or `~` / `~/x` under a home taken from a known cwd.
function absCwd(raw) {
  let c = String(raw || '').trim();
  if (c === '~' || c.startsWith('~/')) {
    const home = homeOf();
    if (!home) { toast('give an absolute folder', true); return null; }
    c = home + c.slice(1);
  }
  if (!c.startsWith('/')) { toast('give an absolute folder', true); return null; }
  return c.length > 1 ? c.replace(/\/+$/, '') || '/' : c;
}
const focusedHello = () => { const p = focused.peek(); return (p && p.s.hello.value) || null; };
const focusedCwd = () => (focusedHello() || {}).cwd || '';

// `how.open(id)` shows the new session (default: a new pane beside the focused
// one); `how.except` is the pane that stops showing its session (the cap).
// `raw` null starts a chat with no folder: the perch makes its own folder in the inbox (201).
// A perch with no home answers 400, which gets a plain sentence. Resolves true
// when a session started. No model is sent unless the person asked for one for
// THIS chat (`:new <folder> <model>`): the default is the Minerva config's
// (minerva.toml, then the member's, then the built-in), never the focused pane's.
async function newSession(raw, model, how, chosen) {
  if (mode() !== 'perch') { toast(HUB, true); return false; }
  const bare = raw == null, cwd = bare ? null : absCwd(raw);
  if (!bare && !cwd) return false;
  const key = cwd || '\0chat';
  if (starting.has(key)) return false;
  if (capped('\0new', how && how.except)) return false; // refuse first, spawn nothing
  const name = bare ? 'a new chat' : tildify(cwd);
  starting.add(key);
  clearTimeout(toastTimer);
  const mine = { mode: 'toast', text: `starting ${bare ? 'a chat' : 'a chat in ' + name}…` };
  ui.hint.value = mine;
  home.busy.value = true;
  home.msg.value = '';
  const body = { ...(cwd ? { cwd } : {}), ...(model ? { model } : {}), ...(chosen ? { mode: chosen } : {}) };
  const r = await hubPost('sessions', body);
  starting.delete(key);
  home.busy.value = false;
  if (ui.hint.peek() === mine) ui.hint.value = null;
  if (r.status === 201 && r.data && r.data.id) {
    (how && how.open ? how.open : (id) => openSession(id))(r.data.id);
    refresh();
    return true;
  }
  const fail = (text) => { home.msg.value = text; toast(text, true); return false; };
  if (bare && r.status === 400) return fail('a chat without a folder is not available on this server yet: use [ open folder… ] and name one');
  if (r.status === 422) return fail(`${name} is outside the folders this server may open`);
  if (r.status === 503) return fail(`the chat did not start: ${(r.data && r.data.error) || 'no reason given'}`);
  if (r.status === 401) return fail('the server refused the login for this page (it restarted?). Run `hoot` again.');
  if (r.status === 0) return fail('the server is not answering');
  return fail(`could not start ${bare ? 'a chat' : 'a chat in ' + name}${r.data && r.data.error ? ': ' + r.data.error : ' (' + r.status + ')'}`);
}

// The start screen's actions (panes/home.js). A folder may be pasted from Windows.
const homeAct = {
  chat: () => newSession(null, undefined),          // always a new inbox chat, never the focused pane's
  project: (p) => newSession(p.path, undefined),
  newProject: () => newProject(),
  forget: (e, p) => forgetMenu(e, p),
  open: (r, beside) => openSession(r.id, { beside }),
};

// `[ new project… ]`: the folder picker, then a name (prefilled), then the project is saved and a chat starts in it.
function newProject() {
  if (mode() !== 'perch') { toast(HUB, true); return; }
  closeMenu();
  openPicker({
    title: 'new project',
    naming: true,
    onDone: async ({ path, name }) => {
      const r = await addProject(path, name);
      if (!r.ok) return r.why;
      newSession(r.project.path, undefined);
      return '';
    },
  });
}

// A project leaves the list; its folder and its chats stay.
function forgetMenu(e, p, at) {
  e.preventDefault();
  openMenu([
    { label: 'new chat here', on: () => newSession(p.path, undefined) },
    { label: 'rename…', hint: 'the label; the folder keeps its name', on: () => startRename('project', p.path) },
    { label: 'remove from the list', hint: 'the folder is not touched', on: async () => { toast((await forgetProject(p)) ? `removed ${p.name}; its folder is untouched` : 'could not remove it', false); } },
  ], at || atPointer(e), { title: p.name });
}

// ---- names: a chat's, or a project's, edited in its own row. Enter saves, Esc (or leaving the field) cancels. A name by hand beats the
// title taken from the first message; an empty one clears it. An inbox chat's folder follows the name (the perch, between turns).
const renaming = signal(null); // { kind: 'chat' | 'project', key } of the row being edited
function startRename(kind, key) {
  closeMenu();
  if (mode() !== 'perch') { toast(HUB, true); return; }
  batch(() => { ui.lrail.value = false; if (!(kind === 'project' && ui.left.peek() === 'projects')) ui.left.value = 'sessions'; renaming.value = { kind, key }; });
}
const BAD_LOGIN = 'the server refused the login for this page (it restarted?). Run `hoot` again.';
async function renameChat(sid, text) {
  if (mode() !== 'perch') { toast(HUB, true); return; }
  const was = list.peek().find((x) => x.id === sid), title = String(text || '').trim();
  if (was && title === label(was)) return;
  const res = await hubPost(`sessions/${encodeURIComponent(sid)}/rename`, { title });
  if (res.status === 200) toast(title ? `renamed to ${title}` : 'name cleared: the chat is called by its first message again');
  else if (res.status === 400) toast((res.data && res.data.error) || 'that name will not do', true);
  else if (res.status === 403) toast("that chat belongs to eidolon's own folder: it cannot be renamed here", true);
  else if (res.status === 404) toast('that chat is gone', true);
  else if (res.status === 401) toast(BAD_LOGIN, true);
  else toast(`rename failed: ${res.status || 'network'} ${(res.data && res.data.error) || ''}`, true);
  refresh();
}
async function renameProj(p, text) {
  const name = String(text || '').trim();
  if (!name || name === p.name) return;
  const r = await renameProject(p, name);
  toast(r.ok ? `renamed to ${name}; the folder keeps its name` : r.why, !r.ok);
}
// The field in a row. Its text is set once, so the 5 s list poll redrawing the row never overwrites what is being typed.
function EditName({ text, name, save }) {
  const ref = useRef(null), done = useRef(false);
  useLayoutEffect(() => { const el = ref.current; if (el) { el.value = text; el.focus(); el.select(); } }, []);
  const end = (commit) => {
    if (done.current) return;
    done.current = true;
    const value = ref.current ? ref.current.value : '';
    renaming.value = null;
    if (commit) save(value);
  };
  return html`<input class="ren" type="text" ref=${ref} spellcheck="false" autocomplete="off" aria-label=${name} maxlength="120"
    onKeyDown=${(e) => { e.stopPropagation(); if (e.key === 'Enter') { e.preventDefault(); end(true); } else if (e.key === 'Escape') { e.preventDefault(); end(false); } }}
    onClick=${(e) => e.stopPropagation()} onBlur=${() => end(false)} />`;
}

// `make it a project…`: the folder picker in "choose a parent" mode, with a name prefilled from the title. The perch MOVES the chat's
// inbox folder to <parent>/<name>, registers it as a project, and reopens the chat in it (between turns).
function promoteChat(r) {
  closeMenu();
  if (mode() !== 'perch') { toast(HUB, true); return; }
  const prefill = slugOf(label(r)) || String(r.cwd || '').split('/').filter(Boolean).pop() || 'project';
  openPicker({
    title: `make "${label(r)}" a project`,
    naming: true, parent: true, prefill, doneLabel: '[ move it here ]',
    onDone: async ({ path, name }) => {
      const res = await hubPost(`sessions/${encodeURIComponent(r.id)}/promote`, { parent: path, name });
      if (res.status === 200) {
        await loadProjects();
        refresh();
        toast(res.data && res.data.warning ? res.data.warning : `${label(r)} is now the project ${name}`, !!(res.data && res.data.warning));
        return '';
      }
      if (res.status === 401) return BAD_LOGIN;
      if (res.status === 0) return 'the server is not answering';
      return (res.data && res.data.error) || `could not make it a project (${res.status})`;
    },
  });
}

// A folder with a space goes in double (or single) quotes: `:new "/home/u/My Project" model`.
const quoteArg = (s) => (/[\s"']/.test(s) ? '"' + s.replace(/"/g, '') + '"' : s);
const projectCwd = () => (isScratch(focusedCwd()) ? '' : focusedCwd());
const newLine = () => 'new ' + quoteArg(projectCwd()) + (projectCwd() ? ' ' : '');
function words(s) {
  const out = [], re = /"([^"]*)"|'([^']*)'|(\S+)/g;
  let m;
  while ((m = re.exec(String(s || '')))) out.push(m[1] ?? m[2] ?? m[3]);
  return out;
}
function newCmd(arg) {
  const w = words(arg);
  if (mode() !== 'perch') { toast(HUB, true); return; }
  if (!w.length) { newSession(null, undefined); return; }
  // A last word that is a mode is the mode (never the first word: that is the folder); no mode word is the home's default_mode.
  const chosen = w.length > 1 && ['manual', 'auto', 'yolo'].includes(w[w.length - 1]) ? w.pop() : undefined;
  if (w.length > 2) { toast('too many words: quote a folder that has a space, :new "/my dir" [model] [manual|auto|yolo]', true); return; }
  newSession(w[0], w[1], undefined, chosen); // a model only when typed
}

// `new session ▸`: the folders of open and live sessions (max 8), then any folder.
function newPick(how) {
  const shows = 'default model';
  // A new inbox chat, then the projects: the inbox folder of another chat is never offered again.
  const dirs = sorted(projects.peek()).slice(0, 12).map((p) => ({ label: p.name, hint: shows, on: () => newSession(p.path, undefined, how) }));
  return { label: 'new session', hint: shows, sub: [{ label: 'new chat', hint: shows, on: () => newSession(null, undefined, how) }, ...(dirs.length ? [{ sep: true }, ...dirs] : []), { sep: true }, { label: 'new project…', on: () => newProject() }] };
}

// `p` is the pane being split: a chat pane (it can be mirrored), an xpane (it cannot, but a split anchors on it all the same), or null
// (the picker with no pane focused). What features registered in splitExtras comes last.
function splitItems(p, dir) {
  const { chat, at: anchor } = anchorOf(p);
  const others = chat ? sessions().filter(([sid]) => sid !== chat.sid) : sessions();
  const mirror = chat && { label: 'mirror', hint: dir === 'row' ? ':vsplit' : ':split', on: () => openPane(chat.sid, { at: anchor, dir }) };
  const extra = splitExtraItems(p, dir), more = extra.length ? [{ sep: true }, ...extra] : [];
  if (mode() === 'perch') {
    const at = (sid) => openPane(sid, { at: anchor, dir });
    const lp = livePicks(at), kp = keptPicks(at);
    return [
      ...(chat ? [mirror] : []),
      newPick({ open: at }),
      kp ? { label: 'resume…', sub: kp } : { label: 'resume…', off: 'no kept sessions' },
      lp ? { label: 'existing session', sub: lp } : { label: 'existing session', off: 'no live sessions' },
      ...more,
    ];
  }
  return [
    ...(chat ? [mirror] : []),
    { label: 'new session', off: HUB },
    { label: 'resume…', off: HUB },
    others.length ? { label: 'existing session', sub: others.map(([sid, s]) => ({ label: sessionName(s), on: () => openPane(sid, { at: anchor, dir }) })) }
      : { label: 'existing session', off: HUB },
    ...more,
  ];
}

function showItems(p) {
  const open = sessions().map(([sid, s]) => (sid === p.sid ? { label: sessionName(s), tick: true, off: 'shown here' }
    : { label: sessionName(s), on: () => showIn(p.id, sid) }));
  if (mode() === 'perch') {
    const here = new Set(sessions().map(([sid]) => sid));
    const more = live(list.peek()).filter((r) => !here.has(r.id)).map((r) => ({ label: label(r), on: () => showIn(p.id, r.id) }));
    const kp = keptPicks((sid) => showIn(p.id, sid));
    return [
      ...open, ...more,
      { sep: true },
      newPick({ open: (sid) => showIn(p.id, sid), except: p }),
      kp ? { label: 'resume…', sub: kp } : { label: 'resume…', off: 'no kept sessions' },
    ];
  }
  return [...open, { sep: true }, { label: 'new session', off: HUB }, { label: 'resume…', off: HUB }];
}

// ctrl+enter: the split picker under the focused pane (along its longer side),
// or in the middle with none. Door mode keeps its toast.
function picker() {
  if (mode() !== 'perch') { toast("the picker needs Minerva's server: open it with hoot; :vsplit mirrors this one"); return; }
  const p = focusedAny.peek(); // a chat pane or an xpane
  const el = p && document.querySelector(`.pane[data-pane="${p.id}"]`);
  const at = el ? under(el) : { x: innerWidth / 2, y: innerHeight / 2, x2: innerWidth / 2, y2: innerHeight / 2 };
  openMenu(splitItems(p || null, longSide(p)), at, { title: 'new pane', owner: 'picker' });
}

// Which way a split of pane `p` (of either kind) goes: along its longer side, so panes stay near square (dwindle).
function longSide(p) {
  const el = p && document.querySelector(`.pane[data-pane="${p.id}"]`);
  const r = el && el.getBoundingClientRect();
  return r && r.height > r.width ? 'col' : 'row';
}

// A terminal, an editor or a file manager on the machine running the door,
// at the session's folder. Greyed when there is no door to ask.
const openItems = (p) => {
  const off = p.s.launchOff();
  return [['terminal', 'terminal here'], ['editor', 'editor here'], ['files', 'files here']]
    .map(([w, label]) => ({ label, off, on: () => p.s.launch(w) }));
};

const paneMenu = (p, at) => openMenu([
  { label: 'open', sub: openItems(p) },
  { label: 'split right', sub: splitItems(p, 'row') },
  { label: 'split down', sub: splitItems(p, 'col') },
  { label: 'show…', sub: showItems(p) },
  { label: 'mirror', on: () => mirrorPane(p.id) },
  { label: 'reconnect', hint: ':reconnect', on: () => p.s.reconnect() },
  { label: 'close', hint: '⌥w  :q', on: () => closePane(p.id) },
], at, { title: 'pane', owner: 'pane-' + p.id });

// The border's one split glyph: both sides, each with the same picker.
const splitMenu = (p, at) => openMenu([
  { label: 'split right', sub: splitItems(p, 'row') },
  { label: 'split down', sub: splitItems(p, 'col') },
], at, { title: 'split', owner: `split-${p.id}` });

// Stop a door of this perch, after asking. 202: stopping; 409: not ours / not live.
async function stopSession(sid, name) {
  const r = await hubPost(`sessions/${encodeURIComponent(sid)}/stop`);
  if (r.status === 202) toast(`stopping ${name}`);
  else if (r.status === 409) toast('that chat is not running here', true);
  else if (r.status === 401) toast('the server refused the login for this page (it restarted?). Run `hoot` again.', true);
  else toast(`stop failed: ${r.status || 'network'} ${(r.data && r.data.error) || ''}`, true);
  refresh();
  setTimeout(refresh, 1500); // the door takes a moment to exit
}
// A new chat copied from `sid`, whole (perch fork.rs); it opens in a pane beside the one showing the original, with a line saying where it came from.
async function forkSession(sid, name) {
  toast(`forking ${name}…`);
  const r = await hubPost(`sessions/${encodeURIComponent(sid)}/fork`, {});
  if (r.status === 201 && r.data && r.data.id) {
    const from = ui.panes.peek().find((x) => x.sid === sid);
    const p = from ? openPane(r.data.id, { at: from.id }) : openPane(r.data.id);
    if (p) p.s.notes = [`forked from ${name}`];
    toast(`forked ${name}`);
  } else if (r.status === 401) toast('the server refused the login for this page (it restarted?). Run `hoot` again.', true);
  else if (r.status === 404) toast('that chat is gone', true);
  else toast(`fork failed: ${r.status || 'network'} ${(r.data && r.data.error) || ''}`, true);
  refresh();
}
// A new chat that keeps the history of `sid` up to the reply `record` (perch pending.rs): a copy with no door yet, opened beside the
// original, whose first message is the turn that makes the branch stay. The perch says in words why a point cannot be forked from.
async function forkFrom(sid, record, name) {
  toast(`forking ${name} from here…`);
  const r = await hubPost(`sessions/${encodeURIComponent(sid)}/fork`, { at: record });
  if (r.status === 201 && r.data && r.data.id) {
    await refresh(); // the row says `pending` before the pane asks what to draw
    const from = ui.panes.peek().find((x) => x.sid === sid);
    const p = from ? openPane(r.data.id, { at: from.id }) : openPane(r.data.id);
    if (p) { p.s.notes = [`forked from ${name}, from a reply`]; setTimeout(() => p.focusComposer && p.focusComposer(), 400); }
    toast('forked: write its first message');
  } else if (r.status === 401) toast('the server refused the login for this page (it restarted?). Run `hoot` again.', true);
  else if (r.status === 404) toast('that chat is gone', true);
  else toast(r.data && r.data.error ? r.data.error : `fork failed: ${r.status || 'network'}`, true);
  refresh();
}
function copyId(id) {
  const ok = () => toast('copied ' + id), no = () => toast('could not copy; the id is ' + id, true);
  try { navigator.clipboard.writeText(id).then(ok, no); } catch { no(); }
}
const stopAsk = (sid, name, at) => openMenu([
  { label: 'stop', on: () => stopSession(sid, name) },
  { label: 'keep it', on: () => {} },
], at, { title: `stop ${name}? its panes will show "stopped"` });

// What features registered in chatMenuItems add at the end of a sidebar chat menu: they get the chat's list row (a pane's chat with no
// row yet: `{ id, cwd }`) and `{ p, at }`, the pane showing it (or null) and where the menu is, so a menu of their own can open there.
function chatExtras(row, p, at) {
  const more = chatMenuExtras(row, { p: p || null, at });
  return more.length ? [{ sep: true }, ...more] : [];
}

function sessionMenu(e, p, at0) {
  e.preventDefault();
  const f = focused.value, at = at0 || atPointer(e), perch = mode() === 'perch';
  const row = list.value.find((x) => x.id === p.sid), hl = p.s.hello.value;
  openMenu([
    { label: 'open beside', hint: 'new pane', on: () => openPane(p.sid) },
    { label: 'show in focused pane', off: !f ? 'no pane focused' : f.sid === p.sid ? 'shown there' : null, on: () => showIn(f.id, p.sid) },
    perch ? { label: 'fork (latest)', hint: 'new chat', on: () => forkSession(p.sid, sessionName(p.s)) } : { label: 'fork (latest)', off: HUB },
    perch ? { label: 'stop this chat', on: () => stopAsk(p.sid, sessionName(p.s), at) } : { label: 'stop this chat', off: HUB },
    ...markItems(row, at),
    ...chatExtras(row || { id: p.sid, cwd: hl ? hl.cwd : '' }, p, at),
  ], at, { title: sessionName(p.s) });
}

// A live session no pane shows: the same menu, by id.
function idleMenu(e, r, at0) {
  e.preventDefault();
  const f = focused.value, at = at0 || atPointer(e);
  openMenu([
    { label: 'open here', on: () => openSession(r.id) },
    { label: 'open beside', hint: 'new pane', on: () => openPane(r.id) },
    { label: 'fork (latest)', hint: 'new chat', on: () => forkSession(r.id, label(r)) },
    { label: 'stop this chat', on: () => stopAsk(r.id, label(r), at) },
    ...markItems(r, at),
    ...chatExtras(r, null, at),
  ], at, { title: label(r) });
}

// An older chat (eidolon's own folder): to read here or beside; continuing is the action in its pane.
function olderMenu(e, r, at0) {
  e.preventDefault();
  const at = at0 || atPointer(e);
  openMenu([
    { label: 'open here (read-only)', on: () => openSession(r.id) },
    { label: 'open beside', hint: 'new pane', on: () => openPane(r.id) },
    { label: 'fork (latest)', hint: 'a copy in this app', on: () => forkSession(r.id, label(r)) },
    { label: 'copy id', on: () => copyId(r.id) },
    ...chatExtras(r, null, at),
  ], at, { title: label(r) });
}

function keptMenu(e, r, at0) {
  e.preventDefault();
  const at = at0 || atPointer(e);
  openMenu([
    { label: 'open here', on: () => openSession(r.id) },
    { label: 'open beside', hint: 'new pane', on: () => openPane(r.id) },
    ...(r.pending ? [] : [
      { label: 'resume (may continue the last turn)', on: () => resumeSid(r.id) },
      { label: 'fork (latest)', hint: 'new chat', on: () => forkSession(r.id, label(r)) },
    ]),
    { label: 'copy id', on: () => copyId(r.id) },
    ...markItems(r, at),
    ...chatExtras(r, null, at),
  ], at, { title: label(r) });
}

// pin / archive / delete for a chat of this app (never eidolon's own folder, never one running in another app). A deleted chat
// goes to the trash group at the bottom of the sidebar, where it can be restored; a running one is stopped first.
function markItems(r, at) {
  if (!r || mode() !== 'perch' || r.origin || r.state === 'live-elsewhere') return [];
  const isLive = r.state === 'live', gone = r.archived || r.auto_other;
  return [
    { sep: true },
    { label: 'rename…', hint: r.inbox ? 'its folder follows the name' : 'a name by hand', on: () => startRename('chat', r.id) },
    ...(r.inbox ? [{ label: 'make it a project…', hint: 'move its folder out of the inbox', on: () => promoteChat(r) }] : []),
    r.pinned ? { label: 'unpin', on: () => markChat(r, 'unpin') } : { label: 'pin', hint: 'to the top', on: () => markChat(r, 'pin') },
    isLive ? { label: 'archive', off: 'running: stop it first' }
      : gone ? { label: 'unarchive', hint: 'back to the list', on: () => markChat(r, 'unarchive') }
      : { label: 'archive', hint: 'to other', on: () => markChat(r, 'archive') },
    isLive ? { label: 'stop and delete', hint: 'to the trash', on: () => openMenu([
      { label: 'stop and delete', on: () => markChat(r, 'trash', { stop: true }) },
      { label: 'keep it', on: () => {} },
    ], at, { title: `stop and delete ${label(r)}? it goes to the trash` }) }
      : { label: 'delete', hint: 'to the trash', on: () => markChat(r, 'trash') },
  ];
}
const MARKED = { pin: 'pinned', unpin: 'unpinned', archive: 'moved to other', unarchive: 'moved back', trash: 'moved to the trash (restore it from trash, at the bottom)' };
async function markChat(r, verb, body) {
  const res = await hubPost(`sessions/${encodeURIComponent(r.id)}/${verb}`, body);
  if (res.status === 204) {
    if (verb === 'trash') for (const p of ui.panes.peek().filter((x) => x.sid === r.id)) closePane(p.id);
    toast(`${label(r)}: ${MARKED[verb]}`);
  } else if (res.status === 409) toast((res.data && res.data.error) || 'that chat is running', true);
  else if (res.status === 401) toast('the server refused the login for this page (it restarted?). Run `hoot` again.', true);
  else toast(`${verb} failed: ${res.status || 'network'} ${(res.data && res.data.error) || ''}`, true);
  refresh();
}
async function restoreChat(t) {
  const res = await hubPost(`trash/${encodeURIComponent(t.id)}/restore`);
  if (res.status === 204) toast(`${t.title || t.id}: restored`);
  else if (res.status === 409) toast('a chat with that name already exists; delete or rename that one first', true);
  else toast(`restore failed: ${res.status || 'network'} ${(res.data && res.data.error) || ''}`, true);
  refresh();
}
const emptyAsk = (n, at) => openMenu([
  { label: `permanently delete ${n} chat${n === 1 ? '' : 's'}? this can't be undone`, off: ' ' },
  { label: 'delete permanently', on: async () => {
    const res = await hubPost('trash/empty', { confirm: true });
    toast(res.status === 200 ? `${n} chat${n === 1 ? '' : 's'} deleted for good` : `could not empty the trash: ${res.status || 'network'}`, res.status !== 200);
    refresh();
  } },
  { label: 'keep them', on: () => {} },
], at, { title: 'empty the trash' });

const ELSEWHERE = 'running in another app (a terminal chat or another server): view only';
function elsewhereMenu(e, r) {
  e.preventDefault();
  openMenu([
    { label: 'open here', off: ELSEWHERE },
    { label: 'open beside', off: ELSEWHERE },
    { label: 'resume (may continue the last turn)', off: ELSEWHERE },
    { label: 'copy id', on: () => copyId(r.id) },
  ], atPointer(e), { title: label(r) });
}

function statusMenu(e) {
  e.preventDefault();
  const p = focused.value, hl = p && p.s.hello.value;
  openMenu([
    { label: 'loaded', hint: 'model, gate, tools', on: openLoaded },
    { label: 'theme', sub: themes.value.map((n) => ({ label: n, tick: n === theme.value, on: () => pickLook('theme', n) })) },
    { label: 'font', sub: Object.entries(FONTS).map(([k, f]) => ({ label: f.label, hint: k === DEFAULT_FONT ? 'default' : '', tick: k === font.value, on: () => pickLook('font', k) })) },
    p ? { label: 'open', sub: openItems(p) } : { label: 'open', off: 'no pane focused' },
    { sep: true },
    ...Object.entries(cmdTable.value).map(([c, [what, fn]]) => ({ label: ':' + c, hint: what, on: () => fn('') })),
  ], atPointer(e), { title: 'status' });
}

// ---------------------------------------------------------------- views
const act = {
  name: sessionName,
  resume: (p) => { Promise.resolve(p.s.resume()).then(refresh); },
  project,
  focus: focusPane,
  paneMenu,
  splitMenu,
  forkFrom,
  close: closePane,
  leaveAsk,
  bounce,
  focusHead,
  focusAsk: focusQueued,
  submitAsks,
  queue,
  unsent,
  tabs,
  current,
  panelPane,
  letBe,
  cycleAsk,
  toast,
};

// The activity rail: always on screen at the far edge of its side, one icon per
// tab (the icons are BASE_LEFT / BASE_RIGHT's and what features registered); the open tab's icon is lit. Click: open on that tab, or
// close if it is the open one. Both rails alone is compact mode.
// The collapse glyph: only at the outer end of an open panel's title, never in the rail; a closed panel comes back through its rail icons or ctrl+b / ctrl+i.
// The arrow points toward the panel's own edge, the way it folds away.
function Fold({ side, cls }) {
  const left = side === 'l';
  return html`<${K} cls=${cls} title=${'collapse' + (left ? ' sidebar' : ' inspector') + ' · ' + (left ? 'ctrl+b' : 'ctrl+i')} on=${() => toggle(left ? 'lrail' : 'rrail')}>${left ? '«' : '»'}</${K}>`;
}
function Rail({ side }) {
  const left = side === 'l', tabs = tabsOn(side);
  const cur = (left ? ui.left : ui.right).value, closed = (left ? ui.lrail : ui.rrail).value;
  const keys = left ? 'ctrl+b' : 'ctrl+i, [ ]';
  // A tab's badge (a signal it registered) is one character after its icon: a dot, the count's digit, `+` over 9 (core/registry.js badgeText).
  return html`<nav class=${'col arail ' + (left ? 'ral' : 'rar')} aria-label=${left ? 'sidebar tabs' : 'inspector tabs'}>
    ${tabs.map((t) => { const b = badgeText(t.badge ? t.badge.value : 0); return html`<${K} cls=${'ri' + (!closed && t.name === cur ? ' on' : '')} title=${t.name + ' · ' + keys} on=${() => railPick(side, t.name)}>${t.icon}${b && html`<span class="bd o">${b}</span>`}</${K}>`; })}
  </nav>`;
}

function Owl() {
  return html`<div class="owl">${' ,___,  '}<span class="name">minerva</span>${'\n ['}<span class="eyes">O.o</span>${']  '}<span class="faint">Make it,</span>${'\n /)_)   '}<span class="faint">Break it,</span>${'\n  ""    '}<span class="faint">Hack it.</span></div>`;
}

// A row: status dot, name, unread count, and the last message on one dim line.
function SessionItem({ p }) {
  const s = p.s, hl = s.hello.value, conn = s.conn.value, run = s.running.value, last = s.last.value, n = s.unread.value;
  const state = s.bye ? 'kept' : conn !== 'live' ? conn : run === true ? 'running' : run === false ? 'idle' : 'unknown';
  const [dot, cls] = state === 'running' ? ['●', 'hi'] : state === 'idle' || state === 'kept' ? ['○', 'faint']
    : ['◌', conn === 'live' ? 'faint' : 'y'];
  const preview = last ? (last.who === 'bot' ? '' : last.who + ': ') + last.text : hl ? 'no messages yet' : 'connecting…';
  return html`<div class="it" onContextMenu=${(e) => sessionMenu(e, p)}>
    <div class="l1"><span class=${cls} title=${state}>${dot}</span>
      <${K} cls=${'grow nm' + (ui.focus.value === p.id ? ' on' : '')} title=${(hl ? hl.session + '\n' : '') + state}
        on=${() => { focusPane(p.id); toComposer(); }}>${sessionName(s)}${isMirror(p) ? ' (mirror)' : ''}</${K}>
      ${n > 0 && html`<span class="o" title=${n + ' new since you last looked'}>[${n}]</span>`}</div>
    <div class="pv" title=${last ? last.text : ''}>${preview}</div>
  </div>`;
}

// Sessions grouped by project: `▾ name  +`; the name folds the group, `+`
// starts a session there (the hub).
// Under the perch a group also lists the live sessions of that folder that no
// pane shows, dim, `(not open)`.
// The live sessions no pane shows and the ones held elsewhere are listed 15 at a time, then `more…` shows 50 more (as the kept list does).
const idleShown = signal(KEPT_PICK), elseShown = signal(KEPT_PICK);
function Sessions() {
  const perch = mode() === 'perch';
  const groups = new Map();
  let idleSeen = 0, idleRest = 0;
  const grp = (cwd) => { if (!groups.has(cwd)) groups.set(cwd, { ps: [], idle: [] }); return groups.get(cwd); };
  for (const x of ui.panes.value) {
    const hl = x.s.hello.value;
    grp(hl ? hl.cwd : '').ps.push(x);
  }
  if (perch) {
    const shown = new Set(ui.panes.value.map((x) => x.sid));
    for (const r of live(list.value)) {
      if (shown.has(r.id)) continue;
      if (idleSeen++ < idleShown.value) grp(r.cwd || '').idle.push(r); else idleRest++;
    }
  }
  const fold = (cwd) => {
    const f = new Set(ui.folded.value);
    if (!f.delete(cwd)) f.add(cwd);
    ui.folded.value = f;
  };
  if (!groups.size) return html`<div class="faint">no live session</div>`;
  const more = idleRest > 0 && html`<div class="it" key="more-idle"><${K} cls="faint" on=${() => { idleShown.value += 50; }}>more… (${idleRest})</${K}></div>`;
  const out = [...groups].map(([cwd, { ps, idle }]) => {
    // A chat with no door has no folder to name it by: a saved one is grouped as such, a live one is still connecting.
    const gone = ps.length > 0 && ps.every((x) => ['kept', 'stopped', 'dead', 'gone'].includes(x.s.conn.value));
    const open = !ui.folded.value.has(cwd), name = cwd ? project(cwd) : gone ? 'saved' : 'connecting…';
    const unread = ps.reduce((n, x, i) => n + (ps.findIndex((y) => y.s === x.s) === i ? x.s.unread.value : 0), 0);
    return html`<div class="grp" key=${cwd}>
      <div class="gh"><${K} cls="gname" title=${(cwd ? tildify(cwd) + '\n' : '') + (open ? 'fold' : 'open')} on=${() => fold(cwd)}>${open ? '▾' : '▸'} ${name}</${K}>
        <${K} off=${!(perch && cwd)} cls="plus" title=${perch && cwd ? `new session in ${name} (default model)` : 'new session in ' + name + ': ' + HUB} on=${() => newSession(cwd, undefined)}>+</${K}>
        ${!open && unread > 0 && html`<span class="o">[${unread}]</span>`}</div>
      ${open && html`<div class="list">${ps.map((x) => html`<${SessionItem} key=${x.id} p=${x} />`)}
        ${idle.map((r) => html`<${IdleItem} key=${r.id} r=${r} />`)}</div>`}
    </div>`;
  });
  return more ? [...out, more] : out;
}

// A live session of the perch that no pane shows: a click opens a pane on it.
// Middle-click on a row opens it beside (the mousedown is eaten: it would start autoscroll).
const noMiddle = (e) => { if (e.button === 1) e.preventDefault(); };
const beside = (e, sid) => { if (e.button === 1) { e.preventDefault(); openPane(sid); } };
function IdleItem({ r }) {
  return html`<div class="it" onContextMenu=${(e) => idleMenu(e, r)} onMouseDown=${noMiddle} onAuxClick=${(e) => beside(e, r.id)}>
    <div class="l1"><span class="faint">○</span>
      <${K} cls="grow nm faint" title=${r.id + '\nrunning, not open here\nctrl-click or middle-click: open beside'} on=${(e) => openSession(r.id, { beside: e.ctrlKey || e.metaKey })}>${label(r)}  <span class="faint">(not open)</span></${K}></div>
  </div>`;
}

// Live sessions held by another client: shown, never touched.
function Elsewhere() {
  const all = elsewhere(list.value), n = elseShown.value, rs = all.slice(0, n), rest = all.length - rs.length;
  if (!all.length) return null;
  return html`<div class="hdr faint">in other apps</div>
    <div class="list">${rs.map((r) => html`<div class="it" key=${r.id} onContextMenu=${(e) => elsewhereMenu(e, r)}>
      <div class="l1"><span class="faint">◌</span><span class="grow nm faint" title=${ELSEWHERE}>${label(r)}</span></div></div>`)}
    ${rest > 0 && html`<div class="it"><${K} cls="faint" on=${() => { elseShown.value = n + 50; }}>more… (${rest})</${K}></div>`}</div>`;
}

// ---- the chat list under the perch: search, `+ new chat`, one group per project (alphabetical), `no folder`, `other`.
// Date ordering inside a group is the only sort: newest first. A chat is in a project when its folder is the project's
// or inside it. A group shows 8, then `more… (N)` for 50 more.
const query = signal(''), shownBy = signal({}), otherOpen = signal(false);
const GROUP_CAP = 8;
const byNew = (a, b) => (b.mtime || 0) - (a.mtime || 0);

// A live chat is ●, a saved one ○; a pane's own state adds the busy bold, `?` for an ask waiting and `[n]` unread. `where` is the
// group it is drawn in: a pinned row names its project faintly, an `other` row says why it is there (archived / idle). The `⋯`
// at the right edge (on hover or focus) opens the same menu as a right-click.
function ChatRow({ r, where }) {
  const p = ui.panes.value.find((x) => x.sid === r.id), f = focused.value, s = p && p.s;
  const isLive = r.state === 'live', busy = isLive && s && s.running.value === true;
  const asks = s ? s.asks.value.length : 0, n = s ? s.unread.value : 0;
  const cur = !!f && f.sid === r.id;
  const menu = (e, at) => (p ? sessionMenu(e, p, at) : isLive ? idleMenu(e, r, at) : r.origin === 'other' ? olderMenu(e, r, at) : keptMenu(e, r, at));
  const what = isLive ? (busy ? 'running' : 'live') : r.state === 'dead' ? 'exited' : r.origin === 'other' ? "saved, from eidolon's own folder (opens read-only)" : 'saved';
  const pj = where === 'pinned' ? (projectFor(r.cwd) || {}).name : '', tag = r.pending ? 'pending' : where === 'other' ? otherTag(r) : '';
  const ed = renaming.value;
  if (ed && ed.kind === 'chat' && ed.key === r.id) {
    return html`<div class="it chat ed"><span class=${'cd ' + (isLive ? 'hi' : 'faint')}>${isLive ? '●' : '○'}</span><${EditName} text=${label(r)} name="chat name" save=${(t) => renameChat(r.id, t)} /></div>`;
  }
  return html`<div class=${'it chat' + (cur ? ' cur' : '')} onContextMenu=${menu} onMouseDown=${noMiddle} onAuxClick=${(e) => beside(e, r.id)}>
    <${K} cls="crow" title=${label(r) + '\n' + what + '\nctrl-click or middle-click: open beside\nright-click or ⋯: more'}
      on=${(e) => openSession(r.id, { beside: e.ctrlKey || e.metaKey })}>
      <span class=${'cd ' + (isLive ? (busy ? 'hi b' : 'hi') : 'faint')}>${isLive ? '●' : '○'}</span><span class="ct">${label(r)}</span>
      ${pj && html`<span class="faint pj" title="its project">${pj}</span>`}${tag && html`<span class="faint pj" title=${tag === 'pending' ? 'a fork from a reply: it opens when its first message has run' : tag === 'archived' ? 'moved here by hand' : 'idle for 7 days'}>${tag}</span>`}
      ${asks > 0 && html`<span class="y" title="a question is waiting">?</span>`}${n > 0 && html`<span class="o" title=${n + ' new'}>[${n}]</span>`}
      <span class="faint ca">${ageOf(r)}</span></${K}>
    ${!r.origin && html`<${K} cls="mo" title="rename, pin, archive, delete, more" on=${(e) => menu(e, under(e.currentTarget))}>⋯</${K}>`}
  </div>`;
}

// A group's rows: the first GROUP_CAP (or the count asked for), then `more…`.
function Rows({ gk, rows, where }) {
  const n = shownBy.value[gk] || GROUP_CAP, shown = rows.slice(0, n), rest = rows.length - shown.length;
  return html`<div class="list">${shown.map((r) => html`<${ChatRow} key=${r.id} r=${r} where=${where} />`)}
    ${rest > 0 && html`<div class="it"><${K} cls="faint" on=${() => { shownBy.value = { ...shownBy.value, [gk]: n + 50 }; }}>more… (${rest})</${K}></div>`}</div>`;
}

// `p` (a project) makes the header renamable in place and gives it a `⋯`; the inbox's header has neither.
function Head({ name, title, plusTitle, on, menu, p }) {
  const ed = renaming.value, editing = !!p && !!ed && ed.kind === 'project' && ed.key === p.path;
  return html`<div class="gh2" onContextMenu=${menu}>
    ${editing ? html`<${EditName} text=${p.name} name="project name" save=${(t) => renameProj(p, t)} />` : html`<span class="gname" title=${title}>${name}</span>`}
    ${p && !editing && html`<${K} cls="mo" title="rename, remove from the list" on=${(e) => menu(e, under(e.currentTarget))}>⋯</${K}>`}
    <${K} cls="plus" title=${plusTitle} on=${on}>+</${K}></div>`;
}

// Deleted chats, collapsed at the very bottom while there are any: `restore` puts one back, `empty trash` removes them for good
// (one confirm).
const trashOpen = signal(false);
function TrashGroup() {
  const t = trash.value, n = t.length;
  if (!n) return null;
  return html`<div class="grp trash">
    <div class="gh2"><${K} cls="gname" title=${trashOpen.value ? 'fold' : 'chats you deleted; they can be restored'} on=${() => { trashOpen.value = !trashOpen.value; }}>${trashOpen.value ? '▾' : '▸'} trash (${n})</${K}>
      <${K} cls="plus" title="delete them for good" on=${(e) => emptyAsk(n, under(e.currentTarget))}>empty trash</${K}></div>
    ${trashOpen.value && html`<div class="list">${t.map((x) => html`<div class="it chat" key=${x.id}>
      <span class="crow faint"><span class="cd">○</span><span class="ct" title=${x.id}>${x.title || x.id}</span><span class="ca">${age(x.mtime)}</span></span>
      <${K} cls="mo on" title="put it back in the list" on=${() => restoreChat(x)}>restore</${K}></div>`)}</div>`}</div>`;
}

function Chats() {
  const rs = list.value, q = query.value.trim().toLowerCase(), ps = sorted(projects.value);
  const match = (r) => !q || label(r).toLowerCase().includes(q);
  const chats = [...live(rs), ...keptRows(rs)].filter(match).sort(byNew);
  const byP = new Map(ps.map((p) => [p.path, []])), loose = [], pinned = [];
  // A pinned chat is in the `pinned` group and nowhere else.
  for (const r of chats) { const p = projectFor(r.cwd, ps); if (r.pinned) pinned.push(r); else if (p) byP.get(p.path).push(r); else loose.push(r); }
  const oth = older(rs).filter(match);
  const shownProjects = ps.filter((p) => !q || byP.get(p.path).length);
  return html`<div class="chats">
    <div class="sq"><span class="faint">search </span><input type="text" spellcheck="false" autocomplete="off" aria-label="search chats" placeholder="title" value=${query.value}
      onInput=${(e) => { query.value = e.currentTarget.value; }} onKeyDown=${(e) => { if (e.key === 'Escape') { query.value = ''; e.currentTarget.blur(); } }} /></div>
    <div class="newrow"><${K} cls="newchat" title="a chat in the inbox (its folder is named after its title)" on=${() => newSession(null, undefined)}>+ new chat</${K}></div>
    ${pinned.length > 0 && html`<div class="grp"><div class="gh2"><span class="gname" title="pinned chats, newest first">pinned</span></div>
      <${Rows} gk="pinned" rows=${pinned} where="pinned" /></div>`}
    ${shownProjects.map((p) => html`<div class="grp" key=${p.path}>
      <${Head} p=${p} name=${p.name} title=${p.path + '\nright-click or ⋯: rename, remove from the list'} plusTitle=${'new chat in ' + p.name} on=${() => newSession(p.path, undefined)} menu=${(e, at) => forgetMenu(e, p, at)} />
      <${Rows} gk=${'p:' + p.path} rows=${byP.get(p.path)} /></div>`)}
    ${!q && html`<div class="newrow"><${K} cls="newchat" title="pick a folder and name it" on=${newProject}>+ new project</${K}></div>`}
    ${(!q || loose.length > 0) && html`<div class="grp">
      <${Head} name="inbox" title="chats that are not in a project: each has a folder of its own, named after its title. ⋯ › make it a project… moves one out" plusTitle="new chat in the inbox" on=${() => newSession(null, undefined)} />
      ${loose.length ? html`<${Rows} gk="loose" rows=${loose} />` : html`<div class="faint pad">none</div>`}</div>`}
    <${Elsewhere} />
    ${oth.length > 0 && html`<div class="grp">
      <div class="gh2"><${K} cls="gname" title=${otherOpen.value ? 'fold' : "archived chats, chats idle for 7 days, and eidolon's own sessions folder"} on=${() => { otherOpen.value = !otherOpen.value; }}>${otherOpen.value || q ? '▾' : '▸'} other (${oth.length})</${K}></div>
      ${(otherOpen.value || q) && html`<${Rows} gk="other" rows=${oth} where="other" />`}</div>`}
    <${TrashGroup} />
  </div>`;
}

// The inner border of a sidebar: drag it, or focus it and press ←/→
// (shift: 5 cells); a double click goes back to the default width.
function Edge({ d }) {
  const n = ui.sides.value[d], grow = d === 'l' ? { ArrowRight: 1, ArrowLeft: -1 } : { ArrowLeft: 1, ArrowRight: -1 };
  const drag = (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget, box = el.parentElement.getBoundingClientRect(), cw = cell.peek().cw;
    el.setPointerCapture(e.pointerId);
    const move = (m) => setSide(d, (d === 'l' ? m.clientX - box.left : box.right - m.clientX) / cw);
    const ends = ['pointerup', 'pointercancel', 'lostpointercapture'];
    const up = () => {
      el.removeEventListener('pointermove', move);
      for (const t of ends) el.removeEventListener(t, up);
      el.classList.remove('drag');
    };
    el.classList.add('drag');
    el.addEventListener('pointermove', move);
    for (const t of ends) el.addEventListener(t, up);
  };
  const key = (e) => {
    if (!grow[e.key]) return;
    e.preventDefault();
    e.stopPropagation();
    setSide(d, n + grow[e.key] * (e.shiftKey ? 5 : 1));
  };
  return html`<div class=${'edge ' + d} role="separator" tabindex="0" aria-orientation="vertical"
    aria-label=${(d === 'l' ? 'sidebar' : 'inspector') + ' width in cells'} aria-valuenow=${n} aria-valuemin=${SIDE_MIN} aria-valuemax=${SIDE_MAX}
    title="drag to resize (←/→ when focused); double-click resets" onPointerDown=${drag}
    onDblClick=${() => setSide(d, SIDES[d])} onKeyDown=${key}></div>`;
}

function Left() {
  const tab = ui.left.value, rail = ui.lrail.value, panes = ui.panes.value;
  known.value; // redraw once the mode is known
  const p = focused.value, hl = p && p.s.hello.value;
  let body;
  if (tab === 'sessions') {
    body = mode() === 'perch' ? html`<${Owl} />
      ${stale.value && html`<div class="faint">list stale</div>`}
      <${Chats} />`
      : html`<${Owl} />
      ${panes.length ? html`<${Sessions} />` : html`<div class="faint">no pane. <${K} on=${() => openPane()}>open one</${K}></div>`}
      <div class="hdr">peers</div><${Needs}>peer list needs the hub</${Needs}>
      <div class="hdr">kept</div><${Needs}>other sessions need the hub (M2)</${Needs}>`;
  } else if (tab === 'projects') {
    const perch = mode() === 'perch';
    body = perch ? html`<div class="hdr">projects</div>
      ${projects.value.length ? html`<div class="list">${sorted(projects.value).map((p) => html`<div class="it" key=${p.path} onContextMenu=${(e) => forgetMenu(e, p)}>
        <div class="l1">${renaming.value && renaming.value.kind === 'project' && renaming.value.key === p.path
          ? html`<${EditName} text=${p.name} name="project name" save=${(t) => renameProj(p, t)} />`
          : html`<${K} cls="grow nm" title=${p.path + '\nnew chat here\nright-click: rename, remove from the list'} on=${() => newSession(p.path, undefined)}>${p.name}</${K}>`}</div>
        <div class="pv" title=${p.path}>${tildify(p.path)}</div></div>`)}</div>`
        : html`<div class="faint">none yet</div>`}
      <div class="newrow"><${K} cls="newchat" on=${newProject}>+ new project</${K}></div>`
      : html`<div class="hdr">this door</div>
      ${hl ? html`<div class="list"><div class="it"><div class="l1"><span class="faint">▸</span><span class="grow c" title=${hl.cwd}>${tildify(hl.cwd)}</span></div></div></div>` : html`<div class="faint">—</div>`}
      <div class="hdr">others</div><${Needs}>project list needs the hub</${Needs}>`;
  } else if (tab === 'mesh') {
    body = html`<${MeshTab} />`;
  } else {
    // a tab a feature registered: the chat the inspector shows, if any, as for the inspector's tabs
    const t = leftAll.value.find((x) => x.name === tab), q = inspected.value;
    body = t && t.component ? html`<${t.component} key=${tab} s=${q ? q.s : null} sid=${q ? q.sid : null} />` : html`<div class="faint">no such tab</div>`;
  }
  return html`<aside id="left" class="col side frame">
    ${!rail && html`<div class="ftitle"><${Fold} side="l" cls="tab fold" /><span class="tab on">${tab}</span></div>`}
    ${!rail && html`<div class="inner">${body}</div>`}
    ${!rail && html`<${Edge} d="l" />`}
  </aside>`;
}

function Rec({ s }) {
  const vs = s.verdicts.value.slice(-10), asks = s.asks.value.length, calls = s.calls.value, errs = s.fails.value;
  const ctx = s.ctx.value, budget = s.budget.value;
  return html`
    <${Kv} k="turns  " v=${s.turns.value} />
    <${Kv} k="calls  " v=${calls + (errs ? ` (${errs} failed)` : '')} cls=${errs ? 'r' : ''} />
    <${Kv} k="asks   " v=${asks} cls=${asks ? 'o' : ''} />
    <${Kv} k="context" v=${ctx != null ? ctx.toLocaleString() + ' tok' : '—'} />
    <${Kv} k="budget " v=${budget != null ? budget + ' calls left' : '—'} cls=${budget != null && budget < 3 ? 'r' : ''} />
    <div class="hdr">verdicts</div>
    ${vs.length ? vs.map((v) => html`<div class="kv"><span class="hi">${v.tool}</span><span class="grow faint" title=${v.reason}>${v.reason}</span><span>${v.outcome}</span></div>`)
      : html`<div class="faint">none yet</div>`}`;
}

function RunTab({ s }) {
  const open = s.open.value, run = s.running.value;
  return html`<div class="hdr">open calls</div>
    ${open.length ? open.map((c) => html`<div class="kv"><span class="hi">${c.name}</span><span class="grow faint">${c.st.value}</span></div>`)
      : html`<div class="faint">${run === true ? 'thinking…' : 'nothing running'}</div>`}
    <div class="hdr">parked</div><${Needs}>parks of wait_for calls show no live samples: eidolon's web door sends no park-tick (upstream gap 7)</${Needs}>`;
}

// The status line's mode badge: what each approval mode means, for its tooltip.
const MODE_TIPS = { yolo: 'yolo: the gate is answering for you', manual: 'manual: it asks before anything that changes things', auto: "auto: eidolon's rules, it asks only for risky commands" };

// The loaded tab, then the sections features registered under it (core/registry.js loadedSections): each a component taking { s, sid }.
function Loaded(props) {
  return html`<${LoadedTab} ...${props} />${loadedSections.list.map((S, i) => html`<${S} key=${i} ...${props} />`)}`;
}

// The page's own inspector tabs, name -> component. Every tab is a Preact component taking { s: Session, sid }. A tab a feature registered
// (core/registry.js rightTabs) brings its own component and is not listed here; its icon and place in the rail are BASE_RIGHT's, then registration order.
export const INSPECTOR = { rec: Rec, loaded: Loaded, files: FilesTab, diff: DiffTab, graph: GraphTab, run: RunTab };

// The inspector shows the chat that has focus, else the last that had it (an xpane with focus is not a chat), else nothing; a `global` tab
// is drawn either way, with { s: null, sid: null } when there is no chat.
function Right() {
  const tab = ui.right.value, rail = ui.rrail.value, p = inspected.value;
  const t = rightAll.value.find((x) => x.name === tab), Tab = (t && t.component) || INSPECTOR[tab];
  const body = !Tab ? html`<div class="faint">no such tab</div>`
    : p ? html`<${Tab} key=${tab} s=${p.s} sid=${p.sid} />`
    : t && t.global ? html`<${Tab} key=${tab} s=${null} sid=${null} />`
    : html`<div class="faint">no pane focused</div>`;
  return html`<aside id="right" class="col side frame">
    ${!rail && html`<div class="ftitle"><span class="tab on">${tab}</span><${Fold} side="r" cls="tab fold" /></div>`}
    ${!rail && html`<div class="inner">${body}</div>`}
    ${!rail && html`<${Edge} d="r" />`}
  </aside>`;
}

function Divider({ d }) {
  const drag = (e) => {
    e.preventDefault();
    const el = e.currentTarget, box = el.parentElement.getBoundingClientRect();
    el.setPointerCapture(e.pointerId);
    const move = (m) => {
      const at = d.dir === 'row' ? m.clientX - box.left : m.clientY - box.top;
      ui.tree.value = tile.setRatio(ui.tree.value, d.path, (at - d.origin) / d.size);
    };
    // A drag ends on pointerup, and also when the browser takes the pointer
    // away (pointercancel) or capture is lost; otherwise the divider stays stuck.
    const ends = ['pointerup', 'pointercancel', 'lostpointercapture'];
    const up = () => {
      el.removeEventListener('pointermove', move);
      for (const t of ends) el.removeEventListener(t, up);
      el.classList.remove('drag');
    };
    el.classList.add('drag');
    el.addEventListener('pointermove', move);
    for (const t of ends) el.addEventListener(t, up);
  };
  const style = d.dir === 'row' ? `left:${d.x}px;top:${d.y}px;height:${d.h}px` : `left:${d.x}px;top:${d.y}px;width:${d.w}px`;
  return html`<div class=${'divider ' + d.dir} style=${style} title="drag to resize" onPointerDown=${drag}></div>`;
}

function Tiles() {
  const ref = useRef(null);
  useLayoutEffect(() => {
    const el = ref.current;
    const size = () => {
      const r = el.getBoundingClientRect();
      const b = ui.box.value;
      if (b.w !== r.width || b.h !== r.height) ui.box.value = { w: r.width, h: r.height, ...cell.peek() };
    };
    size(); // now, not only when a ResizeObserver fires: it never does in a tab that is not drawing
    const ro = new ResizeObserver(size);
    ro.observe(el);
    addEventListener('resize', size);
    return () => { ro.disconnect(); removeEventListener('resize', size); };
  }, []);
  known.value; // redraw once the mode is known: the start screen is the perch's, and the first render can come before detect() says so
  const panes = ui.panes.value, xs = ui.xpanes.value, tree = ui.tree.value, box = ui.box.value, f = ui.focus.value;
  const lay = tile.layout(tree, box);
  const rect = new Map(lay.panes.map((r) => [r.id, r]));
  let body;
  const gate = ui.gate.value;
  const login = ui.login.value;
  if (!ui.token && login === 'wait') {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle">minerva</div><div class="empty faint">signing in…</div></div>`;
  } else if (!ui.token && login === 'expired') {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle r">link expired</div>
      <div class="empty">${'this link has expired — run `hoot` again'}</div></div>`;
  } else if (!ui.token) {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle r">not signed in</div>
      <div class="empty">${'this page is not signed in.\n\ntype hoot in PowerShell: it opens Minerva already signed in.\n\nto open a bare eidolon server by hand, add #token=<the contents of its token file> to its address; the part after # is never sent to a server.'}</div></div>`;
  } else if (gate) {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle r">${gate === 'refused' ? 'token refused' : 'not answering'}</div>
      <div class="empty">${gate === 'refused' ? 'the server refused the login for this page (it restarted?).\n\nrun `hoot` again.' : 'the server is not answering. this page keeps trying and opens by itself when it does.\n\nif it does not, run `hoot status web`, then `hoot` again.'}</div></div>`;
  } else if (!panes.length && !xs.length && mode() === 'perch') {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle">minerva</div>
      <div class="scroll"><${Home} act=${homeAct} /></div></div>`;
  } else if (!panes.length && !xs.length) {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle faint">no pane</div>
      <div class="empty">no pane open.${'\n\n'}<${K} cls="hi" on=${() => openPane()}>:open</${K}>  reconnect this session${'\n'}<span class="faint">ctrl+enter  new pane with a picker (needs Minerva's server: open it with hoot)</span></div></div>`;
  } else {
    body = [
      ...panes.map((p) => rect.get(p.id) && html`<${Pane} key=${p.id + ' ' + p.sid} p=${p} rect=${rect.get(p.id)} focused=${p.id === f} mirror=${isMirror(p)} act=${act} />`),
      // a pane of a registered kind draws itself; one whose kind is gone is not drawn
      ...xs.map((x) => { const k = paneKinds.get(x.kind); return k && rect.get(x.id) && html`<${k.Pane} key=${x.id} p=${x} rect=${rect.get(x.id)} focused=${x.id === f} act=${act} />`; }),
      ...lay.dividers.map((d) => html`<${Divider} key=${d.path} d=${d} />`),
    ];
  }
  return html`<div id="tiles" class="col" ref=${ref}>${body}${pickerOpen.value && html`<${Picker} />`}${overlay.value && html`<${Overlay} />`}</div>`;
}

function Hint() {
  const h = ui.hint.value;
  if (!h) return html`<div id="hint"></div>`;
  if (h.mode === 'toast') {
    const a = h.act; // toast(text, err, { label, on }): one clickable word after the text; a click dismisses the toast, then does it
    return html`<div id="hint" class=${'toast' + (h.err ? ' err' : '')} aria-live="polite">${h.text}${a && html` <${K} cls="hi" on=${() => { if (ui.hint.peek() === h) ui.hint.value = null; a.on(); }}>${a.label}</${K}>`}</div>`;
  }
  const text = ui.cmd.value || '';
  const m = matches(text);
  return html`<div id="hint" class="menu">${m.length ? m.map((c, i) => html`<${K} cls=${'pair' + (i === 0 && text ? ' sel' : '')} on=${() => runCmd(c)}>${c}</${K}>${i === 0 && text ? html`<span class="faint"> ${cmdTable.value[c][0]}   </span>` : ' '}`)
    : html`<span class="faint">${NO_CMD}</span>`}
    <${K} cls="pair faint" on=${closeCmd}>esc close</${K}></div>`;
}

function CmdLine() {
  const ref = useRef(null);
  useLayoutEffect(() => { const el = ref.current; if (el) { el.focus(); el.setSelectionRange(el.value.length, el.value.length); } }, []);
  const key = (e) => {
    if (e.key === 'Enter') { e.preventDefault(); runCmd(); }
    else if (e.key === 'Escape') { e.preventDefault(); closeCmd(); }
    else if (e.key === 'Tab') {
      e.preventDefault();
      const m = matches(ui.cmd.value);
      if (m.length) ui.cmd.value = m[0] + ' ';
    }
  };
  const blur = () => setTimeout(() => { if (ui.cmd.value != null && document.activeElement !== ref.current) closeCmd(); }, 200);
  return html`<div id="cmd"><span class="colon">:</span><input ref=${ref} type="text" spellcheck="false" autocomplete="off"
    aria-label="command" value=${ui.cmd.value} onInput=${(e) => { ui.cmd.value = e.currentTarget.value; }} onKeyDown=${key} onBlur=${blur} /></div>`;
}

// ---------------------------------------------------------------- the status line
// The TUI's status line (default.rn:419-537): on the left the state badge,
// inverted in the state's colour, then the model, the persona and YOLO when
// the gate is waived, then what needs a look (asks waiting, the connection);
// on the right the gauges and `? keys`. When it does not fit it drops, a
// step at a time (ui.fit, up to FIT): the unknown-state note, the tool
// count, in/out, the context, the word `keys`, the connection word.
const FIT = 5;

function whereNow() {
  const a = document.activeElement;
  if (!a || a === document.body || !a.closest) return 'idle';
  const owned = whereOf(a); // a place a feature names (a terminal, a notepad) comes first (core/registry.js whereOwners)
  if (owned) return owned;
  if (a.closest('.ask')) return a.closest('.aline') ? 'note' : 'ask';
  return a.closest('.composer') ? 'compose' : 'idle';
}
const where = () => { ui.where.value = whereNow(); };
document.addEventListener('focusin', where);
document.addEventListener('focusout', () => setTimeout(where, 0));

// An entry of the key reference presses its key on whatever held focus, so
// an ask or a composer handles it exactly as it handles the key.
const press = (key, mods) => () => {
  const at = document.activeElement && document.activeElement !== document.body ? document.activeElement : document.body;
  at.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...mods }));
};

// What the keys do where focus is now: the reference's first section.
function hereKeys(at) {
  if (at === 'note') return [['⏎', 'take the highlighted option, with the note', press('Enter')], ['↑ ↓', 'back to the options', press('ArrowUp')], ['esc', 'drop the note', press('Escape')]];
  if (at === 'ask') {
    return [['j k', 'options', press('j')], ['⏎', 'take', press('Enter')], ['y', 'yes', press('y')], ['n', 'no', press('n')],
      ['c', 'chat about it', press('c')], ['tab', 'note', press('Tab')], ['s', 'submit every answer', press('s')], ['h l', 'previous / next ask', press('l')], ['esc', 'composer', press('Escape')]];
  }
  if (at === 'compose') return [['⏎', 'send / steer', press('Enter')], ['⌥⏎', 'queue', press('Enter', { altKey: true })], ['esc', 'leave', press('Escape')]];
  return [];
}

const HERE = { ask: 'here: an ask', note: 'here: a note', compose: 'here: the composer' };

function KeyRef() {
  const ref = useRef(null);
  const at = useRef(ui.where.peek()).current; // where focus was when it opened
  useLayoutEffect(() => {
    const away = (e) => { if (ref.current && !ref.current.contains(e.target)) ui.keys.value = false; };
    // Esc closes it wherever focus is (the panel and the composer keep their own Esc).
    const esc = (e) => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); ui.keys.value = false; } };
    document.addEventListener('mousedown', away, true);
    addEventListener('keydown', esc, true);
    return () => { document.removeEventListener('mousedown', away, true); removeEventListener('keydown', esc, true); };
  }, []);
  const here = hereKeys(at);
  const groups = [...(here.length ? [[HERE[at], here]] : []), ...keyGroups.value,
    ['commands', Object.entries(cmdTable.value).map(([c, [what, fn]]) => [':' + c, what, () => fn('')])]];
  const take = (fn) => { ui.keys.value = false; panel.armed.value = true; fn(); }; // a click here is the operator choosing
  return html`<div class="frame pop keyref" ref=${ref} role="dialog" aria-label="key reference" onMouseDown=${(e) => e.preventDefault()}>
    <div class="ftitle">keys<span class="faint"> · click one, esc closes</span></div>
    <div class="kgroups">${groups.map(([name, rows]) => html`<div class="kg"><div class="kgh">${name}</div>
      ${rows.map(([k, what, fn]) => html`<${K} cls="kr" on=${() => take(fn)}><span class="kk">${k}</span><span class="kw">${what}</span></${K}>`)}</div>`)}</div>
  </div>`;
}

// The status line shows what is loaded; switching it is the composer's (its bottom border).
const openLoaded = () => batch(() => { ui.rrail.value = false; ui.right.value = 'loaded'; });

// The context limit of each model the perch lists: Map of key -> tokens | null, from the note on GET /hub/models (core/ctxlimit.js), null
// until that list has been read. Read once the focused chat's model is missing from it, at most once a minute (the perch itself caches
// the listing for ten minutes); a door has no such list, so the limit stays `-` there.
const limits = signal(null);
let limitsAt = 0;
async function loadLimits() {
  if (mode() !== 'perch' || Date.now() - limitsAt < 60000) return;
  limitsAt = Date.now();
  const r = await hubPoll('models');
  if (r.status === 200 && Array.isArray(r.data)) limits.value = limitTable(r.data);
}
effect(() => {
  known.value; // redraw once the mode is known
  const p = focused.value, hl = p && p.s.hello.value;
  if (hl && hl.model && !(limits.value && limits.value.has(hl.model))) loadLimits();
});

// Messages waiting for the focused pane's turn to end: held here, plus any the door holds.
const queuedN = (p) => p.queue.value.length + p.s.queued.value;

function Status() {
  const ref = useRef(null), sig = useRef('');
  const p = focused.value, s = p && p.s, hl = s && s.hello.value;
  const conn = s ? s.conn.value : null, st = stateOf(s);
  const budget = s && s.budget.value, waiting = unsent.value, lvl = ui.fit.value; // answered but not submitted still waits
  const t = tabs.value, cur = current.value, inAsk = ui.where.value === 'ask' || ui.where.value === 'note';
  const place = inAsk && cur ? t.findIndex((x) => x.a === cur.a) + 1 : 0;
  // An xpane with focus says what its kind says (status(p), else title(p), else the kind) where the chat's badge and model would be.
  const fx = p ? null : xById(ui.focus.value) || null, xs = fx ? paneInfo(fx, 'status') ?? paneInfo(fx, 'title') ?? fx.kind : null;
  const now = st + waiting + (p ? 'q' + queuedN(p) : '') + (hl ? hl.model : '') + place + conn + ' ' + ui.box.value.w + (typeof xs === 'string' ? xs : ''); // a resize re-fits
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (sig.current !== now) { sig.current = now; if (ui.fit.peek()) { ui.fit.value = 0; return; } }
    if (el.scrollWidth > el.clientWidth + 1 && ui.fit.peek() < FIT) ui.fit.value = ui.fit.peek() + 1;
  });
  if (ui.cmd.value != null) return html`<footer id="status" class="cmd"><${CmdLine} /></footer>`;
  const dot = html`<span class="faint"> · </span>`;
  const ctx = s && s.ctx.value, table = limits.value, limit = hl && table && table.get(hl.model), chat = p && hl ? modeOf(p.sid, hl) : '';
  const noLimit = mode() !== 'perch' ? 'a bare door reports no context limit' : table ? "eidolon's model list gives no context limit for this model" : 'the model list has not been read';
  const gauges = s ? [
    lvl < 4 && `ctx ${ctx != null ? kfmt(ctx) : '-'}/${limit ? kfmt(limit) : '-'}`,
    lvl < 3 && `in ${kfmt(s.usedIn.value)} out ${kfmt(s.usedOut.value)}`,
    lvl < 2 && `tools ${s.calls.value}`,
  ].filter(Boolean).join(' · ') : '';
  return html`<footer id="status" onContextMenu=${statusMenu}><div class="bar" ref=${ref}>
    <span class="left">
      ${fx && html`<span class="b" title=${'a ' + fx.kind + ' pane has focus'}>${xs}</span>`}
      ${p && html`<span class=${'badge st-' + STATE_CLS[st]} title=${st === '?' ? 'this page connected after the turn began or before any turn event' : ''}>${st}</span>`}
      ${hl && html`<${K} cls="model b" title="the model the door reports: what is loaded" on=${openLoaded}>${hl.model || '?'}</${K}>`}
      ${hl && hl.persona && html`${dot}<${K} cls="persona b" title="the persona: what is loaded" on=${openLoaded}>${hl.persona.name || hl.persona}</${K}>`}
      ${hl && chat && html`${dot}<${K} cls=${'mode b ' + chat} title=${MODE_TIPS[chat]} on=${openLoaded}>${chat === 'yolo' ? 'YOLO' : chat}</${K}>`}
      ${place > 0 && html`${dot}<span class="o">ask ${place}/${t.length}</span>`}
      ${p && queuedN(p) > 0 && html`${dot}<span class="hi qn" title="held here, sent in order when the turn ends">${queuedN(p)} queued</span>`}
      ${waiting > 0 && html`${dot}<${K} cls="o waitn" title=${ui.held.value ? 'the oldest ask takes focus when you pause typing; click to go now' : queue.value.length ? 'focus the oldest ask' : 'every ask is answered: submit sends them'}
        on=${() => focusHead() || (current.peek() && focusQueued(current.peek()))}>${ui.held.value ? '▶ ' : ''}${waiting} waiting</${K}>`}
      ${p && conn !== 'live' && lvl < 5 && html`${dot}<span class=${conn === 'replaying' ? 'faint' : 'warn'}>${connWord(conn)}</span>`}
      ${budget != null && budget < 3 && html`${dot}<span class="warn">${budget} calls left</span>`}
      ${s && s.queued.value > 0 && html`${dot}<span class="faint">${s.queued.value} queued</span>`}
      ${st === '?' && lvl < 1 && html`${dot}<span class="faint unk">state unknown until next turn event</span>`}
    </span>
    <span class="grow"></span>
    <span class="right">${gauges && html`<span class="faint" title=${lvl < 4 && !limit ? noLimit : undefined}>${gauges}</span>`}<span onMouseDown=${(e) => e.preventDefault()}><${K} cls="qk" title="the key reference" on=${keyRef}><span class="kk">?</span>${lvl < 5 ? ' keys' : ''}</${K}></span></span>
  </div></footer>`;
}

function App() {
  const cls = (ui.lrail.value ? 'lrail ' : '') + (ui.rrail.value ? 'rrail ' : '') + (ui.narrow.value ? 'narrow' : '');
  const w = ui.sides.value;
  return html`<main id="main" class=${cls} style=${`--lc:${w.l};--rc:${w.r}`}><${Rail} side="l" />${!ui.lrail.value && html`<${Left} />`}<${Tiles} />${!ui.rrail.value && html`<${Right} />`}<${Rail} side="r" /></main><${Hint} /><${Status} />${ui.keys.value && html`<${KeyRef} />`}<${Menus} />`;
}

effect(() => { const c = cell.value; batch(() => { ui.box.value = { ...ui.box.peek(), ...c }; ui.fit.value = 0; }); });

// ---------------------------------------------------------------- keys
document.addEventListener('keydown', (e) => {
  // First of all: an element a feature owns (a terminal) takes its own keys, ctrl+b, ctrl+i and alt+w included, and none of the page's runs
  // on them (core/registry.js keyOwners). It must stay above the ctrl+b / ctrl+i / alt+w lines below, or those fire in the page as well.
  if (keyClaimed(e.target)) return;
  const k = e.key.toLowerCase(), t = e.target;
  const typing = t && (t.tagName === 'TEXTAREA' || t.tagName === 'INPUT');
  if (typing && !t.closest('.ask')) typedAt = performance.now(); // the pause the ask queue waits for
  // global, even while typing
  if (e.ctrlKey && !e.altKey && k === 'b') { e.preventDefault(); toggle('lrail'); return; }
  if (e.ctrlKey && !e.altKey && k === 'i') { e.preventDefault(); toggle('rrail'); return; }
  // alt+w: by the character first (so AZERTY's w, on the KeyZ key, works),
  // by the physical key only when the character is not ASCII (macOS Option
  // turns alt+w into '∑'); and never while there is text in the composer,
  // where it would eat a keystroke.
  const isW = e.key === 'w' || e.key === 'W' || (e.code === 'KeyW' && /[^\x00-\x7f]/.test(e.key));
  if (e.altKey && !e.ctrlKey && !e.metaKey && isW) {
    if (t && t.tagName === 'INPUT') return;
    if (t && t.tagName === 'TEXTAREA' && t.value) return;
    e.preventDefault(); closePane(); return;
  }
  if (e.ctrlKey && e.key === 'Enter') { e.preventDefault(); picker(); return; }
  if (typing) return;
  if (e.key === ':') { e.preventDefault(); openCmd(); return; }
  if (e.key === '?') { e.preventDefault(); keyRef(); return; }
  if (e.key === 'Escape') { ui.hint.value = null; ui.keys.value = false; closeMenu(); return; }
  if (e.key === 'Tab' && queue.value.length) { e.preventDefault(); cycleAsk(e.shiftKey ? -1 : 1); return; }
  const filter = e.key === '/' && !ui.rrail.value && ui.right.value === 'files' && document.querySelector('#right .filter input');
  if (filter) { e.preventDefault(); filter.focus(); return; }
  const p = focused.value;
  if (!p) return;
  if (e.ctrlKey && k === 'c' && !String(getSelection())) { e.preventDefault(); p.s.cancel(); return; }
  if (e.ctrlKey || e.altKey || e.metaKey || e.repeat) return;
  if (t && t.closest && t.closest('.ask')) return; // an ask's own keys, handled by its block
  if (e.key === 'r' && p.s.canResume() && !menu.value) { e.preventDefault(); resumeKey(); return; }
  if (e.key === '[') { cycleRight(-1); return; }
  if (e.key === ']') { cycleRight(1); return; }
  if (e.key === 'i' || e.key === 'Enter') {
    if (t && t.getAttribute && t.getAttribute('role') === 'button') return;
    e.preventDefault(); toComposer();
  }
});

document.addEventListener('mousedown', outside);
addEventListener('resize', () => { ui.fit.value = 0; });
// Looking at a session reads what arrived in it.
effect(() => { const f = focused.value; if (f) f.s.unread.value = 0; });
// The inspector keeps the chat it last showed while an xpane has focus.
effect(() => { const f = focused.value; if (f) ui.lastChat.value = f.id; });

// ---------------------------------------------------------------- what a feature folder calls back
// A feature cannot import this file (this file imports it): it reaches the page through `host` (core/registry.js has each name's meaning).
const cwdOf = (p) => (!p ? '' : isXPane(p) ? String(paneInfo(p, 'cwd') || '') : (p.s.hello.peek() || {}).cwd || '');
// Open that side's panel on that tab (a closed panel opens, and on a narrow screen the other side closes, as a rail click does).
function showTab(side, name) {
  if (!tabsOn(side).some((t) => t.name === name)) return;
  const left = side === 'l', rail = left ? 'lrail' : 'rrail';
  if (ui[rail].peek()) toggle(rail, name); else (left ? ui.left : ui.right).value = name;
}
Object.assign(host, {
  toast,
  openXPane,
  closePane,
  focusPane,
  focused: () => focusedAny.peek(),
  inspected: () => inspected.peek(),
  chatPanes: () => ui.panes.peek(),
  xpanes: () => ui.xpanes.peek(),
  cwd: () => cwdOf(focusedAny.peek()) || cwdOf(inspected.peek()),
  showTab,
  chatStreams: () => holdingSids(null).size,
  openCmd,
});

// ---------------------------------------------------------------- boot
// A registered command that took a base command's name hid it: say which, once.
for (const name of mergeCommands(BASE_COMMANDS, commands.peek()).overridden) console.info(`minerva: the registered :${name} replaces the page's own :${name}`);
startLook();
ui.token = takeToken();
// A token pasted into this tab's URL later (a hash change, no reload) is
// taken and scrubbed the same way; it is never left sitting in the URL.
addEventListener('hashchange', () => { if (/token=/.test(location.hash)) takeToken(); });
const root = document.getElementById('root');
render(html`<${App} />`, root);
// Narrow: panels overlay the tiles, so crossing into narrow closes both (the rails stay).
ui.narrow.value = innerWidth < NARROW_CELLS * cell.peek().cw;
let wasNarrow = ui.narrow.peek();
const checkNarrow = () => {
  const narrow = innerWidth < NARROW_CELLS * cell.peek().cw;
  batch(() => {
    ui.narrow.value = narrow;
    if (narrow && !wasNarrow) { ui.lrail.value = true; ui.rrail.value = true; }
  });
  wasNarrow = narrow;
};
addEventListener('resize', checkNarrow);
effect(() => { cell.value; checkNarrow(); });
effect(() => { ui.lrail.value; ui.rrail.value; ui.left.value; ui.right.value; saveRails(); });
// Not in rAF: a tab opened in the background must still connect. A door is
// opened on at once; under the perch the page opens panes on the sessions that
// are already live (at most 4) and never starts one.
// Nothing answering (a perch or a door still coming up) is not final: it is
// probed again with a growing, jittered wait until something answers, and the
// screen clears itself then.
async function boot(n = 0) {
  const m = await detect();
  if (m === 'door') { ui.gate.value = null; openPane(DOOR); return; }
  if (m === 'perch') {
    ui.gate.value = null;
    startPolling();
    loadProjects();
    // One pane, on the newest live chat; with none live the start screen shows.
    const newest = rows().filter((x) => x.state === 'live').sort((a, b) => (b.mtime || 0) - (a.mtime || 0))[0];
    if (newest) openPane(newest.id);
    return;
  }
  if (m === 'refused') { ui.gate.value = 'refused'; return; }
  if (n === 0) toast('nothing is answering; trying again', true);
  else ui.gate.value = 'offline';
  setTimeout(() => boot(n + 1), backoffMs(Math.min(n, 4) + 3, 30000));
}
// The one-time link `hoot` opens (#code=...): scrubbed from the URL at once, then traded for a
// login cookie. No token is ever in this page on that path. A used or old link says so in words.
let booted = false;
function bootOnce() { if (!booted) { booted = true; boot(); } }
// The cookie outlives the link: a reload, a new tab, or the launched URL reopened from history has no
// code and no token in it but may already be signed in. One probe says: 200 is signed in.
async function probeCookie() {
  ui.login.value = 'wait';
  const signedIn = (await detect()) === 'perch';
  if (signedIn) ui.token = true;
  ui.login.value = '';
  return signedIn;
}
async function signIn(code) {
  ui.login.value = 'wait';
  const ok = await redeem(code);
  // A refused code with a cookie that still works (the link was opened a second time) is not an expiry.
  if (ok) { ui.token = true; ui.login.value = ''; bootOnce(); }
  else if (await probeCookie()) bootOnce();
  else ui.login.value = 'expired';
}
{
  const code = takeCode();
  if (code !== null) signIn(code);
  else if (ui.token) bootOnce();
  else probeCookie().then((signedIn) => { if (signedIn) bootOnce(); });
}
addEventListener('hashchange', () => { const c = takeCode(); if (c !== null) signIn(c); });
