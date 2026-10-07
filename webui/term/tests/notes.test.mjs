// The notepad (notes/store.js, and notes/index.js plugging it into the registry). Pure: no DOM, no network. The autosave is driven by a clock the
// test turns by hand and an in-memory perch that answers as the Rust one does (a save names the sha it was read at; 409 with the sha now when the
// file moved on); the calls that go through core/api.js meet a stub `fetch`. What a pane draws is not here: the page's view is checked in a browser.
import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import * as R from '../core/registry.js';
import * as api from '../core/api.js';
import * as S from '../notes/store.js';
import { paneTitle, paneStatus, paneClose, ago } from '../notes/pane.js';
import '../notes/index.js';

const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
const drain = async () => { for (let i = 0; i < 3; i++) await new Promise((r) => setImmediate(r)); };
const until = async (f) => { for (let i = 0; i < 50 && !f(); i++) await new Promise((r) => setImmediate(r)); assert.ok(f(), 'it did not happen'); };

// localStorage, as a Map the test can read. Node has none (it warns when asked): the page's default storage is this one.
const store = (init = {}) => {
  const m = new Map(Object.entries(init));
  return { m, getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => { m.set(k, String(v)); }, removeItem: (k) => { m.delete(k); } };
};
Object.defineProperty(globalThis, 'localStorage', { value: store(), configurable: true, writable: true });
const KEY = (name = 'n') => S.STASH + name;

// A clock that moves only when told. `advance(ms)` runs what falls due, in order, and lets the promises that follow settle.
function clock() {
  let t = 5_000_000, id = 0;
  const q = new Map();
  return {
    now: () => t,
    set: (f, ms) => { q.set(++id, { f, at: t + ms }); return id; },
    clear: (i) => { q.delete(i); },
    get pending() { return q.size; },
    async advance(ms) {
      const end = t + ms;
      for (;;) {
        const due = [...q].filter(([, x]) => x.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        q.delete(due[0]);
        t = Math.max(t, due[1].at);
        due[1].f();
        await drain();
      }
      t = end;
      await drain();
    },
  };
}

// The perch's notes, in memory: one note whose sha moves with every accepted save. `hold` (an array) keeps the answers until `release()`;
// `fail` answers every save with that reply instead. `peak` is the most saves ever in the air at once.
function perch(text = 'hello\n') {
  const p = { text, n: 0, calls: [], active: 0, peak: 0, hold: null, fail: null };
  p.sha = 'sha0';
  p.get = async () => ({ status: 200, data: { name: 'n', text: p.text, sha: p.sha, mtime: 1700000000 } });
  p.save = (name, body, base) => {
    p.calls.push({ name, text: body, base });
    p.active++;
    p.peak = Math.max(p.peak, p.active);
    const answer = () => {
      p.active--;
      if (p.fail) return p.fail;
      if (base !== p.sha) return { status: 409, data: { error: 'the note changed', sha: p.sha } };
      p.text = body;
      p.sha = `sha${++p.n}`;
      return { status: 200, data: { sha: p.sha, mtime: 1700000001 } };
    };
    return p.hold ? new Promise((resolve) => p.hold.push(() => resolve(answer()))) : Promise.resolve(answer());
  };
  p.release = () => { const f = p.hold.shift(); if (f) f(); };
  return p;
}

// A Draft over that perch, with its own clock, storage and toasts.
function mk(p, o = {}) {
  const c = o.clock || clock(), toasts = [];
  const d = new S.Draft(o.name || 'n', { paneId: o.paneId, net: p, clock: c, storage: 'storage' in o ? o.storage : store(), say: (t, e) => toasts.push({ t, e }) });
  return { d, c, p, toasts };
}

// fetch, answering from `routes(method, url, body)` -> { status, data } | null (a 404).
const realFetch = globalThis.fetch;
function stubHub(routes) {
  const h = { calls: [] };
  globalThis.fetch = async (url, init = {}) => {
    const body = init.body ? JSON.parse(init.body) : undefined;
    h.calls.push({ method: init.method, url, body });
    const r = routes(init.method, url, body) || { status: 404, data: null };
    return { status: r.status, text: async () => (r.data == null ? '' : JSON.stringify(r.data)) };
  };
  return h;
}
const SESSIONS = (m, u) => (m === 'GET' && u === '/hub/sessions' ? { status: 200, data: [] } : null);

afterEach(async () => {
  await drain();                               // a refresh the test did not wait for finishes against its own stub
  globalThis.fetch = realFetch;
  for (const d of [...S.drafts]) d.drop();
  for (const k of Object.keys(R.host)) delete R.host[k];
});

// ---------------------------------------------------------------- the title
test('titleOf is the perch\'s title_of: the first non-empty line, control characters dropped, 120 characters', () => {
  assert.equal(S.titleOf('hello'), 'hello');
  assert.equal(S.titleOf('\n  \n  groceries  \nmilk'), 'groceries', 'blank lines are skipped, the line is trimmed');
  assert.equal(S.titleOf('a\u0000b'), 'ab', 'a control character is dropped, not turned into a space');
  assert.equal(S.titleOf('\u0007\t\n  \nreal'), 'real', 'a line of nothing but control characters is blank');
  assert.equal(S.titleOf('first\r\nsecond'), 'first', 'a CRLF file');
  assert.equal(S.titleOf(''), '');
  assert.equal(S.titleOf(' \n\t\n'), '');
  assert.equal(S.titleOf('x'.repeat(200)).length, 120);
  assert.equal([...S.titleOf('😀'.repeat(200))].length, 120, 'characters, not UTF-16 units');
  assert.equal(S.titleOf('x'.repeat(119) + ' tail'), 'x'.repeat(119), 'cut at 120, then the end is trimmed');
});

test('a draft keeps the title of its box as it is typed', async () => {
  const { d } = mk(perch('Old title\nbody'));
  await d.start();
  assert.equal(d.title.peek(), 'Old title');
  d.edit('\n\nNew first line\nmore');
  assert.equal(d.title.peek(), 'New first line');
});

// ---------------------------------------------------------------- the autosave
test('autosave waits 0.8 s after the LAST key: every key moves it, and what goes out is the latest text', async () => {
  const { d, c, p } = mk(perch());
  await d.start();
  assert.equal(d.state.peek(), 'saved');
  d.edit('one');
  assert.equal(d.state.peek(), 'dirty');
  assert.equal(c.pending, 1);
  await c.advance(799);
  assert.equal(p.calls.length, 0, 'not yet');
  d.edit('one two');
  await c.advance(799);
  assert.equal(p.calls.length, 0, 'a key 799 ms in moved the wait');
  assert.equal(c.pending, 1, 'one timer, never two');
  await c.advance(1);
  assert.equal(p.calls.length, 1);
  assert.equal(p.calls[0].text, 'one two');
  assert.equal(d.state.peek(), 'saved');
  assert.equal(c.pending, 0);
});

test('typed back to what is saved, nothing is sent', async () => {
  const { d, c, p } = mk(perch('same'));
  await d.start();
  d.edit('same!');
  d.edit('same');
  assert.equal(d.state.peek(), 'saved');
  assert.equal(c.pending, 0);
  await c.advance(5000);
  assert.equal(p.calls.length, 0);
});

test('a save names the sha it was read at, then the sha the last save returned', async () => {
  const { d, c, p } = mk(perch());
  await d.start();
  d.edit('a'); await c.advance(800);
  d.edit('ab'); await c.advance(800);
  d.edit('abc'); await c.advance(800);
  assert.deepEqual(p.calls.map((x) => x.base), ['sha0', 'sha1', 'sha2']);
  assert.equal(d.state.peek(), 'saved');
  assert.equal(p.text, 'abc');
});

test('one save in the air at a time: a key or a blur meanwhile waits, then goes out on the sha the first returned', async () => {
  const p = perch();
  p.hold = [];
  const { d, c } = mk(p);
  await d.start();
  d.edit('one'); await c.advance(800);
  assert.equal(p.calls.length, 1);
  assert.equal(d.state.peek(), 'saving');
  d.edit('one two');
  const blur = d.flush();                      // a blur, ctrl+s, a hidden tab: it waits for the one in the air
  await c.advance(30000);
  assert.equal(p.calls.length, 1, 'no second request while the first is out');
  p.release(); await drain();
  assert.equal(p.calls.length, 2);
  assert.equal(p.calls[1].base, 'sha1', 'the second names what the first returned');
  assert.equal(p.calls[1].text, 'one two');
  p.release();
  assert.equal(await blur, true);
  assert.equal(p.peak, 1, 'never two at once');
  assert.equal(d.state.peek(), 'saved');
  await c.advance(5000);
  assert.equal(p.calls.length, 2, 'and what was typed is sent once, not twice');
});

test('the same text twice sends nothing the second time', async () => {
  const { d, p } = mk(perch());
  await d.start();
  d.edit('x');
  assert.equal(await d.flush(), true);
  assert.equal(await d.flush(), true, 'nothing to save is a yes');
  assert.equal(p.calls.length, 1);
});

// ---------------------------------------------------------------- two tabs: the conflict
test('two tabs on one note: the second save is a 409, which stops the autosave until a way out is picked; keep mine writes over theirs', async () => {
  const p = perch('start\n');
  const A = mk(p), B = mk(p);
  await A.d.start(); await B.d.start();
  A.d.edit('start\nfrom a'); await A.c.advance(800);
  assert.equal(A.d.state.peek(), 'saved');
  B.d.edit('start\nfrom b'); await B.c.advance(800);
  assert.equal(B.d.state.peek(), 'conflict');
  assert.equal(B.d.theirs, p.sha, 'it keeps the sha the perch has now');
  assert.equal(p.text, 'start\nfrom a', 'nothing was overwritten');
  assert.equal(B.d.text, 'start\nfrom b', 'and b still holds its text');
  assert.equal(B.c.pending, 0, 'a conflict is not a failure: no retry is armed');
  const sent = p.calls.length;
  B.d.edit('start\nfrom b, and more');
  await B.c.advance(120000);
  assert.equal(p.calls.length, sent, 'typing on does not send');
  assert.equal(B.d.state.peek(), 'conflict');
  const was = p.sha;
  assert.equal(await B.d.keepMine(), true);
  assert.equal(p.calls[sent].base, was, 'keep mine names the perch\'s sha');
  assert.equal(p.text, 'start\nfrom b, and more');
  assert.equal(B.d.state.peek(), 'saved');
});

test('reload takes the perch\'s text into the box and the next save is on the fresh sha', async () => {
  const p = perch('start\n');
  const A = mk(p), B = mk(p);
  await A.d.start(); await B.d.start();
  A.d.edit('theirs'); await A.c.advance(800);
  B.d.edit('mine'); await B.c.advance(800);
  assert.equal(B.d.state.peek(), 'conflict');
  const rev = B.d.rev.peek();
  assert.equal(await B.d.reload(), true);
  assert.equal(B.d.text, 'theirs');
  assert.ok(B.d.rev.peek() > rev, 'the box is told to re-read its text');
  assert.equal(B.d.state.peek(), 'saved');
  assert.equal(B.d.dirty(), false);
  assert.equal(p.text, 'theirs', 'reload writes nothing');
  B.d.edit('theirs, then more'); await B.c.advance(800);
  assert.equal(p.calls.at(-1).base, 'sha1');
  assert.equal(B.d.state.peek(), 'saved');
  assert.equal(p.text, 'theirs, then more');
});

test('a note that is gone on the next save says so and keeps the text; rescue saves it as a new note', async () => {
  const p = perch();
  p.save = async () => ({ status: 404, data: null });
  const m = mk(p);
  await m.d.start();
  m.d.edit('orphan'); await m.c.advance(800);
  assert.equal(m.d.state.peek(), 'gone');
  assert.equal(m.d.text, 'orphan');
  assert.equal(m.c.pending, 0);
  const hub = stubHub((method, url, body) => (method === 'POST' && url === '/hub/notes' ? { status: 201, data: { name: 'orphan', sha: 'z'.repeat(64), mtime: 1 } } : null));
  assert.equal(await m.d.rescue(), true);
  assert.deepEqual(hub.calls[0].body, { text: 'orphan' });
  assert.equal(m.d.name.peek(), 'orphan');
  assert.equal(m.d.state.peek(), 'saved');
});

// ---------------------------------------------------------------- a save that does not land
test('a save that cannot reach the perch keeps the text, stashes it, and tries again on a growing wait', async () => {
  const p = perch();
  p.fail = { status: 0, data: null };
  const st = store();
  const { d, c } = mk(p, { storage: st });
  await d.start();
  d.edit('precious'); await c.advance(800);
  assert.equal(d.state.peek(), 'failed');
  assert.equal(d.retry, true);
  assert.equal(d.text, 'precious');
  assert.equal(JSON.parse(st.m.get(KEY())).text, 'precious');
  assert.equal(JSON.parse(st.m.get(KEY())).base, 'sha0');
  assert.equal(c.pending, 1, 'a retry is waiting');
  await c.advance(2999); assert.equal(p.calls.length, 1);
  await c.advance(1); assert.equal(p.calls.length, 2, 'after 3 s');
  await c.advance(5999); assert.equal(p.calls.length, 2);
  await c.advance(1); assert.equal(p.calls.length, 3, 'then after 6 s');
  d.edit('precious, edited'); await c.advance(800);
  assert.equal(p.calls.length, 4, 'a key tries sooner than the backoff');
  p.fail = null;
  await c.advance(30000);                      // the wait has grown to its last step
  assert.equal(d.state.peek(), 'saved');
  assert.equal(d.retry, false);
  assert.equal(p.text, 'precious, edited');
  assert.equal(st.m.has(KEY()), false, 'the stash goes once the perch has the text');
});

test('a net that throws is a failed save, not a lost one', async () => {
  const m = mk({ get: async () => ({ status: 200, data: { name: 'n', text: '', sha: 's', mtime: 1 } }), save: async () => { throw new Error('boom'); } });
  await m.d.start();
  m.d.edit('kept'); await m.c.advance(800);
  assert.equal(m.d.state.peek(), 'failed');
  assert.equal(m.d.text, 'kept');
  assert.equal(m.d.retry, true);
});

test('a refusal (413, 400, 401) is said in words and is not retried', async () => {
  for (const [status, data, said] of [[413, { error: 'x' }, /256 KiB/], [400, { error: 'no NUL bytes in a note' }, /no NUL bytes/], [401, null, /refused the login/]]) {
    const p = perch();
    p.fail = { status, data };
    const { d, c } = mk(p);
    await d.start();
    d.edit('text'); await c.advance(800);
    assert.equal(d.state.peek(), 'failed', String(status));
    assert.match(d.why.peek(), said, String(status));
    assert.equal(d.retry, false, String(status));
    assert.equal(c.pending, 0, String(status));
    await c.advance(120000);
    assert.equal(p.calls.length, 1, `${status}: sent once`);
    assert.equal(d.text, 'text');
  }
});

test('the text is in the stash before the request leaves, and gone from it once the perch has it', async () => {
  const p = perch();
  const st = store();
  const seen = [];
  const send = p.save;
  p.save = (n, text, base) => { seen.push(st.m.get(KEY())); return send(n, text, base); };
  const { d } = mk(p, { storage: st });
  await d.start();
  d.edit('before the request');
  await d.flush();
  assert.equal(JSON.parse(seen[0]).text, 'before the request');
  assert.equal(st.m.has(KEY()), false);
});

test('a note over 256 KiB is refused by the page itself, counted in UTF-8 bytes; exactly 256 KiB goes out', async () => {
  const { d, c, p } = mk(perch());
  await d.start();
  d.edit('x'.repeat(S.MAX_BYTES + 1)); await c.advance(800);
  assert.equal(p.calls.length, 0, 'no request');
  assert.equal(d.state.peek(), 'failed');
  assert.match(d.why.peek(), /too big/);
  assert.equal(d.retry, false);
  d.edit('é'.repeat(S.MAX_BYTES / 2 + 1)); await c.advance(800);
  assert.equal(p.calls.length, 0, 'fewer characters than the limit, more bytes');
  d.edit('é'.repeat(S.MAX_BYTES / 2)); await c.advance(800);
  assert.equal(p.calls.length, 1, 'exactly the limit is a note');
  assert.equal(d.state.peek(), 'saved');
});

// ---------------------------------------------------------------- the stash, read back
test('text an earlier session did not get to send comes back: on the same sha it saves itself; on a moved note it is a conflict', async () => {
  const p = perch('v1\n');
  const same = store({ [KEY()]: JSON.stringify({ text: 'v1\nlost words', base: 'sha0', at: 5_000_000 }) });
  const a = mk(p, { storage: same });
  await a.d.start();
  assert.equal(a.d.text, 'v1\nlost words');
  assert.equal(a.d.state.peek(), 'dirty');
  assert.equal(a.toasts.length, 1, 'it says so');
  await a.c.advance(0);
  assert.equal(a.d.state.peek(), 'saved');
  assert.equal(p.text, 'v1\nlost words');
  assert.equal(same.m.has(KEY()), false);

  const q = perch('v2\n');
  const moved = store({ [KEY()]: JSON.stringify({ text: 'typed on v1', base: 'an-older-sha', at: 5_000_000 }) });
  const b = mk(q, { storage: moved });
  await b.d.start();
  assert.equal(b.d.state.peek(), 'conflict');
  assert.equal(b.d.text, 'typed on v1');
  assert.equal(b.d.theirs, 'sha0');
  assert.equal(q.calls.length, 0, 'nothing is sent over a note that moved');
  assert.equal(await b.d.reload(), true);
  assert.equal(b.d.text, 'v2\n');
});

test('a stash older than 30 days, or the same as the file, is dropped, not restored', async () => {
  const p = perch('on disk');
  const old = store({ [KEY()]: JSON.stringify({ text: 'ancient', base: 'sha0', at: 5_000_000 - 31 * 864e5 }) });
  const a = mk(p, { storage: old });
  await a.d.start();
  assert.equal(a.d.text, 'on disk');
  assert.equal(a.d.state.peek(), 'saved');
  assert.equal(old.m.has(KEY()), false);
  const dup = store({ [KEY()]: JSON.stringify({ text: 'on disk', base: 'sha0', at: 5_000_000 }) });
  const b = mk(p, { storage: dup });
  await b.d.start();
  assert.equal(b.d.state.peek(), 'saved');
  assert.equal(dup.m.has(KEY()), false);
  assert.equal(p.calls.length, 0);
});

test('a stash that is not JSON, or has the wrong shape, is ignored', async () => {
  for (const junk of ['not json', '{"text": 5}', 'null', '[]']) {
    const m = mk(perch('file'), { storage: store({ [KEY()]: junk }) });
    await m.d.start();
    assert.equal(m.d.text, 'file', junk);
    assert.equal(m.d.state.peek(), 'saved', junk);
  }
});

test('with no storage (private mode) everything still works', async () => {
  const { d, c, p } = mk(perch(), { storage: null });
  await d.start();
  d.edit('fine'); await c.advance(800);
  assert.equal(p.text, 'fine');
});

test('a note that cannot be read says why and sends nothing', async () => {
  const gone = mk({ get: async () => ({ status: 404, data: null }), save: async () => assert.fail('no save') });
  await gone.d.start();
  assert.equal(gone.d.state.peek(), 'gone');
  gone.d.edit('x'); await gone.c.advance(5000);
  const bin = mk({ get: async () => ({ status: 422, data: { error: 'x' } }), save: async () => assert.fail('no save') });
  await bin.d.start();
  assert.equal(bin.d.state.peek(), 'error');
  assert.match(bin.d.why.peek(), /UTF-8/);
  const odd = mk(perch(), { name: '../etc' });
  await odd.d.start();
  assert.equal(odd.d.state.peek(), 'error', 'a name outside the grammar is never joined to a path');
});

test('text with CRLF in the file is not a change the person made', async () => {
  const { d, p } = mk(perch('a\r\nb\r\n'));
  await d.start();
  assert.equal(d.state.peek(), 'saved');
  d.edit('a\nb\n');                         // a textarea hands back LF
  assert.equal(d.state.peek(), 'saved');
  assert.equal(p.calls.length, 0);
});

// ---------------------------------------------------------------- leaving a note
test('a rename or a trash saves the box first, writes nothing to the old name while it runs, and saves what was typed after', async () => {
  const { d, c, p } = mk(perch());
  await d.start();
  d.edit('before');
  let during = -1;
  const out = await d.exclusive(async () => {
    assert.equal(p.calls.length, 1, 'the box went out first');
    assert.equal(p.text, 'before');
    d.edit('typed during');
    await d.flush();                         // a blur while the file moves
    during = p.calls.length;
    return { ok: true };
  });
  assert.deepEqual(out, { ok: true });
  assert.equal(during, 1, 'nothing was sent while it ran');
  await c.advance(0);
  assert.equal(p.calls.length, 2);
  assert.equal(p.text, 'typed during');
});

test('a rename is not run over an unsaved or conflicting box', async () => {
  const p = perch();
  const A = mk(p), B = mk(p);
  await A.d.start(); await B.d.start();
  A.d.edit('a'); await A.c.advance(800);
  B.d.edit('b'); await B.c.advance(800);
  assert.equal(B.d.state.peek(), 'conflict');
  let ran = false;
  const r = await B.d.exclusive(async () => { ran = true; return { ok: true }; });
  assert.equal(ran, false);
  assert.equal(r.ok, false);
  assert.match(r.why, /conflict/);
});

test('closing a pane saves what is unsaved; when the perch will not take it, it says so and the text stays in the stash', async () => {
  const ok = mk(perch());
  await ok.d.start();
  ok.d.edit('last words');
  await ok.d.close();
  assert.equal(ok.p.text, 'last words');
  assert.equal(S.drafts.has(ok.d), false, 'the draft is forgotten');
  assert.equal(ok.toasts.length, 0);

  const p = perch();
  p.fail = { status: 0, data: null };
  const st = store();
  const bad = mk(p, { storage: st });
  await bad.d.start();
  bad.d.edit('not delivered');
  await bad.d.close();
  assert.equal(bad.toasts.length, 1);
  assert.match(bad.toasts[0].t, /was not saved/);
  assert.equal(bad.toasts[0].e, true);
  assert.equal(JSON.parse(st.m.get(KEY())).text, 'not delivered');
  assert.equal(bad.c.pending, 0, 'a closed pane retries nothing');
});

test('a hidden tab or a closing page sends every open note (flushAll)', async () => {
  const p = perch();
  const A = mk(p, { name: 'a' }), B = mk(p, { name: 'b' });
  await A.d.start(); await B.d.start();
  A.d.edit('one');
  B.d.edit('two');
  S.flushAll();
  await drain();
  assert.deepEqual(p.calls.map((x) => x.name).sort(), ['a', 'b']);
});

test('setName moves a note\'s stash to its new file name', () => {
  const st = store({ [KEY('old-name')]: '{"text":"t","base":"b","at":1}' });
  const d = new S.Draft('old-name', { net: perch(), clock: clock(), storage: st, say: () => {} });
  d.setName('new-name');
  assert.equal(d.name.peek(), 'new-name');
  assert.equal(st.m.has(KEY('old-name')), false);
  assert.equal(st.m.get(KEY('new-name')), '{"text":"t","base":"b","at":1}');
});

test('the pane kind\'s title, status and close read the pane\'s draft; closing saves it', async () => {
  const hub = stubHub((m, u) => (u === '/hub/notes/n' && m === 'GET' ? { status: 200, data: { name: 'n', text: 'old\n', sha: 's0', mtime: 1 } }
    : u === '/hub/notes/n' && m === 'POST' ? { status: 200, data: { sha: 's1', mtime: 2 } } : null));
  const pane = { id: 'p41', kind: 'note', name: 'n' };
  assert.equal(paneTitle(pane), 'note n');
  assert.equal(paneStatus(pane), 'note', 'no draft yet');
  const d = S.draftOf(pane);
  assert.equal(S.draftOf(pane), d, 'one draft per pane');
  await d.start();
  assert.equal(paneStatus(pane), 'note n · saved');
  d.edit('old\nplus');
  assert.equal(paneStatus(pane), 'note n · unsaved');
  paneClose(pane);
  await d.closing;
  const post = hub.calls.find((c) => c.method === 'POST');
  assert.deepEqual(post.body, { text: 'old\nplus', base: 's0' });
  assert.equal(S.draftFor('p41'), null);
});

test('renaming an open note saves it first and the box follows the file; trashing it closes its pane', async () => {
  const p = perch();
  const m = mk(p, { name: 'draft-one', paneId: 'p77' });
  await m.d.start();
  m.d.edit('typed');
  let savedBeforeRename = -1;
  const hub = stubHub((method, url) => {
    if (url === '/hub/notes/draft-one/rename') { savedBeforeRename = p.calls.length; return { status: 200, data: { name: 'better-name' } }; }
    if (url === '/hub/notes/better-name/trash') return { status: 204, data: null };
    return null;
  });
  const closed = [];
  R.host.closePane = (id) => { closed.push(id); };
  const r = await S.renameNote('draft-one', 'Better Name');
  assert.deepEqual(r, { ok: true, name: 'better-name' });
  assert.equal(savedBeforeRename, 1, 'the box went to the perch before the file moved');
  assert.equal(m.d.name.peek(), 'better-name');
  assert.deepEqual(hub.calls.find((c) => c.url.endsWith('/rename')).body, { title: 'Better Name' });
  const t = await S.trashNote('better-name');
  assert.deepEqual(t, { ok: true });
  assert.deepEqual(closed, ['p77']);
  assert.equal(m.d.closed, true);
  assert.equal(S.drafts.has(m.d), false);
});

test('a bad name never reaches a URL', async () => {
  const hub = stubHub(() => null);
  for (const bad of ['../x', 'a/b', 'A', '', 'x'.repeat(49), '-x']) {
    for (const f of [S.renameNote, S.trashNote, S.restoreNote]) assert.equal((await f(bad, 't')).ok, false, `${f.name}(${bad})`);
    assert.equal(S.openNote(bad), null);
  }
  assert.equal(hub.calls.length, 0);
});

// ---------------------------------------------------------------- the lists, and :note
test('a perch without --home answers 404 on the list: notes are off, not an error; then they come back', async () => {
  stubHub((m, u) => SESSIONS(m, u) || (u === '/hub/notes' ? { status: 404, data: null } : null));
  assert.equal(await api.detect(), 'perch');
  await S.refresh();
  assert.equal(S.off.peek(), S.OFF);
  assert.deepEqual(S.rows.peek(), []);
  stubHub((m, u) => SESSIONS(m, u) || (u === '/hub/notes' ? { status: 200, data: [{ name: 'a', title: 'A', size: 1, mtime: 2 }, { name: 'Bad Name', title: 'x', size: 1, mtime: 1 }] }
    : u === '/hub/notes/trash' ? { status: 200, data: [{ name: 'gone-1', title: 'g', size: 1, mtime: 1 }] } : null));
  await S.refresh();
  assert.equal(S.off.peek(), '');
  assert.deepEqual(S.rows.peek().map((r) => r.name), ['a'], 'a row with a name outside the grammar is left out');
  assert.deepEqual(S.trashRows.peek().map((r) => r.name), ['gone-1']);
});

test('a save puts the note at the top of the list at once, with the title of what was saved', async () => {
  stubHub((m, u) => SESSIONS(m, u) || (u === '/hub/notes' ? { status: 200, data: [{ name: 'old', title: 'old', size: 3, mtime: 9 }, { name: 'n', title: 'n', size: 1, mtime: 1 }] }
    : u === '/hub/notes/trash' ? { status: 200, data: [] } : null));
  await api.detect();
  await S.refresh();
  assert.deepEqual(S.rows.peek().map((r) => r.name), ['old', 'n']);
  const { d, c } = mk(perch());
  await d.start();
  d.edit('Fresh title\nbody'); await c.advance(800);
  const top = S.rows.peek()[0];
  assert.equal(top.name, 'n');
  assert.equal(top.title, 'Fresh title');
  assert.equal(top.size, 16);
  assert.equal(top.mtime, Math.floor(c.now() / 1000));
  assert.deepEqual(S.rows.peek().map((r) => r.name), ['n', 'old']);
});

test('`:note groceries` makes a note titled groceries and opens it in a note pane', async () => {
  const hub = stubHub((m, u, body) => SESSIONS(m, u)
    || (m === 'POST' && u === '/hub/notes' ? { status: 201, data: { name: body.title ? 'groceries' : 'note-1', sha: 'a'.repeat(64), mtime: 1 } } : null)
    || (m === 'GET' && (u === '/hub/notes' || u === '/hub/notes/trash') ? { status: 200, data: [] } : null));
  await api.detect();
  const opened = [], toasts = [];
  R.host.xpanes = () => [];
  R.host.openXPane = (kind, data) => { opened.push({ kind, data }); return { id: 'p1', kind, ...data }; };
  R.host.toast = (t, e) => toasts.push({ t, e });
  const posts = () => hub.calls.filter((c) => c.method === 'POST' && c.url === '/hub/notes');

  const cmd = R.commands.get('note');
  assert.ok(cmd, 'the command is registered');
  cmd.run('groceries');
  await until(() => opened.length === 1);
  assert.deepEqual(posts()[0].body, { title: 'groceries' }, 'the title is the perch\'s to turn into a file name');
  assert.deepEqual(opened[0], { kind: 'note', data: { name: 'groceries', fresh: true } });
  assert.match(toasts[0].t, /groceries/);
  assert.equal(toasts[0].e, undefined);

  cmd.run('  weekly shop  ');
  await until(() => posts().length === 2);
  assert.deepEqual(posts()[1].body, { title: 'weekly shop' }, 'trimmed');

  await until(() => opened.length === 2);
  cmd.run('');
  await until(() => posts().length === 3);
  assert.deepEqual(posts()[2].body, {}, 'no title: the perch names it');
  cmd.run('   ');
  await until(() => posts().length === 4);
  assert.deepEqual(posts()[3].body, {});
});

test('createNote sends a title, a text, or both, and says why when the perch refuses', async () => {
  const hub = stubHub((m, u) => (m === 'POST' && u === '/hub/notes' ? { status: 201, data: { name: 'x1', sha: 'a'.repeat(64), mtime: 7 } } : null));
  assert.deepEqual(await S.createNote({ title: 't', text: 'body' }), { ok: true, name: 'x1', sha: 'a'.repeat(64), mtime: 7 });
  assert.deepEqual(hub.calls[0].body, { title: 't', text: 'body' });
  await S.createNote();
  assert.deepEqual(hub.calls[1].body, {});
  stubHub(() => ({ status: 404, data: null }));
  const off = await S.createNote({ title: 't' });
  assert.equal(off.ok, false);
  assert.equal(off.why, S.OFF);
  stubHub(() => ({ status: 201, data: { name: '../evil', sha: 's', mtime: 1 } }));
  assert.equal((await S.createNote({})).ok, false, 'a name outside the grammar is not trusted');
  stubHub(() => ({ status: 413, data: { error: 'x' } }));
  assert.match((await S.createNote({ text: 'x' })).why, /256 KiB/);
});

test('`:note` with no hub (a bare door) makes nothing and says so', async () => {
  const hub = stubHub((m, u) => (u === '/hub/sessions' ? { status: 404, data: null } : null));
  assert.equal(await api.detect(), 'door');
  const toasts = [];
  R.host.toast = (t, e) => toasts.push({ t, e });
  assert.equal(await S.newNote('x'), null);
  assert.equal(toasts.length, 1);
  assert.equal(toasts[0].e, true);
  assert.equal(hub.calls.filter((c) => c.method === 'POST').length, 0);
});

test('a note that is open is focused, not opened twice', () => {
  const focused = [], opened = [];
  R.host.xpanes = () => [{ id: 'p9', kind: 'note', name: 'groceries' }, { id: 'p8', kind: 'chat' }];
  R.host.focusPane = (id) => focused.push(id);
  R.host.openXPane = (kind, data) => { opened.push([kind, data]); return { id: 'p10' }; };
  assert.equal(S.openNote('groceries').id, 'p9');
  assert.deepEqual(focused, ['p9']);
  assert.deepEqual(opened, []);
  S.openNote('other');
  assert.deepEqual(opened, [['note', { name: 'other', fresh: false }]]);
});

// ---------------------------------------------------------------- the plug into the page
test('the notepad registers through the registry: the ✎ tab with a badge signal, the note pane kind, :note, the notepad focus name', () => {
  const tab = R.leftTabs.get('notes');
  assert.equal(tab.icon, '✎');
  assert.equal(typeof tab.component, 'function');
  assert.equal(typeof tab.badge.peek, 'function', 'the badge is a signal (N2b\'s cards.js drives it)');
  assert.ok(R.tabNames('l').includes('notes'));
  const kind = R.paneKinds.get('note');
  assert.equal(typeof kind.Pane, 'function');
  for (const f of ['title', 'status', 'close']) assert.equal(typeof kind[f], 'function', f);
  assert.match(R.commands.get('note').help, /:note \[title\]/);
  const inPad = { closest: (s) => (s === '.notepad' ? {} : null) }, outside = { closest: () => null };
  assert.equal(R.whereOf(inPad), 'notepad', 'app.js already says `note` for an ask\'s note line');
  assert.equal(R.whereOf(outside), null);
  assert.equal(R.focusClaimed(inPad), true, 'an ask never takes the box\'s focus');
  assert.equal(R.focusClaimed(outside), false);
  assert.ok(R.keyRows.peek().some((r) => r.section === 'notes' && r.keys === '^s'));
});

test('the stubs N2b and M3 fill keep the names the tab and the pane import', async () => {
  const cards = await import('../notes/cards.js'), send = await import('../notes/send.js');
  assert.equal(typeof cards.badge.peek, 'function');
  assert.equal(typeof cards.Received, 'function');
  assert.equal(typeof send.Sent, 'function');
  assert.equal(typeof send.sendNote, 'function');
  assert.equal(typeof send.sendOff, 'function');
  assert.equal(typeof send.sendOff({ name: 'n' }), 'string', 'a reason sending is off, until N2b says it is on');
  const tab = src('../notes/tab.js'), pane = src('../notes/pane.js');
  assert.match(tab, /import \{ Received \} from '\.\/cards\.js'/);
  assert.match(tab, /import \{ Sent, sendOff, sendNote \} from '\.\/send\.js'/);
  assert.match(pane, /import \{ sendOff, sendNote \} from '\.\/send\.js'/);
});

test('ago says how long ago in one short word', () => {
  assert.deepEqual([0, 999, 1000, 59000, 60000, 3_599_000, 3_600_000, 86_400_000, -5].map(ago), ['now', 'now', '1s', '59s', '1m', '59m', '1h', '1d', 'now']);
});

// ---------------------------------------------------------------- the page's rules, held in the source
test('the notepad folder keeps the page\'s rules: no <button>, no innerHTML, no raw fetch, no eval, square edges', () => {
  for (const f of ['tab.js', 'pane.js', 'store.js', 'index.js', 'cards.js', 'send.js', 'handoff.js', 'agentmail.js']) {
    const s = src('../notes/' + f);
    assert.doesNotMatch(s, /<button/i, `${f}: the page has no buttons: [ bracketed ] text`);
    assert.doesNotMatch(s, /innerHTML|insertAdjacentHTML|document\.write/, `${f}: a note is text, always drawn as text`);
    assert.doesNotMatch(s, /\bfetch\s*\(/, `${f}: every call goes through core/api.js (it checks the path)`);
    assert.doesNotMatch(s, /\beval\s*\(|new Function/, f);
  }
  const css = src('../notes/notes.css');
  assert.doesNotMatch(css, /border-radius\s*:\s*(?!0\s*[;}])/, 'square edges');
  assert.doesNotMatch(css, /@import|url\(\s*['"]?https?:/, 'nothing from outside the page');
});
