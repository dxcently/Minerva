// The request lane (core/lane.js): every periodic GET waits its turn, one in flight across the page. Pure: fetch is a stub that answers when
// the test says so, so what is in flight at any moment is exactly what the test sees.
import { test, mock, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { hubGet, hubPost, detect, mode } from '../core/api.js';
import { hubPoll, hubPollText, LANE_TIMEOUT_MS } from '../core/lane.js';
import { refresh, list, trash } from '../core/hub.js';

// A fetch whose answers are held until `answer` / `fail`. `calls` is every URL asked for, in order; `peak` the most ever in flight at once.
function fakeFetch() {
  const f = { calls: [], inflight: 0, peak: 0, held: [] };
  f.fetch = (url, init) => new Promise((resolve, reject) => {
    f.calls.push(url);
    f.peak = Math.max(f.peak, ++f.inflight);
    const e = { url };
    e.ok = (body, status) => { f.inflight--; resolve({ status, text: async () => body }); };
    e.no = (err) => { f.inflight--; reject(err); };
    if (init && init.signal) init.signal.addEventListener('abort', () => e.no(Object.assign(new Error('aborted'), { name: 'AbortError' })));
    f.held.push(e);
  });
  const take = (url) => { const i = f.held.findIndex((e) => e.url === url); assert.ok(i >= 0, `${url} is not in flight; in flight: ${f.held.map((e) => e.url)}`); return f.held.splice(i, 1)[0]; };
  f.answer = (url, body = '[]', status = 200) => take(url).ok(body, status);
  f.fail = (url) => take(url).no(new TypeError('fetch failed'));
  return f;
}
const settle = () => new Promise((r) => setImmediate(r)); // every promise step already queued has run

let f, real;
beforeEach(() => { f = fakeFetch(); real = globalThis.fetch; globalThis.fetch = f.fetch; });
afterEach(() => { globalThis.fetch = real; mock.timers.reset(); });

test('at most one request is in flight across 5 pollers, first in first out', async () => {
  const paths = ['sessions', 'trash', 'runs', 'mesh', 'meta/tools'];
  const polls = paths.map((p) => hubPoll(p));
  await settle();
  assert.deepEqual(f.calls, ['/hub/sessions'], 'only the first starts');
  for (const p of paths) { f.answer('/hub/' + p, JSON.stringify({ p })); await settle(); }
  const got = await Promise.all(polls);
  assert.equal(f.peak, 1);
  assert.deepEqual(f.calls, paths.map((p) => '/hub/' + p), 'in the order they were asked');
  assert.deepEqual(got.map((r) => r.data.p), paths, 'each caller gets its own path\'s answer');
});

test('a queued duplicate path makes one fetch and both callers get its answer', async () => {
  const busy = hubPoll('sessions');
  const a = hubPoll('runs/r1'), b = hubPoll('runs/r1');
  await settle();
  assert.equal(a, b, 'the same promise');
  f.answer('/hub/sessions');
  await busy;
  await settle();
  f.answer('/hub/runs/r1', '{"n":1}');
  const [ra, rb] = await Promise.all([a, b]);
  assert.deepEqual(ra, { status: 200, data: { n: 1 } });
  assert.deepEqual(rb, ra);
  assert.deepEqual(f.calls, ['/hub/sessions', '/hub/runs/r1'], 'runs/r1 was fetched once');
});

test('a path in flight is not joined: the next caller waits as one fresh request, queued once', async () => {
  const first = hubPoll('sessions');
  const second = hubPoll('sessions'), third = hubPoll('sessions');
  await settle();
  assert.equal(second, third);
  assert.notEqual(first, second);
  f.answer('/hub/sessions', '[1]');
  assert.deepEqual((await first).data, [1]);
  await settle();
  f.answer('/hub/sessions', '[2]');
  assert.deepEqual((await second).data, [2]);
  assert.deepEqual((await third).data, [2]);
  assert.deepEqual(f.calls, ['/hub/sessions', '/hub/sessions']);
});

test('text and JSON polls of one path are two requests', async () => {
  const j = hubPoll('runs/r1/log'), t = hubPollText('runs/r1/log');
  await settle();
  f.answer('/hub/runs/r1/log', '{"a":1}');
  await settle();
  f.answer('/hub/runs/r1/log', '{"a":1}\n{"a":2}\n');
  assert.deepEqual((await j).data, { a: 1 });
  assert.equal((await t).data, '{"a":1}\n{"a":2}\n', 'the text poll hands the body back unparsed');
});

test('a hubPost or a hubGet issued while the lane is busy starts at once', async () => {
  const polls = [hubPoll('sessions'), hubPoll('trash')];
  await settle();
  const post = hubPost('sessions/x/stop');
  const get = hubGet('models');
  const typed = hubPost('term/t1/input', {});
  await settle();
  assert.deepEqual(f.calls, ['/hub/sessions', '/hub/sessions/x/stop', '/hub/models', '/hub/term/t1/input'], 'the poll in flight, the actions beside it, trash still waiting');
  assert.equal(f.peak, 4);
  for (const u of ['/hub/sessions/x/stop', '/hub/models', '/hub/term/t1/input']) f.answer(u);
  await Promise.all([post, get, typed]);
  f.answer('/hub/sessions');
  await settle();
  f.answer('/hub/trash');
  await Promise.all(polls);
});

test('a failed poll frees the lane', async () => {
  const down = hubPoll('sessions'), next = hubPoll('trash');
  await settle();
  f.fail('/hub/sessions');
  assert.deepEqual(await down, { status: 0, data: { error: 'network error' } });
  await settle();
  assert.deepEqual(f.calls, ['/hub/sessions', '/hub/trash'], 'the next poll started');
  f.answer('/hub/trash');
  assert.equal((await next).status, 200);
});

test('a poll that throws frees the lane for the rest, and rejects only its own callers', async () => {
  const bad = hubPoll('sessions/../x'), ok = hubPoll('trash');
  await assert.rejects(bad, /hub path/);
  await settle();
  f.answer('/hub/trash');
  assert.equal((await ok).status, 200);
  assert.deepEqual(f.calls, ['/hub/trash']);
});

test('a poll the perch never answers is aborted at its timeout, and the lane moves on', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const hung = hubPoll('sessions', 5000), next = hubPoll('trash', 5000);
  await settle();
  mock.timers.tick(4999);
  await settle();
  assert.deepEqual(f.calls, ['/hub/sessions'], 'still waiting at 4999 ms');
  mock.timers.tick(1);
  assert.deepEqual((await hung).status, 0);
  await settle();
  assert.deepEqual(f.calls, ['/hub/sessions', '/hub/trash']);
  f.answer('/hub/trash');
  await next;
});

test('a poll that names no timeout still gets the lane\'s, so one hung request cannot hold the lane for ever', async () => {
  mock.timers.enable({ apis: ['setTimeout'] });
  const hung = hubPoll('runs'), next = hubPoll('mesh');
  await settle();
  mock.timers.tick(LANE_TIMEOUT_MS - 1);
  await settle();
  assert.deepEqual(f.calls, ['/hub/runs']);
  mock.timers.tick(1);
  assert.equal((await hung).status, 0);
  await settle();
  assert.deepEqual(f.calls, ['/hub/runs', '/hub/mesh']);
  f.answer('/hub/mesh');
  await next;
});

// core/hub.js polls the list and the trash through the lane: while something else holds it, neither GET is sent.
test('the session list and the trash poll wait their turn in the lane', async () => {
  const probe = detect();
  await settle();
  f.answer('/hub/sessions', '[]');
  assert.equal(await probe, 'perch');
  assert.equal(mode(), 'perch');
  f.calls.length = 0;
  const other = hubPoll('runs');
  const refreshed = refresh();
  await settle();
  assert.deepEqual(f.calls, ['/hub/runs'], 'the list waits behind the poll in flight');
  f.answer('/hub/runs');
  await other;
  await settle();
  assert.deepEqual(f.calls, ['/hub/runs', '/hub/sessions']);
  f.answer('/hub/sessions', JSON.stringify([{ id: 'a', state: 'live' }]));
  await settle();
  f.answer('/hub/trash', JSON.stringify([{ id: 't', title: 'gone' }]));
  await refreshed;
  assert.equal(f.peak, 1, 'one request at a time through the whole refresh');
  assert.deepEqual(list.value, [{ id: 'a', state: 'live' }]);
  assert.deepEqual(trash.value, [{ id: 't', title: 'gone' }]);
});
