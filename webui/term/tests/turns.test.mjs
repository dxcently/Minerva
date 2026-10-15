// The chat's turn graph (graph/turns.js): a session's rows as turns, the calls, asks and verdicts on each, and the drawing of them. The
// session is the page's own Session (core/state.js), fed the frames a door sends, with its stream stubbed, so the verdict rule is tested where
// it is applied (verdict() in core/state.js), not re-written here. Pure: no DOM; the tree is read as vnodes. A source scan holds that the
// graph reads its session and nothing else.
import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { layout } from '../graph/draw.js';

globalThis.matchMedia = () => ({ matches: false, addEventListener() {}, addListener() {} });
const { Session } = await import('../core/state.js');
const T = await import('../graph/turns.js');

// No stream: a session is fed by hand. Its run pulse is a timer, so each test that sets running closes its session.
Session.prototype.run = function run() {};

// As LF: a checkout with core.autocrlf has CRLF sources.
const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
// A source file's code: whole-line comments and import lines left out.
const code = (rel) => src(rel).split('\n').filter((l) => !/^\s*\/\//.test(l) && !l.startsWith('import ')).join('\n');

// ---------------------------------------------------------------- a chat: the page's own session, fed the frames a door sends
const open = [];
let made = 0;
function chat(...frames) {
  const s = new Session('chat' + ++made, { toast() {} });
  open.push(s);
  for (const f of [{ type: 'hello', protocol: 1, pending: 0 }, { type: 'caught-up' }, ...frames]) s.frame(f);
  s.flush();
  return s;
}
afterEach(() => { for (const s of open.splice(0)) s.close(); });

const you = (record, text) => ({ type: 'user-message', record, text, images: 0 });
const start = (id, name, input = {}) => ({ type: 'tool-call-started', id, name, input, origin: 'model' });
const end = (id, name, is_error = false) => ({ type: 'tool-call-finished', id, name, output: 'ok', is_error });
const verdict = (tool, outcome, reason = 'outside-cwd') => ({ type: 'policy-verdict', tool, outcome, reason });
const settle = () => ({ type: 'turn-settled', stop_reason: 'end-turn', usage: { input_tokens: 100, output_tokens: 10 } });
const ask = (ask_id, call_id, tool, prompt) => ({ type: 'ask', kind: 'approval', ask_id, call_id, tool, input: {}, prompt, answers: ['yes', 'no'] });
const answered = (ask_id) => ({ type: 'ask-settled', ask_id, how: 'answered', answer: 'yes' });
const running = (on) => ({ type: 'turn-state', running: on });

const turnsOf = (s) => T.turnsOf(T.itemsOf(s), s.running.value);
const drawn = (s) => T.graphOf(turnsOf(s));
const pathsOf = (g) => g.graph.states.map((x) => x.path);
const stateOf = (g, path) => g.graph.states.find((x) => x.path === path);
const labelOf = (g, path) => stateOf(g, path).label;

// ---------------------------------------------------------------- items for the pure part (no session)
const call = (id, name = 'bash', st = 'ok') => ({ id, name, st, verdict: null, ask: null });
const run = (n, calls) => [{ kind: 'you', text: 'message ' + n }, ...calls.map((c) => ({ kind: 'tool', call: c })), { kind: 'settle', text: 'end turn' }];

// Every string a vnode tree holds: text children and string props (a title's words are text children).
function words(v, out = []) {
  if (v == null || typeof v === 'boolean') return out;
  if (typeof v !== 'object') { out.push(String(v)); return out; }
  if (Array.isArray(v)) { v.forEach((x) => words(x, out)); return out; }
  for (const [k, x] of Object.entries(v.props || {})) {
    if (k === 'children') words(x, out);
    else if (typeof x === 'string') out.push(x);
  }
  return out;
}

// ---------------------------------------------------------------- turns
test('a turn is what the model did between two settles, and an open last turn counts', () => {
  const s = chat(you(1, 'fix the parser'), start('c1', 'bash'), end('c1', 'bash'), settle(),
    you(2, 'now the tests'), start('c2', 'read'), end('c2', 'read'), settle(),
    you(3, 'and then'), start('c3', 'bash'));
  const t = turnsOf(s);
  assert.equal(t.length, 3);
  assert.deepEqual(t.map((x) => x.label), ['fix the parser', 'now the tests', 'and then']);
  assert.deepEqual(t.map((x) => x.open), [false, false, true]);
  assert.match(t[0].settle, /end turn/);
});

test('calls are grouped per turn, in the order they started', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), start('c2', 'read'), end('c1', 'bash'), end('c2', 'read'), settle(),
    you(2, 'b'), start('c3', 'grep'), end('c3', 'grep'), settle());
  const t = turnsOf(s);
  assert.deepEqual(t[0].calls.map((c) => c.id), ['c1', 'c2']);
  assert.deepEqual(t[1].calls.map((c) => c.id), ['c3']);
});

test('a line that is no turn of its own (a settle, a sys line) makes no turn', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), settle(), { type: 'turn-budget', calls_left: 2 }, settle(),
    you(2, 'b'), start('c2', 'bash'), settle());
  assert.equal(turnsOf(s).length, 2);
});

test('a model message with no user row is a turn of its own: a resumed turn after a trigger is drawn as one', () => {
  const s = chat(you(1, 'build it'), start('c1', 'wait_for'), end('c1', 'wait_for'), settle(),
    { type: 'trigger-fired', call_id: 'c1', condition: 'build done', outcome: 'fired', record: 2 },
    { type: 'assistant-message', text: 'The build finished.', thinking: '', redacted: 0, record: 3 }, settle());
  const t = turnsOf(s);
  assert.equal(t.length, 2);
  assert.equal(t[1].label, '');
  assert.equal(t[1].open, false);
  const g = drawn(s);
  assert.equal(labelOf(g, 'turn 2'), '#2 turn');
  assert.ok(g.marks.visited.includes('turn 2'));
  assert.equal(T.captionOf(t), '2 turns · 1 call');
});

test('a model message with no words (a thinking-only one) makes no turn', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), end('c1', 'bash'), settle(),
    { type: 'assistant-message', text: '', thinking: 'hm', redacted: 0, record: 2 }, settle());
  assert.equal(turnsOf(s).length, 1);
});

test('a cancelled turn is closed by its stop: not open, not running, and its calls read stopped', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), { type: 'cancelled', usage: {}, calls: 1 });
  const t = turnsOf(s);
  assert.equal(t.length, 1);
  assert.equal(t[0].open, false);
  assert.equal(t[0].settle, 'stopped after 1 call');
  assert.equal(t[0].current, false);
  const g = drawn(s);
  assert.equal(stateOf(g, 'turn 1 call 1').description, 'bash · stopped');
  assert.ok(!g.marks.current.length);
});

test('a stop closes its turn: message, call, stop, message, call is two turns, the first one stopped', () => {
  const s = chat(you(1, 'first'), start('c1', 'bash'), { type: 'cancelled', usage: {}, calls: 1 }, you(2, 'second'), start('c2', 'read'), end('c2', 'read'), settle());
  const t = turnsOf(s);
  assert.equal(t.length, 2);
  assert.deepEqual(t.map((x) => x.label), ['first', 'second']);
  assert.deepEqual(t[0].calls.map((c) => [c.id, c.st]), [['c1', 'stop']]);
  assert.deepEqual(t[1].calls.map((c) => c.id), ['c2']);
  assert.equal(t[0].open, false);
  assert.equal(t[1].open, false);
  const g = drawn(s);
  assert.equal(labelOf(g, 'turn 2'), '#2 second');
  assert.equal(stateOf(g, 'turn 1 call 1').description, 'bash · stopped');
  assert.ok(!g.marks.current.length);
});

test('the newest turn is current only while the session says it is running', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), running(true));
  assert.equal(turnsOf(s)[0].current, true);
  const g = drawn(s);
  assert.ok(g.marks.current.includes('turn 1'));
  assert.ok(g.marks.current.includes('turn 1 call 1'));
  s.close();
});

// ---------------------------------------------------------------- the verdict rule, through the page's session
test('a verdict on the only open call of its name is written on that call, not drawn twice', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), verdict('bash', 'judged'), end('c1', 'bash'), settle());
  const [t] = turnsOf(s);
  assert.equal(t.calls[0].verdict.outcome, 'judged');
  assert.equal(t.verdicts.length, 0);
  const g = drawn(s);
  assert.equal(labelOf(g, 'turn 1 call 1'), 'bash · judged');
  assert.ok(!pathsOf(g).some((p) => p.includes('verdict')));
});

test('the ambiguous case: two open calls of one name, so the verdict is a box of its own', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), start('c2', 'bash'), verdict('bash', 'refused'), end('c1', 'bash'), end('c2', 'bash'), settle());
  const [t] = turnsOf(s);
  assert.ok(t.calls.every((c) => c.verdict === null));
  assert.equal(t.verdicts.length, 1);
  const g = drawn(s);
  assert.equal(labelOf(g, 'turn 1 verdict 1'), 'refused: bash');
  assert.ok(g.marks.failed.includes('turn 1 verdict 1'));
  assert.equal(labelOf(g, 'turn 1 call 1'), 'bash');
});

test('a second verdict on a call that already holds one is a box of its own, not a second write on that call', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), verdict('bash', 'judged'), verdict('bash', 'refused'), end('c1', 'bash'), settle());
  const [t] = turnsOf(s);
  assert.equal(t.calls[0].verdict.outcome, 'judged');
  assert.equal(t.verdicts.length, 1);
  assert.equal(t.verdicts[0].outcome, 'refused');
  const g = drawn(s);
  assert.equal(labelOf(g, 'turn 1 call 1'), 'bash · judged');
  assert.equal(labelOf(g, 'turn 1 verdict 1'), 'refused: bash');
  assert.ok(g.marks.failed.includes('turn 1 verdict 1'));
});

// Upstream gap 14: core/state.js counts every open call of the name before it attaches (state.js:755), so with two open calls of the name, one
// holding a verdict, the next verdict is a box of its own and does not land on the other call.
test('a verdict is not written on a call when another open call of its name already holds one', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), verdict('bash', 'judged'), start('c2', 'bash'), verdict('bash', 'refused'), end('c1', 'bash'), end('c2', 'bash'), settle());
  const [t] = turnsOf(s);
  assert.equal(t.calls.find((c) => c.id === 'c1').verdict.outcome, 'judged');
  assert.equal(t.calls.find((c) => c.id === 'c2').verdict, null);
  assert.equal(t.verdicts.length, 1);
  const g = drawn(s);
  assert.equal(labelOf(g, 'turn 1 verdict 1'), 'refused: bash');
  assert.ok(g.marks.failed.includes('turn 1 verdict 1'));
  assert.equal(labelOf(g, 'turn 1 call 2'), 'bash');
});

test('a verdict that arrives after its call finished has no open call to go to: its own box', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), end('c1', 'bash'), verdict('bash', 'judged'), settle());
  const [t] = turnsOf(s);
  assert.equal(t.calls[0].verdict, null);
  assert.equal(t.verdicts.length, 1);
  assert.equal(labelOf(drawn(s), 'turn 1 verdict 1'), 'judged: bash');
});

test('an open call of another name does not take the verdict', () => {
  const s = chat(you(1, 'a'), start('c1', 'read'), start('c2', 'bash'), verdict('bash', 'judged'), end('c1', 'read'), end('c2', 'bash'), settle());
  const [t] = turnsOf(s);
  assert.equal(t.calls.find((c) => c.id === 'c1').verdict, null);
  assert.equal(t.calls.find((c) => c.id === 'c2').verdict.outcome, 'judged');
  assert.equal(t.verdicts.length, 0);
});

test('a refused or declined verdict on a call marks the call failed, though the call itself finished ok', () => {
  for (const outcome of ['refused', 'declined']) {
    const g = drawn(chat(you(1, 'a'), start('c1', 'bash'), verdict('bash', outcome), end('c1', 'bash'), settle()));
    assert.ok(g.marks.failed.includes('turn 1 call 1'), outcome);
    assert.equal(labelOf(g, 'turn 1 call 1'), `bash · ${outcome}`);
    assert.equal(stateOf(g, 'turn 1 call 1').description.split('\n')[0], `bash · ${outcome}`);
  }
  const judged = drawn(chat(you(1, 'a'), start('c1', 'bash'), verdict('bash', 'judged'), end('c1', 'bash'), settle()));
  assert.ok(!judged.marks.failed.includes('turn 1 call 1'));
  assert.ok(judged.marks.visited.includes('turn 1 call 1'));
});

// ---------------------------------------------------------------- asks
test('an ask rides on its call while it waits, and reads answered once it is settled', () => {
  const waiting = chat(you(1, 'a'), start('c1', 'bash'), ask(1, 'c1', 'bash', 'bash. Run it?'), running(true));
  assert.equal(turnsOf(waiting)[0].calls[0].ask.how, 'open');
  const g = drawn(waiting);
  assert.equal(labelOf(g, 'turn 1 call 1 ask'), 'ask: bash. Run it?');
  assert.ok(g.marks.current.includes('turn 1 call 1 ask'));

  const done = chat(you(1, 'a'), start('c1', 'bash'), ask(1, 'c1', 'bash', 'bash. Run it?'), answered(1), end('c1', 'bash'), settle());
  assert.equal(turnsOf(done)[0].calls[0].ask.how, 'answered');
  const gd = drawn(done);
  assert.ok(gd.marks.visited.includes('turn 1 call 1 ask'));
  assert.ok(!gd.marks.current.includes('turn 1 call 1 ask'));
});

test('an ask with no call is a box of its turn', () => {
  const s = chat(you(1, 'a'), { type: 'ask', kind: 'question', ask_id: 7, prompt: 'Which file?', options: [{ label: 'a.txt' }, { label: 'b.txt' }] });
  const t = turnsOf(s);
  assert.equal(t[0].asks.length, 1);
  assert.equal(labelOf(drawn(s), 'turn 1 ask 1'), 'ask: Which file?');
});

// A turn that never settled and is not running (a saved chat, or one whose run is unknown) is not current: its open call reads not finished
// and its open asks are not current, as the turn box beside them says not settled.
test('a turn that never settled and is not running: its open call reads not finished, and nothing in it is current', () => {
  const dead = chat(you(1, 'a'), start('c1', 'bash'));
  assert.equal(turnsOf(dead)[0].current, false);
  const g = drawn(dead);
  assert.equal(stateOf(g, 'turn 1').description.split('\n')[1], '1 call · not settled');
  assert.equal(stateOf(g, 'turn 1 call 1').description, 'bash · not finished');
  assert.deepEqual(g.marks.current, []);

  const asked = chat(you(1, 'a'), start('c1', 'bash'), ask(1, 'c1', 'bash', 'bash. Run it?'),
    { type: 'ask', kind: 'question', ask_id: 2, prompt: 'Which file?', options: [{ label: 'a.txt' }, { label: 'b.txt' }] });
  const ga = drawn(asked);
  assert.equal(stateOf(ga, 'turn 1 call 1').description, 'bash · not finished');
  assert.equal(stateOf(ga, 'turn 1 call 1 ask').description, 'bash. Run it?\nopen');
  assert.equal(stateOf(ga, 'turn 1 ask 1').description, 'Which file?\nopen');
  assert.deepEqual(ga.marks.current, []);
  assert.ok(ga.marks.visited.includes('turn 1 call 1 ask') && ga.marks.visited.includes('turn 1 ask 1'));

  // The same open call in a running turn still reads running and is current: the fix does not reach a live call.
  const live = chat(you(1, 'a'), start('c1', 'bash'), running(true));
  assert.equal(stateOf(drawn(live), 'turn 1 call 1').description, 'bash · running');
});

// A settled ask leaves the queue as cancelled, or as answered elsewhere when a 409 takes it (state.js send()), and reads so on the call or
// the turn. A dropped ask is never current, whether its turn runs or not.
test('an ask dropped cancelled reads cancelled, one dropped elsewhere reads answered elsewhere, and neither is current', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), ask(1, 'c1', 'bash', 'bash. Run it?'),
    { type: 'ask', kind: 'question', ask_id: 2, prompt: 'Which file?', options: [{ label: 'a.txt' }, { label: 'b.txt' }] }, running(true));
  s.frame({ type: 'ask-settled', ask_id: 1, how: 'cancelled' });
  s.dropAsk(s.asks.value.find((a) => a.id === 2), { how: 'elsewhere' });
  s.flush();
  const g = drawn(s);
  assert.equal(stateOf(g, 'turn 1 call 1 ask').description, 'bash. Run it?\ncancelled');
  assert.equal(stateOf(g, 'turn 1 ask 1').description, 'Which file?\nanswered elsewhere');
  assert.ok(g.marks.visited.includes('turn 1 call 1 ask') && g.marks.visited.includes('turn 1 ask 1'));
  assert.ok(!g.marks.current.includes('turn 1 call 1 ask') && !g.marks.current.includes('turn 1 ask 1'));
});

// ---------------------------------------------------------------- caps and the caption
test('the newest 20 turns are drawn, with the rest as one box; the newest 10 calls of a turn, with the rest as one box', () => {
  const items = [];
  for (let i = 1; i <= 25; i++) items.push(...run(i, [call('c' + i)]));
  const g = T.graphOf(T.turnsOf(items));
  assert.equal(g.graph.states.filter((x) => x.kind === 'turn').length, T.TURN_CAP);
  assert.equal(stateOf(g, 'earlier turns').label, `${25 - T.TURN_CAP} earlier turns`);
  assert.ok(g.graph.edges.some((e) => e.from === 'earlier turns' && e.to === 'turn 6' && e.kind === 'then'));
  assert.ok(!pathsOf(g).includes('turn 5'));
  assert.equal(stateOf(g, 'earlier turns').description, 'Not drawn: the newest 20 of 25 turns are drawn.');

  const many = [];
  for (let k = 1; k <= 13; k++) many.push(call('k' + k));
  const h = T.graphOf(T.turnsOf(run(1, many)));
  assert.equal(labelOf(h, 'turn 1 earlier calls'), '+3 earlier calls');
  assert.equal(stateOf(h, 'turn 1 earlier calls').description, 'Not drawn: the newest 10 of 13 calls are drawn.');
  assert.equal(h.graph.states.filter((x) => x.kind === 'call').length, T.CALL_CAP);
  assert.ok(pathsOf(h).includes('turn 1 call 4') && pathsOf(h).includes('turn 1 call 13'));
  assert.ok(!pathsOf(h).includes('turn 1 call 3'));
});

test('asks and verdicts on no call are capped like calls: the newest 10 of each, one box for the rest, the asks then the verdicts in one chain from the turn\'s last box', () => {
  const items = [{ kind: 'you', text: 'a' }];
  for (let k = 1; k <= 13; k++) items.push({ kind: 'verdict', f: { tool: 'bash', outcome: 'judged', reason: 'r' + k } });
  for (let k = 1; k <= 12; k++) items.push({ kind: 'ask', ask: { id: 'q' + k, prompt: 'Q' + k, how: 'answered' } });
  items.push({ kind: 'settle', text: 'end turn' });
  const g = T.graphOf(T.turnsOf(items));
  assert.equal(labelOf(g, 'turn 1 earlier asks'), '+2 earlier asks');
  assert.equal(stateOf(g, 'turn 1 earlier asks').description, 'Not drawn: the newest 10 of 12 asks are drawn.');
  assert.equal(g.graph.states.filter((x) => /^turn 1 ask \d+$/.test(x.path)).length, T.CALL_CAP);
  assert.ok(pathsOf(g).includes('turn 1 ask 3') && !pathsOf(g).includes('turn 1 ask 2'));
  assert.equal(labelOf(g, 'turn 1 earlier verdicts'), '+3 earlier verdicts');
  assert.equal(stateOf(g, 'turn 1 earlier verdicts').description, 'Not drawn: the newest 10 of 13 verdicts are drawn.');
  assert.equal(g.graph.states.filter((x) => /^turn 1 verdict \d+$/.test(x.path)).length, T.CALL_CAP);
  assert.ok(pathsOf(g).includes('turn 1 verdict 4') && pathsOf(g).includes('turn 1 verdict 13'));
  assert.ok(!pathsOf(g).includes('turn 1 verdict 3'));
  const edge = (from, to) => g.graph.edges.find((e) => e.from === from && e.to === to);
  assert.equal(edge('turn 1', 'turn 1 earlier asks').kind, 'ask');
  assert.ok(edge('turn 1 earlier asks', 'turn 1 ask 3'));
  assert.equal(edge('turn 1 ask 12', 'turn 1 earlier verdicts').kind, 'verdict');
  assert.ok(edge('turn 1 earlier verdicts', 'turn 1 verdict 4'));
  assert.ok(edge('turn 1 verdict 12', 'turn 1 verdict 13'));
  assert.equal(edge('turn 1', 'turn 1 earlier verdicts'), undefined);
  assert.equal(edge('turn 1', 'turn 1 verdict 13'), undefined);
});

test('a turn\'s calls run as one chain from the turn box, an ask sits in that chain under its call, and the next turn hangs off the last box', () => {
  const items = [...run(1, [call('a1'), { ...call('a2'), ask: { id: 'q', prompt: 'Run it?', how: 'open' } }, call('a3')]), ...run(2, [call('b1')])];
  const g = T.graphOf(T.turnsOf(items));
  const edge = (from, to) => g.graph.edges.find((e) => e.from === from && e.to === to);
  assert.equal(edge('turn 1', 'turn 1 call 1').kind, 'call');
  assert.ok(edge('turn 1 call 1', 'turn 1 call 2'));
  assert.equal(edge('turn 1 call 2', 'turn 1 call 2 ask').kind, 'ask');
  assert.equal(edge('turn 1 call 2 ask', 'turn 1 call 3').kind, 'call');
  assert.equal(edge('turn 1 call 2', 'turn 1 call 3'), undefined);
  assert.equal(edge('turn 1 call 3', 'turn 2').kind, 'then');
  assert.equal(edge('turn 1', 'turn 2'), undefined);
  assert.equal(edge('turn 1', 'turn 1 call 2'), undefined);
  assert.equal(edge('turn 1', 'turn 1 call 2 ask'), undefined);
});

test('a long chat is a column: 10 turns of 2 calls lay out narrow and tall, not fanned out sideways', () => {
  const items = [];
  for (let i = 1; i <= 10; i++) items.push(...run(i, [call('a' + i), call('b' + i)]));
  const g = T.graphOf(T.turnsOf(items));
  const lay = layout(g.graph.states, g.graph.edges);
  assert.ok(!lay.fallback, 'dagre threw: ' + lay.error);
  assert.ok(lay.width < 300, 'width ' + lay.width);
  assert.ok(lay.height > 3 * lay.width, `height ${lay.height} width ${lay.width}`);
});

// The inspector's content width at the default font: 34 cells of 8 px (app.js SIDES.r, theme.css), less 8 px of padding each side (term.css).
// A drawing wider than that is scaled down to fit (graph.css: max-width 100%), its text with it, so a chat is drawn at full size only within it.
const INSPECTOR_UNITS = 34 * 8 - 2 * 8;

test('a three-turn chat with calls, a loose verdict and asks is drawn no wider than the inspector at the default font', () => {
  const s = chat(you(1, 'fix the parser'), start('c1', 'bash'), verdict('bash', 'judged'), end('c1', 'bash'), start('c2', 'read'), end('c2', 'read'), settle(),
    you(2, 'now the tests'), start('c3', 'bash'), start('c4', 'bash'), verdict('bash', 'refused'), end('c3', 'bash', true), end('c4', 'bash'), settle(),
    you(3, 'and then the release'), start('c5', 'bash'), ask(1, 'c5', 'bash', 'bash. Push it?'),
    { type: 'ask', kind: 'question', ask_id: 2, prompt: 'Which branch?', options: [{ label: 'main' }, { label: 'dev' }] }, running(true));
  const g = drawn(s);
  assert.equal(labelOf(g, 'turn 2 verdict 1'), 'refused: bash');
  assert.equal(labelOf(g, 'turn 3 ask 1'), 'ask: Which branch?');
  const lay = layout(g.graph.states, g.graph.edges);
  assert.ok(!lay.fallback, 'dagre threw: ' + lay.error);
  assert.ok(lay.width <= INSPECTOR_UNITS, `width ${lay.width} is wider than the inspector's ${INSPECTOR_UNITS}`);
  s.close();
});

test('the caption says what there is, that the newest turn runs, and that old turns are cut', () => {
  const s = chat(you(1, 'a'), start('c1', 'bash'), running(true));
  assert.equal(T.captionOf(turnsOf(s)), '1 turn · 1 call · running');
  s.close();
  const items = [];
  for (let i = 1; i <= 25; i++) items.push(...run(i, []));
  assert.match(T.captionOf(T.turnsOf(items)), /the newest 20 are drawn$/);
});

// ---------------------------------------------------------------- the drawing
test('the drawing lays out under dagre: no fallback column, no error, a box per state', () => {
  const items = [];
  for (let i = 1; i <= 25; i++) items.push(...run(i, [call('c' + i, 'bash', 'err'), call('d' + i, 'read')]));
  const g = T.graphOf(T.turnsOf(items));
  const lay = layout(g.graph.states, g.graph.edges);
  assert.ok(!lay.fallback, 'dagre threw: ' + lay.error);
  assert.equal(lay.error, undefined);
  assert.equal(lay.nodes.length, g.graph.states.length);
  assert.ok(g.marks.failed.includes('turn 25 call 1'), 'the newest turn is drawn, and its failed call is marked');
});

test('the tree holds this chat\'s words and no other chat\'s, and says so when there is nothing', () => {
  const a = chat(you(1, 'fix the parser'), start('c1', 'bash'), end('c1', 'bash'), settle());
  const b = chat(you(1, 'write the notes'), start('d1', 'read'), end('d1', 'read'), settle());
  const wa = words(T.TurnGraph({ s: a })).join('\n'), wb = words(T.TurnGraph({ s: b })).join('\n');
  assert.ok(wa.includes('fix the parser') && !wa.includes('write the notes'));
  assert.ok(wb.includes('write the notes') && !wb.includes('fix the parser'));
  assert.ok(words(T.TurnGraph({ s: null })).join('').includes('no chat to draw'));
  const empty = chat();
  assert.ok(words(T.TurnGraph({ s: empty })).join('').includes('no turns yet'));
});

// ---------------------------------------------------------------- what a model, a tool or a page sent: cleaned, drawn as text
// A character a line can lie with: a control (a line break is the drawing's own), a zero-width mark or a bidi mark. The odd code points are
// built with cp(), not written out, so this file holds only what it says.
const cp = (...ns) => String.fromCodePoint(...ns);
const lies = (s) => [...s].some((ch) => {
  const c = ch.codePointAt(0);
  return (c < 0x20 && c !== 0x0a) || (c >= 0x7f && c <= 0x9f) || (c >= 0x200b && c <= 0x200f) || (c >= 0x2028 && c <= 0x202e) || (c >= 0x2060 && c <= 0x206f) || c === 0xfeff;
});

test('every string a model, a tool or a page sent is cleaned: no bidi mark, no zero-width mark, no control character is drawn', () => {
  const evil = 'ba' + cp(0x202e) + 'sh' + cp(0x07);
  const a = drawn(chat(you(1, 'fi' + cp(0x200b) + 'x it'), start('c1', evil), ask(1, 'c1', evil, 'Run' + cp(0x202e) + ' it?' + cp(0x07)), end('c1', evil), settle()));
  assert.equal(labelOf(a, 'turn 1'), '#1 fix it');
  assert.equal(labelOf(a, 'turn 1 call 1'), 'bash');
  assert.equal(labelOf(a, 'turn 1 call 1 ask'), 'ask: Run it?');
  const b = drawn(chat(you(1, 'a'), verdict('ba' + cp(0x202e) + 'sh', 'judged' + cp(0x07), 'out' + cp(0x202e) + 'side' + cp(0x07))));
  assert.equal(labelOf(b, 'turn 1 verdict 1'), 'judged: bash');
  assert.equal(stateOf(b, 'turn 1 verdict 1').description.split('\n')[1], 'outside');
  for (const g of [a, b]) {
    for (const st of g.graph.states) {
      assert.equal(lies(st.label), false, st.path + ' label');
      assert.equal(lies(st.description), false, st.path + ' description');
    }
  }
});

// ---------------------------------------------------------------- the source: it reads its session and nothing else
// What the source scan forbids: a read of the focus, the inspected chat or a hub call, the mode, a fetch or a post, the DOM, a global, a dynamic import.
const FORBIDDEN = /\bui\.|\bfocus|\binspected\b|\blastChat\b|\bhub[A-Z]\w*\(|\bmode\(|\bfetch\(|\bpost\(|innerHTML|\beval\(|\bwindow\.|\bdocument\.|globalThis|\bimport\(/;

test('the graph reads only the session it is handed: its imports are the ui, the drawing and the cleaning, and no global or hub call', () => {
  const body = code('../graph/turns.js');
  assert.deepEqual([...src('../graph/turns.js').matchAll(/^import .* from '(.*)';$/gm)].map((m) => m[1]), ['../core/ui.js', './draw.js', '../renderers/jev.js']);
  assert.doesNotMatch(body, FORBIDDEN);
  assert.match(body, /export function TurnGraph\(\{ s, sid \}\)/);
});

test('the source scan catches each thing it forbids when planted, and passes a plain read', () => {
  for (const bad of ['ui.focus', 'const p = focusedPane', 'if (inspected)', 'lastChat()', 'hubGet(x)', 'mode()', 'fetch(u)', 'post(u)', 'el.innerHTML = s',
    'eval(s)', 'window.x', 'document.body', 'globalThis.x', "await import('x')"]) assert.match(bad, FORBIDDEN, bad);
  assert.doesNotMatch('const turns = s.chunks.value.map(itemOf);', FORBIDDEN);
});
