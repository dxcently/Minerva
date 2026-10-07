// Where a feature folder plugs into the page, so it never edits app.js: small lists that the page reads when it draws, routes a key or
// handles a frame, and the one `host` object it calls back through. A folder registers when its module is imported (app.js imports
// terms/index.js, notes/index.js, allow/list.js); a later registration of the same key replaces the earlier one in place. Pure: no DOM,
// so node tests it (tests/registry.test.mjs). A bad registration is said once on the console and left out: it never stops the page.
//
//   every list has   X.add(entry)  X.get(key)  X.remove(key)  X.list (the array; reading it in a render subscribes)  X.peek() (it does not)
//
//   rightTabs.add({ name, icon, component, global?, badge? })      an inspector tab, after the base ones, `component` takes { s, sid };
//   leftTabs.add({ name, icon, component, badge? })                a left panel tab, `component` takes { s, sid } of the inspected chat (null without)
//       `global`: drawn with no chat to look at ({ s: null, sid: null }). `badge`: a signal; 0, false, null: none; true: a dot; a number: its
//       digit (+ over 9); a text: its first character. Drawn after the icon as <span class="bd o">.
//   paneKinds.add({ kind, Pane, title?, status?, close?, cwd? })  a pane that is not a chat (host.openXPane(kind, data))
//       Pane takes { p, rect, focused, act } and draws ONE <section class="pane frame" style=${paneStyle(rect)} data-pane=${p.id} data-kind=${p.kind}
//       tabindex="-1" onMouseDown=${() => act.focus(p.id)}> (the page finds a pane by that data-pane); title(p), status(p) (text or vnode, for the
//       status line while the pane has focus), cwd(p) (a folder) and close(p) (called once when the pane leaves the tiles) are all optional
//   splitExtras.add((p, dir) => [item])                            more lines in every split menu; p is the pane being split (a chat or an xpane, or null)
//   chatMenuItems.add((row, { p, at }) => [item])                  more lines in the sidebar chat menus; row is the perch's list row for the chat
//   commands.add({ name, help, run })                              a `:` command; a base command of the same name is REPLACED (logged once at boot)
//   keyRows.add({ section?, keys, what, run, replaces? })          a row of the key reference; `replaces` is the `keys` text of the row it takes the place of
//   loadedSections.add(Component)                                  drawn under the loaded tab, takes { s, sid }
//   composerExtras.add(Component)                                  drawn above a chat's composer, takes { p, s, act }; returns null for a chat it has nothing for
//   keyOwners.add((el) => bool)                                    true: this element handles its own keys, the page's keys never see them
//   focusOwners.add((el) => bool)                                  true: focus is here and stays; an ask never takes it, and it is where focus returns to
//   whereOwners.add((el) => name | null)                           what the status line and the key reference call the place focus is
//   frameTaps.add((sid, frame, live) => void)                      sees every frame of every open chat stream, read only; live is false while a replay runs
//   extraStreams                                                   a signal: connections held besides the chats' (the terminal stream); chats get 4 - it
//
// The item shape of the menus is core/menu.js's: { label, on?, off?, sub?, tick?, hint? } | { sep: true }.
import { signal } from './ui.js';
import * as tile from './tile.js';

const NAME = /^[a-z][a-z0-9-]*$/;         // a tab or a pane kind: it is kept in localStorage and put in a class and an attribute
const WORD = /^[A-Za-z][A-Za-z0-9_-]*$/;  // a `:` command: one word, never `__proto__`
const isFn = (f) => typeof f === 'function';
const filled = (s) => typeof s === 'string' && s.trim() !== '';
const isSignal = (x) => !!x && typeof x === 'object' && isFn(x.peek) && 'value' in x;

// A registered function that throws is said once, then called on without a fuss: a feature that breaks must not take a key or a frame with it.
const told = new WeakSet();
function threw(fn, err) {
  if (told.has(fn)) return;
  told.add(fn);
  console.error('minerva: a registered function threw (said once):', err);
}
const safely = (fn, ...args) => { try { return fn(...args); } catch (err) { threw(fn, err); return undefined; } };

// ---------------------------------------------------------------- one list
// Ordered, replace-by-key. The array sits in a signal, so what draws from it redraws when something registers late (from the console).
export class Registry {
  constructor(what, keyOf, check) {
    this.what = what;
    this.keyOf = keyOf;
    this.check = check;  // (entry) => '' or why not
    this.items = signal([]);
  }
  get list() { return this.items.value; }
  peek() { return this.items.peek(); }
  get(key) { return this.items.value.find((x) => this.keyOf(x) === key) || null; }
  add(entry) {
    const bad = this.check(entry);
    if (bad) { console.error(`minerva: ${this.what} not registered: ${bad}`); return null; }
    const cur = this.items.peek(), key = this.keyOf(entry), at = cur.findIndex((x) => this.keyOf(x) === key);
    this.items.value = at < 0 ? [...cur, entry] : cur.map((x, i) => (i === at ? entry : x));
    return entry;
  }
  remove(key) {
    const cur = this.items.peek(), next = cur.filter((x) => this.keyOf(x) !== key);
    if (next.length === cur.length) return false;
    this.items.value = next;
    return true;
  }
}
const fns = (what) => new Registry(what, (f) => f, (f) => (isFn(f) ? '' : 'give a function'));

const tabCheck = (t) => (!t ? 'give a tab' : !NAME.test(t.name) ? 'name is a short lower-case word' : !filled(t.icon) ? 'icon is a symbol'
  : !isFn(t.component) ? 'component is a Preact component' : t.badge != null && !isSignal(t.badge) ? 'badge is a signal' : '');
const kindCheck = (k) => (!k ? 'give a kind' : !NAME.test(k.kind) ? 'kind is a short lower-case word' : !isFn(k.Pane) ? 'Pane is a Preact component'
  : ['title', 'status', 'close', 'cwd'].find((f) => k[f] != null && !isFn(k[f])) ? 'title, status, close and cwd are functions' : '');
const commandCheck = (c) => (!c ? 'give a command' : !WORD.test(c.name) ? 'name is one word' : typeof c.help !== 'string' ? 'help is a sentence' : !isFn(c.run) ? 'run is a function' : '');
const rowCheck = (r) => (!r ? 'give a row' : !filled(r.keys) ? 'keys is the text of the key column' : !filled(r.what) ? 'what is the text of the row' : !isFn(r.run) ? 'run is a function' : '');

export const rightTabs = new Registry('right tab', (t) => t.name, tabCheck);
export const leftTabs = new Registry('left tab', (t) => t.name, tabCheck);
export const paneKinds = new Registry('pane kind', (k) => k.kind, kindCheck);
export const splitExtras = fns('split extra');
export const chatMenuItems = fns('chat menu extra');
export const commands = new Registry('command', (c) => c.name, commandCheck);
export const keyRows = new Registry('key row', (r) => (r.section || 'panes') + '\n' + r.keys, rowCheck);
export const loadedSections = fns('loaded section');
export const composerExtras = fns('composer extra');
export const keyOwners = fns('key owner');
export const focusOwners = fns('focus owner');
export const whereOwners = fns('where owner');
export const frameTaps = fns('frame tap');

// ---------------------------------------------------------------- tabs
// The tabs the page has of its own, in their order; the rails draw these, then what registered. app.js holds their components (INSPECTOR, Left()).
export const BASE_LEFT = [{ name: 'sessions', icon: '≡' }, { name: 'projects', icon: '▦' }, { name: 'mesh', icon: '⋈' }];
export const BASE_RIGHT = [
  { name: 'rec', icon: '◉' }, { name: 'loaded', icon: '▣' }, { name: 'files', icon: '▤' }, { name: 'diff', icon: '±' }, { name: 'graph', icon: '⋔' }, { name: 'run', icon: '▶' },
];

// The base, then each registered tab in registration order. A registered tab named like a base one takes its place in the row, so a saved
// tab name still means the same slot. `component` is null for a base tab (the page has it).
export function mergeTabs(base, regs) {
  const out = base.map((t) => ({ name: t.name, icon: t.icon, component: null, global: false, badge: null }));
  for (const r of regs) {
    const e = { name: r.name, icon: r.icon, component: r.component, global: !!r.global, badge: r.badge || null };
    const at = out.findIndex((t) => t.name === e.name);
    if (at < 0) out.push(e); else out[at] = e;
  }
  return out;
}
export const tabsOf = (side) => (side === 'l' ? mergeTabs(BASE_LEFT, leftTabs.list) : mergeTabs(BASE_RIGHT, rightTabs.list));
export const tabNames = (side) => tabsOf(side).map((t) => t.name);

// A saved tab name stays if the tab is still there, else the default.
export const pickTab = (saved, names, dflt) => (names.includes(saved) ? saved : dflt);
// The tab `d` (1 or -1) steps from `cur`, round the ends; from a name that is not there, the first or the last.
export function cycleTab(names, cur, d) {
  const i = names.indexOf(cur);
  if (i < 0) return names[d > 0 ? 0 : names.length - 1];
  return names[(i + d + names.length) % names.length];
}
// What a rail icon's badge signal says, in one character: true a dot, a number its digit (+ over 9), a text its first character.
export function badgeText(v) {
  if (v === true) return '●';
  if (typeof v === 'number') { const n = Math.floor(v); return n > 0 ? (n > 9 ? '+' : String(n)) : ''; }
  if (typeof v === 'string') return [...v][0] || '';
  return '';
}

// ---------------------------------------------------------------- commands and key rows
// `base` is the page's { name: [help, run] }. A registered name replaces the base entry where it stands (the order is the base's, new names
// last); `overridden` names the ones that hid a base command, for the boot log.
export function mergeCommands(base, regs) {
  const table = Object.create(null), overridden = [];
  for (const [name, row] of Object.entries(base)) table[name] = row;
  for (const c of regs) {
    if (Object.hasOwn(base, c.name) && !overridden.includes(c.name)) overridden.push(c.name);
    table[c.name] = [c.help, c.run];
  }
  return { table, overridden };
}

// `base` is the key reference's [[section, [[keys, what, run]]]]. A registered row takes the place of the row whose keys text is its
// `replaces` (else its own `keys`), looked for in its section, else in every one; no such row: it is added at the end of its section (default
// `panes`), and a section that is not there yet comes last.
export function mergeKeyRows(base, regs) {
  const groups = base.map(([name, rows]) => [name, [...rows]]);
  for (const r of regs) {
    const row = [r.keys, r.what, r.run], target = r.replaces || r.keys, section = r.section || 'panes';
    let done = false;
    for (const [name, rows] of groups) {
      if (done || (r.section && name !== r.section)) continue;
      const at = rows.findIndex((x) => x[0] === target);
      if (at >= 0) { rows[at] = row; done = true; }
    }
    if (done) continue;
    const g = groups.find(([name]) => name === section);
    if (g) g[1].push(row); else groups.push([section, [row]]);
  }
  return groups;
}

// ---------------------------------------------------------------- who owns a key, a focus, a name
// An owner answers for an element. The first to claim it wins; one that throws counts as no.
const claim = (owners, el) => !!el && owners.peek().some((f) => !!safely(f, el));
export const keyClaimed = (el) => claim(keyOwners, el);
export const focusClaimed = (el) => claim(focusOwners, el);
// The name of the first owner that has one, else null (the page's own names come after).
export function whereOf(el) {
  if (!el) return null;
  for (const f of whereOwners.peek()) {
    const n = safely(f, el);
    if (typeof n === 'string' && n) return n;
  }
  return null;
}

// ---------------------------------------------------------------- frames
// core/state.js calls this for every frame, before it reads it. Each tap once, in registration order, a throwing one skipped.
export function runTaps(sid, frame, live = false) {
  for (const tap of frameTaps.peek()) safely(tap, sid, frame, live);
}

// ---------------------------------------------------------------- the connection budget
// The browser holds six connections per host across all its tabs; the chats' streams take 4 of them, a poll one, an answer one. A terminal
// stream (terms/) takes one of the chats' four: `extraStreams` says how many such streams are open.
export const MAX_SIDS = 4;
export const extraStreams = signal(0);
export const chatCap = (extra = extraStreams.peek()) => Math.max(1, MAX_SIDS - Math.max(0, Math.floor(extra) || 0)); // chats always get one
// Whether a chat `sid` may not take a stream now: it holds none yet and the chats already holding one fill the cap. `sids` is the Set of those.
export const overCap = (sids, sid, extra = extraStreams.peek()) => !sids.has(sid) && sids.size >= chatCap(extra);
// How many more streams there are room for, given how many chat sessions hold one: what a feature checks before it opens its own.
export const streamRoom = (open, extra = extraStreams.peek()) => MAX_SIDS - Math.max(0, Math.floor(extra) || 0) - open;

// ---------------------------------------------------------------- panes that are not chats
// A chat pane is { id, sid, s, ... }; any other is { id, kind, ... } and lives in ui.xpanes. Both are leaves of the one tile tree.
export const isXPane = (p) => !!p && typeof p.kind === 'string';
// What a split of pane `p` works on: the chat it may mirror (null for an xpane, which has nothing to mirror) and the id it anchors on.
export const anchorOf = (p) => ({ chat: p && !isXPane(p) ? p : null, at: p ? p.id : undefined });
// A pane's place and size from its tile rectangle (a pane and its neighbour share one border line).
export const paneStyle = (r) => `left:${r.x - (r.x > 0 ? 1 : 0)}px;top:${r.y - (r.y > 0 ? 1 : 0)}px;width:${r.w + (r.x > 0 ? 1 : 0)}px;height:${r.h + (r.y > 0 ? 1 : 0)}px`;

// The state a new xpane / a closed one leaves behind, as plain data: { xpanes, tree, focus }. app.js sets its signals from the result.
export function addXPane(st, x, { at = st.focus, box, dir } = {}) {
  return { xpanes: [...st.xpanes, x], tree: tile.add(st.tree, at, x.id, box, dir), focus: x.id };
}
export function dropXPane(st, id) {
  const tree = tile.remove(st.tree, id), rest = tile.leaves(tree);
  return { xpanes: st.xpanes.filter((x) => x.id !== id), tree, focus: st.focus === id ? rest[rest.length - 1] || null : st.focus };
}
// What a pane kind says about a pane, or undefined: the kind's title(p), status(p), cwd(p) or close(p).
export function paneInfo(p, what) {
  const k = p && paneKinds.get(p.kind), f = k && k[what];
  return isFn(f) ? safely(f, p) : undefined;
}

// ---------------------------------------------------------------- menus
// The items every registered function offers, in registration order; one that throws or answers with no list offers none.
const collect = (fns, ...args) => {
  const out = [];
  for (const f of fns) { const r = safely(f, ...args); if (Array.isArray(r)) out.push(...r.filter(Boolean)); }
  return out;
};
export const splitExtraItems = (p, dir) => collect(splitExtras.peek(), p, dir);
export const chatMenuExtras = (row, ctx) => collect(chatMenuItems.peek(), row, ctx);

// ---------------------------------------------------------------- the page, as a feature sees it
// app.js fills this in once, before it draws (a feature folder cannot import app.js: app.js imports it). Call it from handlers, not at import.
//   toast(text, err?, act?)      a line in the hint row; act = { label, on } adds a clickable word to it
//   openXPane(kind, data?, { at?, dir? })   a pane of a registered kind, beside pane `at` (default the focused one) to the right ('row') or below ('col')
//   closePane(id)  focusPane(id)  focused()  inspected()  chatPanes()  xpanes()
//     focused() is the focused pane of either kind; inspected() the focused chat, else the last one that was (what the inspector shows)
//   cwd()                        the folder of the focused pane (a chat's, else its kind's cwd(p)), else of the inspected chat, else ''
//   showTab('l' | 'r', name)     open that side's panel on that tab
//   chatStreams()                how many chat sessions hold an event stream now (with streamRoom: may a feature open its own?)
//   openCmd(prefill?)            the `:` line
export const host = {};
