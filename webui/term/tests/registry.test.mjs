// The registries (core/registry.js): where a feature folder plugs into the page. Pure: no DOM. app.js is the only file that draws from them and it
// cannot load here, so the last tests read its source (and session.js's, state.js's, index.html's) to hold the wiring in place: the order of the
// key owner before ctrl+b, the taps before the switch, the Home check counting xpanes. core/state.js loads with two stubs it needs at import.
import { test, mock, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { signal } from '../core/ui.js';
import * as tile from '../core/tile.js';
import * as R from '../core/registry.js';

// As LF: a checkout with core.autocrlf has CRLF sources, and the wiring tests search for '\n}\n'.
const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
const C = () => null;
const names = (tabs) => tabs.map((t) => t.name);
afterEach(() => mock.restoreAll());

// ---------------------------------------------------------------- tabs
// The tables app.js had before the registry: a saved tab name and the rail icons must mean the same as they did.
const OLD = {
  left: ['sessions', 'projects', 'mesh'],
  right: ['rec', 'loaded', 'files', 'diff', 'graph', 'run'],
  icons: { sessions: '≡', projects: '▦', mesh: '⋈', rec: '◉', loaded: '▣', files: '▤', diff: '±', graph: '⋔', run: '▶' },
};

test('with nothing registered the tab tables are the old ones: the same names, order and icons', () => {
  const l = R.mergeTabs(R.BASE_LEFT, []), r = R.mergeTabs(R.BASE_RIGHT, []);
  assert.deepEqual(names(l), OLD.left);
  assert.deepEqual(names(r), OLD.right);
  for (const t of [...l, ...r]) {
    assert.equal(t.icon, OLD.icons[t.name], t.name);
    assert.equal(t.component, null, `${t.name}: the page holds a base tab's component`);
    assert.equal(t.global, false);
    assert.equal(t.badge, null);
  }
});

test('registered tabs follow the base ones in registration order; one named like a base tab takes its place', () => {
  const badge = signal(0);
  const t = R.mergeTabs(R.BASE_RIGHT, [{ name: 'zeta', icon: 'z', component: C }, { name: 'alpha', icon: 'a', component: C, global: true, badge }]);
  assert.deepEqual(names(t), [...OLD.right, 'zeta', 'alpha']);
  assert.equal(t.find((x) => x.name === 'zeta').global, false);
  const alpha = t.find((x) => x.name === 'alpha');
  assert.equal(alpha.global, true);
  assert.equal(alpha.badge, badge);
  assert.equal(alpha.component, C);
  const over = R.mergeTabs(R.BASE_RIGHT, [{ name: 'diff', icon: 'D', component: C }]);
  assert.deepEqual(names(over), OLD.right, 'the row keeps its order, so a saved name keeps its slot');
  assert.equal(over[3].icon, 'D');
  assert.equal(over[3].component, C);
  assert.deepEqual(names(R.mergeTabs(R.BASE_LEFT, [{ name: 'notes', icon: 'n', component: C }])), [...OLD.left, 'notes']);
});

test('the registry hands the tables to the rails: a late registration shows, a removed one is gone', () => {
  R.rightTabs.add({ name: 'zz-late', icon: 'z', component: C });
  try {
    assert.deepEqual(R.tabNames('r').slice(0, OLD.right.length), OLD.right);
    assert.ok(R.tabNames('r').includes('zz-late'));
    assert.deepEqual(R.tabNames('l').slice(0, OLD.left.length), OLD.left);
    assert.ok(!R.tabNames('l').includes('zz-late'), 'a right tab is not a left one');
  } finally { R.rightTabs.remove('zz-late'); }
  assert.ok(!R.tabNames('r').includes('zz-late'));
});

test('the stubs register themselves: terms last on the right (global), notes on the left, each a single line', async () => {
  await import('../terms/index.js');
  await import('../notes/index.js');
  await import('../allow/list.js');
  const r = R.tabsOf('r'), l = R.tabsOf('l');
  assert.deepEqual(names(r).slice(0, OLD.right.length), OLD.right, 'the base keeps its order');
  assert.deepEqual(names(l).slice(0, OLD.left.length), OLD.left);
  const terms = r.find((t) => t.name === 'terms'), notes = l.find((t) => t.name === 'notes');
  assert.ok(terms && notes);
  assert.equal(terms.icon, '⌨︎', 'a text-style keyboard, never the emoji');
  assert.equal(terms.global, true);
  assert.equal(notes.icon, '✎');
  assert.equal(notes.global, false);
  assert.equal(typeof terms.component, 'function');
  assert.equal(typeof notes.component, 'function', 'the notepad (slice N2a, tests/notes.test.mjs) is no one-line stub any more; its rail badge is the signal cards.js holds');
  assert.equal(typeof notes.badge.peek, 'function');
  assert.equal(R.loadedSections.peek().length, 1, 'allow/list.js is the one loaded section');
  assert.equal(R.loadedSections.peek()[0](), null, 'and it draws nothing yet');
});

test('a saved tab name still validates; one no tab has falls back to the default', () => {
  const right = names(R.mergeTabs(R.BASE_RIGHT, [{ name: 'terms', icon: 't', component: C }]));
  for (const n of OLD.right) assert.equal(R.pickTab(n, right, 'rec'), n);
  for (const n of OLD.left) assert.equal(R.pickTab(n, OLD.left, 'sessions'), n);
  assert.equal(R.pickTab('terms', right, 'rec'), 'terms', 'a registered tab, saved, comes back');
  assert.equal(R.pickTab('terms', OLD.right, 'rec'), 'rec', 'the same name once the tab is gone');
  for (const v of [undefined, null, '', 5, 'nope']) assert.equal(R.pickTab(v, OLD.right, 'rec'), 'rec');
});

test('cycleTab steps round both ends, and from a name that is not there starts at an end', () => {
  const n = ['a', 'b', 'c'];
  assert.equal(R.cycleTab(n, 'a', 1), 'b');
  assert.equal(R.cycleTab(n, 'c', 1), 'a');
  assert.equal(R.cycleTab(n, 'a', -1), 'c');
  assert.equal(R.cycleTab(n, 'x', 1), 'a');
  assert.equal(R.cycleTab(n, 'x', -1), 'c');
});

test('a rail badge is one character: a dot, a digit, + over nine, the first of a text, nothing for the rest', () => {
  assert.equal(R.badgeText(true), '●');
  assert.equal(R.badgeText(1), '1');
  assert.equal(R.badgeText(9), '9');
  assert.equal(R.badgeText(10), '+');
  assert.equal(R.badgeText(3.7), '3');
  assert.equal(R.badgeText('new'), 'n');
  for (const v of [0, -2, 0.5, NaN, false, null, undefined, '', {}]) assert.equal(R.badgeText(v), '', String(v));
});

test('a bad registration is said once on the console and left out; the page goes on', () => {
  const said = mock.method(console, 'error', () => {});
  const before = R.rightTabs.peek().length;
  assert.equal(R.rightTabs.add(null), null);
  assert.equal(R.rightTabs.add({ name: 'Bad Name', icon: 'x', component: C }), null);
  assert.equal(R.rightTabs.add({ name: 'ok', icon: '', component: C }), null);
  assert.equal(R.rightTabs.add({ name: 'ok', icon: 'x', component: 'nope' }), null);
  assert.equal(R.rightTabs.add({ name: 'ok', icon: 'x', component: C, badge: 3 }), null, 'a badge is a signal');
  assert.equal(R.paneKinds.add({ kind: 'k', Pane: C, close: 'x' }), null);
  assert.equal(R.commands.add({ name: '__proto__', help: 'x', run: C }), null);
  assert.equal(R.commands.add({ name: 'two words', help: 'x', run: C }), null);
  assert.equal(R.keyOwners.add('not a function'), null);
  assert.equal(R.rightTabs.peek().length, before);
  assert.equal(said.mock.calls.length, 9);
});

test('a list replaces by key in place, removes, and reads the same through get, list and peek', () => {
  const l = new R.Registry('thing', (x) => x.k, () => '');
  l.add({ k: 'a', v: 1 });
  l.add({ k: 'b', v: 2 });
  l.add({ k: 'a', v: 3 });
  assert.deepEqual(l.peek().map((x) => x.v), [3, 2], 'a replaced in place, not moved');
  assert.equal(l.get('b').v, 2);
  assert.equal(l.list, l.peek());
  assert.equal(l.get('zz'), null);
  assert.equal(l.remove('a'), true);
  assert.equal(l.remove('a'), false);
  assert.deepEqual(l.peek().map((x) => x.k), ['b']);
});

// ---------------------------------------------------------------- the stream budget
test('the chats get 4 streams, one fewer for each other stream held, and never fewer than one', () => {
  assert.deepEqual([0, 1, 2, 3, 4, 9].map((n) => R.chatCap(n)), [4, 3, 2, 1, 1, 1]);
  assert.deepEqual([-2, NaN, 0.9, 1.9, undefined].map((n) => R.chatCap(n === undefined ? 0 : n)), [4, 4, 4, 3, 4]);
  assert.equal(R.MAX_SIDS, 4);
});

test('capped: a chat that holds no stream is over the cap when the others fill it, extraStreams included', () => {
  const three = new Set(['a', 'b', 'c']), four = new Set(['a', 'b', 'c', 'd']);
  assert.equal(R.overCap(three, 'd', 0), false, 'three of four');
  assert.equal(R.overCap(three, 'd', 1), true, 'three of the three the terminal stream leaves');
  assert.equal(R.overCap(three, 'a', 1), false, 'a chat that holds one is never refused its own');
  assert.equal(R.overCap(four, 'e', 0), true);
  assert.equal(R.overCap(four, 'a', 0), false);
  assert.equal(R.overCap(new Set(), 'a', 3), false, 'one is always left for a chat');
  assert.equal(R.overCap(new Set(['a']), 'b', 3), true);
});

test('overCap and chatCap read extraStreams when none is given', () => {
  try {
    R.extraStreams.value = 1;
    assert.equal(R.chatCap(), 3);
    assert.equal(R.overCap(new Set(['a', 'b', 'c']), 'd'), true);
    R.extraStreams.value = 0;
    assert.equal(R.chatCap(), 4);
    assert.equal(R.overCap(new Set(['a', 'b', 'c']), 'd'), false);
  } finally { R.extraStreams.value = 0; }
});

test('streamRoom says how many more streams a feature may open', () => {
  assert.equal(R.streamRoom(3, 0), 1);
  assert.equal(R.streamRoom(3, 1), 0, 'the terminal stream is already one of the four');
  assert.equal(R.streamRoom(4, 0), 0);
  assert.equal(R.streamRoom(0, 1), 3);
  assert.equal(R.streamRoom(1), 3, 'extraStreams when none is given');
});

// ---------------------------------------------------------------- owners
const el = { name: 'an element' };
const clear = (list, fns) => { for (const f of fns) list.remove(f); };

test('keyOwners: the first claim wins, later owners are not asked, one that throws counts as no', () => {
  const said = mock.method(console, 'error', () => {});
  const calls = [];
  const no = () => { calls.push('no'); return false; }, boom = () => { calls.push('boom'); throw new Error('x'); };
  const yes = () => { calls.push('yes'); return true; }, late = () => { calls.push('late'); return true; };
  assert.equal(R.keyClaimed(el), false, 'no owner: nobody claims it');
  for (const f of [no, boom, yes, late]) R.keyOwners.add(f);
  try {
    assert.equal(R.keyClaimed(el), true);
    assert.deepEqual(calls, ['no', 'boom', 'yes'], 'asked in registration order, up to the first claim');
    assert.equal(R.keyClaimed(el), true);
    assert.equal(said.mock.calls.length, 1, 'a throwing owner is said once, not at every key');
    assert.equal(R.keyClaimed(null), false, 'no element, no claim');
  } finally { clear(R.keyOwners, [no, boom, yes, late]); }
  assert.equal(R.keyClaimed(el), false);
});

test('focusOwners are their own list: a key owner does not claim a focus, and the reverse', () => {
  const yes = () => true;
  R.keyOwners.add(yes);
  try {
    assert.equal(R.keyClaimed(el), true);
    assert.equal(R.focusClaimed(el), false);
  } finally { R.keyOwners.remove(yes); }
  R.focusOwners.add(yes);
  try {
    assert.equal(R.focusClaimed(el), true);
    assert.equal(R.keyClaimed(el), false);
    assert.equal(R.whereOf(el), null);
  } finally { R.focusOwners.remove(yes); }
});

test('whereOwners: the first name wins; null, an empty text, a non-text and a throw all pass to the next; none: null', () => {
  mock.method(console, 'error', () => {});
  const seen = [];
  const fs = [() => null, () => '', () => 5, () => { throw new Error('x'); }, (e) => { seen.push(e); return 'term'; }, () => 'notepad'];
  assert.equal(R.whereOf(el), null);
  for (const f of fs) R.whereOwners.add(f);
  try {
    assert.equal(R.whereOf(el), 'term');
    assert.deepEqual(seen, [el], 'the element reaches the owner');
    assert.equal(R.whereOf(null), null);
  } finally { clear(R.whereOwners, fs); }
  assert.equal(R.whereOf(el), null);
});

// ---------------------------------------------------------------- commands and key rows
const fnQ = () => 'q', fnTerm = () => 'native', fnWeb = () => 'web';
const BASE = { q: ['close the pane', fnQ], term: ['open a terminal at the focused session\'s folder', fnTerm], edit: ['edit', fnQ], font: ['font', fnQ] };

test('a registered command replaces the base entry where it stands; new names come last; the base is untouched', () => {
  const { table, overridden } = R.mergeCommands(BASE, [{ name: 'term', help: 'a web terminal', run: fnWeb }, { name: 'window', help: 'native', run: fnTerm }]);
  assert.deepEqual(Object.keys(table), ['q', 'term', 'edit', 'font', 'window']);
  assert.deepEqual(table.term, ['a web terminal', fnWeb]);
  assert.equal(table.q, BASE.q);
  assert.deepEqual(overridden, ['term'], 'the boot log names what was hidden');
  assert.deepEqual(BASE.term, ['open a terminal at the focused session\'s folder', fnTerm]);
  const none = R.mergeCommands(BASE, []);
  assert.deepEqual(Object.keys(none.table), Object.keys(BASE));
  assert.deepEqual(none.overridden, []);
});

test('the command table has no prototype: `:constructor` and `:toString` are no commands', () => {
  const { table } = R.mergeCommands(BASE, []);
  for (const w of ['constructor', 'toString', 'hasOwnProperty', '__proto__']) assert.equal(table[w], undefined, w);
});

test('commands registered twice keep the latest; the override and the key row it goes with come out together', () => {
  const had = { commands: R.commands.peek().length, rows: R.keyRows.peek().length }; // the notepad has registered `:note` and its key rows by now
  R.commands.add({ name: 'term', help: 'first', run: fnQ });
  R.commands.add({ name: 'term', help: 'a web terminal', run: fnWeb });
  R.commands.add({ name: 'window', help: 'native', run: fnTerm });
  R.keyRows.add({ section: 'panes', replaces: ':term :edit :files', keys: ':term :window', what: ':term (web terminal) · :window term|edit|files (native)', run: fnWeb });
  try {
    const { table, overridden } = R.mergeCommands(BASE, R.commands.list);
    assert.deepEqual(overridden, ['term']);
    assert.deepEqual(table.term, ['a web terminal', fnWeb]);
    assert.deepEqual(Object.keys(table).slice(-1), ['window']);
    const KEYREF = [['anywhere', [['i', 'write', fnQ]]], ['panes', [['⌥w  :q', 'close', fnQ], [':term :edit :files', 'open a terminal / editor / file manager', fnTerm], ['/', 'filter', fnQ]]]];
    const rows = R.mergeKeyRows(KEYREF, R.keyRows.list);
    assert.deepEqual(rows[1][1].map((r) => r[0]), ['⌥w  :q', ':term :window', '/'], 'the old row is replaced in place, not duplicated');
    assert.equal(rows[1][1][1][1], ':term (web terminal) · :window term|edit|files (native)');
    assert.equal(rows[1][1][1][2], fnWeb);
  } finally { R.commands.remove('term'); R.commands.remove('window'); R.keyRows.remove('panes\n:term :window'); }
  assert.equal(R.commands.peek().length, had.commands);
  assert.equal(R.keyRows.peek().length, had.rows);
});

test('key rows: found in the named section, else anywhere; a row that replaces nothing is added to its section; a new section comes last', () => {
  const KEYREF = [['anywhere', [['a', 'one', fnQ], ['b', 'two', fnQ]]], ['panes', [['c', 'three', fnQ]]]];
  const row = (o) => ({ keys: 'k', what: 'w', run: fnWeb, ...o });
  let g = R.mergeKeyRows(KEYREF, [row({ replaces: 'b' })]);
  assert.deepEqual(g[0][1].map((r) => r[0]), ['a', 'k'], 'no section: found in any');
  g = R.mergeKeyRows(KEYREF, [row({ section: 'panes', replaces: 'b' })]);
  assert.deepEqual(g[0][1].map((r) => r[0]), ['a', 'b'], 'the named section does not hold it: nothing replaced');
  assert.deepEqual(g[1][1].map((r) => r[0]), ['c', 'k'], 'so the row is added to that section');
  g = R.mergeKeyRows(KEYREF, [row()]);
  assert.deepEqual(g[1][1].map((r) => r[0]), ['c', 'k'], 'the default section is panes');
  g = R.mergeKeyRows(KEYREF, [row({ section: 'notes' })]);
  assert.deepEqual(g.map((x) => x[0]), ['anywhere', 'panes', 'notes']);
  g = R.mergeKeyRows(KEYREF, [row({ keys: 'k1' }), row({ keys: 'k2', replaces: 'k1' })]);
  assert.deepEqual(g[1][1].map((r) => r[0]), ['c', 'k2'], 'a later row can replace an earlier one');
  assert.deepEqual(KEYREF[0][1].map((r) => r[0]), ['a', 'b'], 'the base rows are not changed');
  assert.equal(KEYREF.length, 2);
});

// ---------------------------------------------------------------- frame taps
test('runTaps: every tap once, in order, with the session, the frame and whether it is live; a throwing tap stops no other', () => {
  const said = mock.method(console, 'error', () => {});
  const seen = [], f = { type: 'hello' };
  const a = (sid, fr, live) => seen.push(['a', sid, fr, live]), boom = () => { throw new Error('tap'); }, b = (sid, fr, live) => seen.push(['b', sid, fr, live]);
  for (const t of [a, boom, b]) R.frameTaps.add(t);
  try {
    R.runTaps('s1', f, true);
    R.runTaps('s1', f);
    assert.deepEqual(seen, [['a', 's1', f, true], ['b', 's1', f, true], ['a', 's1', f, false], ['b', 's1', f, false]]);
    assert.equal(said.mock.calls.length, 1, 'said once');
  } finally { clear(R.frameTaps, [a, boom, b]); }
  R.runTaps('s1', f);
  assert.equal(seen.length, 4, 'a removed tap hears nothing');
});

// core/state.js reads matchMedia at load and a Session opens its stream at once: both stubbed, the stream never answers.
async function sessionClass() {
  globalThis.matchMedia = () => ({ matches: false, addEventListener() {}, addListener() {} });
  globalThis.fetch = () => new Promise(() => {});
  return (await import('../core/state.js')).Session;
}
const HELLO = { type: 'hello', protocol: 1, session: 'x', cwd: '/home/u', model: 'm', pending: 0 };

test('Session.frame gives every frame to the taps first, replayed ones as not live, and a throwing tap changes nothing in the dispatch', async () => {
  mock.method(console, 'error', () => {});
  const Session = await sessionClass();
  const seen = [], boom = () => { throw new Error('tap'); }, tap = (sid, f, live) => seen.push([sid, f.type, live]);
  R.frameTaps.add(boom);
  R.frameTaps.add(tap);
  const s = new Session('s1', { toast() {} });
  try {
    s.frame(HELLO);
    assert.equal(s.hello.value.model, 'm', 'the dispatch went on after the throwing tap');
    s.frame({ type: 'context-size', tokens: 5 });
    assert.equal(s.ctx.value, 5);
    s.frame({ type: 'caught-up' });
    assert.equal(s.conn.value, 'live');
    s.frame({ type: 'turn-state', running: true });
    assert.equal(s.running.value, true);
    assert.deepEqual(seen, [['s1', 'hello', false], ['s1', 'context-size', false], ['s1', 'caught-up', false], ['s1', 'turn-state', true]]);
    s.frame({ type: 'context-size', tokens: 5 });
    assert.equal(seen.length, 4, 'a live copy of a replayed frame is dropped before the taps: one tap per frame');
  } finally { clear(R.frameTaps, [boom, tap]); s.close(); }
});

// ---------------------------------------------------------------- panes that are not chats
const BOX = { w: 1000, h: 600, cw: 10, ch: 20 };
const chat = { id: 'p1', sid: 'a', s: {} };
const consistent = (st, chats) => {
  const leaves = tile.leaves(st.tree), want = [...chats, ...st.xpanes.map((x) => x.id)];
  assert.deepEqual([...leaves].sort(), [...want].sort(), 'the leaves of the tree are the chat panes and the xpanes');
  assert.equal(new Set(leaves).size, leaves.length, 'no leaf twice');
  if (st.focus != null) assert.ok(leaves.includes(st.focus), 'the focus is a pane that is there');
  else assert.equal(leaves.length, 0, 'no focus only with no pane');
  const lay = tile.layout(st.tree, BOX);
  assert.equal(lay.panes.length, leaves.length);
  assert.equal(lay.dividers.length, Math.max(0, leaves.length - 1));
};

test('an xpane joins the tile tree beside a chat and leaves it again; the tree, the focus and the list stay consistent', () => {
  const st = { xpanes: [], tree: tile.add(null, null, 'p1', BOX), focus: 'p1' };
  const a = R.addXPane(st, { id: 'p2', kind: 'test' }, { box: BOX, dir: 'row' });
  consistent(a, ['p1']);
  assert.equal(a.focus, 'p2', 'the new pane takes focus, as a new chat pane does');
  assert.deepEqual(st.xpanes, [], 'the state given is not changed');
  assert.deepEqual(tile.leaves(st.tree), ['p1']);
  const b = R.addXPane(a, { id: 'p3', kind: 'test', n: 7 }, { at: 'p2', box: BOX, dir: 'col' });
  consistent(b, ['p1']);
  assert.equal(b.xpanes[1].n, 7);
  assert.deepEqual(tile.leaves(b.tree), ['p1', 'p2', 'p3']);
  assert.equal(b.tree.b.dir, 'col', 'p2 was split below, so p3 sits under it, inside the right half');
  const c = R.dropXPane(b, 'p3');
  consistent(c, ['p1']);
  assert.equal(c.focus, 'p2', 'the focused pane closed: focus goes to the last one left');
  const d = R.dropXPane(b, 'p2');
  consistent(d, ['p1']);
  assert.equal(d.focus, 'p3', 'another pane closed: focus stays');
  assert.deepEqual(d.xpanes.map((x) => x.id), ['p3']);
  const e = R.dropXPane(R.dropXPane(c, 'p2'), 'zz');
  consistent(e, ['p1']);
  assert.equal(e.focus, 'p1');
});

test('an xpane with no chat open is the whole tree, and closing it empties the tree and the focus', () => {
  const a = R.addXPane({ xpanes: [], tree: null, focus: null }, { id: 'p1', kind: 'test' }, { box: BOX });
  consistent(a, []);
  assert.equal(a.focus, 'p1');
  const b = R.dropXPane(a, 'p1');
  consistent(b, []);
  assert.equal(b.tree, null);
  assert.equal(b.focus, null);
});

test('with no `at` the new xpane splits the focused pane', () => {
  const st = { xpanes: [], tree: tile.add(tile.add(null, null, 'p1', BOX), 'p1', 'p2', BOX), focus: 'p2' };
  const a = R.addXPane(st, { id: 'p3', kind: 'test' }, { box: BOX });
  assert.deepEqual(tile.leaves(a.tree), ['p1', 'p2', 'p3']);
  assert.equal(a.tree.b.b.leaf, 'p3', 'under p2, the focused one');
});

test('a split of a pane works on the chat it may mirror and the id it anchors on: an xpane has nothing to mirror', () => {
  const x = { id: 'p2', kind: 'term', s: 'a field of its own' };
  assert.equal(R.isXPane(chat), false);
  assert.equal(R.isXPane(x), true);
  assert.equal(R.isXPane(null), false);
  assert.deepEqual(R.anchorOf(chat), { chat, at: 'p1' });
  assert.deepEqual(R.anchorOf(x), { chat: null, at: 'p2' });
  assert.deepEqual(R.anchorOf(null), { chat: null, at: undefined });
});

test('a pane is placed from its tile rectangle exactly as the chat pane was: one border line shared with its neighbour', () => {
  assert.equal(R.paneStyle({ x: 0, y: 0, w: 100, h: 50 }), 'left:0px;top:0px;width:100px;height:50px');
  assert.equal(R.paneStyle({ x: 100, y: 20, w: 50, h: 30 }), 'left:99px;top:19px;width:51px;height:31px');
  assert.equal(R.paneStyle({ x: 100, y: 0, w: 50, h: 30 }), 'left:99px;top:0px;width:51px;height:30px');
  assert.equal(R.paneStyle({ x: 0, y: 20, w: 50, h: 30 }), 'left:0px;top:19px;width:50px;height:31px');
});

test('a pane kind answers for its panes: title, status, cwd and close, each optional, one that throws is undefined', () => {
  mock.method(console, 'error', () => {});
  const closed = [];
  R.paneKinds.add({ kind: 'zz-kind', Pane: C, title: (p) => 'T ' + p.id, status: () => { throw new Error('x'); }, cwd: (p) => p.dir, close: (p) => closed.push(p.id) });
  R.paneKinds.add({ kind: 'zz-bare', Pane: C });
  try {
    const p = { id: 'p9', kind: 'zz-kind', dir: '/home/u' };
    assert.equal(R.paneInfo(p, 'title'), 'T p9');
    assert.equal(R.paneInfo(p, 'cwd'), '/home/u');
    assert.equal(R.paneInfo(p, 'status'), undefined);
    R.paneInfo(p, 'close');
    assert.deepEqual(closed, ['p9']);
    assert.equal(R.paneInfo({ id: 'p8', kind: 'zz-bare' }, 'title'), undefined);
    assert.equal(R.paneInfo({ id: 'p7', kind: 'nobody' }, 'title'), undefined);
    assert.equal(R.paneInfo(null, 'title'), undefined);
    assert.equal(R.paneKinds.get('zz-kind').Pane, C);
  } finally { R.paneKinds.remove('zz-kind'); R.paneKinds.remove('zz-bare'); }
});

// ---------------------------------------------------------------- menu extras, sections
test('split and chat menu extras: each registered function once, items flattened in order, bad answers left out', () => {
  mock.method(console, 'error', () => {});
  const calls = [];
  const a = (p, dir) => { calls.push(['a', p, dir]); return [{ label: 'one' }, null, { label: 'two' }]; };
  const none = () => null, text = () => 'not a list', boom = () => { throw new Error('x'); }, z = () => [{ label: 'three' }];
  const fs = [a, none, text, boom, z];
  for (const f of fs) R.splitExtras.add(f);
  R.chatMenuItems.add((row, ctx) => [{ label: row.id + ctx.at }]);
  try {
    const p = { id: 'p1' };
    assert.deepEqual(R.splitExtraItems(p, 'row').map((i) => i.label), ['one', 'two', 'three']);
    assert.deepEqual(calls, [['a', p, 'row']]);
    assert.deepEqual(R.chatMenuExtras({ id: 'c1' }, { p: null, at: '@' }).map((i) => i.label), ['c1@']);
  } finally { clear(R.splitExtras, fs); R.chatMenuItems.items.value = []; }
  assert.deepEqual(R.splitExtraItems(null, 'row'), []);
});

test('loaded sections and composer extras keep their order and take a component once', () => {
  const A = () => null, B = () => null;
  for (const list of [R.loadedSections, R.composerExtras]) {
    const was = list.peek().length;
    list.add(A); list.add(B); list.add(A);
    try { assert.deepEqual(list.peek().slice(was), [A, B]); } finally { list.remove(A); list.remove(B); }
    assert.equal(list.peek().length, was);
  }
});

// ---------------------------------------------------------------- the wiring in the files that cannot load here
const appJs = src('../app.js');
const has = (text, re, why) => assert.ok(re.test(text), why || `no match for ${re}`); // assert.match would print the whole file
const within = (text, from, to) => { const i = text.indexOf(from); assert.ok(i >= 0, `missing: ${from}`); const j = text.indexOf(to, i); assert.ok(j > i, `missing after ${from}: ${to}`); return text.slice(i, j); };

test('wiring: core/state.js gives each frame to the taps before it reads it', () => {
  const s = src('../core/state.js');
  has(s, /import \{ runTaps \} from '\.\/registry\.js'/);
  const tap = s.indexOf('runTaps(this.sid, f, this.live)'), sw = s.indexOf('switch (f.type)');
  assert.ok(tap > 0 && sw > tap, 'the taps come before the switch');
  assert.ok(s.indexOf('dupInWindow(f)) return') < tap, 'and after a duplicate is dropped');
});

test('wiring: the key owners are asked before the page\'s own keys, ctrl+b, ctrl+i and alt+w among them', () => {
  const keys = within(appJs, "document.addEventListener('keydown'", "document.addEventListener('mousedown'");
  const claim = keys.indexOf('if (keyClaimed(e.target)) return;');
  assert.ok(claim >= 0, 'the early return is there');
  for (const later of ["k === 'b'", "k === 'i'", 'isW', "e.key === 'Enter'", 'typedAt', 'typing']) assert.ok(claim < keys.indexOf(later), `before ${later}`);
  assert.ok(keys.slice(0, claim).split('\n').length <= 4, 'nothing but its comment above it');
});

test('wiring: a focus a feature owns is never taken by an ask, and is where focus goes back to', () => {
  has(appJs, /const busy = [^\n]*focusClaimed\(at\)/);
  has(appJs, /focusClaimed\(at\)\)\) returnTo = at/);
  const where = within(appJs, 'function whereNow()', 'const where = ');
  assert.ok(where.indexOf('whereOf(a)') > 0 && where.indexOf('whereOf(a)') < where.indexOf("closest('.ask')"), 'the owners name the place first');
});

test('wiring: capped() counts with the cap the registry works out, not a fixed 4', () => {
  const capped = within(appJs, 'function capped(', '\n}\n');
  assert.ok(capped.includes('overCap(holdingSids(except), sid)') && capped.includes('chatCap()'));
  assert.ok(!capped.includes('MAX_SIDS)'), 'no `< MAX_SIDS` left in the arithmetic');
});

test('wiring: Tiles draws the xpanes, and a pane of either kind keeps the start screen away', () => {
  const tiles = within(appJs, 'function Tiles()', 'function Hint()');
  assert.ok(tiles.includes('ui.xpanes.value') && tiles.includes('paneKinds.get(x.kind)'));
  assert.equal((tiles.match(/!panes\.length && !xs\.length/g) || []).length, 2, 'both empty-screen branches count xpanes');
  assert.ok(appJs.includes('const focusedAny = computed('), 'and the picker, close and split reach either kind');
  has(within(appJs, 'function picker()', 'function longSide'), /focusedAny\.peek\(\)/);
  has(within(appJs, 'function closePane(', 'const focusPane'), /xById\(id\)/);
});

test('wiring: the inspector shows the inspected chat, and a global tab with none; the rails draw badges', () => {
  const right = within(appJs, 'function Right()', 'function Divider');
  assert.ok(right.includes('inspected.value') && right.includes('t.global'));
  assert.ok(within(appJs, 'function Rail(', 'function Owl').includes('badgeText('));
  assert.ok(appJs.includes('const inspected = computed(() => focused.value || byId(ui.lastChat.value) || null)'));
});

test('wiring: app.js imports the stubs, and still has the key row and the base commands the terminal slices replace', () => {
  for (const m of ['terms/index.js', 'notes/index.js', 'allow/list.js']) assert.ok(appJs.includes(`import './${m}'`), m);
  assert.ok(within(appJs, "['panes', [", "['/'").includes("':term :edit :files'"), 'the row T4b replaces, in the panes section');
  const base = within(appJs, 'const BASE_COMMANDS = {', '\n};');
  for (const c of ['term', 'edit', 'files']) has(base, new RegExp(`\\n  ${c}: \\[`));
  assert.ok(appJs.includes('mergeCommands(BASE_COMMANDS, commands.peek()).overridden'), 'the boot log names an override');
});

test('wiring: host has every name the features are told about', () => {
  const host = within(appJs, 'Object.assign(host, {', '\n});');
  for (const n of ['toast', 'openXPane', 'closePane', 'focusPane', 'focused', 'inspected', 'chatPanes', 'xpanes', 'cwd', 'showTab', 'chatStreams', 'openCmd']) {
    has(host, new RegExp(`\\b${n}\\b`), n);
  }
});

test('wiring: a chat pane draws the composer extras above its composer, shares paneStyle, and exports Title and Glyphs', () => {
  const s = src('../panes/session.js');
  const pane = within(s, 'export function Pane(', 'export function Title(');
  assert.ok(pane.indexOf('<${Outbox}') < pane.indexOf('composerExtras.list') && pane.indexOf('composerExtras.list') < pane.indexOf('<${Composer}'));
  assert.ok(pane.includes('style=${paneStyle(rect)}'));
  has(s, /export function Glyphs\(/);
});

test('wiring: index.html links the xterm stylesheet', () => {
  has(src('../index.html'), /<link rel="stylesheet" href="vendor\/xterm\.css">/);
});
