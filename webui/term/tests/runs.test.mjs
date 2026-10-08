// The graph tab's runs (graph/runs.js, graph/view.js): the poll intervals per state, the log cursor, the answers, the parked cap, the decision-log
// states and the words the services line and a run's row say. The polls run against a stub fetch that answers at once, on mock timers, so what
// is asked at each moment is exactly what the test sees. The journal is tests/fixtures/jev/b (57 records, ids 0 to 56, arrived).
import { test, mock, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { hubPathOk } from '../core/api.js';
import { K } from '../core/ui.js';
import * as R from '../graph/runs.js';
import * as V from '../graph/view.js';
import { newRun, feedText, view } from '../graph/jevrun.js';

// As LF: a checkout with core.autocrlf has CRLF sources.
const read = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
// A source file's code: whole-line comments left out, so a file may say in a comment what it does not do.
const code = (rel) => read(rel).split('\n').filter((l) => !/^\s*\/\//.test(l)).join('\n');
const settle = () => new Promise((r) => setImmediate(r)); // every promise step already queued has run
const tick = async (ms) => { mock.timers.tick(ms); await settle(); };

const JOURNAL = read('./fixtures/jev/b/journal.ndjson').trim().split('\n');
const RECS = JOURNAL.map((l) => JSON.parse(l));
const ndjson = (after) => RECS.filter((r) => after === null || r.id > after).map((r) => JSON.stringify(r)).join('\n');

// ---------------------------------------------------------------- a stub perch: answers at once, every call kept
let calls, routes, listeners;
function perch(fn) { routes = fn; calls = []; }
globalThis.fetch = async (url, init = {}) => {
  calls.push({ path: url.replace(/^\/hub\//, ''), method: init.method || 'GET', body: init.body });
  const r = routes(url.replace(/^\/hub\//, ''), init.method || 'GET', init.body) || { status: 404, data: null };
  const text = r.text !== undefined ? r.text : r.data === null || r.data === undefined ? '' : JSON.stringify(r.data);
  return { status: r.status, text: async () => text };
};
const paths = () => calls.map((c) => c.path);
const page = (state) => { globalThis.document = { visibilityState: state, addEventListener: (t, f) => { listeners[t] = f; }, removeEventListener: (t) => { delete listeners[t]; } }; };

// the perch's answers for one run, 'r1', whose state the test changes
const HEALTH = { jev: { configured: true, up: true, status: 'ok', jevlike: true, openjev: true, ckpt: 'jev-base-v0.pt', loaded: {}, decisions_log: 'off' },
  browser: { configured: true, up: true, status: 'ok' } };
const ROW = { rid: 'r1', graph: 'wiki-hop', args: { start: 'Ancient_Egypt', goal: 'Felidae' }, started_ms: 1000, resumes: 0 };
let runState = 'running';
function oneRun(path) {
  if (path === 'runs/health') return { status: 200, data: HEALTH };
  if (path === 'runs') return { status: 200, data: [{ ...ROW, state: runState }] };
  if (path === 'runs/r1') return { status: 200, data: { ...ROW, state: runState, graph: null } };
  const m = /^runs\/r1\/log(?:\?after=(\d+))?$/.exec(path);
  if (m) return { status: 200, text: ndjson(m[1] === undefined ? null : Number(m[1])) };
  return null;
}

beforeEach(() => {
  R.resetRuns();
  listeners = {};
  runState = 'running';
  perch(oneRun);
});
afterEach(() => {
  R.unwatch();
  mock.timers.reset();
  delete globalThis.document;
});

// ---------------------------------------------------------------- the intervals, as numbers
test('the look at a run is 1 s while it runs, 5 s while it is parked, and none once it has ended', () => {
  assert.equal(R.RUNNING_MS, 1000);
  assert.equal(R.PARKED_MS, 5000);
  assert.equal(R.pollMs('running'), 1000);
  assert.equal(R.pollMs('parked'), 5000);
  assert.equal(R.pollMs(''), 1000, 'a state not yet known is taken as moving');
  for (const s of ['done', 'failed', 'cancelled', 'abandoned', 'refused', 'lost']) assert.equal(R.pollMs(s), null, s);
});

test('the list and the services are asked every 10 s while the page is visible', () => {
  assert.equal(R.HEALTH_MS, 10000);
});

test('after failed looks the retry doubles from 2 s, never shorter than the usual interval and never past 10 s', () => {
  assert.equal(R.retryMs(1000, 0), 1000, 'no failure: the usual interval');
  assert.equal(R.retryMs(1000, 1), 2000);
  assert.equal(R.retryMs(1000, 2), 4000);
  assert.equal(R.retryMs(1000, 3), 8000);
  assert.equal(R.retryMs(1000, 9), 10000);
  assert.equal(R.retryMs(5000, 1), 5000, 'a parked run keeps its own interval as the floor');
  assert.equal(R.retryMs(5000, 4), 10000);
});

// ---------------------------------------------------------------- the paths
test('ids and graph names reach a path only when they are what the perch makes', () => {
  assert.equal(R.ridOk('r12'), true);
  for (const bad of ['12', 'r', 'r1/../x', 'R1', 'r1?x=1', null]) assert.equal(R.ridOk(bad), false, String(bad));
  assert.equal(R.graphPath('wiki-hop'), 'runs/graphs/wiki-hop');
  assert.equal(R.graphPath('../x'), null);
  assert.equal(R.detailPath('r1'), 'runs/r1');
  assert.equal(R.detailPath('x'), null);
});

test('the log path carries the cursor only once there is one, and the whitelist takes every path the page makes', () => {
  assert.equal(R.logPath('r1', null), 'runs/r1/log', 'the first poll has no query at all: after=0 would skip record 0');
  assert.equal(R.logPath('r1', 0), 'runs/r1/log?after=0');
  assert.equal(R.logPath('r1', 56), 'runs/r1/log?after=56');
  assert.equal(R.logPath('r1', -1), 'runs/r1/log', 'a cursor that is not a whole number is no cursor');
  assert.equal(R.logPath('r1', 1.5), 'runs/r1/log');
  assert.equal(R.logPath('../x', 3), null);
  for (const p of [R.logPath('r1', null), R.logPath('r1', 56), R.detailPath('r1'), R.graphPath('wiki-hop'), 'runs/health', 'runs']) {
    assert.equal(hubPathOk(p), true, p);
  }
});

// ---------------------------------------------------------------- the clock: what is asked, and when
test('the tab asks the services and the list at open and every 10 s while visible, and nothing while hidden or closed', async () => {
  mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  page('visible');
  const stop = R.openTab();
  await settle();
  assert.deepEqual(paths(), ['runs/health', 'runs']);
  await tick(9999);
  assert.equal(calls.length, 2, 'not before 10 s');
  await tick(1);
  assert.deepEqual(paths(), ['runs/health', 'runs', 'runs/health', 'runs']);
  page('hidden');
  await tick(10000);
  assert.equal(calls.length, 4, 'a hidden page asks nothing');
  stop();
  page('visible');
  await tick(30000);
  assert.equal(calls.length, 4, 'closed: nothing');
});

test('a tab opened while hidden asks nothing, and asks at once when the page comes back into view', async () => {
  mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  page('hidden');
  const stop = R.openTab();
  await settle();
  assert.deepEqual(paths(), [], 'opened hidden: nothing yet');
  page('visible');
  listeners.visibilitychange();
  await settle();
  assert.deepEqual(paths(), ['runs/health', 'runs']);
  stop();
  assert.equal(listeners.visibilitychange, undefined, 'the listener is taken away when the tab closes');
});

test('a shown run is looked at 1 s apart while it runs, 5 s apart while it is parked, and not once it has ended', async () => {
  mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  page('visible');
  R.watch('r1');
  await settle();
  assert.deepEqual(paths(), ['runs/r1', 'runs/r1/log'], 'the first look sends no cursor');
  runState = 'parked';
  await tick(999);
  assert.equal(calls.length, 2, 'the look after a running one is 1 s on');
  await tick(1);
  assert.deepEqual(paths().slice(2), ['runs/r1', 'runs/r1/log?after=56'], 'the cursor is the last record held');
  await tick(4999);
  assert.equal(calls.length, 4, 'the look that saw it parked waits 5 s');
  await tick(1);
  assert.equal(calls.length, 6);
  runState = 'done';
  await tick(5000);
  assert.equal(calls.length, 8, 'the look that sees it done is the last one');
  await tick(600000);
  assert.equal(calls.length, 8, 'an ended run is not looked at again');
});

test('a hidden page looks at nothing, and looks again once it shows', async () => {
  mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  page('visible');
  R.watch('r1');
  await settle();
  assert.equal(calls.length, 2);
  page('hidden');
  await tick(1000);
  await tick(60000);
  assert.equal(calls.length, 2, 'hidden: nothing');
  page('visible');
  R.poke();
  await settle();
  assert.equal(calls.length, 4, 'shown again: one look, then the usual interval');
  await tick(1000);
  assert.equal(calls.length, 6);
});

test('unwatching a run stops its looks at once', async () => {
  mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  page('visible');
  R.watch('r1');
  await settle();
  R.unwatch();
  await tick(60000);
  assert.equal(calls.length, 2);
});

test('the log cursor moves on and no record is held twice, even when every look asks for the whole log again', async () => {
  mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  page('visible');
  perch((path) => {
    if (path === 'runs/r1') return { status: 200, data: { ...ROW, state: 'running', graph: null } };
    if (/^runs\/r1\/log(\?after=\d+)?$/.test(path)) return { status: 200, text: ndjson(null) };
    return null;
  });
  R.watch('r1');
  await settle();
  await tick(1000);
  await tick(1000);
  const m = R.model.value;
  assert.equal(m.v.rev, 57, 'each of the 57 records taken once');
  assert.equal(m.v.lastId, 56);
  assert.equal(calls.filter((c) => c.path.startsWith('runs/r1/log')).length, 3);
  assert.equal(m.v.steps.length, view(feedText(newRun({ graph: null }), ndjson(null))).steps.length);
});

test('a look asked for while one is still going waits for it: one look in flight, the held look taken once, then one look a second', async () => {
  mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  page('visible');
  let release;
  const held = new Promise((r) => { release = r; });
  let first = true;
  perch((path) => {
    if (path === 'runs/r1') {
      // the first look's detail is held until the test lets it go; the looks after it answer at once
      const holdIt = first;
      first = false;
      const row = JSON.stringify({ ...ROW, state: 'running', graph: null });
      return { status: 200, text: holdIt ? held.then(() => row) : row };
    }
    const m = /^runs\/r1\/log(?:\?after=(\d+))?$/.exec(path);
    return m ? { status: 200, text: ndjson(m[1] === undefined ? null : Number(m[1])) } : null;
  });
  R.watch('r1');
  await settle();
  assert.deepEqual(paths(), ['runs/r1'], 'the first look is in flight');
  R.poke(); // a look asked for while that one is going (the page came back into view, say)
  await tick(5000);
  assert.deepEqual(paths(), ['runs/r1'], 'no second look is sent while the first is in flight');
  release();
  await settle(); // the held look goes on to its log, and the look asked for meanwhile waits behind it
  assert.deepEqual(paths(), ['runs/r1', 'runs/r1/log'], 'one look at a time on the wire');
  await tick(1); // then that one look again, from the cursor, straight away
  assert.deepEqual(paths(), ['runs/r1', 'runs/r1/log', 'runs/r1', 'runs/r1/log?after=56'], 'the look asked for meanwhile runs once, from the cursor');
  assert.equal(R.model.value.v.rev, 57, 'each of the 57 records taken once');
  assert.equal(R.model.value.v.lastId, 56);
  const before = calls.length;
  await tick(1000);
  assert.equal(calls.length - before, 2, 'then one look a second');
  assert.equal(calls.at(-1).path, 'runs/r1/log?after=56', 'and the looks after it ask from the cursor');
});

test('an answer that is late or overlaps takes nothing twice and never moves the cursor back', () => {
  const run = newRun({ graph: null });
  const lines = (lo, hi) => RECS.filter((r) => r.id >= lo && r.id <= hi).map((r) => JSON.stringify(r)).join('\n');
  const ok = (text) => ({ status: 200, data: text });
  assert.equal(R.afterOf(run), null, 'nothing held: no cursor');
  assert.deepEqual(R.foldLog(run, ok(lines(0, 20))), { ok: true, fresh: true, sentence: '' });
  assert.equal(R.afterOf(run), 20);
  assert.equal(R.foldLog(run, ok(lines(10, 30))).fresh, true, 'an overlap that goes on is fresh');
  assert.equal(R.afterOf(run), 30);
  assert.equal(R.foldLog(run, ok(lines(5, 25))).fresh, false, 'a late answer adds nothing');
  assert.equal(R.afterOf(run), 30, 'and the cursor stays where it was');
  assert.equal(view(run).rev, 31, 'records 0 to 30, each taken once');
  assert.equal(view(run).steps.length, view(feedText(newRun({ graph: null }), lines(0, 30))).steps.length);
  assert.deepEqual(R.foldLog(run, ok('')), { ok: true, fresh: false, sentence: '' }, 'an empty body is nothing new, never an error');
});

test('a look at the log that fails says why; only a gone run, or a request the perch will never take, stops the looks', () => {
  const run = newRun({ graph: null });
  const gone = R.foldLog(run, { status: 404, data: { error: 'no such run' } });
  assert.deepEqual([gone.ok, gone.gone, gone.stop, gone.sentence], [false, true, true, 'no such run']);
  const big = R.foldLog(run, { status: 413, data: { error: 'the whole log is over 16 MiB' } });
  assert.deepEqual([big.stop, big.gone], [true, false]);
  const slow = R.foldLog(run, { status: 504, data: { error: 'eidolon took over 15 s' } });
  assert.deepEqual([slow.stop, slow.sentence], [false, 'eidolon took over 15 s'], 'a slow read is asked again');
  assert.equal(R.foldLog(run, { status: 0, data: { error: 'network error' } }).sentence, 'the perch did not answer');
});

// ---------------------------------------------------------------- answers
test('an answer is the 0-based number the park prompt prints, or the exact label when that number is another option\'s label', () => {
  // the park prompt prints `  0  0.000  Egypt` and `  1  0.000  Cat` (fixtures/jev/c)
  const two = ['Egypt', 'Cat'];
  assert.equal(R.answerPayload(0, two), '0');
  assert.equal(R.answerPayload(1, two), '1');
  assert.equal(R.answerPayload(1, ['1', '0']), '0', 'the number 1 is the first option\'s label, so the second is answered by its own label');
  assert.equal(R.answerPayload(0, ['1', '0']), '1', 'the first is answered by its label, which is the number 1 that picks it');
  assert.equal(R.answerPayload(2, two), null, 'out of the menu');
  assert.equal(R.answerPayload(-1, two), null);
  assert.equal(R.answerPayload(0, []), null);
  assert.equal(R.answerPayload(0, 'Egypt'), null, 'no menu, no answer');
});

test('an answer the perch would refuse is not sent: empty, over 1 KiB, or with a control character; stop is an answer', () => {
  assert.match(R.answerOk(''), /^write an answer/);
  assert.match(R.answerOk('   '), /^write an answer/);
  assert.match(R.answerOk('a\u0007b'), /control characters/);
  assert.match(R.answerOk('x'.repeat(1025)), /1 KiB/);
  assert.equal(R.answerOk('x'.repeat(1024)), '');
  assert.equal(R.answerOk('0'), '');
  assert.equal(R.STOP, 'stop');
  assert.equal(R.answerOk(R.STOP), '');
});

test('answering posts the text as it was typed, and a refused answer is never sent', async () => {
  perch((path, method) => (path === 'runs/r1/answer' && method === 'POST' ? { status: 202, data: { rid: 'r1', state: 'running' } } : oneRun(path)));
  assert.deepEqual(await R.answerRun('r1', '1'), { ok: true, sentence: '' });
  const post = calls.find((c) => c.method === 'POST');
  assert.equal(post.path, 'runs/r1/answer');
  assert.deepEqual(JSON.parse(post.body), { text: '1' });
  // stop is an answer like any other: it goes out as the text stop
  assert.equal((await R.answerRun('r1', R.STOP)).ok, true);
  assert.deepEqual(JSON.parse(calls.filter((c) => c.method === 'POST').at(-1).body), { text: 'stop' });
  const n = calls.length;
  assert.equal((await R.answerRun('r1', 'a\u0001')).ok, false);
  assert.equal((await R.answerRun('nope', '1')).sentence, 'that is not a run');
  assert.equal(calls.length, n, 'nothing sent for a refused answer');
  assert.equal(R.acting.value, '');
});

// ---------------------------------------------------------------- one shared browser: the parked cap
test('a second start while a run holds the browser is refused with the perch\'s sentence, and the holder is named', async () => {
  perch((path, method) => (path === 'runs' && method === 'POST' ? { status: 409, data: { error: 'run r1 is parked: there is one shared browser', run: 'r1' } } : oneRun(path)));
  assert.deepEqual(await R.startRun('wiki-hop', { start: 'A' }), { ok: false, sentence: 'run r1 is parked: there is one shared browser' });
  assert.equal(R.notice.value.holder, 'r1');
});

test('while a run is parked the holder is the one sentence names; only a run that holds the browser is a holder', () => {
  const holder = { rid: 'r3', state: 'parked' };
  const g = R.runGate({ fields: [{ name: 'start', type: 'string', required: true }], values: { start: 'A' }, holder });
  assert.equal(g.ok, false);
  assert.equal(g.why, R.busySentence('r3'));
  assert.match(g.why, /one shared browser: finish it, cancel it or abandon it first/);
  assert.equal(g.holder, holder);
  assert.equal(R.holderOf([{ rid: 'r2', state: 'done' }, holder]), holder);
  assert.equal(R.holderOf([{ rid: 'r2', state: 'done' }]), null);
  assert.match(R.runGate({ fields: [], values: {}, holder: { rid: 'r4', state: 'running' } }).why, /^run r4 is still going or parked/);
});

test('a refusal says the perch\'s own sentence, and names a holder only for the one-at-a-time 409', () => {
  assert.deepEqual(R.refusalOf({ status: 409, data: { error: 'busy', run: 'r7' } }), { text: 'busy', cls: 'o', holder: 'r7' });
  assert.deepEqual(R.refusalOf({ status: 503, data: { error: 'jev is down: start it with X', why: 'x' } }), { text: 'jev is down: start it with X', cls: 'o', holder: null });
  assert.equal(R.refusalOf({ status: 0, data: null }).text, 'the perch did not answer');
  assert.equal(R.refusalOf({ status: 401, data: null }).text, 'the perch refused this page\'s login (it restarted?)');
});

test('a park is answerable only when eidolon can resume it, and the reason is its own', () => {
  assert.deepEqual(R.parkGate({ state: 'parked' }, null), { ok: true, why: '' });
  assert.equal(R.parkGate({ state: 'parked' }, { unresumable: { step: 3, reason: 'jev_choose failed' } }).why, 'jev_choose failed');
  assert.equal(R.parkGate({ state: 'parked', refused: 'eidolon would not resume it' }, null).why, 'eidolon would not resume it');
  assert.match(R.parkGate({ state: 'running' }, null).why, /still going/);
  assert.match(R.parkGate({ state: 'done' }, null).why, /not waiting for an answer/);
});

// ---------------------------------------------------------------- the new-run form
const GRAPH = { meta: { jev: { input: { required: ['start'], properties: { start: { type: 'string' }, goal: { type: 'string', description: 'the goal' },
  depth: { type: 'integer' }, flag: { type: 'boolean' } } } } } };

test('the form has one field per string or number parameter, and names the ones it cannot take', () => {
  const { fields, skipped } = R.fieldsOf(GRAPH);
  assert.deepEqual(fields.map((f) => [f.name, f.type, f.required]), [['start', 'string', true], ['goal', 'string', false], ['depth', 'integer', false]]);
  assert.deepEqual(skipped.map((f) => f.name), ['flag']);
  assert.deepEqual(R.fieldsOf({}), { fields: [], skipped: [] });
});

test('the arguments are the ones typed: an empty optional is left out, a number is a number, a missing or bad one is named', () => {
  const { fields } = R.fieldsOf(GRAPH);
  assert.deepEqual(R.argsOf(fields, { start: ' Ancient_Egypt ', goal: '', depth: '3' }), { args: { start: 'Ancient_Egypt', depth: 3 }, missing: [], bad: [] });
  assert.deepEqual(R.argsOf(fields, { goal: 'x', depth: 'x3' }), { args: { goal: 'x' }, missing: ['start'], bad: ['depth'] });
  assert.deepEqual(R.argsOf(fields, { start: 'a', depth: '2.5' }).bad, ['depth'], 'an integer takes no fraction');
});

test('a start is refused for the form or the one browser, never for the services', () => {
  const ok = { fields: R.fieldsOf(GRAPH).fields, skipped: [], values: { start: 'A' } };
  assert.deepEqual(R.runGate(ok), { ok: true, why: '' });
  assert.equal(R.runGate({ ...ok, values: {} }).why, 'write start');
  assert.equal(R.runGate({ fields: null, loading: true }).why, 'reading the graph…');
  assert.equal(R.runGate({ fields: null }).why, 'choose a graph');
  assert.equal(R.runGate({ ...ok, starting: true }).why, 'starting…');
  assert.equal(R.runGate({ fields: [], skipped: [{ name: 'list', type: 'array', required: true }], values: {} }).why, 'this graph needs list (array), which this form cannot take');
  assert.match(R.runGate({ ...ok, values: { start: 'A', depth: 'x' } }).why, /depth must be a number/);
});

// ---------------------------------------------------------------- the services line and the decision log
const SVC = (jev, browser = { configured: true, up: true, status: 'ok' }) => ({ status: 200, data: { jev, browser } });
const JEV_UP = { configured: true, up: true, status: 'ok', jevlike: true, openjev: true, ckpt: 'jev-base-v0.pt', loaded: { jevlike: false, openjev: false },
  openjev_dtype: 'float32' };

test('the services line is one line of the services, with the weights folded away until details', () => {
  const parts = V.servicesParts(SVC({ ...JEV_UP, decisions_log: 'off' }));
  assert.equal(V.servicesText(SVC({ ...JEV_UP, decisions_log: 'off' })),
    'jev ● up · jevlike jev-base-v0.pt (weights present; loads on first call) · openjev weights present (float32, loads on first call, ~24 s) · decision log off · browser ● up');
  assert.deepEqual(parts.filter((p) => !p.detail).map((p) => p.text), ['jev ● up', 'decision log off', 'browser ● up']);
});

test('the decision log is shown in each state, and no state greys or refuses a run', () => {
  const logPart = (jev) => V.servicesParts(SVC(jev)).find((p) => p.key === 'log').text;
  assert.equal(logPart({ ...JEV_UP, decisions_log: 'off' }), 'decision log off');
  assert.equal(logPart({ ...JEV_UP, decisions_log: '/home/you/.cache/eidolon/extensions/jev/decisions.jsonl' }),
    'decision log on: /home/you/.cache/eidolon/extensions/jev/decisions.jsonl');
  assert.equal(logPart({ ...JEV_UP }), 'decision log: unknown', 'an old jev that does not say');
  assert.equal(logPart({ configured: true, up: false, why: 'connection refused', start: 'jev-serve' }), 'decision log: unknown', 'jev down: unknown');
});

test('[ run ] stays enabled, and a start goes out, in every decision-log state: the health is held where the gate could read it', async () => {
  const jevs = [{ ...JEV_UP, decisions_log: 'off' }, { ...JEV_UP, decisions_log: '/x/decisions.jsonl' }, { ...JEV_UP }];
  for (const jev of jevs) {
    R.resetRuns();
    perch((path, method) => (path === 'runs' && method === 'POST' ? { status: 202, data: { rid: 'r9', state: 'running' } } : oneRun(path)));
    R.health.value = SVC(jev); // the state the services line shows: a gate that read it would see it here
    assert.match(V.servicesText(SVC(jev)), /decision log/);
    const input = { chosen: CHOSEN, form: FORM, acting: '', holder: null };
    assert.deepEqual(R.formGate(input), { ok: true, why: '' });
    assert.equal(button(newRunTree(R.formGate(input)), '[ run ]').props.off, false, 'enabled');
    assert.deepEqual(R.startFromForm(input), { ok: true, why: '' }, 'and the start is made');
    await settle();
    const post = calls.find((c) => c.method === 'POST');
    assert.deepEqual([post.path, JSON.parse(post.body)], ['runs', { graph: 'wiki-hop', args: { start: 'Ancient_Egypt' } }]);
  }
});

test('a jev that does not name its checkpoint says so, and a down service shows its start command as text', () => {
  const { ckpt, ...noCkpt } = JEV_UP;
  assert.match(V.servicesText(SVC(noCkpt)), /^jev ● up · jevlike checkpoint: unknown \(/);
  const down = V.servicesText(SVC({ configured: true, up: false, why: 'connection refused', start: 'eidolon jev serve' }, { configured: false }));
  assert.equal(down, 'jev ○ down (connection refused), start it: eidolon jev serve · decision log: unknown · browser: not configured');
  assert.equal(V.servicesText(null), 'services: asking the perch…');
  assert.equal(V.servicesText({ status: 404, data: null }), 'services: unknown (this perch has no such route (an older perch?))');
});

// ---------------------------------------------------------------- the words of a run
test('a run in the list says its state, how long it has gone or how long ago it ended, and its args', () => {
  const now = 1000 + 125000;
  const r = V.rowLine({ rid: 'r1', graph: 'wiki-hop', state: 'running', started_ms: 1000, args: { start: 'Ancient_Egypt', goal: 'Felidae' }, resumes: 0 }, now);
  assert.deepEqual([r.rid, r.graph, r.state, r.cls, r.when], ['r1', 'wiki-hop', 'running', 'hi', '2m']);
  assert.equal(r.title, 'start Ancient_Egypt, goal Felidae');
  const done = V.rowLine({ rid: 'r2', graph: 'wiki-hop', state: 'cancelled', started_ms: 1, ended_ms: now - 30000, why: 'the page asked and the run was stopped', resumes: 0 }, now);
  assert.equal(done.when, '30s ago');
  assert.equal(done.cls, 'o');
});

test('the status of a run in words: a cancel reads cancelled, never failed, and a park says what it waits for', () => {
  const text = (row, v = null) => V.statusParts(row, v, 0).map((p) => p.text).join('');
  assert.equal(text({ state: 'cancelled', why: '' }), 'cancelled · the page asked and the run was stopped');
  assert.equal(text({ state: 'failed', why: 'the run took longer than 15m' }), 'failed: the run took longer than 15m');
  assert.equal(text({ state: 'parked', refused: 'F0 c: jev_choose failed' }, { waiting: { park: { why: 'top 0.015 under floor 0.350' } } }),
    'parked: waiting for an answer · top 0.015 under floor 0.350 · eidolon would not resume it: F0 c: jev_choose failed');
  assert.equal(text({ state: 'done' }, { status: 'arrived' }), 'done · arrived');
  assert.equal(text({ state: 'done' }, { status: 'exhausted', end: { exhausted: 'decisions' } }), 'done · ran out of decisions');
  assert.equal(text({ state: 'abandoned' }), 'abandoned · a parked run given up; nothing was started');
});

test('the path keeps its last pages, the budgets say what is left, and a model spend is red', () => {
  assert.equal(V.pathText(['Ancient Egypt', 'Cat', 'Felidae'], false), 'Ancient Egypt → Cat → Felidae');
  assert.equal(V.pathText(['a', 'b', 'c', 'd', 'e', 'f', 'g'], true), '… → c → d → e → f → g → …');
  assert.equal(V.pathText([], true), '');
  const b = V.budgetParts({ decisions: { used: 3, cap: 10, left: 7 }, actions: { used: 2, cap: 4, left: 2 }, escalations: { used: 1, cap: 1, left: 0 },
    host: { used: 9, cap: 200, left: 191 } });
  assert.deepEqual(b.map((x) => x.text), ['decisions 7 left', 'actions 2 left', 'asks 0 left', 'host steps 191 left']);
  assert.equal(b[2].cls, 'r');
  assert.equal(V.spendLine([]).text, 'model spend: none');
  assert.equal(V.spendLine([{ kind: 'x' }]).cls, 'r');
});

// ---------------------------------------------------------------- the components, as their trees
// A component is called with its props and its tree read: nothing is mounted, no DOM.
const nodes = (v, out = []) => {
  const go = (x) => { if (Array.isArray(x)) x.forEach(go); else if (x && typeof x === 'object' && x.props) { out.push(x); go(x.props.children); } };
  go(v);
  return out;
};
const words = (c) => [].concat(c).flat(Infinity).map((x) => (typeof x === 'string' ? x : '')).join('');
const button = (tree, text) => nodes(tree).find((n) => n.type === K && words(n.props.children) === text);
const FORM = { open: true, graph: 'wiki-hop', values: { 'wiki-hop': { start: 'Ancient_Egypt' } } };
const LIST = { ok: true, graphs: [{ name: 'wiki-hop' }], sentence: '' };
const CHOSEN = { name: 'wiki-hop', state: 'ok', ...R.fieldsOf(GRAPH), sentence: '' };
const newRunTree = (gate, rows = [], over = {}) => V.NewRun({ list: LIST, form: FORM, chosen: CHOSEN, gate, rows, busy: false, onPickGraph() {}, onField() {},
  onRun() {}, onClose() {}, onAbandon() {}, ...over });

test('the new run offers [ run ] greyed with the reason, and [ abandon ] for the parked run that holds the browser', () => {
  const missing = R.runGate({ fields: CHOSEN.fields, skipped: [], values: {} });
  assert.equal(button(newRunTree(missing), '[ run ]').props.off, true);
  assert.equal(button(newRunTree(missing), '[ run ]').props.title, 'write start');
  const rows = [{ rid: 'r3', graph: 'wiki-hop', state: 'parked' }];
  const gate = R.runGate({ fields: CHOSEN.fields, skipped: [], values: FORM.values['wiki-hop'], holder: rows[0] });
  let abandoned = null;
  const tree = newRunTree(gate, rows, { onAbandon: (rid) => { abandoned = rid; } });
  assert.equal(button(tree, '[ run ]').props.off, true, 'greyed while a run is parked');
  assert.ok(nodes(tree).some((n) => words(n.props.children) === R.busySentence('r3')), 'says why');
  const abandon = button(tree, '[ abandon ]');
  assert.equal(abandon.props.off, false);
  abandon.props.on();
  assert.equal(abandoned, 'r3', 'the parked run is the one given up');
  const running = R.runGate({ fields: CHOSEN.fields, skipped: [], values: FORM.values['wiki-hop'], holder: { rid: 'r4', state: 'running' } });
  assert.equal(button(newRunTree(running, [{ rid: 'r4', graph: 'wiki-hop', state: 'running' }]), '[ abandon ]'), undefined, 'a running run is cancelled, not abandoned');
});

test('[ run ] is greyed while an action is in flight, and only then, and a start in flight says so', () => {
  const input = { chosen: CHOSEN, form: FORM, acting: '', holder: null };
  assert.equal(button(newRunTree(R.formGate(input), [], { busy: false }), '[ run ]').props.off, false);
  assert.equal(button(newRunTree(R.formGate(input), [], { busy: true }), '[ run ]').props.off, true, 'busy greys it');
  assert.equal(R.formGate({ ...input, acting: 'start' }).why, 'starting…', 'a start in flight says so');
});

test('the run shown has [ cancel ] while it runs and [ abandon ] while it is parked, and says it is cancelling once asked', () => {
  const runTree = (row, acting = '') => V.RunView({ rid: 'r1', m: null, listed: { ...ROW, ...row }, now: 2000, acting, onCancel() {}, onAbandon() {} });
  assert.equal(button(runTree({ state: 'running' }), '[ cancel ]').props.off, false);
  assert.equal(button(runTree({ state: 'running', stopping: true }), '[ cancelling… ]').props.off, true);
  assert.equal(button(runTree({ state: 'parked' }), '[ abandon ]').props.off, false);
  assert.equal(button(runTree({ state: 'parked' }, 'abandon'), '[ abandon ]').props.off, true, 'one action at a time');
  assert.equal(button(runTree({ state: 'done' }), '[ cancel ]'), undefined);
});

// ---------------------------------------------------------------- the source
test('the graph tab keeps to its rules: no markup from the perch, no gate on the decision log, no start of a service', () => {
  const files = ['../graph/runs.js', '../graph/view.js', '../graph/index.js', '../graph/steps.js', '../graph/turns.js'];
  for (const f of files) {
    const src = code(f);
    assert.doesNotMatch(src, /innerHTML|dangerouslySetInnerHTML|\beval\(|new Function|window\.open/, f);
    assert.doesNotMatch(src, /JEV_DECISIONS_LOG|restart it with/, f);
  }
  assert.doesNotMatch(code('../graph/runs.js'), /decisions_log/, 'the gate reads no service');
  // The gate and the run of the new-run form are one line each in index.js, and neither takes the health or the decision log.
  const gateLines = code('../graph/index.js').split('\n').filter((l) => /^\s*const (gate|run)\b/.test(l));
  assert.equal(gateLines.length, 2, 'the gate line and the run line');
  for (const l of gateLines) assert.doesNotMatch(l, /health|resp|decisions_log|services/, l.trim());
  // The page starts nothing: every POST the graph tab makes goes to a runs route (a service's start is never one of them).
  for (const f of files) assert.doesNotMatch(code(f), /hubPost\(\s*['"`](?!runs)/, f);
});

// The body of one top-level declaration of a source: from its line to the next line that starts at column 0 with anything but `}` or `};`.
function declOf(src, name) {
  const lines = src.split('\n');
  const at = lines.findIndex((l) => new RegExp(`^(?:export )?(?:async )?(?:function|const|let) ${name}\\b`).test(l));
  if (at < 0) return null;
  let end = at + 1;
  while (end < lines.length && (lines[end] === '}' || !/^\S/.test(lines[end]))) end += 1;
  return lines.slice(at, end).join('\n');
}

test('the periodic reads of the graph tab go through the lane and a person\'s actions do not (a source scan of runs.js)', () => {
  const src = code('../graph/runs.js');
  const decl = (name) => { const d = declOf(src, name); assert.ok(d, `runs.js declares ${name}`); return d; };
  // the periodic reads: the shown run's detail and log, the services and the list
  assert.match(decl('live'), /json: \(path\) => hubPoll\(path, 6000\)/);
  assert.match(decl('live'), /text: \(path\) => hubPollText\(path, 16000\)/);
  assert.match(decl('loadHealth'), /hubPoll\('runs\/health', 4000\)/);
  assert.match(decl('loadRuns'), /hubPoll\('runs', 6000\)/);
  assert.match(decl('refresh'), /loadHealth\(\)/);
  assert.match(decl('refresh'), /loadRuns\(true\)/);
  // those run on the timers and the page's events: none of them sends a read that skips the lane
  for (const name of ['pass', 'watch', 'openTab', 'refresh', 'loadHealth']) assert.doesNotMatch(decl(name), /\bhub(?:Get|Text|Post)\(/, name);
  // a person's actions go straight out and never wait in the lane
  for (const name of ['startRun', 'answerRun', 'cancelRun', 'abandonRun']) {
    assert.doesNotMatch(decl(name), /hubPoll/, name);
    assert.match(decl(name), /hubPost\(/, name);
  }
});
