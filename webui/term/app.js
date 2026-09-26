// The screen: sidebars, tiles, the hint line, the status line, keys, the `:`
// line. Sessions live in core/state.js and panes draw them (panes/session.js);
// this file arranges panes and routes every action. Every action is reachable
// by mouse; keys and the `:` line are shortcuts to the same functions.
import { html, render, K, HUB, signal, computed, effect, batch, useRef, useLayoutEffect, useMemo } from './core/ui.js';
import { takeToken, DOOR } from './core/api.js';
import { Session, tildify, kfmt, stateOf, STATE_CLS } from './core/state.js';
import * as tile from './core/tile.js';
import { menu, openMenu, closeMenu, outside, atPointer, under, Menus } from './core/menu.js';
import { Pane } from './panes/session.js';
import { panel } from './panes/asks.js';
import { FilesTab } from './panes/files.js';
import { FONTS, DEFAULT_FONT, theme, themes, font, cell, setTheme, setFont, resolve, startLook } from './core/look.js';

const LEFT_TABS = ['sessions', 'projects', 'mesh'];
const RIGHT_TABS = ['rec', 'loaded', 'files', 'diff', 'graph', 'run']; // components: INSPECTOR below
// Sidebar widths in cells: the defaults a double-click on the border goes back to, and the bounds.
const SIDES = { l: 30, r: 34 }, SIDE_MIN = 16, SIDE_MAX = 72, SIDES_KEY = 'minerva.sidebars';

// ---------------------------------------------------------------- screen state
const ui = {
  panes: signal([]),            // [{ id, sid, s: Session }]; panes on one sid share s; a pane's sid can change (showIn)
  tree: signal(null),           // core/tile.js split tree of pane ids
  focus: signal(null),          // pane id
  left: signal('sessions'),
  right: signal('rec'),
  lrail: signal(innerWidth < 900),
  rrail: signal(innerWidth < 900),
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
  token: false,
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
const byId = (id) => ui.panes.value.find((p) => p.id === id);
const focused = computed(() => byId(ui.focus.value) || null);
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
  return p ? p.s : new Session(sid, { toast, seen: (s) => !!focused.peek() && focused.peek().s === s });
};

// A new pane on `sid`, splitting pane `at` (default: the focused one) to the
// right (`dir` 'row'), below ('col'), or along its longer side.
function openPane(sid = DOOR, { at = ui.focus.value, dir } = {}) {
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
  if (!p) { openPane(); return; }
  openPane(p.sid, { at: p.id, dir });
}

// Show session `sid` in pane `id` instead of the one it shows. The pane is
// drawn afresh (Tiles keys it by id and sid); the old Session closes when no
// pane still shows it. Messages queued in the pane are for the session it
// showed: switching drops them, so it asks first.
function showIn(id, sid, sure = false) {
  const p = byId(id);
  if (!p || p.sid === sid) return;
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
  toast(last ? 'pane closed, and this page stopped following its session (the door keeps it). :open follows it again'
    : 'pane closed; another pane still shows its session');
}

const focusPane = (id) => { if (ui.focus.value !== id) ui.focus.value = id; };
const withPane = (fn) => () => { const p = focused.value; if (p) fn(p); else toast('no pane focused'); };
const cancel = withPane((p) => p.s.cancel());
const toComposer = withPane((p) => p.focusComposer && p.focusComposer());

// Every open session once, as [sid, Session].
const sessions = () => [...new Map(ui.panes.value.map((p) => [p.sid, p.s]))];
const sessionName = (s) => {
  const hl = s.hello.value;
  return hl ? hl.session.split('/').pop().replace(/\.[^.]+$/, '') || 'session' : s.sid;
};
// A session's project is its working directory, named by its last part.
const project = (cwd) => String(cwd || '').split('/').filter(Boolean).pop() || cwd || '?';

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
  if (at && at.tagName === 'TEXTAREA' && at.closest('.composer')) returnTo = at;
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
  const busy = holdsText(at) || menu.peek() || ui.cmd.peek() != null || here;
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

const toggle = (rail) => { ui[rail].value = !ui[rail].value; };
function cycleRight(d) {
  const i = RIGHT_TABS.indexOf(ui.right.value);
  ui.right.value = RIGHT_TABS[(i + d + RIGHT_TABS.length) % RIGHT_TABS.length];
}

// ---------------------------------------------------------------- hint line
function toast(text, err) {
  if (ui.hint.value && ui.hint.value.mode === 'menu') return;
  ui.hint.value = { mode: 'toast', text, err };
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { if (ui.hint.value && ui.hint.value.mode === 'toast') ui.hint.value = null; }, err ? 7000 : 3500);
}

// The key reference (`?`), as the TUI's (`space ?`): every key by where it
// works, the commands last. The first section is what the keys do where
// focus is now. Each entry is clickable and does what its key does; an
// entry under `asks` takes you to the ask panel, where the key works.
const KEYREF = [
  ['anywhere', [
    ['i', 'write in the focused pane', toComposer],
    [':', 'a command (tab completes)', () => openCmd()],
    ['tab ⇧tab', 'next / previous waiting ask', () => cycleAsk(1)],
    ['^c', 'stop the turn', cancel],
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
    ['^b', 'the sessions sidebar', () => toggle('lrail')],
    ['^i', 'the inspector', () => toggle('rrail')],
    ['[ ]', 'inspector tab', () => cycleRight(1)],
    ['/', 'filter files (files tab)', () => { ui.rrail.value = false; ui.right.value = 'files'; requestAnimationFrame(() => { const f = document.querySelector('#right .filter input'); if (f) f.focus(); }); }],
  ]],
];

const keyRef = () => { ui.keys.value = !ui.keys.peek(); };

// ---------------------------------------------------------------- the : line
const COMMANDS = {
  q: ['close the pane', () => closePane()],
  close: ['close the pane', () => closePane()],
  open: ['open a pane on this door (or on session id <arg>, with the hub)', (arg) => openPane(arg || DOOR)],
  vsplit: ['mirror the focused pane to the right', () => mirrorPane(undefined, 'row')],
  split: ['mirror the focused pane below', () => mirrorPane(undefined, 'col')],
  stop: ['cancel the running turn', cancel],
  submit: ['send every answered ask (all must be answered)', () => submitAsks() || toast(queue.peek().length ? 'answer every ask to submit' : 'nothing to submit')],
  queue: ['queue text (or the composer) behind the turn', (arg) => withPane((p) => p.s.submit('queue', arg || ''))()],
  reconnect: ['drop the stream and replay', withPane((p) => p.s.reconnect())],
  sidebar: ['toggle the left sidebar', () => toggle('lrail')],
  inspector: ['toggle the inspector', () => toggle('rrail')],
  keys: ['the key reference', () => keyRef()],
  theme: ['colours: :theme <name> (none: list them)', (arg) => pickLook('theme', arg)],
  font: ['font: :font <name> (none: list them)', (arg) => pickLook('font', arg)],
};

function pickLook(what, arg) {
  const names = what === 'theme' ? themes.value : Object.keys(FONTS);
  const cur = (what === 'theme' ? theme : font).value;
  if (!arg) { toast(`${what}s: ${names.map((n) => (n === cur ? `[${n}]` : n)).join(' · ')}`); return; }
  const set = what === 'theme' ? setTheme : setFont;
  set(resolve(names, arg)).then((err) => err && toast(err, true));
}

function openCmd() {
  closeMenu();
  batch(() => { ui.cmd.value = ''; ui.hint.value = { mode: 'menu' }; });
}
function closeCmd() {
  batch(() => { ui.cmd.value = null; if (ui.hint.value && ui.hint.value.mode === 'menu') ui.hint.value = null; });
}
const matches = (text) => {
  const w = (text || '').trim().split(/\s+/)[0] || '';
  return Object.keys(COMMANDS).filter((c) => c.startsWith(w));
};
function runCmd(text = ui.cmd.value || '') {
  const [w, ...rest] = text.trim().split(/\s+/);
  closeCmd();
  if (!w) return;
  const c = COMMANDS[w];
  if (c) c[1](rest.join(' '));
  else toast(`:${w}: session commands arrive with the registry (M4)`, true);
}

// ---------------------------------------------------------------- menus
// What a split opens: in M1 only a mirror; new, resume and another session
// arrive with the hub.
function splitItems(p, dir) {
  const others = sessions().filter(([sid]) => sid !== p.sid);
  return [
    { label: 'mirror', hint: dir === 'row' ? ':vsplit' : ':split', on: () => openPane(p.sid, { at: p.id, dir }) },
    { label: 'new session', off: HUB },
    { label: 'resume…', off: HUB },
    others.length ? { label: 'existing session', sub: others.map(([sid, s]) => ({ label: sessionName(s), on: () => openPane(sid, { at: p.id, dir }) })) }
      : { label: 'existing session', off: HUB },
  ];
}

function showItems(p) {
  return [
    ...sessions().map(([sid, s]) => (sid === p.sid ? { label: sessionName(s), tick: true, off: 'shown here' }
      : { label: sessionName(s), on: () => showIn(p.id, sid) })),
    { sep: true },
    { label: 'new session', off: HUB },
    { label: 'resume…', off: HUB },
  ];
}

const paneMenu = (p, at) => openMenu([
  { label: 'split right', sub: splitItems(p, 'row') },
  { label: 'split down', sub: splitItems(p, 'col') },
  { label: 'show…', sub: showItems(p) },
  { label: 'mirror', on: () => mirrorPane(p.id) },
  { label: 'reconnect', hint: ':reconnect', on: () => p.s.reconnect() },
  { label: 'close', hint: '⌥w  :q', on: () => closePane(p.id) },
], at, { title: 'pane', owner: 'pane-' + p.id });

const splitMenu = (p, dir, at) => openMenu(splitItems(p, dir), at,
  { title: dir === 'row' ? 'split right' : 'split down', owner: `split-${dir}-${p.id}` });

function sessionMenu(e, p) {
  e.preventDefault();
  const f = focused.value;
  openMenu([
    { label: 'open in new pane', on: () => openPane(p.sid) },
    { label: 'show in focused pane', off: !f ? 'no pane focused' : f.sid === p.sid ? 'shown there' : null, on: () => showIn(f.id, p.sid) },
    { label: 'fork (latest)', off: HUB },
    { label: 'close its door', off: HUB },
  ], atPointer(e), { title: sessionName(p.s) });
}

function statusMenu(e) {
  e.preventDefault();
  const p = focused.value, hl = p && p.s.hello.value;
  openMenu([
    { label: 'loaded', hint: 'model, gate, tools', on: openLoaded },
    { label: 'theme', sub: themes.value.map((n) => ({ label: n, tick: n === theme.value, on: () => pickLook('theme', n) })) },
    { label: 'font', sub: Object.entries(FONTS).map(([k, f]) => ({ label: f.label, hint: k === DEFAULT_FONT ? 'default' : '', tick: k === font.value, on: () => pickLook('font', k) })) },
    { sep: true },
    ...Object.entries(COMMANDS).map(([c, [what, fn]]) => ({ label: ':' + c, hint: what, on: () => fn('') })),
  ], atPointer(e), { title: 'status' });
}

// ---------------------------------------------------------------- views
const act = {
  name: sessionName,
  project,
  focus: focusPane,
  paneMenu,
  splitMenu,
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

// Frame-title tabs, plus the arrow that folds the sidebar to its rail
// (pointing at the edge it folds to).
function Tabs({ names, cur, pick, collapse, title, side }) {
  const fold = html`<${K} cls="tab fold" title=${title} on=${collapse}>${side === 'left' ? '«' : '»'}</${K}>`;
  return html`<div class="ftitle">
    ${side === 'left' && fold}
    ${names.map((n) => html`<${K} cls=${'tab' + (n === cur ? ' on' : '')} on=${() => pick(n)}>${n === cur ? `[${n}]` : n}</${K}>`)}
    ${side !== 'left' && fold}
  </div>`;
}

const Needs = ({ children }) => html`<div class="needs">${children}</div>`;
const Kv = ({ k, v, cls = '' }) => html`<div class="kv"><span class="faint">${k}</span><span class=${'grow ' + cls}>${String(v)}</span></div>`;

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
function Sessions() {
  const groups = new Map();
  for (const x of ui.panes.value) {
    const hl = x.s.hello.value, cwd = hl ? hl.cwd : '';
    if (!groups.has(cwd)) groups.set(cwd, []);
    groups.get(cwd).push(x);
  }
  const fold = (cwd) => {
    const f = new Set(ui.folded.value);
    if (!f.delete(cwd)) f.add(cwd);
    ui.folded.value = f;
  };
  return [...groups].map(([cwd, ps]) => {
    const open = !ui.folded.value.has(cwd), name = cwd ? project(cwd) : 'connecting…';
    const unread = ps.reduce((n, x, i) => n + (ps.findIndex((y) => y.s === x.s) === i ? x.s.unread.value : 0), 0);
    return html`<div class="grp" key=${cwd}>
      <div class="gh"><${K} cls="gname" title=${(cwd ? tildify(cwd) + '\n' : '') + (open ? 'fold' : 'open')} on=${() => fold(cwd)}>${open ? '▾' : '▸'} ${name}</${K}>
        <${K} off cls="plus" title=${'new session in ' + name + ': ' + HUB}>+</${K}>
        ${!open && unread > 0 && html`<span class="o">[${unread}]</span>`}</div>
      ${open && html`<div class="list">${ps.map((x) => html`<${SessionItem} key=${x.id} p=${x} />`)}</div>`}
    </div>`;
  });
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
  const p = focused.value, hl = p && p.s.hello.value;
  let body;
  if (tab === 'sessions') {
    body = html`<${Owl} />
      ${panes.length ? html`<${Sessions} />` : html`<div class="faint">no pane. <${K} on=${() => openPane()}>open one</${K}></div>`}
      <div class="hdr">peers</div><${Needs}>peer list needs the hub</${Needs}>
      <div class="hdr">kept</div><${Needs}>other sessions need the hub (M2)</${Needs}>`;
  } else if (tab === 'projects') {
    body = html`<div class="hdr">this door</div>
      ${hl ? html`<div class="list"><div class="it"><div class="l1"><span class="faint">▸</span><span class="grow c" title=${hl.cwd}>${tildify(hl.cwd)}</span></div></div></div>` : html`<div class="faint">—</div>`}
      <div class="hdr">others</div><${Needs}>project list needs the hub</${Needs}>`;
  } else {
    body = html`<div class="note">node        paired  grants  seen</div>
      <div class="note dim">──────────  ──────  ──────  ────</div>
      <${Needs}>mesh reads Aoide through the hub (M3)</${Needs}>`;
  }
  return html`<aside id="left" class="col side frame">
    ${!rail && html`<${Tabs} names=${LEFT_TABS} cur=${tab} pick=${(n) => { ui.left.value = n; }} side="left" collapse=${() => toggle('lrail')} title="collapse to a rail (ctrl+b)" />`}
    ${!rail && html`<div class="inner">${body}</div>`}
    ${!rail && html`<${Edge} d="l" />`}
    ${rail && html`<div class="rail k" role="button" tabindex="0" title="open the sidebar (ctrl+b)" onClick=${() => toggle('lrail')}>${tab} ▾</div>`}
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
    <div class="hdr">parked</div><${Needs}>live park samples need the park-tick frame (gap 7)</${Needs}>`;
}

// What this door has loaded, as far as it says: the hello frame names the
// model and the gate (yolo); the tools are the ones called so far (no door
// route lists them), MCP tools grouped by server when named `mcp__<server>__<tool>`.
// The rest is listed dim with what it needs, not hidden.
function LoadedTab({ s }) {
  const hl = s.hello.value, open = useMemo(() => signal(false), []);
  if (!hl) return html`<div class="faint">waiting for the door's hello</div>`;
  const tools = [...s.tools.value].sort((a, b) => a[0].localeCompare(b[0]));
  const m = String(hl.model || '?'), at = m.search(/[/:]/);
  const mcp = new Map();
  for (const [t, n] of tools) {
    const x = /^mcp__(.+?)__(.+)$/.exec(t);
    if (x) { if (!mcp.has(x[1])) mcp.set(x[1], []); mcp.get(x[1]).push([x[2], n]); }
  }
  const plain = tools.filter(([t]) => !t.startsWith('mcp__'));
  const count = ([t, n]) => html`<div class="kv"><span class="hi">${t}</span><span class="grow"></span><span class="faint">${n}×</span></div>`;
  const per = hl.persona;
  return html`
    <div class="hdr">persona</div>
    ${per ? html`<div class="m b">${per.name || per}</div>${per.text && html`<${K} cls="faint" on=${() => { open.value = !open.value; }}>${open.value ? '▾ ' : '▸ '}excerpt</${K}>${open.value ? html`<div class="pre">${per.text}</div>` : html`<div class="faint">${String(per.text).slice(0, 120)}…</div>`}`}`
      : html`<${Needs}>the persona needs the hub (the door's hello does not name it)</${Needs}>`}
    <div class="hdr">model</div>
    <${Kv} k="model    " v=${at > 0 ? m.slice(at + 1) : m} cls="c" />
    ${at > 0 ? html`<${Kv} k="provider " v=${m.slice(0, at)} />` : html`<${Needs}>provider: not in the model key</${Needs}>`}
    <${Needs}>effort needs the hub</${Needs}>
    <div class="hdr">gate</div>
    <${Kv} k="mode     " v=${hl.yolo ? 'YOLO: the gate answers for you' : 'gated: it asks when policy says'} cls=${hl.yolo ? 'r' : ''} />
    <${Kv} k="verdicts " v=${s.verdicts.value.length} />
    <${Needs}>the policy itself needs the hub</${Needs}>
    <div class="hdr">tools used</div>
    ${plain.length ? plain.map(count) : html`<div class="faint">none called yet</div>`}
    <${Needs}>the full tool list needs the hub (the door lists none)</${Needs}>
    <div class="hdr">mcp servers</div>
    ${mcp.size ? [...mcp].map(([srv, ts]) => html`<div class="kv"><span class="m">${srv}</span></div>${ts.map(([t, n]) => html`<div class="kv"><span class="faint">  </span><span class="hi">${t}</span><span class="grow"></span><span class="faint">${n}×</span></div>`)}`)
      : html`<${Needs}>MCP servers need the hub (none called yet)</${Needs}>`}
    <div class="hdr">extensions</div><${Needs}>extensions need the hub</${Needs}>
    <div class="hdr">session</div>
    <${Kv} k="file     " v=${tildify(hl.session)} />
    <${Kv} k="cwd      " v=${tildify(hl.cwd)} />
    <${Kv} k="protocol " v=${hl.protocol ?? '?'} />`;
}

// Git state (branch, +/−, PR) belongs at the top of this tab once a source exists.
function DiffTab({ s }) {
  const files = [...s.touched.value];
  return html`<div class="hdr">git</div><${Needs}>branch, +/− and PR need the hub</${Needs}>
    <div class="hdr">changed</div>
    ${files.length ? files.map(([f, n]) => html`<div class="kv"><span class="grow c" title=${f}>${f}</span><span class="faint">${n} call${n > 1 ? 's' : ''}</span></div>`)
      : html`<div class="faint">no edit or write calls yet</div>`}`;
}

// The graph tab's slot. M3 replaces this with a Preact graph component
// (see web-ui.md section 10); it gets the same { s, sid } props as every tab.
const GraphTab = () => html`<${Needs}>turn graph needs the hub (M3)</${Needs}>`;

// Inspector tab -> component. Every tab is a Preact component taking
// { s: Session, sid }, so a new tab (or a real graph) is one entry here.
export const INSPECTOR = { rec: Rec, loaded: LoadedTab, files: FilesTab, diff: DiffTab, graph: GraphTab, run: RunTab };

function Right() {
  const tab = ui.right.value, rail = ui.rrail.value, p = focused.value;
  const Tab = INSPECTOR[tab];
  const body = p && !rail ? html`<${Tab} key=${tab} s=${p.s} sid=${p.sid} />` : html`<div class="faint">no pane focused</div>`;
  return html`<aside id="right" class="col side frame">
    ${!rail && html`<${Tabs} names=${RIGHT_TABS} cur=${tab} pick=${(n) => { ui.right.value = n; }} collapse=${() => toggle('rrail')} title="collapse to a rail (ctrl+i)" />`}
    ${!rail && html`<div class="inner">${body}</div>`}
    ${!rail && html`<${Edge} d="r" />`}
    ${rail && html`<div class="rail k" role="button" tabindex="0" title="open the inspector (ctrl+i)" onClick=${() => toggle('rrail')}>${tab} ▾</div>`}
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
  const panes = ui.panes.value, tree = ui.tree.value, box = ui.box.value, f = ui.focus.value;
  const lay = tile.layout(tree, box);
  const rect = new Map(lay.panes.map((r) => [r.id, r]));
  let body;
  if (!ui.token) {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle r">no token</div>
      <div class="empty">${'this page has no token.\n\nopen it from the launcher, or from the URL `eidolon web` printed\nwith #token=<contents of the token file> on the end.\n\nthe token is never sent to a server in the URL: the fragment stays in the browser.'}</div></div>`;
  } else if (!panes.length) {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle faint">no pane</div>
      <div class="empty">no pane open.${'\n\n'}<${K} cls="hi" on=${() => openPane()}>:open</${K}>  reconnect this session${'\n'}<span class="faint">ctrl+enter  new pane with a picker (arrives with the hub, M2)</span></div></div>`;
  } else {
    body = [
      ...panes.map((p) => rect.get(p.id) && html`<${Pane} key=${p.id + ' ' + p.sid} p=${p} rect=${rect.get(p.id)} focused=${p.id === f} mirror=${isMirror(p)} act=${act} />`),
      ...lay.dividers.map((d) => html`<${Divider} key=${d.path} d=${d} />`),
    ];
  }
  return html`<div id="tiles" class="col" ref=${ref}>${body}</div>`;
}

function Hint() {
  const h = ui.hint.value;
  if (!h) return html`<div id="hint"></div>`;
  if (h.mode === 'toast') return html`<div id="hint" class=${'toast' + (h.err ? ' err' : '')} aria-live="polite">${h.text}</div>`;
  const text = ui.cmd.value || '';
  const m = matches(text);
  return html`<div id="hint" class="menu">${m.length ? m.map((c, i) => html`<${K} cls=${'pair' + (i === 0 && text ? ' sel' : '')} on=${() => runCmd(c)}>${c}</${K}>${i === 0 && text ? html`<span class="faint"> ${COMMANDS[c][0]}   </span>` : ' '}`)
    : html`<span class="faint">session commands arrive with the registry (M4)</span>`}
    <${K} cls="pair faint" on=${closeCmd}>esc close</${K}></div>`;
}

function CmdLine() {
  const ref = useRef(null);
  useLayoutEffect(() => { ref.current && ref.current.focus(); }, []);
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
  const groups = [...(here.length ? [[HERE[at], here]] : []), ...KEYREF,
    ['commands', Object.entries(COMMANDS).map(([c, [what, fn]]) => [':' + c, what, () => fn('')])]];
  const take = (fn) => { ui.keys.value = false; panel.armed.value = true; fn(); }; // a click here is the operator choosing
  return html`<div class="frame pop keyref" ref=${ref} role="dialog" aria-label="key reference" onMouseDown=${(e) => e.preventDefault()}>
    <div class="ftitle">keys<span class="faint"> · click one, esc closes</span></div>
    <div class="kgroups">${groups.map(([name, rows]) => html`<div class="kg"><div class="kgh">${name}</div>
      ${rows.map(([k, what, fn]) => html`<${K} cls="kr" on=${() => take(fn)}><span class="kk">${k}</span><span class="kw">${what}</span></${K}>`)}</div>`)}</div>
  </div>`;
}

// The status line shows what is loaded; switching it is the composer's (its bottom border).
const openLoaded = () => batch(() => { ui.rrail.value = false; ui.right.value = 'loaded'; });

// Messages waiting for the focused pane's turn to end: held here, plus any the door holds.
const queuedN = (p) => p.queue.value.length + p.s.queued.value;

function Status() {
  const ref = useRef(null), sig = useRef('');
  const p = focused.value, s = p && p.s, hl = s && s.hello.value;
  const conn = s ? s.conn.value : null, st = stateOf(s);
  const budget = s && s.budget.value, waiting = unsent.value, lvl = ui.fit.value; // answered but not submitted still waits
  const t = tabs.value, cur = current.value, inAsk = ui.where.value === 'ask' || ui.where.value === 'note';
  const place = inAsk && cur ? t.findIndex((x) => x.a === cur.a) + 1 : 0;
  const now = st + waiting + (p ? 'q' + queuedN(p) : '') + (hl ? hl.model : '') + place + conn + ' ' + ui.box.value.w; // a resize re-fits
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (sig.current !== now) { sig.current = now; if (ui.fit.peek()) { ui.fit.value = 0; return; } }
    if (el.scrollWidth > el.clientWidth + 1 && ui.fit.peek() < FIT) ui.fit.value = ui.fit.peek() + 1;
  });
  if (ui.cmd.value != null) return html`<footer id="status" class="cmd"><${CmdLine} /></footer>`;
  const dot = html`<span class="faint"> · </span>`;
  const ctx = s && s.ctx.value, limit = hl && hl.context_limit;
  const gauges = s ? [
    lvl < 4 && `ctx ${ctx != null ? kfmt(ctx) : '-'}/${limit ? kfmt(limit) : '-'}`,
    lvl < 3 && `in ${kfmt(s.usedIn.value)} out ${kfmt(s.usedOut.value)}`,
    lvl < 2 && `tools ${s.calls.value}`,
  ].filter(Boolean).join(' · ') : '';
  return html`<footer id="status" onContextMenu=${statusMenu}><div class="bar" ref=${ref}>
    <span class="left">
      <span class=${'badge st-' + STATE_CLS[st]} title=${st === '?' ? 'this page connected after the turn began or before any turn event' : ''}>${st}</span>
      ${hl && html`<${K} cls="model b" title="the model the door reports: what is loaded" on=${openLoaded}>${hl.model || '?'}</${K}>`}
      ${hl && hl.persona && html`${dot}<${K} cls="persona b" title="the persona: what is loaded" on=${openLoaded}>${hl.persona.name || hl.persona}</${K}>`}
      ${hl && hl.yolo && html`${dot}<${K} cls="yolo b" title="the gate is answering for you" on=${openLoaded}>YOLO</${K}>`}
      ${place > 0 && html`${dot}<span class="o">ask ${place}/${t.length}</span>`}
      ${p && queuedN(p) > 0 && html`${dot}<span class="hi qn" title="held here, sent in order when the turn ends">${queuedN(p)} queued</span>`}
      ${waiting > 0 && html`${dot}<${K} cls="o waitn" title=${ui.held.value ? 'the oldest ask takes focus when you pause typing; click to go now' : queue.value.length ? 'focus the oldest ask' : 'every ask is answered: submit sends them'}
        on=${() => focusHead() || (current.peek() && focusQueued(current.peek()))}>${ui.held.value ? '▶ ' : ''}${waiting} waiting</${K}>`}
      ${p && conn !== 'live' && lvl < 5 && html`${dot}<span class=${conn === 'replaying' ? 'faint' : 'warn'}>${conn}</span>`}
      ${budget != null && budget < 3 && html`${dot}<span class="warn">${budget} calls left</span>`}
      ${s && s.queued.value > 0 && html`${dot}<span class="faint">${s.queued.value} queued</span>`}
      ${st === '?' && lvl < 1 && html`${dot}<span class="faint unk">state unknown until next turn event</span>`}
    </span>
    <span class="grow"></span>
    <span class="right">${gauges && html`<span class="faint">${gauges}</span>`}<span onMouseDown=${(e) => e.preventDefault()}><${K} cls="qk" title="the key reference" on=${keyRef}><span class="kk">?</span>${lvl < 5 ? ' keys' : ''}</${K}></span></span>
  </div></footer>`;
}

function App() {
  const cls = (ui.lrail.value ? 'lrail ' : '') + (ui.rrail.value ? 'rrail' : '');
  const w = ui.sides.value;
  return html`<main id="main" class=${cls} style=${`--lc:${w.l};--rc:${w.r}`}><${Left} /><${Tiles} /><${Right} /></main><${Hint} /><${Status} />${ui.keys.value && html`<${KeyRef} />`}<${Menus} />`;
}

effect(() => { const c = cell.value; batch(() => { ui.box.value = { ...ui.box.peek(), ...c }; ui.fit.value = 0; }); });

// ---------------------------------------------------------------- keys
document.addEventListener('keydown', (e) => {
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
  if (e.ctrlKey && e.key === 'Enter') { e.preventDefault(); toast('new pane with a picker arrives with the hub (M2); :vsplit mirrors this one'); return; }
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

// ---------------------------------------------------------------- boot
startLook();
ui.token = takeToken();
// A token pasted into this tab's URL later (a hash change, no reload) is
// taken and scrubbed the same way; it is never left sitting in the URL.
addEventListener('hashchange', () => { if (/token=/.test(location.hash)) takeToken(); });
const root = document.getElementById('root');
render(html`<${App} />`, root);
let wasNarrow = innerWidth < 760;
addEventListener('resize', () => {
  const narrow = innerWidth < 760;
  if (narrow && !wasNarrow) batch(() => { ui.lrail.value = true; ui.rrail.value = true; });
  wasNarrow = narrow;
});
if (ui.token) openPane(DOOR); // not in rAF: a tab opened in the background must still connect
