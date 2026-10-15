// The web terminals (terms/): the page's side of the terminal stream and the screens, pure: no DOM, no network. The stream and the input are
// reached through fetch, which each test that needs one stubs (and afterEach puts back); the screen is injected through the model's hooks,
// or built on a stand-in for xterm (fakeXterm) with the page's few globals stubbed (withPage). The activity folder's taps and gate are
// called directly. The last tests read the sources, for what only a source can hold (no innerHTML, one window.open, no literal colour,
// one lane poll, the line in every header).
import { test, mock, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import * as M from '../terms/model.js';
import * as V from '../terms/view.js';
import * as A from '../terms/activity.js';
import { host, extraStreams, frameTaps, rightTabs } from '../core/registry.js';
import { detect, mode } from '../core/api.js';

// As LF: a checkout with core.autocrlf has CRLF sources, and the source scans search for single lines.
const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
const termsSrc = () => readdirSync(fileURLToPath(new URL('../terms/', import.meta.url)))
  .filter((f) => f.endsWith('.js')).map((f) => [f, src('../terms/' + f)]);
const b64 = (s) => Buffer.from(s, 'utf8').toString('base64');
const bytesOf = (s) => new Uint8Array(Buffer.from(s, 'utf8'));
const tick = () => new Promise((r) => setTimeout(r, 0));
const settle = () => new Promise((r) => setImmediate(r));   // not the timers a test may mock: promises settle, the clock does not move
const row = (over = {}) => ({ id: 't1', title: '', fg: 'bash', by: 'operator', cwd: '/w', cols: 120, rows: 32, alive: true, seq: 0, started_ms: 1000, last_ms: 0, hold: false, ...over });
const realFetch = globalThis.fetch;
// The hooks view.js set when it loaded: afterEach puts the model's no-op hooks back, so a test of the screen's hooks calls these.
const VIEW = { ...M.hooks };

// The page's mode as detect() sets it from the perch's answer: 'perch' is a 200 list, 'door' a 404. Leaves the detect stub in fetch.
async function modeAs(kind) {
  globalThis.fetch = async () => (kind === 'perch' ? { status: 200, text: async () => '[]' } : { status: 404, text: async () => '' });
  await detect();
  assert.equal(mode(), kind);
}

afterEach(() => {
  mock.restoreAll();
  M._reset();
  host.toast = undefined; host.focused = undefined; host.showTab = undefined; host.chatStreams = undefined;
  globalThis.fetch = realFetch;
});

// ---------------------------------------------------------------- the stack's order and its freeze
test('the stack ranks the inspected chat first, then the most recent output', () => {
  const rows = [
    row({ id: 't1', last_ms: 500 }),
    row({ id: 't2', last_ms: 900, chat: 'c1' }),
    row({ id: 't3', last_ms: 1200 }),
    row({ id: 't4', last_ms: 300, chat: 'c1' }),
  ];
  const linked = (r) => r.chat === 'c1';
  assert.deepEqual(M.rankRows(rows, linked), ['t2', 't4', 't3', 't1'], 'linked by recency, then the rest by recency');
  assert.deepEqual(M.rankRows(rows, () => false), ['t3', 't2', 't1', 't4'], 'no chat: recency alone');
});

test('reorderDue: at most every 2 s, and never while frozen (the stack sets that flag on hover or focus)', () => {
  assert.equal(M.reorderDue(10000, 9000, false), false, 'under 2 s since the last');
  assert.equal(M.reorderDue(11000, 9000, false), true, 'at 2 s, free');
  assert.equal(M.reorderDue(20000, 9000, true), false, 'frozen: never, however long');
});

// ---------------------------------------------------------------- folds
test('a fold is open by default for the chat\'s terminals and the two newest, and a person\'s choice wins', () => {
  const rows = [
    row({ id: 't1', started_ms: 100, chat: 'c1' }),
    row({ id: 't2', started_ms: 200 }),
    row({ id: 't3', started_ms: 300 }),
    row({ id: 't4', started_ms: 50 }),
  ];
  const dflt = M.foldDefault(rows, (r) => r.chat === 'c1');
  assert.deepEqual([...dflt].sort(), ['t1', 't2', 't3'], 'the chat\'s t1 and the two newest t3, t2');
  assert.equal(M.foldOpen({}, 't4', dflt), false, 'the older one with no link is closed');
  assert.equal(M.foldOpen({ t4: 'o' }, 't4', dflt), true, 'a person opened it');
  assert.equal(M.foldOpen({ t3: 'c' }, 't3', dflt), false, 'a person closed a default-open one');
});

test('the fold kept in localStorage holds only terminal ids with o or c; a broken value is no choices', () => {
  assert.deepEqual(M.cleanFold({ t1: 'o', t2: 'c', 'x': 'o', t3: 'maybe', __proto__x: 1 }), { t1: 'o', t2: 'c' });
  assert.deepEqual(M.cleanFold(['t1']), {});
  assert.deepEqual(M.cleanFold(null), {});
  assert.deepEqual(M.loadFold(), {}, 'no localStorage in node: no choices, no throw');
  assert.doesNotThrow(() => M.saveFold({ t1: 'o' }));
});

// ---------------------------------------------------------------- the input coalescer
test('input: chunks keep their order, join into one call while one is in flight, and one call is in flight at a time', async () => {
  const calls = [];
  let flying = 0, peak = 0;
  const q = M.inputQueue(async (b) => {
    flying++; peak = Math.max(peak, flying);
    calls.push(Buffer.from(b).toString());
    await tick();
    flying--;
    return 204;
  }, () => assert.fail('no refusal expected'));
  q.push(bytesOf('a'));
  q.push(bytesOf('b'));
  q.push(bytesOf('c'));
  for (let i = 0; i < 10; i++) await tick();
  assert.deepEqual(calls, ['a', 'bc'], 'the first alone, then the two that waited, joined and in order');
  assert.equal(peak, 1, 'never two in flight');
  assert.equal(q.size, 0);
});

test('input: a paste over 64 KiB goes in sequential chunks of at most 64 KiB', async () => {
  const sizes = [];
  const q = M.inputQueue(async (b) => { sizes.push(b.length); await tick(); return 204; }, () => assert.fail('no refusal expected'));
  q.push(new Uint8Array(150 * 1024).fill(0x61));
  for (let i = 0; i < 10; i++) await tick();
  assert.deepEqual(sizes, [64 * 1024, 64 * 1024, 22 * 1024]);
});

test('input: a refused call is said once, drops what waits, and is never retried', async () => {
  const sent = [], refused = [];
  let first = true;
  const q = M.inputQueue(async (b) => {
    sent.push(Buffer.from(b).toString());
    if (first) { first = false; await tick(); return 409; }
    return 204;
  }, (st) => refused.push(st));
  q.push(bytesOf('x'));
  q.push(bytesOf('y'));
  for (let i = 0; i < 6; i++) await tick();
  assert.deepEqual(sent, ['x'], 'the refused call is not sent again, and y (waiting) is dropped');
  assert.deepEqual(refused, [409]);
  q.push(bytesOf('z'));
  for (let i = 0; i < 4; i++) await tick();
  assert.deepEqual(sent, ['x', 'z'], 'a new key after the refusal goes out as a new call');
});

test('input: a call that throws counts as no answer (status 0), and is said', async () => {
  const refused = [];
  const q = M.inputQueue(async () => { throw new Error('network'); }, (st) => refused.push(st));
  q.push(bytesOf('k'));
  for (let i = 0; i < 4; i++) await tick();
  assert.deepEqual(refused, [0]);
  assert.match(M.refusalWords(0), /no answer from the perch/);
  assert.match(M.refusalWords(409), /ended/);
  assert.match(M.refusalWords(777), /777/);
});

// ---------------------------------------------------------------- frames
test('base64 frame bytes decode across a UTF-8 character split between two frames', () => {
  const text = 'a€b😀c';
  const whole = Buffer.from(text, 'utf8');
  // A cut inside the three-byte euro sign and inside the four-byte emoji: one character in each of two frames.
  const cut1 = 2, cut2 = 8;
  const frames = [whole.subarray(0, cut1), whole.subarray(cut1, cut2), whole.subarray(cut2)].map((b) => b.toString('base64'));
  const dec = new TextDecoder();
  let out = '';
  for (const f of frames) out += dec.decode(M.decodeB64(f), { stream: true });
  out += dec.decode();
  assert.equal(out, text, 'the streaming decoder joins the split characters');
  assert.deepEqual([...M.decodeB64(b64('hi'))], [104, 105]);
  assert.throws(() => M.decodeB64('%%%'), 'an invalid base64 is a throw the caller takes');
});

test('the perch row is checked and cut to what the page reads', () => {
  assert.equal(M.vet({ id: '../t1' }), null, 'an id that is no terminal id');
  assert.equal(M.vet({ id: 't0x' }), null);
  const v = M.vet({ id: 't7', title: 'vim', by: 'agent', chat: 'c1', cols: 9999, rows: -3, alive: true, agent_last_ms: 42.7 });
  assert.equal(v.by, 'agent');
  assert.equal(v.chat, 'c1');
  assert.equal(v.cols, 400, 'clamped to the perch\'s range');
  assert.equal(v.rows, 5);
  assert.equal(v.agent_last_ms, 42);
  assert.equal(M.vet({ id: 't7', by: 'something' }).by, 'operator', 'an unknown owner is the operator\'s');
});

test('a title or an agent\'s line is text: controls and bidi characters go, the length is cut', () => {
  assert.equal(M.clipText('a\u0007b‮c​', 80), 'a bc', 'a control becomes a space, a bidi or zero-width mark goes');
  assert.equal(M.clipText('x'.repeat(100), 80).length, 80);
  assert.equal(M.clipText('😀'.repeat(5), 3), '😀😀😀', 'counted in characters, not code units');
  assert.equal(M.clipText(undefined, 5), '');
});

test('onFrame: a list makes the roster and the entries; output goes to the screen hook and moves the seq; a frame for an unknown id is dropped', () => {
  const made = [], out = [];
  M.setHooks({ made: (id) => made.push(id), out: (id, b) => out.push([id, Buffer.from(b).toString()]) });
  M.onFrame({ type: 'term-out', id: 't1', seq: 3, data: b64('early') }, 1000);
  assert.deepEqual(out, [], 'a frame before the list is for nothing the page holds');
  M.onFrame({ type: 'term-list', terms: [row({ id: 't1' }), row({ id: 't2', by: 'agent' })] });
  assert.equal(M.roster.peek().length, 2);
  assert.deepEqual(made, ['t1', 't2'], 'an entry for each listed terminal');
  assert.equal(M.status.peek(), 'live');
  M.onFrame({ type: 'term-out', id: 't1', seq: 17, data: b64('hello') }, 1000);
  assert.deepEqual(out, [['t1', 'hello']]);
  assert.equal(M.live.get('t1').seq, 17, 'the seq a resume asks from');
});

test('onFrame: a row\'s last_ms moves at most once a second under output', () => {
  M.setHooks({ out() {} });
  M.onFrame({ type: 'term-list', terms: [row({ id: 't1', last_ms: 0 })] });
  M.onFrame({ type: 'term-out', id: 't1', seq: 1, data: b64('a') }, 1000);
  assert.equal(M.roster.peek()[0].last_ms, 1000);
  M.onFrame({ type: 'term-out', id: 't1', seq: 2, data: b64('b') }, 1500);
  assert.equal(M.roster.peek()[0].last_ms, 1000, 'within the second: not moved');
  M.onFrame({ type: 'term-out', id: 't1', seq: 3, data: b64('c') }, 2100);
  assert.equal(M.roster.peek()[0].last_ms, 2100);
});

test('onFrame: an agent typing is a line, a time and an event; an operator\'s input is none of those', () => {
  const events = [];
  M.setHooks({ event: (ev) => events.push(ev) });
  M.onFrame({ type: 'term-list', terms: [row({ id: 't1' })] });
  M.onFrame({ type: 'term-input', id: 't1', by: 'operator', chat: 'c1', text: 'ls' }, 3000);
  assert.deepEqual(events, [], 'the operator typing is not an agent event');
  M.onFrame({ type: 'term-input', id: 't1', by: 'agent', chat: 'c1', text: 'ls\r' }, 3000);
  assert.deepEqual(events, [{ kind: 'type', id: 't1', chat: 'c1' }]);
  assert.equal(M.typedAt.peek().t1, 3000);
  assert.equal(M.agentLine.peek().t1, 'ls ', 'the line is text: a carriage return is a space (the perch sends its return sign, not a control)');
  assert.equal(M.roster.peek()[0].agent_last_ms, 3000);
});

test('onFrame: an agent\'s new terminal is an open event; a closed one leaves the roster and its screen is let go', () => {
  const events = [], gone = [];
  M.setHooks({ event: (ev) => events.push(ev), gone: (id) => gone.push(id) });
  M.onFrame({ type: 'term-list', terms: [] });
  M.onFrame({ type: 'term-open', id: 't9', term: row({ id: 't9', by: 'agent', chat: 'c2' }) });
  assert.deepEqual(events, [{ kind: 'open', id: 't9', chat: 'c2' }]);
  assert.equal(M.roster.peek().length, 1);
  M.onFrame({ type: 'term-closed', id: 't9' });
  assert.equal(M.roster.peek().length, 0);
  assert.deepEqual(gone, ['t9']);
  assert.equal(M.live.has('t9'), false);
});

test('onFrame: a size, a title, a hold and an exit change the row and nothing else', () => {
  const sizes = [];
  M.setHooks({ size: (id, c, r) => sizes.push([id, c, r]) });
  M.onFrame({ type: 'term-list', terms: [row({ id: 't1' })] });
  M.onFrame({ type: 'term-size', id: 't1', cols: 80, rows: 24 });
  M.onFrame({ type: 'term-title', id: 't1', title: 'vim  x', fg: 'vim' });
  M.onFrame({ type: 'term-hold', id: 't1', hold: true });
  M.onFrame({ type: 'term-exit', id: 't1', code: 0, signal: null });
  const r = M.roster.peek()[0];
  assert.deepEqual([r.cols, r.rows, r.title, r.fg, r.hold, r.alive], [80, 24, 'vim  x', '', true, false]);
  assert.equal(r.exit.code, 0, 'the exit code the perch sent');
  assert.deepEqual(sizes, [['t1', 80, 24]]);
});

test('a term-exit carries the perch\'s flat code and signal into the row (a null is no value)', () => {
  M.onFrame({ type: 'term-list', terms: [row({ id: 't2' }), row({ id: 't3' })] });
  M.onFrame({ type: 'term-exit', id: 't2', code: 3, signal: null });
  M.onFrame({ type: 'term-exit', id: 't3', code: null, signal: 9 });
  const [a, b] = M.roster.peek();
  assert.deepEqual(a.exit, { code: 3, signal: null });
  assert.deepEqual(b.exit, { code: null, signal: 9 });
  assert.equal(a.alive, false);
});

test('a list replaces the roster: a terminal no longer listed is dropped with its screen, one still listed keeps its entry', () => {
  const gone = [], made = [];
  M.setHooks({ gone: (id) => gone.push(id), made: (id) => made.push(id) });
  M.applyList([row({ id: 't1' }), row({ id: 't2' })]);
  assert.deepEqual(made, ['t1', 't2']);
  const keep = M.live.get('t1');
  M.applyList([row({ id: 't1' }), row({ id: '../x' }), row({ id: 't1' })]);
  assert.deepEqual(M.roster.peek().map((r) => r.id), ['t1'], 'a bad id and a repeat are not rows');
  assert.deepEqual(gone, ['t2']);
  assert.equal(M.live.get('t1'), keep, 'the kept terminal keeps its entry');
});

test('a since list asks for the terminals whose seq the page has, and only those', () => {
  assert.equal(M.sinceOf([['t1', 4096], ['t3', 0]]), 't1:4096,t3:0');
  assert.equal(M.sinceOf([['t1', -1], ['x', 2], ['t2', 1.5], ['t4', 7]]), 't4:7');
  assert.equal(M.sinceOf([]), '');
});

test('a refusal is a sentence the perch did not write', () => {
  assert.match(M.refusalWords(503), /queue is full/);
  assert.match(M.refusalWords(404), /no such terminal/);
});

test('ages read in seconds, minutes, hours and days; nothing without a start', () => {
  assert.equal(M.ageText(0, 5000), '');
  assert.equal(M.ageText(1000, 46000), '45s');
  assert.equal(M.ageText(0 + 1, 1 + 3 * 60 * 1000), '3m');
  assert.equal(M.ageText(1, 2 * 3600 * 1000 + 1), '2h');
  assert.equal(M.ageText(1, 2 * 86400 * 1000 + 1), '2d');
});

// ---------------------------------------------------------------- the screens' pure parts
test('the theme comes from the --base0X the look wrote: each slot maps, a value that is not #rrggbb is left to xterm', () => {
  const v = {};
  for (const d of '0123456789ABCDEF') v['base0' + d] = '#' + d.repeat(6).toLowerCase();
  const t = V.xtermTheme(v);
  assert.equal(t.background, v.base00);
  assert.equal(t.foreground, v.base05);
  assert.equal(t.cursor, v.base06);
  assert.equal(t.red, v.base08);
  assert.equal(t.green, v.base0B);
  assert.equal(t.brightWhite, v.base07);
  assert.equal(t.brightBlack, v.base03);
  assert.equal(V.xtermTheme({ base00: 'red', base05: '#abc' }).background, undefined);
  assert.equal(V.xtermTheme({ base00: 'red' }).foreground, undefined);
});

test('the queries the page swallows are the perch\'s answers and the two it never gives, with their exact forms', () => {
  assert.deepEqual(V.QUERIES.map((q) => q.name), [
    'DA1', 'DA2', 'DSR', 'DECRQM', 'DECRQM DEC', 'CSI 18 t', 'DECRQSS', 'XTVERSION', 'OSC 4/10/11/12',
  ]);
  const q = (name) => V.QUERIES.find((x) => x.name === name);
  assert.equal(q('DA1').when([]), true);
  assert.equal(q('DA1').when([0]), true);
  assert.equal(q('DA1').when([1]), false, 'a DA1 with a parameter that is not 0 is no query');
  assert.equal(q('DSR').when([5]), true);
  assert.equal(q('DSR').when([6]), true);
  assert.equal(q('DSR').when([0]), false, 'CSI 0 n is not a status request');
  assert.equal(q('CSI 18 t').when([18]), true);
  assert.equal(q('CSI 18 t').when([14]), false, 'CSI 14 t (pixels) is not swallowed');
  assert.equal(q('OSC 4/10/11/12').when('11;?'), true, 'asking');
  assert.equal(q('OSC 4/10/11/12').when('11;rgb:ff/ff/ff'), false, 'setting is not swallowed');
  assert.deepEqual(q('OSC 4/10/11/12').ids, [4, 10, 11, 12]);
});

test('links open only as http or https: any other scheme is refused, and the text is never run', () => {
  assert.equal(V.linkUrl('https://example.org/a?b=1'), 'https://example.org/a?b=1');
  assert.equal(V.linkUrl('http://example.org'), 'http://example.org/');
  assert.equal(V.linkUrl('javascript:alert(1)'), null);
  assert.equal(V.linkUrl('file:///etc/passwd'), null);
  assert.equal(V.linkUrl('data:text/html,<script>x</script>'), null);
  assert.equal(V.linkUrl('not a url'), null);
});

// ---------------------------------------------------------------- the screen, on a stand-in for xterm
// The page's globals a screen reads (the document, the colours, the reduced-motion query): stood in for, and put back after.
function withPage(fn, { reduced = false } = {}) {
  const keys = ['document', 'getComputedStyle', 'matchMedia'];
  const saved = keys.map((k) => [k, k in globalThis, globalThis[k]]);
  globalThis.document = { documentElement: {}, body: fakeEl(), createElement: () => fakeEl() };
  globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });
  globalThis.matchMedia = () => ({ matches: reduced });
  try { return fn(); } finally {
    for (const [k, had, v] of saved) { if (had) globalThis[k] = v; else delete globalThis[k]; }
  }
}

// An element as far as the screens need it: one parent at a time (appending moves it, as the DOM does), and its children.
function fakeEl() {
  const el = {
    parent: null, children: [],
    appendChild(c) {
      if (c.parent) c.parent.children = c.parent.children.filter((x) => x !== c);
      c.parent = el;
      el.children.push(c);
      return c;
    },
    setAttribute() {},
  };
  return el;
}

// A stand-in for xterm's Terminal: what the screen makes it with (its options), what it registers on the parser, and its bell.
function fakeXterm() {
  const made = { options: null, csi: [], dcs: [], osc: [], bell: null };
  class Terminal {
    constructor(opts) {
      made.options = opts;
      this.element = fakeEl();
      this.parser = {
        registerCsiHandler: (id, fn) => made.csi.push({ id, fn }),
        registerDcsHandler: (id, fn) => made.dcs.push({ id, fn }),
        registerOscHandler: (n, fn) => made.osc.push({ id: n, fn }),
      };
    }
    loadAddon() {}
    open(parent) { parent.appendChild(this.element); }
    onData() {}
    onBinary() {}
    onBell(cb) { made.bell = cb; }
    resize() {}
    dispose() {}
  }
  class FitAddon {}
  return { x: { Terminal, FitAddon }, made };
}

// The screen of one terminal, made as the page makes it (view.js create), on the stand-in. The entry is the model's own.
function screenFor(id) {
  M.applyList([row({ id })]);
  const e = M.live.get(id);
  e.host = fakeEl();
  const fake = fakeXterm();
  V.create(e, fake.x);
  return fake.made;
}

test('a screen is made with the page\'s link handler, the terminal\'s own font and a blinking cursor; its bell flashes the header', () => {
  withPage(() => {
    const made = screenFor('t7');
    assert.equal(made.options.linkHandler, V.LINKS, 'the link handler is the page\'s, never xterm\'s default');
    assert.equal(made.options.cursorBlink, true, 'no reduced motion: the cursor blinks');
    assert.equal(made.options.fontFamily, V.FONT);
    made.bell();
    assert.equal(typeof M.bellAt.peek().t7, 'number', 'a bell is the header\'s flash');
  });
});

test('under reduced motion the cursor does not blink', () => {
  withPage(() => {
    assert.equal(screenFor('t8').options.cursorBlink, false);
  }, { reduced: true });
});

test('a terminal\'s one element moves between hosts and is never in two: a mount moves it, an unmount by a former host leaves it, the host gives it back to the shelf', () => {
  withPage(() => {
    screenFor('t11');
    const e = M.live.get('t11');
    const first = e.host, second = fakeEl();
    assert.equal(first.children.length, 1, 'made in its first host');
    V.mountTerm('t11', second);
    assert.equal(first.children.length, 0, 'moved, not copied');
    assert.equal(second.children.length, 1);
    assert.equal(V.mountedIn('t11'), second);
    V.unmountTerm('t11', first);
    assert.equal(second.children.length, 1, 'a former host leaves it alone');
    V.unmountTerm('t11', second);
    assert.equal(second.children.length, 0);
    assert.equal(V.mountedIn('t11'), null, 'and it is on the shelf, shown by no host');
    assert.equal(globalThis.document.body.children.length, 1, 'the shelf is out of sight, in the body');
  });
});

test('the queries are swallowed on the screen\'s own parser: 7 CSI forms, DECRQSS, and OSC 4/10/11/12, each taken exactly when its form is the query', () => {
  withPage(() => {
    const made = screenFor('t9');
    const q = (n) => V.QUERIES.find((x) => x.name === n);
    const csi = (n) => made.csi.find((r) => r.id === q(n).id);
    assert.equal(made.csi.length, 7);
    assert.equal(made.dcs.length, 1, 'DECRQSS');
    assert.deepEqual(made.osc.map((r) => r.id).sort((a, b) => a - b), [4, 10, 11, 12]);
    assert.equal(csi('DA1').fn([]), true, 'DA1 is taken: xterm writes no reply');
    assert.equal(csi('DSR').fn([6]), true);
    assert.equal(csi('DSR').fn([0]), false, 'a CSI 0 n is not a status request: left to xterm');
    assert.equal(csi('XTVERSION').fn([0]), true, 'XTVERSION is taken, so neither side answers');
    assert.equal(made.dcs[0].fn(), true, 'DECRQSS is taken');
    assert.equal(made.osc.find((r) => r.id === 11).fn('11;?'), true, 'an OSC 11 asking is taken');
    assert.equal(made.osc.find((r) => r.id === 11).fn('11;rgb:00/00/00'), false, 'setting a colour is not');
  });
});

test('a link opens only on ctrl or cmd, only as http or https, in a new tab with noopener; any other scheme is refused and said', () => {
  const opened = [], toasts = [];
  const had = 'window' in globalThis, savedWindow = globalThis.window;
  globalThis.window = { open: (...a) => opened.push(a) };
  host.toast = (text, err) => toasts.push({ text, err });
  try {
    V.LINKS.activate({ ctrlKey: false, metaKey: false }, 'https://example.org/');
    assert.equal(opened.length, 0, 'a plain click opens nothing');
    V.LINKS.activate({ ctrlKey: true }, 'https://example.org/a');
    V.LINKS.activate({ metaKey: true }, 'http://example.org/b');
    assert.deepEqual(opened, [['https://example.org/a', '_blank', 'noopener'], ['http://example.org/b', '_blank', 'noopener']]);
    V.LINKS.activate({ ctrlKey: true }, 'javascript:alert(1)');
    V.LINKS.activate({ ctrlKey: true }, 'file:///etc/passwd');
    assert.equal(opened.length, 2, 'no other scheme reaches window.open');
    assert.deepEqual(toasts.map((t) => t.err), [true, true], 'each refused link is said');
    assert.equal(V.LINKS.allowNonHttpProtocols, false);
    V.LINKS.activate(undefined, 'https://example.org/');
    assert.equal(opened.length, 2, 'no event, no open, no throw');
  } finally {
    if (had) globalThis.window = savedWindow; else delete globalThis.window;
  }
});

// ---------------------------------------------------------------- the stream, the roster, the bounds
test('the terminal stream takes one of the four connection slots: with three chat streams it opens and holds one; with four it says slot', () => {
  globalThis.fetch = () => new Promise(() => {});   // the connection never answers: it stays open for the test
  host.chatStreams = () => 3;
  assert.equal(M.startStream(), true);
  assert.equal(extraStreams.peek(), 1, 'the terminal stream holds a slot');
  assert.equal(M.streamUp(), true);
  M.stopStream();
  assert.equal(extraStreams.peek(), 0, 'and gives it back');
  host.chatStreams = () => 4;
  assert.equal(M.startStream(), false);
  assert.equal(M.status.peek(), 'slot');
  assert.equal(extraStreams.peek(), 0, 'no slot, no stream');
  assert.equal(M.streamUp(), false);
});

test('the stream closes 10 s after the last view gives back; a view that comes back inside that keeps it open', () => {
  globalThis.fetch = () => new Promise(() => {});
  host.chatStreams = () => 0;
  mock.timers.enable({ apis: ['setTimeout'] });
  try {
    const give1 = M.watch();
    assert.equal(M.streamUp(), true);
    give1();
    mock.timers.tick(M.STOP_AFTER - 1);
    assert.equal(M.streamUp(), true, 'not before 10 s');
    const give2 = M.watch();
    mock.timers.tick(M.STOP_AFTER * 2);
    assert.equal(M.streamUp(), true, 'a view is showing: the stream stays');
    give2();
    mock.timers.tick(M.STOP_AFTER - 1);
    assert.equal(M.streamUp(), true);
    mock.timers.tick(1);
    assert.equal(M.streamUp(), false, 'at 10 s, closed');
    assert.equal(M.status.peek(), 'closed');
    assert.equal(extraStreams.peek(), 0, 'and the slot is given back');
  } finally {
    mock.timers.reset();
  }
});

test('the roster keeps the perch\'s table of 16: a 17th terminal is not kept and has no entry', () => {
  M.applyList(Array.from({ length: 17 }, (_, i) => row({ id: 't' + (i + 1) })));
  assert.equal(M.roster.peek().length, M.MAX_TERMS);
  assert.equal(M.roster.peek().at(-1).id, 't16');
  assert.equal(M.live.has('t17'), false);
});

test('output past 2 MiB unparsed resyncs the terminal once: the seq is forgotten, and no second resync while the reset is on its way', () => {
  const pending = [];
  const e = {
    id: 't5', term: { write: (bytes, cb) => { if (cb) pending.push(cb); } }, fit: null, host: null, pend: null,
    seq: 9, queue: [], backlog: 0, loading: false, resyncing: false, follow: false,
  };
  M.live.set('t5', e);
  const MB = new Uint8Array(1024 * 1024);
  V.push(e, MB, false);
  V.push(e, MB, false);
  assert.equal(e.seq, 9, 'two MiB is the bound, not past it');
  V.push(e, new Uint8Array(1), false);
  assert.equal(e.seq, undefined, 'past it: the next connection asks for a reset');
  assert.equal(e.resyncing, true);
  e.seq = 12;
  V.push(e, MB, false);
  assert.equal(e.seq, 12, 'not again while the reset is on its way');
  while (pending.length) pending.shift()();
  assert.equal(e.backlog, 0, 'the screen has parsed what it was given');
});

test('an input the perch refuses is said once, is not sent again, and the next key goes out as a new call', async () => {
  const calls = [], toasts = [];
  globalThis.fetch = async (url, o) => { calls.push([url, o.method]); return { status: 409, text: async () => '' }; };
  host.toast = (text, err) => toasts.push({ text, err });
  M.applyList([row({ id: 't6' })]);
  M.typed('t6', bytesOf('ls\r'));
  await settle();
  await settle();
  assert.deepEqual(calls, [['/hub/term/t6/input', 'POST']], 'one POST, and it is not retried');
  assert.deepEqual(toasts, [{ text: 'input not sent: the terminal has ended', err: true }], 'said once');
  M.typed('t6', bytesOf('y'));
  await settle();
  await settle();
  assert.equal(calls.length, 2, 'a new key goes out as a new call');
});

test('an input call with no answer is a refusal at INPUT_MS (10 s), not before', async () => {
  const toasts = [];
  globalThis.fetch = (url, o) => new Promise((_, reject) => { o.signal.addEventListener('abort', () => reject(new Error('aborted'))); });
  host.toast = (text, err) => toasts.push({ text, err });
  M.applyList([row({ id: 't6' })]);
  mock.timers.enable({ apis: ['setTimeout'] });
  try {
    M.typed('t6', bytesOf('x'));
    await settle();
    mock.timers.tick(M.INPUT_MS - 1);
    await settle();
    assert.equal(toasts.length, 0, 'still waiting at 9.999 s');
    mock.timers.tick(1);
    await settle();
    assert.deepEqual(toasts, [{ text: 'input not sent: no answer from the perch', err: true }]);
  } finally {
    mock.timers.reset();
  }
});

test('the bounds are the spec\'s: the roster poll, the stream\'s close, the input\'s timeout and chunk, the backlog, the table, the lines, the rows', () => {
  assert.deepEqual({
    POLL_MS: M.POLL_MS, STOP_AFTER: M.STOP_AFTER, INPUT_MS: M.INPUT_MS, ROW_MS: M.ROW_MS, MAX_CHUNK: M.MAX_CHUNK, BACKLOG_MAX: M.BACKLOG_MAX,
    MAX_TERMS: M.MAX_TERMS, LINE_MAX: M.LINE_MAX, TITLE_MAX: M.TITLE_MAX, LIVE_ROWS: V.LIVE_ROWS, GAP_MS: A.GAP_MS,
  }, {
    POLL_MS: 5000, STOP_AFTER: 10000, INPUT_MS: 10000, ROW_MS: 1000, MAX_CHUNK: 65536, BACKLOG_MAX: 2097152,
    MAX_TERMS: 16, LINE_MAX: 200, TITLE_MAX: 80, LIVE_ROWS: 8, GAP_MS: 10000,
  });
  assert.equal(V.FONT, '"Cascadia Mono", Consolas, monospace');
});

test('an agent\'s line is cut to LINE_MAX characters and a title to TITLE_MAX, both as text', () => {
  M.applyList([row({ id: 't1' })]);
  M.onFrame({ type: 'term-input', id: 't1', by: 'agent', chat: 'c1', text: 'y'.repeat(1000) }, 3000);
  assert.equal(M.agentLine.peek().t1.length, M.LINE_MAX);
  assert.equal(V.titleText('z'.repeat(300)).length, M.TITLE_MAX);
  assert.equal(V.titleText('<b>x'), '<b>x', 'a title is text: markup is shown, never made');
});

// ---------------------------------------------------------------- the activity folder
test('the frame tap sees only the agent\'s term_open and term_send, and a term_send names a real terminal id', () => {
  const call = (name, input, type = 'tool-call-started') => ({ type, id: 'c1', name, input });
  assert.deepEqual(A.tapEvent('c1', call('term_send', { id: 't4', text: 'ls' })), { kind: 'type', id: 't4', chat: 'c1' });
  assert.deepEqual(A.tapEvent('c1', call('term_open', { cwd: '/x' })), { kind: 'open', id: null, chat: 'c1' });
  assert.equal(A.tapEvent('c1', call('term_read', { id: 't4' })), null, 'read is not an event');
  assert.equal(A.tapEvent('c1', call('term_list', {})), null);
  assert.equal(A.tapEvent('c1', call('term_close', { id: 't4' })), null);
  assert.equal(A.tapEvent('c1', call('bash', { command: 'ls' })), null);
  assert.equal(A.tapEvent('c1', call('term_send', { id: '../t4' })), null, 'a bad id is no terminal');
  assert.equal(A.tapEvent('c1', call('term_send', {})), null);
  assert.equal(A.tapEvent('c1', call('term_send', { id: 't4' }, 'tool-call-finished')), null, 'only the start of a call');
  assert.equal(A.tapEvent(null, call('term_send', { id: 't4' })), null);
});

test('one event per terminal per 10 s: the gate lets the first through and holds the rest until the gap has passed', () => {
  const g = A.makeGate(10000);
  assert.equal(g.allow('type t4', 0), true);
  assert.equal(g.allow('type t4', 5000), false);
  assert.equal(g.allow('type t5', 5000), true, 'another terminal is its own key');
  assert.equal(g.allow('type t4', 10000), true, 'the gap has passed');
});

test('a tap that is replayed says nothing, and a live term_send raises the badge and a toast with [ show ]', () => {
  const toasts = [];
  host.toast = (text, err, act) => toasts.push({ text, err, act });
  A.tabShown.value = false;
  A.badge.value = false;
  A.tap('c1', { type: 'tool-call-started', name: 'term_send', input: { id: 't71' } }, false);
  assert.equal(toasts.length, 0, 'not live: history');
  assert.equal(A.badge.peek(), false);
  A.tap('c1', { type: 'tool-call-started', name: 'term_send', input: { id: 't71' } }, true);
  assert.equal(toasts.length, 1);
  assert.match(toasts[0].text, /agent in .* is typing in t71$/);
  assert.equal(toasts[0].act.label, '[ show ]');
  assert.equal(A.badge.peek(), true, 'the rail icon has its dot');
});

test('the badge is not raised while the ⌨ tab shows, and an open raises the rail once for the focused chat only', () => {
  const shows = [];
  const toasts = [];
  host.toast = (text) => toasts.push(text);
  host.showTab = (side, name) => shows.push([side, name]);
  A.badge.value = false;
  A.tabShown.value = true;
  A.report({ kind: 'type', id: 't72', chat: 'c2' });
  assert.equal(A.badge.peek(), false, 'the tab is showing: no dot');
  A.tabShown.value = false;
  host.focused = () => ({ sid: 'c3', id: 'p1' });
  A.report({ kind: 'open', id: 't73', chat: 'c3' }, 100000);
  A.report({ kind: 'open', id: 't73', chat: 'c3' }, 100001);
  assert.deepEqual(shows, [['r', 'terms']], 'once per terminal, and on the focused chat\'s');
  host.focused = () => ({ kind: 'term', id: 'p2', term: 't74' });
  A.report({ kind: 'open', id: 't74', chat: 'c3' }, 200000);
  assert.equal(shows.length, 1, 'a focused terminal pane is no chat: the rail stays');
  host.focused = () => ({ sid: 'c9', id: 'p3' });
  A.report({ kind: 'open', id: 't75', chat: 'c3' }, 300000);
  assert.equal(shows.length, 1, 'another chat\'s agent does not switch it');
  assert.equal(toasts.length, 4, 'every key is said once in its window: t72, t73, t74, t75');
  assert.match(toasts[0], /is typing in t72$/);
  assert.match(toasts[1], /opened t73$/);
});

test('an agent opening a terminal is one toast: the open with no id yet only raises the badge, and the id\'s open that follows says it once', () => {
  const toasts = [];
  host.toast = (text) => toasts.push(text);
  A.tabShown.value = false;
  A.badge.value = false;
  A.report({ kind: 'open', id: null, chat: 'c4' }, 500000);
  assert.equal(toasts.length, 0, 'no toast for an open with no id');
  assert.equal(A.badge.peek(), true, 'the rail icon has its dot');
  A.report({ kind: 'open', id: 't90', chat: 'c4' }, 500100);
  A.report({ kind: 'open', id: 't90', chat: 'c4' }, 500200);
  assert.equal(toasts.length, 1, 'one toast for the terminal; a repeat inside 10 s is not said again');
  assert.match(toasts[0], /^agent in c4 opened t90$/);
});

test('the roster diff: a new agent terminal is an open, one whose agent input moved is a type; nothing on a first look', () => {
  assert.deepEqual(M.diffRoster(null, [row({ id: 't1', by: 'agent' })]), [], 'a first look is a baseline');
  const prev = [row({ id: 't1' }), row({ id: 't2', by: 'agent', agent_last_ms: 5 })];
  const next = [
    row({ id: 't1' }),
    row({ id: 't2', by: 'agent', agent_last_ms: 9, chat: 'c1' }),
    row({ id: 't3', by: 'agent', chat: 'c1' }),
    row({ id: 't4', by: 'operator' }),
  ];
  assert.deepEqual(M.diffRoster(prev, next), [
    { kind: 'type', id: 't2', chat: 'c1' },
    { kind: 'open', id: 't3', chat: 'c1' },
  ]);
});

test('a roster diff that finds an agent terminal raises the badge through report', () => {
  const toasts = [];
  host.toast = (text) => toasts.push(text);
  A.tabShown.value = false;
  A.badge.value = false;
  const before = [row({ id: 't1' })];
  const after = [row({ id: 't1' }), row({ id: 't80', by: 'agent', chat: 'c5' })];
  for (const ev of M.diffRoster(before, after)) A.report(ev, 400000);
  assert.equal(A.badge.peek(), true);
  assert.match(toasts[0], /opened t80$/);
});

// ---------------------------------------------------------------- the table's 17th, the reset, the queue, the poll, the registry (fix round 3)
test('the 17th terminal from term-open is refused as the list is: no row, no screen, no event', () => {
  const made = [], events = [];
  M.setHooks({ made: (id) => made.push(id), event: (ev) => events.push(ev) });
  M.applyList(Array.from({ length: 16 }, (_, i) => row({ id: 't' + (i + 1) })));
  made.length = 0;
  M.onFrame({ type: 'term-open', id: 't17', term: row({ id: 't17', by: 'agent', chat: 'c7' }) });
  assert.equal(M.roster.peek().length, M.MAX_TERMS, 'the table stays at 16');
  assert.equal(M.live.has('t17'), false, 'no entry for the 17th');
  assert.deepEqual(made, [], 'so no screen is made for it');
  assert.deepEqual(events, [], 'and it is not said');
});

test('a reset writes the RIS sequence first, then its bytes: the snapshot paints over a clear screen', () => {
  const writes = [];
  const e = { id: 't20', term: { write: (b) => writes.push(b) }, fit: null, host: null, pend: null, seq: 0, queue: [], queued: 0, backlog: 0, loading: false, resyncing: false, follow: false };
  M.live.set('t20', e);
  const snap = bytesOf('snap');
  V.push(e, snap, true);
  assert.deepEqual(writes, ['\x1bc', snap]);
});

test('a reset that arrives while the screen is still over 2 MiB does not resync again; its own bytes clear the mark once parsed', () => {
  const pending = [];
  const e = {
    id: 't21', term: { write: (bytes, cb) => { if (cb) pending.push(cb); } }, fit: null, host: null, pend: null, seq: 9,
    queue: [], queued: 0, backlog: 0, loading: false, resyncing: false, follow: false,
  };
  M.live.set('t21', e);
  const MB = new Uint8Array(1024 * 1024);
  for (let i = 0; i < 3; i++) V.push(e, MB, false);
  assert.equal(e.seq, undefined, 'past 2 MiB: resynced');
  assert.equal(e.resyncing, true);
  e.seq = 12;
  VIEW.reset('t21', bytesOf('snap'));   // the stream's reset hook, as view.js holds it
  assert.equal(e.seq, 12, 'the reset does not forget the seq again');
  assert.equal(e.resyncing, true, 'the mark stands while the backlog it waits behind is unparsed');
  while (pending.length) pending.shift()();
  assert.equal(e.resyncing, false, 'the reset\'s own bytes are parsed: the mark clears');
  assert.equal(e.backlog, 0);
});

test('output queued before a screen exists is bounded: past 2 MiB the queue is dropped and the terminal resynced; the reset starts it again', () => {
  const e = { id: 't22', term: null, fit: null, host: null, pend: null, seq: 5, queue: [], queued: 0, backlog: 0, loading: false, resyncing: false, follow: false };
  M.live.set('t22', e);
  const MB = new Uint8Array(1024 * 1024);
  V.push(e, MB, false);
  V.push(e, MB, false);
  assert.equal(e.queue.length, 2, 'two MiB is the bound, not past it');
  V.push(e, new Uint8Array(1), false);
  assert.equal(e.queue.length, 0, 'past it: the queue is dropped');
  assert.equal(e.seq, undefined, 'and the terminal is resynced');
  assert.equal(e.resyncing, true);
  V.push(e, MB, false);
  assert.equal(e.queue.length, 0, 'bytes that come while the resync is on its way are not kept');
  V.push(e, bytesOf('snap'), true);
  assert.equal(e.queue.length, 1, 'the reset starts the queue again');
  assert.equal(e.queue[0].reset, true);
  assert.equal(e.resyncing, false);
  V.push(e, bytesOf('after'), false);
  assert.equal(e.queue.length, 2, 'and what follows the reset is kept');
});

test('a reset bigger than the bound is a snapshot, not backlog: it resyncs nothing, on a screen or in the queue before one', () => {
  const pending = [];
  const on = { id: 't23', term: { write: (b, cb) => { if (cb) pending.push(cb); } }, fit: null, host: null, pend: null, seq: 4, queue: [], queued: 0, backlog: 0, loading: false, resyncing: false, follow: false };
  M.live.set('t23', on);
  V.push(on, new Uint8Array(3 * 1024 * 1024), true);
  assert.equal(on.seq, 4, 'no resync for a reset, however big');
  assert.equal(on.resyncing, false);
  while (pending.length) pending.shift()();
  assert.equal(on.backlog, 0, 'and the backlog balances: a reset counts nothing either way');
  const queued = { id: 't24', term: null, fit: null, host: null, pend: null, seq: 4, queue: [], queued: 0, backlog: 0, loading: false, resyncing: false, follow: false };
  M.live.set('t24', queued);
  V.push(queued, new Uint8Array(3 * 1024 * 1024), true);
  assert.equal(queued.seq, 4, 'the same before a screen exists');
  assert.equal(queued.queue.length, 1, 'the reset is kept for the screen');
});

test('the roster poll: the first answer is a baseline (an agent terminal already there says nothing), and the next says a new agent terminal once', async () => {
  await modeAs('perch');
  A._resetPoll();
  const toasts = [], asked = [];
  const answers = [
    [row({ id: 't93', by: 'agent', chat: 'c4' }), row({ id: 't94' })],
    [row({ id: 't93', by: 'agent', chat: 'c4' }), row({ id: 't94' }), row({ id: 't95', by: 'agent', chat: 'c5' })],
  ];
  host.toast = (text) => toasts.push(text);
  A.tabShown.value = false;
  A.badge.value = false;
  globalThis.fetch = async (url) => { asked.push(url); return { status: 200, text: async () => JSON.stringify(answers.shift()) }; };
  await A.poll();
  assert.deepEqual(M.roster.peek().map((r) => r.id), ['t93', 't94'], 'the list is applied with no stream running');
  assert.deepEqual(toasts, [], 'the first look is a baseline: nothing already there is news');
  await A.poll();
  assert.equal(toasts.length, 1, 'the new agent terminal is said once');
  assert.match(toasts[0], /^agent in c5 opened t95$/);
  assert.equal(A.badge.peek(), true);
  assert.deepEqual(asked, ['/hub/term', '/hub/term']);
});

test('the roster poll asks nothing in door mode, and nothing while the terminal stream is up', async () => {
  const asked = [];
  await modeAs('door');
  globalThis.fetch = async (url) => { asked.push(url); return { status: 200, text: async () => '[]' }; };
  await A.poll();
  assert.deepEqual(asked, [], 'door mode: the perch is not asked for its terminals');
  await modeAs('perch');
  host.chatStreams = () => 0;
  globalThis.fetch = (url) => {
    asked.push(url);
    return String(url).startsWith('/hub/term/events') ? new Promise(() => {}) : Promise.resolve({ status: 200, text: async () => '[]' });
  };
  assert.equal(M.startStream(), true);
  asked.length = 0;
  await A.poll();
  assert.deepEqual(asked, [], 'the stream is up: it says the roster, and the poll does not ask');
});

test('a poll answered after the terminal stream opened changes nothing and says nothing: the stream\'s roster stands', async () => {
  await modeAs('perch');
  host.chatStreams = () => 0;
  const toasts = [];
  host.toast = (text) => toasts.push(text);
  A._resetPoll();
  globalThis.fetch = async () => ({ status: 200, text: async () => JSON.stringify([row({ id: 't97' })]) });
  await A.poll();   // the first look: the baseline
  let answer = null;
  globalThis.fetch = (url) => (String(url).startsWith('/hub/term/events') ? new Promise(() => {})
    : new Promise((res) => { answer = res; }));
  const p = A.poll();   // its request goes out and waits
  assert.equal(M.startStream(), true, 'the stream opens while the poll waits');
  M.applyList([row({ id: 't97' }), row({ id: 't98', by: 'agent', chat: 'c6' })]);   // the stream's own roster
  answer({ status: 200, text: async () => JSON.stringify([row({ id: 't97' }), row({ id: 't99', by: 'agent', chat: 'c6' })]) });
  await p;
  assert.deepEqual(M.roster.peek().map((r) => r.id), ['t97', 't98'], 'the late answer is not applied over the stream\'s list');
  assert.deepEqual(toasts, [], 'and the poll says nothing while the stream is up');
  M.stopStream();
});

test('the folder registers at import: the frame tap, the ⌨ tab on the right with the activity badge, and the poll started at boot', async () => {
  await import('../terms/index.js');
  assert.ok(frameTaps.peek().includes(A.tap), 'the frame tap is in the registry');
  const tab = rightTabs.get('terms');
  assert.ok(tab, 'the ⌨ tab is registered on the right');
  assert.equal(tab.badge, A.badge, 'with the activity badge');
  assert.equal(tab.global, true);
  assert.match(src('../terms/index.js'), /^frameTaps\.add\(tap\);$/m, 'the tap is added at import');
  assert.match(src('../terms/index.js'), /^startActivity\(\);$/m, 'the poll starts at import (node has no document, so it does nothing here)');
});

// ---------------------------------------------------------------- the sources: what only the text can hold
test('the agent\'s last input line is in every header: the fold does not hide it', () => {
  const tab = src('../terms/tab.js');
  const line = tab.match(/\$\{line && html`<span class="tline"[^`]*`\}/);
  assert.ok(line, 'the header renders the line, when there is one');
  assert.doesNotMatch(line[0], /open/, 'the fold does not gate it');
  assert.doesNotMatch(tab, /open && line/, 'no block row under the header that only an open terminal shows');
});

test('no page source writes HTML from a string, evaluates a string, or sets a colour as a literal', () => {
  for (const [f, s] of termsSrc()) {
    assert.doesNotMatch(s, /innerHTML|insertAdjacentHTML|dangerouslySetInnerHTML|\beval\(|new Function/, f);
    assert.doesNotMatch(s, /['"`]#[0-9a-fA-F]{3,8}['"`]/, f + ': a colour literal (take it from --base0X)');
  }
});

test('the one window.open is the link handler, behind the http and https check; no other file opens a window', () => {
  const opens = termsSrc().flatMap(([f, s]) => s.split('\n').filter((l) => /window\.open\(/.test(l)).map((l) => [f, l.trim()]));
  assert.equal(opens.length, 1, JSON.stringify(opens));
  assert.equal(opens[0][0], 'view.js');
  assert.match(src('../terms/view.js'), /const u = linkUrl\(text\);[\s\S]*if \(!u\)[\s\S]*window\.open\(u,/, 'the http check comes first');
});

test('the input is a raw POST that never enters the lane, and the lane has one poll: the roster', () => {
  const m = src('../terms/model.js');
  assert.match(m, /application\/octet-stream/);
  assert.equal((m.match(/hubPoll\(/g) || []).length, 1, 'the only lane call is the roster poll');
  assert.doesNotMatch(m, /hubPost\(/, 'input is not sent as JSON');
  assert.doesNotMatch(m, /document\.|window\./, 'the model touches no DOM');
});

test('the model and the activity folder build no URL from a raw string: the input path is /hub/term/<id>/input with a checked id', () => {
  const m = src('../terms/model.js');
  assert.match(m, /if \(!TERM_ID\.test\(id\)\) return 0;/);
  assert.match(m, /\/hub\/term\/\$\{id\}\/input/);
  assert.match(m, /\/hub\/term\/events/);
});

// The ⌨ tab's handlers run in the browser, which node cannot reach; each line below is the text of one wiring, so a change to it
// (a hold flipped, a pane not named, a wheel not captured, a save dropped, a freeze unwired, a line removed) turns its test red.
const TAB_WIRING = [
  ['take over and hand back send the flipped hold', /on=\$\{\(\) => setHold\(id, !row\.hold\)\}/],
  ['the pop-out opens a pane that names its terminal', /openXPane\('term', \{ term: id \}\)/],
  ['the pane link focuses that pane', /focusPane\(pane\.id\)/],
  ['the wheel listener is capture and not passive', /\{ capture: true, passive: false \}/],
  ['the wheel stops its default, stops propagation and scrolls the stack', /const wheel = \(e\) => \{ e\.preventDefault\(\); e\.stopPropagation\(\); el\.scrollTop \+= e\.deltaY; \};/],
  ['a new terminal sends the inspected chat, and nothing without one', /const body = chat \? \{ chat \} : \{\};/],
  ['a fold is saved as it is set', /M\.saveFold\(next\)/],
  ['a focus inside the stack freezes the order', /querySelector\(':focus'\)/],
  ['the order is recomputed through reorderDue with the freeze', /M\.reorderDue\(Date\.now\(\), lastOrder\.current, frozen\.current\)/],
  ['the yolo line says what it does', /yolo: agents type into these terminals without asking\./],
  ['a new terminal is said opened only on a 201 with an id', /r\.status === 201 && r\.data && typeof r\.data\.id === 'string'/],
];
for (const [what, re] of TAB_WIRING) {
  test('the ⌨ tab wiring: ' + what, () => {
    assert.match(src('../terms/tab.js'), re);
  });
}
