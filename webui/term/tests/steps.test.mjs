// The steps of a run (graph/steps.js, park.js, context.js): the words of each frame over the real wiki-hop journals (tests/fixtures/jev, cases a to
// l), the verdict sentences, the arrival and action lines, the end of every kind of run, the inline ask and the reasons its buttons are off, jev's
// view (folded, with the ties after the cut flagged), the frames drawn (the last eight, `[ all N steps ]`) and what a click sends. A journal no
// fixture holds (two options that agree in their first 32 bytes) is built here, record by record. The components are drawn to a vnode tree and
// read back as text and as elements, with their K buttons clicked through K's own handler; none of them uses a hook. fetch is a stub; no DOM.
import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import * as S from '../graph/steps.js';
import * as P from '../graph/park.js';
import * as C from '../graph/context.js';
import * as R from '../graph/runs.js';
import { reduce } from '../graph/jevrun.js';

// As LF: a checkout with core.autocrlf has CRLF sources.
const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
// A source file's code: whole-line comments left out, so a file may say in a comment what it does not do.
const code = (rel) => read(rel).split('\n').filter((l) => !/^\s*\/\//.test(l)).join('\n');
const journal = (c) => read(`./fixtures/jev/${c}/journal.ndjson`).trim().split('\n').map((l) => JSON.parse(l));
const CASES = [...'abcdefghijkl'];
const V = Object.fromEntries(CASES.map((c) => [c, reduce(journal(c))]));
const step = (c, n) => V[c].steps[n - 1];
// case b as the log held it when its first invocation had ended: parked at step 2, the ask unanswered (the fixture's case.json says 30 records)
const PARKED_B = reduce(journal('b').slice(0, 30));

// ---------------------------------------------------------------- a tree read back
// A vnode as the text it draws. Components are called (none of these uses a hook), so the text is what a person would read.
function textOf(v) {
  if (v === null || v === undefined || typeof v === 'boolean') return '';
  if (typeof v === 'string' || typeof v === 'number') return String(v);
  if (Array.isArray(v)) return v.map(textOf).join('');
  if (typeof v.type === 'function') return textOf(v.type({ ...v.props }));
  return textOf(v.props && v.props.children);
}
// Every element the tree draws, in order, with its props and the text under it.
function elements(v, out = []) {
  if (v === null || v === undefined || typeof v === 'boolean' || typeof v === 'string' || typeof v === 'number') return out;
  if (Array.isArray(v)) { for (const x of v) elements(x, out); return out; }
  if (typeof v.type === 'function') return elements(v.type({ ...v.props }), out);
  out.push({ props: v.props || {}, text: textOf(v) });
  return elements(v.props && v.props.children, out);
}
// The one element whose text includes `label` and that is live (K gives it an onClick) or off (K gives it class `dis`, no onClick).
const live = (tree, label) => elements(tree).filter((e) => e.text.includes(label) && typeof e.props.onClick === 'function');
const off = (tree, label) => elements(tree).filter((e) => e.text.includes(label) && typeof e.props.class === 'string' && e.props.class.split(' ').includes('dis'));
// A click, as K's own handler takes it (it stops the event, then calls `on`).
const click = (el) => el.props.onClick({ stopPropagation() {}, preventDefault() {}, currentTarget: null, key: 'Enter' });

// ---------------------------------------------------------------- the stub perch: a click's answer, every request kept
let sent = [];
const settle = () => new Promise((r) => setImmediate(r)); // the click's request is in the air and answered
globalThis.fetch = async (url, init = {}) => {
  sent.push({ path: url.replace(/^\/hub\//, ''), method: init.method || 'GET', body: init.body });
  return { status: 202, text: async () => '' };
};
afterEach(() => { sent = []; });

// ---------------------------------------------------------------- the frame's words, over the real journals
test('the verdict of a chooser that was sure: its top, floor and margin, and that the scorer picked', () => {
  assert.equal(S.verdictOf(step('a', 2), false).text, 'top 0.90 ≥ floor 0.35 · margin 0.80 ≥ 0.05 → picked by the scorer');
  assert.equal(S.verdictOf(step('a', 2), false).cls, 'up');
});

test('the verdict of a chooser that was not sure asks a person: the margin under its bound, in the words the plan gives', () => {
  assert.equal(S.verdictOf(step('b', 2), false).text, 'top 0.50 ≥ floor 0.35 · margin 0.00 < 0.05 → asked a person');
  assert.match(S.verdictOf(step('i', 2), false).text, /^top 0\.02 < floor [\d.]+ · margin 0\.00 < [\d.]+ → asked a person$/, 'under the floor');
});

test('a rule pick and a single option say so, and a chooser that did not answer says why it has no verdict', () => {
  assert.equal(S.verdictOf(step('f', 4), false).text, 'picked by rule (label equals goal)');
  assert.equal(S.verdictOf(step('f', 2), false).text, 'only one option');
  assert.equal(S.verdictOf(step('d', 2), true).text, 'jev_choose did not answer before the run ended', 'the run ended: no answer is coming');
  assert.equal(S.verdictOf(step('d', 2), false).text, 'waiting for jev_choose', 'the run is still going');
  assert.equal(S.verdictOf(step('c', 2), false).text, 'chooser unavailable: stub: the model is not loaded → asked a person');
  assert.equal(S.verdictOf(step('e', 1), false), null, 'a step with no choice has no verdict');
});

test('the arrival check: the heading against the goal, and the entailment against the threshold of the run\'s graph', () => {
  assert.deepEqual(S.arrivalOf(step('b', 2).arrival).map((x) => x.text), ['heading Ancient Egypt ≠ Felidae', 'entail 0.10 < 0.80']);
  assert.deepEqual(S.arrivalOf(step('b', 4).arrival).map((x) => x.text), ['heading Felidae = Felidae'], 'arrived: no entailment was asked');
  assert.deepEqual(S.arrivalOf(step('g', 2).arrival).map((x) => x.text), ['heading Ancient Egypt ≠ Felidae', 'entail failed: stub: the entail model is not loaded']);
  assert.equal(S.arrivalOf(null).length, 0);
});

test('the action: a click names the link and where it went, and a failure or an unknown ref is said in words', () => {
  assert.equal(S.actionOf({ tool: 'browser_click', ref: 'e3', refOk: true, label: 'Nile River', result: { url: 'x', title: 'Nile — Wikipedia' }, error: null, pending: false }).text,
    'clicked "Nile River" → Nile — Wikipedia');
  assert.equal(S.actionOf(step('b', 2).action).text, 'clicked "Egypt" → https://en.wikipedia.org/wiki/Egypt', 'no title: the url');
  assert.equal(S.actionOf({ tool: 'browser_click', ref: 'e3', refOk: true, label: 'Egypt', error: 'stale ref', pending: false }).text, 'clicked "Egypt" failed: stale ref');
  assert.equal(S.actionOf({ tool: 'browser_click', ref: 'e9', refOk: false, label: null, error: null, pending: false }).text, 'clicked e9, which is not on the page');
  assert.equal(S.actionOf(step('b', 1).action).text, 'open → https://en.wikipedia.org/wiki/Ancient_Egypt', 'not a click: what it did');
});

test('the time per step: from ts_ms of the run, and how long the step took', () => {
  assert.equal(S.tookText(24), '24 ms');
  assert.equal(S.tookText(640), '640 ms');
  assert.equal(S.tookText(1500), '1.5 s');
  assert.equal(S.tookText(65000), '1m 5s');
  assert.equal(S.tookText(null), '');
});

// ---------------------------------------------------------------- the end, from the last jev_mark
test('an arrival: the page, the decisions and the asks, the re-asks counted', () => {
  assert.equal(S.endOf(V.b).text, 'arrived at "Felidae" in 2 decisions, 1 asked');
  assert.equal(S.endOf(V.f).text, 'arrived at "Egypt" in 3 decisions, 0 asked');
  assert.equal(S.endOf(V.h).text, 'arrived at "Felidae" in 2 decisions, 1 asked, 1 re-asked', 'an answer that was no option re-asked the same park');
  assert.equal(S.endOf(PARKED_B), null, 'a run still parked has no end');
});

test('case e reads failed although the door said completed (exit 0), and gives the journal\'s reason', () => {
  const e = S.endOf(V.e);
  assert.equal(e.cls, 'r');
  assert.equal(e.text, "failed: observation from 'browser_snapshot' was truncated in transport: 2059 of 13578 declared characters arrived");
  assert.equal(e.note, 'the door said completed');
});

test('case d is cancelled, and says so with its reason', () => {
  assert.equal(S.endOf(V.d).text, 'cancelled: the run was cancelled');
});

// ---------------------------------------------------------------- the end the perch gives a run its journal leaves parked
test('case l: a cancel that lost the race leaves the journal parked, and the end line gives the in-flight tool as the reason', () => {
  assert.equal(V.l.status, 'parked', 'the journal still reads parked');
  assert.equal(V.l.interrupted, true, 'its tool came back cancelled while the tool was running');
  assert.equal(S.endOf(V.l), null, 'the journal has no end');
  assert.deepEqual(S.rowEnd({ state: 'cancelled' }, V.l), { text: 'cancelled: the run was cancelled while its tool was running', cls: 'o' });
  assert.ok(textOf(S.Steps({ rid: 'r3', v: V.l, row: { state: 'cancelled', rid: 'r3' } }))
    .endsWith('cancelled: the run was cancelled while its tool was running'), 'drawn under the last frame');
});

test('an abandoned park: no answer was sent, and the end line says so', () => {
  assert.deepEqual(S.rowEnd({ state: 'abandoned' }, PARKED_B), { text: 'abandoned: no answer was sent at this park', cls: 'o' });
  assert.ok(textOf(S.Steps({ rid: 'r6', v: PARKED_B, row: { state: 'abandoned', rid: 'r6' } }))
    .endsWith('abandoned: no answer was sent at this park'));
});

test('a parked run the perch still holds has no end line; a cancel with no interrupted tool gives the plain reason; a journal end wins', () => {
  assert.equal(S.rowEnd({ state: 'parked' }, PARKED_B), null, 'a run waiting for an answer has not ended');
  assert.equal(S.rowEnd({ state: 'cancelled' }, PARKED_B).text, 'cancelled: the run was cancelled', 'no interrupted tool: the plain reason');
  assert.equal(S.rowEnd({ state: 'cancelled' }, V.d), null, 'case d ends in its own journal: the perch adds no second end line');
  assert.equal(textOf(S.Steps({ rid: 'r5', v: V.d, row: { state: 'cancelled', rid: 'r5' } })).split('cancelled:').length - 1, 1, 'one end line');
  assert.equal(S.rowEnd(null, PARKED_B), null, 'no perch row: no end line');
});

test('exhausted and stopped each say which budget or what stopped it', () => {
  assert.equal(S.endOf({ end: { status: 'exhausted', exhausted: 'steps' } }).text, 'exhausted: out of decision steps');
  assert.equal(S.endOf({ end: { status: 'stopped', reason: 'I give up' } }).text, 'stopped: I give up');
});

// ---------------------------------------------------------------- the bars, the page and the choice
test('the bars of a scored choice: the options with their p, the pick as the scorer or a person chose it', () => {
  const b = S.barsOf(step('b', 2));
  assert.deepEqual(b.rows, [{ label: 'Egypt', p: 0.5 }, { label: 'Cat', p: 0.5 }]);
  assert.equal(b.pick, 'Egypt', 'a person answered Egypt');
  assert.equal(S.barsOf(step('f', 2)), null, 'a single option has no bars');
  assert.equal(S.barsOf(step('c', 2)), null, 'a chooser that did not score has none');
});

test('a truncated page says how many characters arrived, and a page that could not be read says why', () => {
  const tree = S.Steps({ rid: 'r1', v: V.e, row: { state: 'done' } });
  assert.ok(textOf(tree).includes('page cut short: 2059 of 13578 characters arrived'));
  const unread = { ...V.e, steps: [{ ...V.e.steps[0], page: { error: 'stub: the page would not load' } }] };
  assert.ok(textOf(S.Steps({ rid: 'r1', v: unread, row: { state: 'done' } })).includes('page: stub: the page would not load'));
});

// ---------------------------------------------------------------- the park: the ask, its answers and the reasons its buttons are off
test('an answered park says what its answer did: the option picked, the stop, or the answer that asked again', () => {
  assert.equal(P.answeredLine(step('b', 2).parks[0]).text, 'answered 0: Egypt');
  const [bad, good] = step('h', 2).parks;
  assert.equal(P.answeredLine(bad).text, 'answered "banana": not one of the options, asked again');
  assert.equal(P.answeredLine(good).text, 'answered 0: Egypt', 'the second ask was answered');
  assert.equal(P.answeredLine({ answer: null }), null);
});

test('case c: the answer is off and names the errored step eidolon refuses to replay', () => {
  const labels = ['Egypt', 'Cat'];
  const g = P.askGate({ row: { state: 'parked' }, v: V.c, busy: false, index: 0, labels });
  assert.equal(g.ok, false);
  assert.equal(g.run, true, 'the run\'s own reason');
  assert.ok(g.why.includes('step 5 (w1-5 jev_entail)'), g.why);
  assert.equal(P.stopGate({ row: { state: 'parked' }, v: V.c, busy: false }).ok, false, 'a stop is refused the same way');
});

test('the reasons the buttons are off: a child running, a run that is over, a refused try, an action in flight, nothing picked', () => {
  const labels = ['Egypt', 'Cat'];
  assert.equal(P.askGate({ row: { state: 'running' }, v: PARKED_B, busy: false, index: 0, labels }).why, 'a process of that run is still going');
  assert.equal(P.askGate({ row: { state: 'done' }, v: PARKED_B, busy: false, index: 0, labels }).why, 'the run is done, not waiting for an answer');
  assert.equal(P.askGate({ row: { state: 'parked', refused: 'eidolon would not resume it' }, v: PARKED_B, busy: false, index: 0, labels }).why, 'eidolon would not resume it');
  assert.equal(P.askGate({ row: { state: 'parked' }, v: PARKED_B, busy: true, index: 0, labels }).why, 'an action is still going');
  const none = P.askGate({ row: { state: 'parked' }, v: PARKED_B, busy: false, index: null, labels });
  assert.deepEqual([none.ok, none.why, none.run], [false, 'select an option first', false], 'the person\'s own to fix, not the run\'s');
  assert.equal(P.askGate({ row: { state: 'parked' }, v: PARKED_B, busy: false, index: 0, labels }).ok, true);
});

test('an option no text can name is off: its label is refused by the perch, its number names another', () => {
  assert.equal(P.askGate({ row: { state: 'parked' }, v: PARKED_B, busy: false, index: 0, labels: ['a\u0001', '0'] }).why,
    'no text can name this option; its number and label name others');
  assert.equal(R.answerPayload(0, ['1', '0']), '1', 'the exact label wins over the number, so the label names option 0');
});

test('the options as drawn: the index, p to three places (a dash where the chooser gave none), the label cut short', () => {
  assert.deepEqual(P.optionsOf({ options: [{ label: 'Egypt', p: 0.5 }, { label: 'Cat', p: null }] }),
    [{ i: 0, p: '0.500', label: 'Egypt' }, { i: 1, p: '—', label: 'Cat' }]);
  const cut = P.optionsOf({ options: [{ label: 'x'.repeat(60), p: 0.1 }] })[0].label;
  assert.equal([...cut].length, 48, 'cut to 48 characters');
  assert.ok(cut.endsWith('…'), 'the cut says so');
});

test('a reason the perch or eidolon wrote is drawn clean: no bidi mark or control character reaches the page, a title included', () => {
  const row = { state: 'parked', refused: 'eidolon\u202e would not\u0007 resume it' };
  const tree = P.ParkAsk({ rid: 'r7', park: PARKED_B.steps[1].parks[0], live: true, row, v: PARKED_B });
  const drawn = elements(tree).flatMap((e) => [e.text, ...Object.values(e.props).filter((x) => typeof x === 'string')]);
  assert.ok(drawn.includes('eidolon would not resume it'), 'the reason line, cleaned');
  assert.ok(elements(tree).some((e) => e.props.title === 'eidolon would not resume it'), 'the button title, cleaned');
  assert.ok(drawn.every((s) => !/[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]/.test(s)), 'nothing drawn holds a mark or a control');
});

test('a park with no options: the answer is off and says so, stop stays live and sends stop', async () => {
  const park = { call_id: 'p0', n: 1, prompt: '', ts_ms: null, why: 'the menu was empty', cause: null, ask: 'Which link?', retry: null, options: [],
    answer: null, answer_ts_ms: null, waited_ms: null, kind: null, invalid: false, picked: null, stop: null };
  const row = { state: 'parked', rid: 'r9' };
  assert.deepEqual(P.askGate({ row, v: PARKED_B, busy: false, index: null, labels: [] }),
    { ok: false, why: 'this park has no options: stop is the only answer', run: false }, 'no option to name: the park says so, not "select an option first"');
  const tree = P.ParkAsk({ rid: 'r9', park, live: true, row, v: PARKED_B });
  assert.ok(textOf(tree).includes('this park has no options: stop is the only answer'));
  assert.equal(off(tree, '[ answer ]').length, 1, 'answer is off');
  assert.equal(live(tree, '[ stop ]').length, 1, 'stop is live for a parked run with no options');
  click(live(tree, '[ stop ]')[0]);
  await settle();
  assert.deepEqual(sent.map((x) => [x.path, JSON.parse(x.body).text]), [['runs/r9/answer', 'stop']]);
});

test('a parked run: select an option, then [ answer ] sends its number and [ stop ] sends stop', async () => {
  const rid = 'r7';
  const tree = () => S.Steps({ rid, v: PARKED_B, row: { state: 'parked', rid } });
  assert.equal(textOf(tree()).includes('Which link leads toward the article "Felidae"?'), true);
  assert.ok(off(tree(), '[ answer ]').length === 1, 'nothing picked: answer is off');
  assert.ok(live(tree(), '[ stop ]').length === 1, 'stop is live for a parked run');
  click(live(tree(), '0  0.500  Egypt')[0]);
  assert.ok(live(tree(), '[ answer ]').length === 1, 'an option picked: answer is live');
  click(live(tree(), '[ answer ]')[0]);
  await settle();
  assert.deepEqual(sent.map((x) => [x.path, JSON.parse(x.body).text]), [['runs/r7/answer', '0']]);
  click(live(tree(), '[ stop ]')[0]);
  await settle();
  assert.deepEqual(sent.map((x) => [x.path, JSON.parse(x.body).text]), [['runs/r7/answer', '0'], ['runs/r7/answer', 'stop']]);
});

test('a parked run whose answer is refused (case c): both buttons are off and the reason is on the page', () => {
  const tree = S.Steps({ rid: 'r8', v: V.c, row: { state: 'parked', rid: 'r8' } });
  assert.ok(textOf(tree).includes('step 5 (w1-5 jev_entail)'));
  assert.equal(off(tree, '[ answer ]').length, 1);
  assert.equal(off(tree, '[ stop ]').length, 1);
  assert.equal(live(tree, '[ answer ]').length, 0);
});

// ---------------------------------------------------------------- jev's view: folded, the cut and the ties
test('jev\'s view is folded by default; opened, it shows the context and each option cut, with ties flagged after the cut', () => {
  const tree = () => S.Steps({ rid: 'r5', v: TIE_RUN, row: { state: 'done' } });
  assert.equal(textOf(tree()).includes(C.TIE), false, 'folded: no flag yet');
  click(live(tree(), "[ jev's view ]")[0]);
  const open = textOf(tree());
  assert.equal(open.split(C.TIE).length - 1, 2, 'both options that tie are flagged');
  assert.ok(open.includes('the context as jevlike reads it: all 36 bytes'));
});

test('the cut of the context and of each option: what jevlike reads, and only the options it reads alike are flagged', () => {
  const v = C.viewOf({ context_cut: 'Target article: B', context_bytes: 216, context_clipped: true, options_cut: ['aaa', 'bbb'], options_clipped: [true, false],
    collisions: [[0, 1]] });
  assert.equal(v.context.clipped, true);
  assert.deepEqual(v.options.map((o) => [o.clipped, o.tie]), [[true, true], [false, true]]);
  assert.equal(C.contextLine(v.context), 'the context as jevlike reads it: the first 192 of 216 bytes');
  assert.equal(C.viewOf(null), null);
  assert.equal(C.viewOf(step('e', 1).jev), null, 'a step with no choice has no view');
});

test('the real cut of case i: sixty-four options, the first 32 bytes of each, and the context cut at 192', () => {
  const j = V.i.steps[1].jev;
  assert.equal(j.options_cut.length, 64, 'all sixty-four options are cut, not only the eight drawn');
  assert.equal(j.options_clipped.length, 64, 'and each says whether its cut clipped it');
  assert.ok(j.options_cut.every((t) => Buffer.byteLength(t) <= 32), 'every option is cut to 32 bytes');
  assert.ok(j.context_bytes > 192 && j.context_clipped, 'the context is longer than jevlike reads');
});

// ---------------------------------------------------------------- the frames drawn
test('the frames drawn: the last eight, then all of them on `[ all N steps ]`', () => {
  const many = { ...V.b, steps: Array.from({ length: 10 }, (_, k) => ({ ...V.b.steps[1], n: k + 1 })) };
  const frames = (t) => textOf(t).split("[ jev's view ]").length - 1;
  const tree = () => S.Steps({ rid: 'r3', v: many, row: { state: 'done' } });
  assert.equal(frames(tree()), 8);
  assert.ok(textOf(tree()).includes('[ all 10 steps ]'));
  click(live(tree(), '[ all 10 steps ]')[0]);
  assert.equal(frames(tree()), 10);
  click(live(tree(), '[ last 8 ]')[0]); // the view is module state: put it back
});

test('more than eight bars: the top eight, then `… N more`, and all of them on a click', () => {
  const tree = () => S.Steps({ rid: 'r9', v: V.i, row: { state: 'failed' } });
  assert.ok(textOf(tree()).includes('… 56 more'));
  click(live(tree(), '… 56 more')[0]);
  assert.ok(!textOf(tree()).includes('… 56 more'));
  assert.ok(textOf(tree()).includes('[ top 8 ]'));
});

test('no run, no steps: nothing is drawn; a run with its frames has the end under them', () => {
  assert.equal(S.Steps({ rid: 'r1', v: null, row: null }), null);
  assert.equal(S.Steps({ rid: 'r1', v: reduce([]), row: null }), null);
  assert.ok(textOf(S.Steps({ rid: 'r2', v: V.b, row: { state: 'done' } })).endsWith('arrived at "Felidae" in 2 decisions, 1 asked'));
});

// ---------------------------------------------------------------- a run with no step: its end line, under no frame
// Built record by record: no fixture ends before its first step. A jev_mark enter opens a step, so each of these journals stops before one, and
// the end line is all that is drawn (no frame, no `[ all N steps ]`).
const START_REC = { id: 0, ts_ms: 1000, kind: { WorkflowStart: { plugin: 'jev', workflow: 'wiki-hop', version: '0.2.0', hash: 'h', args: JSON.stringify({ start: 'A', goal: 'B' }) } } };
// a final mark on a failed state, before any enter: the graph ended failed, and the door said completed (exit 0), as case e's did
const NO_STEP_FAILED = [START_REC,
  { id: 1, ts_ms: 1010, kind: { WorkflowStep: { run: 1, index: 1, call_id: 'w1-1', tool: 'jev_mark', input: JSON.stringify({ graph: 'wiki-hop', kind: 'final', state: 'failed', step: 1, actions: 0 }) } } },
  { id: 2, ts_ms: 1011, kind: { ToolResult: { tool_use_id: 'w1-1', content: 'marked', is_error: false } } },
  { id: 3, ts_ms: 1020, kind: { WorkflowEnd: { status: 'Completed' } } }];
// SIGTERM while the graph was being read: the in-flight jev_graph never came back, and the door said failed, so the run is cancelled before any step
const NO_STEP_CANCELLED = [START_REC,
  { id: 1, ts_ms: 1010, kind: { WorkflowStep: { run: 1, index: 1, call_id: 'w1-1', tool: 'jev_graph', input: JSON.stringify({ graph: 'wiki-hop' }) } } },
  { id: 2, ts_ms: 1020, kind: { WorkflowEnd: { status: 'Failed' } } }];
// a park the journal ends on before any step (no enter yet): the journal reads parked, and only the perch's abandon gives it an end
const NO_STEP_PARKED = [START_REC, { id: 1, ts_ms: 1020, kind: { WorkflowEnd: { status: 'Parked' } } }];

test('a failed run with no step: its end line alone, the door said completed kept as the note', () => {
  const v = reduce(NO_STEP_FAILED);
  assert.equal(v.steps.length, 0, 'no step: no frame');
  assert.equal(v.status, 'failed');
  assert.equal(textOf(S.Steps({ rid: 'r10', v, row: { state: 'failed', rid: 'r10' } })), 'failed: the graph ended in its failed state · the door said completed');
});

test('a run cancelled before its first step: its reason alone, under no frame', () => {
  const v = reduce(NO_STEP_CANCELLED);
  assert.equal(v.steps.length, 0, 'no step: no frame');
  assert.equal(v.status, 'cancelled');
  assert.equal(textOf(S.Steps({ rid: 'r11', v, row: { state: 'cancelled', rid: 'r11' } })), 'cancelled: the run was cancelled');
});

test('a run the perch abandoned with no step: the abandon line alone', () => {
  const v = reduce(NO_STEP_PARKED);
  assert.equal(v.steps.length, 0, 'no step: no frame');
  assert.equal(v.status, 'parked');
  assert.equal(textOf(S.Steps({ rid: 'r12', v, row: { state: 'abandoned', rid: 'r12' } })), 'abandoned: no answer was sent at this park');
});

test('a run still going with no step yet draws nothing, and a parked one the perch still holds draws nothing', () => {
  assert.equal(S.Steps({ rid: 'r13', v: reduce([START_REC]), row: { state: 'running', rid: 'r13' } }), null);
  assert.equal(S.Steps({ rid: 'r13', v: reduce([START_REC, NO_STEP_PARKED[1]]), row: { state: 'parked', rid: 'r13' } }), null);
});

test('the action and the arrival of case b, as the page reads them in its frames', () => {
  const t = textOf(S.Steps({ rid: 'r4', v: V.b, row: { state: 'done' } }));
  assert.ok(t.includes('heading Ancient Egypt ≠ Felidae'));
  assert.ok(t.includes('clicked "Egypt" → https://en.wikipedia.org/wiki/Egypt'));
  assert.ok(t.includes('0.500  Egypt'), 'a bar with its p');
});

// ---------------------------------------------------------------- a tie no fixture holds: two options agreeing in their first 32 bytes
// Built record by record in the shape tests/fixtures/jev/README.md gives: a WorkflowStart, a jev_mark entering the state, and a jev_choose with its
// result. The two options share their first 32 bytes (the cut jevlike reads), so jevlike cannot tell them apart.
const PREFIX = 'The first thirty-two bytes are a'; // 32 bytes exactly
const OPTS = [`${PREFIX} Alpha`, `${PREFIX} Beta`, 'Cat'];
const TIE_JOURNAL = [
  { id: 0, ts_ms: 1000, kind: { WorkflowStart: { plugin: 'jev', workflow: 'wiki-hop', version: '0.2.0', hash: 'h', args: JSON.stringify({ start: 'A', goal: 'B' }) } } },
  { id: 1, ts_ms: 1010, kind: { WorkflowStep: { run: 1, index: 1, call_id: 'w1-1', tool: 'jev_mark', input: JSON.stringify({ graph: 'wiki-hop', kind: 'enter', state: 'reading', step: 1, actions: 0 }) } } },
  { id: 2, ts_ms: 1011, kind: { ToolResult: { tool_use_id: 'w1-1', content: 'marked', is_error: false } } },
  { id: 3, ts_ms: 1020, kind: { WorkflowStep: { run: 1, index: 2, call_id: 'w1-2', tool: 'jev_choose', input: JSON.stringify({ context: 'Target article: B\nCurrent article: A', options: OPTS }) } } },
  { id: 4, ts_ms: 1500, kind: { ToolResult: { tool_use_id: 'w1-2', content: JSON.stringify({ probs: { [OPTS[0]]: 0.6, [OPTS[1]]: 0.3, Cat: 0.1 }, best: OPTS[0] }), is_error: false } } },
];
const TIE_RUN = reduce(TIE_JOURNAL);

test('a tie after the cut: both options flagged, the third not; the cut lines carry it into the view', () => {
  const j = TIE_RUN.steps[0].jev;
  assert.deepEqual(j.collisions, [[0, 1]]);
  assert.equal(j.top_tie, true, 'the top option is one of the tied pair');
  const v = C.viewOf(j);
  assert.deepEqual(v.options.map((o) => o.tie), [true, true, false]);
  assert.equal(C.contextLine(v.context), 'the context as jevlike reads it: all 36 bytes');
});

// ---------------------------------------------------------------- the sources: what the files do and do not do
test('the three files draw text only: no innerHTML, no eval, and a park answers through runs.js, not a request of its own', () => {
  for (const f of ['../graph/steps.js', '../graph/park.js', '../graph/context.js']) {
    const c = code(f);
    assert.doesNotMatch(c, /innerHTML|outerHTML|insertAdjacentHTML|dangerouslySetInnerHTML|\beval\(/, f);
    assert.doesNotMatch(c, /from '\.\.\/core\/(api|lane)\.js'/, f + ': the request is runs.js\'s, not the page\'s');
  }
  assert.doesNotMatch(code('../graph/park.js'), /\bfetch\(|hubPost|hubGet|hubText/);
  assert.match(code('../graph/park.js'), /answerRun\(rid, answerPayload\(index, labels\)\)/);
  assert.match(code('../graph/park.js'), /answerRun\(rid, STOP\)/);
});

test('steps.js is drawn by view.js with the run, its rid and its detail, and exports the component the view mounts', () => {
  assert.match(read('../graph/steps.js'), /export function Steps\(\{ rid, v, row \}\)/);
  assert.match(read('../graph/view.js'), /<\$\{Steps\} rid=\$\{rid\} v=\$\{v\} row=\$\{row\} \/>/);
  assert.equal(typeof R.answerRun, 'function');
});
