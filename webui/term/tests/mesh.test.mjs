// The mesh tab (mesh/view.js, mesh/index.js): what /hub/mesh and /hub/mesh/events said, as rows and sentences. Pure: view.js has no DOM, and the
// two polls in index.js run against a stub fetch. The input is tests/fixtures/aoide/: snapshot.json and events.json are what a real perch
// (R2b) answered in front of fake_aoide, mesh.json and outbox.json are R2a's fixtures as aoide prints them, snapshot-none.json is a perch with no Aoide.
import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { hubPathOk } from '../core/api.js';
import * as V from '../mesh/view.js';
import * as M from '../mesh/index.js';

// As LF: a checkout with core.autocrlf has CRLF sources.
const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
const fixture = (name) => JSON.parse(read('./fixtures/aoide/' + name));
// A source file's code: whole-line comments left out, so a file may say in a comment what it does not do.
const code = (rel) => read(rel).split('\n').filter((l) => !/^\s*\/\//.test(l)).join('\n');
const settle = () => new Promise((r) => setImmediate(r));

const snap = fixture('snapshot.json'), none = fixture('snapshot-none.json'), events = fixture('events.json');
const NOW = Date.parse('2026-10-07T09:50:00Z');
const hhmmss = (ts) => { const d = new Date(ts * 1000), p = (n) => String(n).padStart(2, '0'); return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`; };

// ---------------------------------------------------------------- the header and the not-available sentence
test('the header is the program, its path and the daemon, as one line', () => {
  assert.equal(V.headerText(V.headerParts(snap)), 'aoide 0.0.26 · ~/.local/bin/aoide · aoided ● 3 ms');
  assert.equal(V.headerParts(snap)[2].title, '/run/user/1000/aoide/aoided.sock', 'the socket is the hover');
});

test('the daemon part says an answer, no answer, or no socket; the program part says a missing version or a missing program', () => {
  const at = (daemon, aoide = snap.aoide) => V.headerText(V.headerParts({ ...snap, aoide, daemon }));
  assert.equal(at({ socket: '/s.sock', ping_ms: null }), 'aoide 0.0.26 · ~/.local/bin/aoide · aoided ○ no answer');
  assert.equal(at({ socket: null, ping_ms: null }), 'aoide 0.0.26 · ~/.local/bin/aoide · aoided: no socket known');
  assert.equal(at({ socket: '/s.sock', ping_ms: 0 }), 'aoide 0.0.26 · ~/.local/bin/aoide · aoided ● 0 ms', 'an instant answer is still an answer');
  assert.equal(at(snap.daemon, { path: '~/bin/aoide', version: null }), 'aoide (version not reported) · ~/bin/aoide · aoided ● 3 ms');
  assert.equal(at(snap.daemon, null), 'no aoide program is configured · aoided ● 3 ms');
  assert.equal(V.headerText(V.headerParts({})), 'no aoide program is configured · aoided: no socket known', 'a bare object does not throw');
});

test('a perch with no Aoide is the not-available sentence, in its own words, and every part is empty', () => {
  const s = V.stateOf({ status: 200, data: none });
  assert.equal(s.state, 'unavailable');
  assert.match(s.sentence, /^Aoide is not available here: this perch has no aoide program/);
  assert.equal(s.sentence, none.errors[0]);
  assert.deepEqual(V.nodeRows(none), []);
  assert.match(V.driftView(none.mesh).sentence, /^no mesh report/);
  assert.equal(V.graphView(none.graph).sentence, 'no stage graph here');
  assert.match(V.outboxView(none.outbox).sentence, /^no outbox report/);
  assert.equal(V.stateOf({ status: 200, data: { ...none, errors: [] } }).sentence, V.NO_AOIDE, 'with no sentence from the perch the page has its own');
});

test('an answer that is not a 200 is a failure with the reason; no answer yet is loading; 200 with an Aoide is ok', () => {
  assert.deepEqual(V.stateOf(null), { state: 'loading', sentence: '' });
  assert.deepEqual(V.stateOf({ status: 0, data: { error: 'network error' } }), { state: 'failed', sentence: 'the mesh view could not be read: the perch did not answer' });
  assert.match(V.stateOf({ status: 401, data: null }).sentence, /refused this page's login/);
  assert.match(V.stateOf({ status: 404, data: null }).sentence, /no such route/);
  assert.equal(V.stateOf({ status: 502, data: { error: 'aoide is on fire' } }).sentence, 'the mesh view could not be read: aoide is on fire');
  assert.equal(V.stateOf({ status: 200, data: snap }).state, 'ok');
  assert.equal(V.stateOf({ status: 200, data: 'text' }).state, 'failed', 'a body that is not an object is no snapshot');
});

// ---------------------------------------------------------------- the nodes: both registry shapes, and the ages
test('both registry shapes become rows: the flat words (before P-CHARTER), the words per mesh (after it), and a node with neither', () => {
  const rows = V.nodeRows(snap);
  assert.deepEqual(rows.map((r) => r.name), ['osaka', 'yomi', 'bridge'], 'the registry\'s order');
  const [osaka, yomi, bridge] = rows;
  assert.deepEqual([osaka.verified, osaka.grants, osaka.added], [true, 'message, read', '2026-09-01']);
  assert.deepEqual([yomi.verified, yomi.grants, yomi.added], [true, 'alpha: message · beta: message, spawn', '2026-10-01']);
  assert.deepEqual([bridge.verified, bridge.grants, bridge.added, bridge.cached, bridge.meshes], [false, 'none listed', '2026-10-02', 'no roster cached', '—']);
  // a node that has both forms in one entry shows both
  const both = V.nodeRows({ registry: [{ name: 'a', verified: true, allows: ['read'], grants: { m: ['spawn'] }, addedAt: '' }], cache: [] });
  assert.equal(both[0].grants, 'read · m: spawn');
  assert.equal(both[0].added, '—', 'no date is a dash');
});

test('the meshes of a node are where it has a grant or a row in mesh --json, and the same fixture files give them', () => {
  const [osaka, yomi, bridge] = V.nodeRows(snap);
  assert.equal(osaka.meshes, 'alpha beta', 'two drift rows, no grants');
  assert.equal(yomi.meshes, 'alpha beta', 'two grants, one drift row: each mesh once');
  assert.equal(bridge.meshes, '—', 'undeclared in mesh --json is not a mesh of its own');
  // the snapshot's mesh part is the data of R2a's mesh.json, and its outbox the data of R2a's outbox.json: the page reads what the runner returns
  assert.deepEqual(snap.mesh, fixture('mesh.json').data);
  assert.deepEqual(snap.outbox, fixture('outbox.json').data);
  assert.match(read('./fixtures/aoide/version.txt'), /^aoide 0\.0\.26\n?$/);
});

test('a roster is shown as when it was cached, moves on with the time since the answer, and an old one says old; presence is never claimed', () => {
  const [osaka, yomi] = V.nodeRows(snap);
  assert.deepEqual([osaka.cached, osaka.old, osaka.cachedSecs], ['cached 3m ago', false, 180]);
  assert.deepEqual([yomi.cached, yomi.old], ['cached 2d ago · old', true]);
  assert.equal(V.nodeRows(snap, 120)[0].cached, 'cached 5m ago', 'two minutes after the answer, the age is two minutes more');
  assert.equal(V.nodeRows(snap, -50)[0].cached, 'cached 3m ago', 'a clock that went backwards moves nothing');
  assert.equal(V.nodeRows({ registry: [{ name: 'a' }], cache: [{ name: 'a', age_secs: 90000, text: '', cut: true }] })[0].cached, 'cached 1d ago · old · cut at 64 KiB');
  assert.equal(V.NOT_PROBED, 'presence is not probed: ages are when each roster was last cached');
  // the words a person reads (a colour class like "up" is not one)
  const said = [...V.nodeRows(snap).flatMap((r) => [r.grants, r.meshes, r.added, r.cached]), V.NOT_PROBED, V.MESHES_NOTE, ...V.headerParts(snap).map((p) => p.text)].join('\n');
  assert.doesNotMatch(said, /\b(online|offline|present|alive|reachable|connected|up|down|seen)\b/i, 'no word that claims a node is there');
});

test('a roster cached for a node the registry no longer lists is its own row, said so', () => {
  const rows = V.nodeRows({ registry: [{ name: 'a', verified: true }], cache: [{ name: 'a', age_secs: 5, text: '' }, { name: 'ghost', age_secs: 61, text: '' }] });
  assert.deepEqual(rows.map((r) => [r.name, r.listed, r.cached]), [['a', true, 'cached 5s ago'], ['ghost', false, 'cached 1m ago']]);
  assert.equal(rows[1].grants, '—');
});

test('ageText counts seconds, minutes, hours and days, and says ? for what is not a time', () => {
  assert.deepEqual([0, 59, 60, 3599, 3600, 86399, 86400, 400000].map(V.ageText), ['0s', '59s', '1m', '59m', '1h', '23h', '1d', '4d']);
  assert.deepEqual([null, undefined, 'x', -1, NaN].map(V.ageText), ['?', '?', '?', '?', '?']);
});

// ---------------------------------------------------------------- mesh --json, the graph, the outbox
test('the drift is a section per mesh with the rows that differ, the unverified ones coloured, and the undeclared nodes', () => {
  const d = V.driftView(snap.mesh);
  assert.equal(d.sentence, '');
  assert.deepEqual(d.sections.map((s) => [s.name, s.head]), [['alpha', 'grant message · 3 declared · 2 differ'], ['beta', 'grant none listed · 2 declared · 1 differs']]);
  assert.deepEqual(d.sections[0].rows, [
    { node: 'osaka', what: 'unverified', colour: 'o', detail: '' },
    { node: 'yomi', what: 'via-mismatch', colour: '', detail: 'declared ssh://yomi · recorded none' },
  ]);
  assert.deepEqual(d.undeclared, ['multi', 'bridge']);
});

test('a mesh report that is missing, empty or in another shape says so, and never throws', () => {
  assert.match(V.driftView(null).sentence, /^no mesh report/);
  assert.equal(V.driftView({ report: { sections: [], undeclared: [] } }).sentence, 'mesh --json lists no mesh');
  assert.match(V.driftView({ report: 'x' }).sentence, /shape this page does not read/);
  assert.match(V.driftView({ schema: 2, meshes: [] }).sentence, /shape this page does not read/, 'a later aoide\'s shape is said, not guessed at');
  assert.deepEqual(V.driftView({ report: { sections: [null, 3, { name: 'm', rows: [null, { node: 'n' }] }] } }).sections.map((s) => [s.name, s.rows.map((r) => [r.node, r.what])]), [['m', [['n', '?']]]]);
});

test('the stage graph is counts and a list: the kinds by count, a pet name before a title before a name, children as +N', () => {
  const g = V.graphView(snap.graph);
  assert.equal(g.head, '4 nodes · 2 edges');
  assert.equal(g.kinds, 'session 2 · node 1 · project 1');
  assert.deepEqual(g.rows.map((r) => [r.kind, r.label, r.agent, r.state, r.kids]), [
    ['project', 'p', '', '', 0], ['session', 'brisk-fox', 'claude', 'running', 0], ['session', '<b>bold</b> title', 'codex', 'idle', 0], ['node', 'osaka', '', 'fresh', 1],
  ]);
  assert.equal(g.more, 0);
  assert.equal(V.graphView({ nodes: [], total: 0, kinds: {}, edges: 1 }).head, '0 nodes · 1 edge');
});

test('a graph the perch cut says how many are not listed, and the page lists at most GRAPH_SHOWN', () => {
  const nodes = Array.from({ length: 500 }, (_, i) => ({ id: 'n' + i, kind: 'session' }));
  const g = V.graphView({ nodes, total: 612, kinds: { session: 612 }, edges: 0 });
  assert.equal(g.rows.length, V.GRAPH_SHOWN);
  assert.equal(g.more, 612 - V.GRAPH_SHOWN);
  assert.equal(g.head, '612 nodes · 0 edges');
});

test('the outbox says what waits, what was refused, and how long ago and how soon, from the times it is given', () => {
  const o = V.outboxView(snap.outbox, NOW);
  assert.equal(o.head, '2 waiting · 1 refused');
  assert.deepEqual(o.rows[0], { to: 'osaka/minerva', id: '66666666', status: 'retrying', refused: false, tries: 2, detail: 'connection refused (remote) · last try 38m ago · next try was due 8m ago' });
  assert.deepEqual(o.rows[1], { to: 'bridge/minerva', id: '77777777', status: 'refused', refused: true, tries: 5, detail: 'message is not granted (remote) · last try 1h ago' });
  assert.match(V.outboxView(snap.outbox, Date.parse('2026-10-07T09:00:00Z')).rows[0].detail, /next try in 42m$/, 'a time still to come is "in"');
  assert.deepEqual(V.outboxView({ entries: [] }), { sentence: 'nothing is waiting in the outbox', head: '', rows: [] });
  assert.match(V.outboxView({ summary: {} }).sentence, /shape this page does not read/);
});

test('the perch\'s own sentences about what it left out come through, at most 8, cut short', () => {
  assert.deepEqual(V.noteLines({ errors: ['a registry field was left out', '', 7, null] }), ['a registry field was left out']);
  assert.equal(V.noteLines({ errors: Array.from({ length: 20 }, (_, i) => 'e' + i) }).length, 8);
  assert.ok(V.noteLines({ errors: ['x'.repeat(1000)] })[0].length <= 240);
  assert.deepEqual(V.noteLines(snap), []);
});

// ---------------------------------------------------------------- the events tail
test('an event line reads both shapes in the wild: the feed aoided keeps and the audit log', () => {
  const lines = events.events.map(V.eventLine);
  assert.deepEqual(lines.map((l) => [l.cls, l.text]), [
    ['audit', 'hand-edit (aoided) file=sessions.json'],
    ['gate', 'pair-parked (a2a-door) direction=inbound id=abc12345 name=box-a originAddr=10.0.0.5'],
    ['audit', 'mail.send (cli) ok spooled to osaka/minerva'],
    ['gate', 'pending node=osaka note=<script>alert(1)</script> [31m'],
  ]);
  assert.deepEqual(lines.map((l) => l.at), [1791366000, 1791366020, 1791366040, 1791366050].map(hhmmss), 'the time is the event\'s own, local');
  assert.equal(V.clock(undefined), '--:--:--');
  assert.equal(V.clock(1791366000000), hhmmss(1791366000), 'milliseconds are taken as such');
});

test('the tail takes each answer in: events join the held ones, the cursor moves, reset clears, a failed poll keeps both', () => {
  let f = V.foldFeed(V.emptyFeed(), { status: 200, data: events }, 1000);
  assert.deepEqual([f.lines.length, f.next, f.why, f.at], [4, 794, '', 1000]);
  const more = { events: [{ class: 'audit', kind: 'late', ts: 1791366100 }], next: 900, reset: false };
  f = V.foldFeed(f, { status: 200, data: more }, 2000);
  assert.deepEqual([f.lines.length, f.next, f.lines[4].text], [5, 900, 'late']);
  const kept = V.foldFeed(f, { status: 0, data: { error: 'network error' } }, 3000);
  assert.deepEqual([kept.lines, kept.next, kept.why], [f.lines, 900, 'the perch did not answer'], 'a hiccup keeps what was held');
  const cleared = V.foldFeed(kept, { status: 200, data: { events: [{ class: 'gate', kind: 'again' }], next: 40, reset: true } }, 4000);
  assert.deepEqual([cleared.lines.map((l) => l.text), cleared.next, cleared.why], [['again'], 40, ''], 'reset drops everything held before it');
  assert.match(V.foldFeed(f, { status: 200, data: { oops: 1 } }).why, /not in a shape/);
});

test('the tail holds at most HELD events, the newest, and draws SHOWN of them newest first with the rest counted', () => {
  const batch = (from, n) => ({ events: Array.from({ length: n }, (_, i) => ({ class: 'audit', kind: 'e' + (from + i) })), next: from + n, reset: false });
  let f = V.foldFeed(V.emptyFeed(), { status: 200, data: batch(0, 150) });
  f = V.foldFeed(f, { status: 200, data: batch(150, 100) });
  assert.equal(f.lines.length, V.HELD);
  assert.equal(f.lines[0].text, 'e50', 'the oldest 50 were dropped');
  const v = V.feedView(f);
  assert.equal(v.lines.length, V.SHOWN);
  assert.deepEqual([v.lines[0].text, v.older, v.sentence], ['e249', V.HELD - V.SHOWN, '']);
});

test('the poll path has no cursor the first time and then the last answer\'s next, and the hub accepts both', () => {
  assert.equal(V.feedPath(V.emptyFeed()), 'mesh/events');
  const f = V.foldFeed(V.emptyFeed(), { status: 200, data: events });
  assert.equal(V.feedPath(f), 'mesh/events?after=794');
  assert.equal(V.feedPath({ ...f, next: 0 }), 'mesh/events?after=0', 'a cursor of 0 is a cursor');
  assert.ok(hubPathOk(V.feedPath(V.emptyFeed())) && hubPathOk(V.feedPath(f)));
});

test('a tab reopened after a minute starts the tail again instead of catching up through a backlog', () => {
  const f = V.foldFeed(V.emptyFeed(), { status: 200, data: events }, 10000);
  assert.equal(V.resumeFeed(f, 10000 + V.FEED_STALE_MS), f, 'a minute is still fresh');
  assert.deepEqual(V.resumeFeed(f, 10000 + V.FEED_STALE_MS + 1), V.emptyFeed());
  assert.deepEqual(V.resumeFeed(V.emptyFeed(), 5), V.emptyFeed());
});

test('with nothing to show, the feed says why it may be empty and never that nothing happened', () => {
  const asked = V.foldFeed(V.emptyFeed(), { status: 200, data: { events: [], next: 0, reset: false } }, 5);
  const empty = V.feedView(asked);
  assert.equal(empty.sentence, V.EVENTS_EMPTY);
  assert.match(empty.sentence, /--aoide-events/);
  assert.match(empty.sentence, /empty, missing or not set/);
  assert.match(empty.sentence, /Nothing shown is not the same as nothing happening/);
  assert.equal(V.feedView(V.emptyFeed()).sentence, 'reading the events feed…', 'before the first answer it says it is reading');
  const bytes = V.feedView(V.foldFeed(V.emptyFeed(), { status: 200, data: { events: [], next: 120, reset: false } }, 5));
  assert.equal(bytes.sentence, 'the feed has 120 bytes and none of it is an audit or gate event');
  const failed = V.feedView(V.foldFeed(V.emptyFeed(), { status: 502, data: { error: 'the events file could not be read as a plain file' } }, 5));
  assert.equal(failed.sentence, 'the events feed could not be read: the events file could not be read as a plain file');
  assert.equal(failed.cls, 'o');
  for (const text of [V.EVENTS_EMPTY, bytes.sentence, failed.sentence]) assert.doesNotMatch(text, /no activity|nothing is happening|quiet/i);
});

// ---------------------------------------------------------------- hostile text
// Everything here is another node's or a model's. view.js hands back strings and numbers; the component draws them as text nodes.
const HOSTILE = '<img src=x onerror=alert(1)>' + String.fromCharCode(0x1b) + '[31m' + String.fromCharCode(0x07, 0x202e, 0x200b, 0xfeff, 0x85) + ' end';
const hostileSnap = () => ({
  available: true, aoide: { path: HOSTILE, version: HOSTILE },
  daemon: { socket: HOSTILE, ping_ms: 3 },
  registry: [{ name: HOSTILE, verified: true, allows: [HOSTILE], grants: { [HOSTILE]: [HOSTILE], __proto__x: [] }, addedAt: HOSTILE },
    { name: 'constructor', verified: false, allows: [], grants: {}, addedAt: '2026-01-01' }],
  cache: [{ name: HOSTILE, age_secs: 1, text: HOSTILE, cut: false }, { name: 'constructor', age_secs: 1, text: HOSTILE, cut: false }],
  graph: { nodes: [{ id: HOSTILE, kind: HOSTILE, name: HOSTILE, petname: HOSTILE, state: HOSTILE, agent: HOSTILE, children: 2 }], total: 1, kinds: { [HOSTILE]: 1 }, edges: 0 },
  mesh: { report: { sections: [{ name: HOSTILE, grant: [HOSTILE], declared: 1, rows: [{ node: HOSTILE, class: '__proto__', declared: HOSTILE, recorded: HOSTILE },
    { node: 'n', class: 'constructor' }, { node: 'm', class: 'toString' }] }], undeclared: [HOSTILE] } },
  outbox: { entries: [{ to: HOSTILE, msgid: HOSTILE, tries: 1, lastTryAt: HOSTILE, delivery: { status: HOSTILE, reason: HOSTILE, reasonScope: HOSTILE, nextAttemptAt: HOSTILE } }] },
  errors: [HOSTILE],
});

// Every leaf of a value: strings must be clean, and the value must be data (no function, no vnode, no class instance).
function leaves(v, seen = []) {
  if (typeof v === 'string') { seen.push(v); return seen; }
  if (v === null || v === undefined || typeof v === 'number' || typeof v === 'boolean') return seen;
  assert.ok(Array.isArray(v) || Object.getPrototypeOf(v) === Object.prototype, `not plain data: ${Object.prototype.toString.call(v)}`);
  assert.ok(!('$$typeof' in v) && !('__k' in v) && !('props' in v && 'type' in v), 'a vnode');
  for (const x of Array.isArray(v) ? v : Object.values(v)) leaves(x, seen);
  return seen;
}
const DIRTY = new RegExp('[' + [[0, 0x1f], [0x7f, 0x9f], [0x200b, 0x200f], [0x2028, 0x202e], [0x2060, 0x206f], [0xfeff, 0xfeff]]
  .map(([a, b]) => String.fromCharCode(a) + '-' + String.fromCharCode(b)).join('') + ']');

test('hostile text in every part stays a plain string: markup is left as the characters it is, controls and bidi marks are gone', () => {
  const s = hostileSnap(), ev = { class: 'gate', kind: HOSTILE, source: HOSTILE, status: HOSTILE, message: HOSTILE, ts: HOSTILE, payload: { [HOSTILE]: HOSTILE } };
  const out = {
    state: V.stateOf({ status: 200, data: s }), header: V.headerParts(s), nodes: V.nodeRows(s), drift: V.driftView(s.mesh), graph: V.graphView(s.graph),
    outbox: V.outboxView(s.outbox, NOW), notes: V.noteLines(s), event: V.eventLine(ev), failed: V.stateOf({ status: 500, data: { error: HOSTILE } }),
    feed: V.feedView(V.foldFeed(V.emptyFeed(), { status: 200, data: { events: [ev, { class: 'audit', payload: HOSTILE }, { class: 'audit', payload: { a: { b: HOSTILE }, c: HOSTILE } }], next: 9 } }, 1)),
    gone: V.feedView(V.foldFeed(V.emptyFeed(), { status: 500, data: { error: HOSTILE } }, 1)),
  };
  const strings = leaves(out);
  assert.ok(strings.length > 40, 'a lot of text was checked');
  for (const t of strings) assert.doesNotMatch(t, DIRTY, `unclean text: ${JSON.stringify(t)}`);
  assert.ok(JSON.stringify(out).includes('<img src=x onerror=alert(1)>'), 'the markup is still there as text: it is drawn as a text node, not escaped here');
  assert.ok(strings.every((t) => t.length <= 300), 'every string is cut short');
});

test('a hostile class or node name is no key, no colour and no crash: the colour comes from a table and is a string', () => {
  const d = V.driftView(hostileSnap().mesh);
  assert.deepEqual(d.sections[0].rows.map((r) => [r.what, r.colour]), [['__proto__', ''], ['constructor', ''], ['toString', '']]);
  const rows = V.nodeRows(hostileSnap());
  assert.equal(rows.length, 2);
  assert.equal(rows.find((r) => r.name === 'constructor').cached, 'cached 1s ago', 'a node named constructor has its roster');
  assert.equal(typeof V.eventLine({ class: 'constructor' }).cls, 'string');
  assert.equal(V.eventLine({ class: 'secret', kind: 'x' }).cls, 'audit', 'only gate is gate');
});

test('a huge string is cut before it reaches a row', () => {
  const big = 'x'.repeat(100000);
  const rows = V.nodeRows({ registry: [{ name: big, allows: [big], addedAt: big }], cache: [] });
  for (const t of leaves(rows)) assert.ok(t.length <= 200, `${t.length} characters`);
  assert.ok(V.eventLine({ kind: big, message: big }).text.length <= 200);
});

// ---------------------------------------------------------------- the polls (index.js), against a stub fetch
let calls, real, answers;
beforeEach(() => {
  calls = []; answers = [];
  real = globalThis.fetch;
  globalThis.fetch = (url) => new Promise((resolve) => {
    calls.push(url);
    const give = (body, status = 200) => resolve({ status, text: async () => JSON.stringify(body) });
    if (answers.length) give(...answers.shift()); else give({}, 404);
  });
  M.feed.value = V.emptyFeed();
  M.snap.value = null;
  M.good.value = null;
});
afterEach(() => { globalThis.fetch = real; });

test('two events polls at once ask once and fold once: the same events are never held twice', async () => {
  answers.push([{ events: [{ class: 'audit', kind: 'one' }], next: 50, reset: false }]);
  await Promise.all([M.pollFeed(), M.pollFeed()]);
  assert.deepEqual(calls, ['/hub/mesh/events']);
  assert.deepEqual(M.feed.value.lines.map((l) => l.text), ['one']);
  assert.equal(M.feed.value.next, 50);
  answers.push([{ events: [{ class: 'gate', kind: 'two' }], next: 90, reset: false }]);
  await M.pollFeed();
  assert.deepEqual(calls, ['/hub/mesh/events', '/hub/mesh/events?after=50'], 'the second poll carries the cursor');
  assert.deepEqual(M.feed.value.lines.map((l) => l.text), ['one', 'two']);
});

test('a snapshot that fails after a good one leaves the good one in place', async () => {
  answers.push([snap]);
  await M.loadSnap(false);
  const first = M.good.value;
  assert.equal(first.status, 200);
  answers.push([{ error: 'aoide is slow' }, 502]);
  await M.loadSnap(true);
  assert.equal(M.snap.value.status, 502, 'the last answer is the failure');
  assert.equal(M.good.value, first, 'the good one is kept to be drawn with the reason');
  assert.deepEqual(calls, ['/hub/mesh', '/hub/mesh']);
  assert.equal(M.busy.value, false);
});

test('an unavailable answer is a good answer: it replaces what was held', async () => {
  answers.push([snap]);
  await M.loadSnap(false);
  answers.push([none]);
  await M.loadSnap(false);
  assert.equal(V.stateOf(M.good.value).state, 'unavailable');
});

// ---------------------------------------------------------------- what the tab's source may do
test('the tab asks only for mesh and the events tail, polls in the lane while visible, and draws nothing as markup', () => {
  const idx = code('../mesh/index.js'), view = code('../mesh/view.js');
  const asked = [...idx.matchAll(/hub(?:Get|Poll)\((\S+?),/g)].map((m) => m[1]);
  assert.deepEqual([...new Set(asked)].sort(), ["'mesh'", 'V.feedPath(feed.peek())'].sort(), 'the only paths asked for');
  assert.match(idx, /import \{ hubPoll \} from '\.\.\/core\/lane\.js'/);
  assert.match(idx, /hubPoll\(V\.feedPath/, 'the events tail is a lane poll');
  assert.match(idx, /lane \? await hubPoll\('mesh'/, 'the periodic snapshot is a lane poll and the open one a plain hubGet');
  assert.equal([...idx.matchAll(/visible\(\)/g)].length >= 4, true, 'every timer and the first ask check that the page is visible');
  assert.match(idx, /clearInterval\(events\)[\s\S]*clearInterval\(whole\)[\s\S]*removeEventListener\('visibilitychange'/, 'leaving the tab stops all of it');
  for (const src of [idx, view]) {
    assert.doesNotMatch(src, /innerHTML|dangerouslySetInnerHTML|insertAdjacentHTML|outerHTML|document\.write|\beval\(|new Function|\bfetch\(/);
    assert.doesNotMatch(src, /no activity|\bonline\b|\boffline\b/i, 'the tab never says nothing happened, or that a node is up or down');
  }
  assert.doesNotMatch(idx, /\bsetInterval\([^)]*hubGet/);
  assert.match(read('../index.html'), /href="mesh\/mesh\.css"/);
});

test('the door has no mesh to read: it says the tab needs the hub, and the sentence is the tab\'s', () => {
  assert.match(code('../mesh/index.js'), /mode\(\) === 'door'\) return html`<\$\{Needs\}>the mesh needs the hub/);
});
