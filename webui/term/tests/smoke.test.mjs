// What the page may ask the perch for: HUB_PATH, the whitelist every hub call passes (core/api.js). Pure: no DOM, no network (fetch is a stub).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { HUB_PATH, hubPathOk, hubGet, hubPost, hubText, hubDelete } from '../core/api.js';

const ACCEPTED = [
  'tree?session=a&path=b', 'tree?session=a&path=', 'sessions', 'sessions/x/log', 'sessions/x/first', 'sessions?x=1', 'projects', 'projects/rename',
  'dirs', 'dirs?path=%2Fhome%2Fu', 'models', 'trash', 'trash/empty', 'trash/t1/restore', 'runs', 'runs/x', 'runs/x/log?after=3', 'mesh', 'meta/tools',
  'install/keys', 'term', 'term/events', 'term/t3/input', 'notes/x/send', 'mail/abc/place',
];
// The page's own words, then what a route's name may be mistaken for.
const NOT_ROUTES = ['', '/', '/sessions', 'tree', 'treex', 'tree/x', '../x', 'termx', 'terminal', 'sessionsx', 'trashed', 'notesx/a', 'mailbox', 'file?session=a', 'redeem', 'events', 'Sessions'];
// A `..` segment, however the browser's URL parser or the server may read it.
const CLIMBS = ['sessions/../x', 'sessions/..', '../sessions', 'runs/x/../../y', 'sessions/%2e%2e/x', 'sessions/%2E%2E', 'sessions/.%2e/x', 'sessions/..%2fx',
  'sessions/a%2f..%2fb', 'sessions\\..\\x', 'sessions/x\\..', 'mesh/./..'];
// Looks like a climb and is not one.
const FINE = ['sessions/a..b/x', 'sessions/..x', 'sessions/x..', 'sessions/.../x', 'sessions/x/log?after=..', 'tree?session=a&path=..', 'dirs?path=%2Ffoo%2F..%2Fbar'];

test('HUB_PATH accepts the perch route families, tree only with a query', () => {
  for (const p of ACCEPTED) {
    assert.ok(HUB_PATH.test(p), `HUB_PATH should match ${p}`);
    assert.ok(hubPathOk(p), `hubPathOk should accept ${p}`);
  }
});

test('HUB_PATH rejects a name that only starts like a route, and anything else', () => {
  for (const p of NOT_ROUTES) {
    assert.ok(!HUB_PATH.test(p), `HUB_PATH should not match ${JSON.stringify(p)}`);
    assert.ok(!hubPathOk(p), `hubPathOk should refuse ${JSON.stringify(p)}`);
  }
  for (const p of [undefined, null, 5, {}, ['sessions']]) assert.ok(!hubPathOk(p), `hubPathOk should refuse ${String(p)}`);
});

test('a `..` segment is refused however it is spelt, and only a segment', () => {
  for (const p of CLIMBS) assert.ok(!hubPathOk(p), `hubPathOk should refuse ${p}`);
  assert.ok(!hubPathOk('sessions/%'), 'a bad escape is no path');
  for (const p of FINE) assert.ok(hubPathOk(p), `hubPathOk should accept ${p}`);
});

// The whitelist is the one hubCall uses: a revert in api.js shows here, not only in the pattern.
test('every hub call goes through the whitelist', async () => {
  const seen = [];
  const real = globalThis.fetch;
  globalThis.fetch = async (url, init) => { seen.push([init.method, url]); return { status: 200, text: async () => '[]' }; };
  try {
    for (const call of [hubGet, hubText, (p) => hubPost(p, {}), (p) => hubDelete(p, {})]) {
      for (const p of ['sessions/../x', 'termx', 'tree', '../x', 'terminal']) await assert.rejects(async () => call(p), /hub path/, p);
    }
    assert.deepEqual(seen, [], 'a refused path never reaches fetch');
    await hubGet('runs/x');
    await hubGet('tree?session=a&path=b');
    await hubPost('term/t3/input', {});
    await hubDelete('projects', { path: '/x' });
    assert.deepEqual(seen, [['GET', '/hub/runs/x'], ['GET', '/hub/tree?session=a&path=b'], ['POST', '/hub/term/t3/input'], ['DELETE', '/hub/projects']]);
  } finally { globalThis.fetch = real; }
});
