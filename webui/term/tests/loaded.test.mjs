// The loaded and diff tabs (panes/loaded.js, panes/diff.js): what they say from what /hub/meta answers. Pure: the components need a DOM and
// are not drawn here; the words, the merge of the tool list with the calls seen, the store of answers and the paths are. core/state.js (which
// loaded.js imports for `tildify`) reads matchMedia at load, so it is stubbed before the dynamic import.
import { test, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { detect, mode, hubPathOk } from '../core/api.js';
import { hubPoll } from '../core/lane.js';

globalThis.matchMedia = () => ({ matches: false, addEventListener() {}, addListener() {} });
const L = await import('../panes/loaded.js');
const D = await import('../panes/diff.js');

// As LF: a checkout with core.autocrlf has CRLF sources.
const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
const settle = () => new Promise((r) => setImmediate(r));
// A source file's code: whole-line comments left out, so a file may say in a comment what it does not do.
const code = (rel) => src(rel).split('\n').filter((l) => !/^\s*\/\//.test(l)).join('\n');

// ---------------------------------------------------------------- the merge of the tool list with the calls
const LIST = [
  { name: 'read', approval: 'read_only', desc: 'Read a file.' },
  { name: 'write', approval: 'mutating' },
  { name: 'bash', approval: 'mutating' },
  { name: 'browser_click', approval: 'mutating', plugin: 'browser' },
  { name: 'jev_graph', approval: 'read_only', plugin: 'jev', desc: 'Run a jev graph.' },
];

test('the full tool list merges with the used counts, used first, in eidolon\'s order, then the unlisted ones called, then the rest', () => {
  const used = new Map([['jev_graph', 2], ['mystery', 1], ['write', 5], ['aaa_unlisted', 3]]);
  const rows = L.mergeTools(LIST, used);
  assert.deepEqual(rows.map((t) => [t.name, t.n]), [
    ['write', 5], ['jev_graph', 2],             // called, as eidolon lists them (not by count)
    ['aaa_unlisted', 3], ['mystery', 1],        // called but not in the list: by name
    ['read', 0], ['bash', 0], ['browser_click', 0], // not called: eidolon's order
  ]);
  const click = rows.find((t) => t.name === 'browser_click');
  assert.deepEqual(click, { name: 'browser_click', approval: 'mutating', plugin: 'browser', desc: '', n: 0 }, 'the fields come through, absent ones as empty text');
  assert.equal(rows.find((t) => t.name === 'jev_graph').desc, 'Run a jev graph.');
  assert.equal(rows.length, LIST.length + 2, 'every listed tool once, plus the two unlisted ones');
});

test('with no list the tools called are all there is, by name; pairs, zero counts and junk are accepted', () => {
  assert.deepEqual(L.mergeTools(null, new Map([['write', 2], ['bash', 1], ['edit', 0]])).map((t) => [t.name, t.n]), [['bash', 1], ['write', 2]]);
  assert.deepEqual(L.mergeTools(undefined, [['b', 1], ['a', 4]]).map((t) => t.name), ['a', 'b'], 'an array of pairs works as a Map');
  assert.deepEqual(L.mergeTools([null, { name: 3 }, { name: 'ok' }, 'x'], null).map((t) => t.name), ['ok'], 'entries without a text name are left out');
  assert.deepEqual(L.mergeTools([], new Map()), []);
  assert.deepEqual(L.mergeTools(LIST, new Map()).map((t) => t.name), LIST.map((t) => t.name), 'nothing called: the list as it came');
});

// ---------------------------------------------------------------- the guard line, the gate note
test('the guard line: on names the file, off says why, the stub is the off case', () => {
  assert.equal(L.guardLine({ on: true, file: '~/.config/eidolon/minerva-guard.rn' }), 'guard rows on (~/.config/eidolon/minerva-guard.rn)');
  assert.equal(L.guardLine({ on: false, file: null, why: 'the policy directory is not writable' }), 'guard rows are off: the policy directory is not writable');
  assert.equal(L.guardLine({ on: false, file: null, why: 'not built yet' }), 'guard rows are off: not built yet', 'S0P-a\'s stub, as /hub/meta/policy sends it');
  assert.equal(L.guardLine({ on: false }), 'guard rows are off: no reason given');
  assert.equal(L.guardLine({ on: true, file: null }), 'guard rows on');
  assert.equal(L.guardLine(null), '');
  assert.equal(L.guardLine(undefined), '');
});

test('a guard that is on with the note about `enabled = false` draws eidolon\'s gate sentence; no note, no line', () => {
  const sentence = "eidolon's gate is on in Minerva auto (your config turns it off elsewhere)";
  assert.equal(L.gateNote({ on: true, file: '~/g.rn', why: 'your config has [policy] enabled = false; Minerva turns it on' }), sentence);
  assert.equal(L.gateNote({ on: true, file: '~/g.rn', why: 'enabled=false in [policy]' }), sentence, 'however the note spaces it');
  assert.equal(L.GATE_NOTE, sentence);
  assert.equal(L.gateNote({ on: true, file: '~/g.rn' }), '', 'a plain on has no note');
  assert.equal(L.gateNote({ on: false, file: null, why: 'enabled = false' }), '', 'an off guard says its why in the guard line');
  assert.equal(L.gateNote({ on: true, file: '~/g.rn', why: 'some other note' }), 'some other note', 'a note of another kind is shown as the guard wrote it');
  assert.equal(L.gateNote(null), '');
});

// ---------------------------------------------------------------- the default mode, the connectors
test('the default mode line names where the mode came from', () => {
  assert.equal(L.homeLine({ default_mode: 'auto', source: 'minerva.toml' }), 'new chats: auto (minerva.toml)');
  assert.equal(L.homeLine({ default_mode: 'yolo', source: 'unset' }), 'new chats: yolo (unset → yolo)');
  assert.equal(L.homeLine({ default_mode: 'manual', source: 'somewhere' }), 'new chats: manual (somewhere)');
  assert.equal(L.homeLine({ default_mode: '', source: 'unset' }), '');
  assert.equal(L.homeLine(null), '');
});

test('the connectors line marks each of mneme and melete, and says eidolon has no MCP list', () => {
  assert.equal(L.connectorsLine({ mneme: true, melete: false }), 'eidolon has no MCP client list; its connectors: mneme ✓ melete ✗');
  assert.equal(L.connectorsLine({ mneme: false, melete: true }), 'eidolon has no MCP client list; its connectors: mneme ✗ melete ✓');
  assert.equal(L.connectorsLine({ mneme: true, melete: true }), 'eidolon has no MCP client list; its connectors: mneme ✓ melete ✓');
  assert.equal(L.connectorsLine({}), 'eidolon has no MCP client list; its connectors: mneme ✗ melete ✗');
});

test('the sentence for an answer that was not a 200 is the perch\'s own, else what happened', () => {
  assert.equal(L.said({ status: 409, data: { error: 'manual mode has no config' } }), 'manual mode has no config');
  assert.equal(L.said({ status: 0, data: { error: 'network error' } }), 'the perch did not answer', 'status 0 is the lane\'s: nothing came back');
  assert.equal(L.said({ status: 0, data: null }), 'the perch did not answer');
  assert.equal(L.said({ status: 502, data: null }), 'the perch answered 502');
  assert.equal(L.said({ status: 401, data: null }), "the perch refused this page's login (it restarted?)");
});

// ---------------------------------------------------------------- the diff tab's branch line
test('the branch line: a branch, a detached head, no repository, and the perch\'s sentence for the rest', () => {
  const sha = 'a'.repeat(40);
  assert.deepEqual(D.branchLine({ status: 200, data: { branch: 'main' } }), { text: 'main', cls: 'c' });
  assert.deepEqual(D.branchLine({ status: 200, data: { branch: null, sha } }), { text: 'detached at ' + 'a'.repeat(12), title: sha, cls: 'o' });
  assert.deepEqual(D.branchLine({ status: 200, data: { branch: null } }), { text: 'no git repository in this folder', cls: 'faint' }, '200 with a null branch is no repo, not an error');
  assert.deepEqual(D.branchLine({ status: 404, data: { error: 'no folder is known for this chat' } }), { text: 'no folder is known for this chat', cls: 'faint' });
  assert.equal(D.branchLine({ status: 0, data: { error: 'network error' } }).text, 'the perch did not answer');
  assert.equal(D.branchLine({ status: 404, data: null }).text, 'the perch has no folder for this chat', 'the unknown chat comes with an empty body');
  assert.equal(D.branchLine({ status: 409, data: { state: 'starting' } }).text, 'this chat is starting: no folder to read');
  assert.equal(D.branchLine({ status: 409, data: { state: 'kept', error: "this chat's folder isn't known" } }).text, "this chat's folder isn't known");
  assert.equal(D.branchLine({ status: 503, data: null }).text, 'the perch is busy; asked again soon');
  assert.equal(D.branchLine({ status: 502, data: null }).text, 'the perch answered 502');
  assert.equal(D.branchLine(null).text, '…', 'nothing asked yet');
});

test('the diff tab\'s note says what is not read, once, and the branch path names the chat and passes the hub whitelist', () => {
  assert.equal(D.GIT_NOTE, '+/− and PR state are not read: Minerva never runs git on a folder (its filters could run code)');
  assert.equal(D.gitPath('c-1'), 'meta/git?session=c-1');
  assert.equal(D.gitPath('a b/c'), 'meta/git?session=a%20b%2Fc', 'the id is escaped, never joined raw');
  for (const p of [D.gitPath('c-1'), D.gitPath('a/../b'), 'meta/tools', 'meta/plugins', 'meta/policy', 'meta/policy?mode=manual', 'meta/connectors', 'meta/home']) {
    assert.ok(hubPathOk(p), p + ' is a path the page may ask for');
  }
});

// ---------------------------------------------------------------- the store of answers
let calls, plan, real, parked, signals;
// `plan[url]` is a list of answers handed out in order: [status, body] or 'down'. A url in `parked` is not answered until `parked[url]()` runs
// (the request is in flight: it holds a connection, or the lane); `signals[url]` says whether the call carried an abort signal (a timeout).
function stubFetch(url, init) {
  calls.push(url);
  signals[url] = !!(init && init.signal);
  const next = (plan[url] || []).shift();
  const answer = () => (!next || next === 'down' ? Promise.reject(new TypeError('fetch failed')) : Promise.resolve({ status: next[0], text: async () => JSON.stringify(next[1]) }));
  if (url in parked) return new Promise((resolve) => { parked[url] = () => resolve(answer()); });
  return answer();
}
beforeEach(() => { calls = []; plan = {}; parked = {}; signals = {}; real = globalThis.fetch; globalThis.fetch = stubFetch; });
afterEach(() => { globalThis.fetch = real; });

test('with no perch (a bare door) nothing is asked for and nothing is stored', async () => {
  assert.notEqual(mode(), 'perch');
  await L.pull('meta/tools', 0);
  await L.pull('meta/home');
  assert.deepEqual(calls, []);
  assert.equal(L.answer('meta/tools').value, null);
});

test('on a perch an answer is asked for once while it is fresh, again when it is not, and a failed refresh keeps the good answer', async () => {
  plan['/hub/sessions'] = [[200, []]];
  assert.equal(await detect(), 'perch');
  calls.length = 0;

  plan['/hub/meta/tools'] = [[200, [{ name: 'read' }]], [200, [{ name: 'read' }, { name: 'write' }]], [502, { error: 'eidolon failed' }], 'down'];
  await L.pull('meta/tools', 60000);
  assert.deepEqual(calls, ['/hub/meta/tools']);
  assert.deepEqual(L.answer('meta/tools').value.data, [{ name: 'read' }]);
  await L.pull('meta/tools', 60000);
  assert.equal(calls.length, 1, 'inside the fresh window: no second request');

  await L.pull('meta/tools', 0);
  assert.equal(calls.length, 2, 'fresh 0 asks every time');
  assert.equal(L.answer('meta/tools').value.data.length, 2);

  await L.pull('meta/tools', 0);
  assert.equal(calls.length, 3);
  const kept = L.answer('meta/tools').value;
  assert.equal(kept.status, 200, 'a 502 after a good answer leaves the good one');
  assert.equal(kept.data.length, 2);
  await L.pull('meta/tools', 0);
  assert.equal(L.answer('meta/tools').value.status, 200, 'and so does a call that never got through');
  assert.equal(L.answer('meta/tools').value, kept);

  plan['/hub/meta/policy?mode=manual'] = [[409, { error: 'manual mode has no config' }]];
  await L.pull('meta/policy?mode=manual', 0);
  assert.deepEqual(L.answer('meta/policy?mode=manual').value.data, { error: 'manual mode has no config' }, 'with nothing good to keep, the failure is the answer');
  assert.equal(L.answer('meta/policy?mode=manual').value.status, 409);
});

test('two askers of one path share the request in flight', async () => {
  plan['/hub/meta/plugins'] = [[200, { text: 'browser 0.1.0' }]];
  await Promise.all([L.pull('meta/plugins', 0), L.pull('meta/plugins', 0)]);
  await settle();
  assert.deepEqual(calls, ['/hub/meta/plugins']);
  assert.equal(L.answer('meta/plugins').value.data.text, 'browser 0.1.0');
});

// ---------------------------------------------------------------- opening a tab is not a poll: it never waits in the lane
// The lane (core/lane.js) is one request in flight across the page. `/hub/sessions` is parked here, so the lane is held by it the whole time:
// whatever goes out meanwhile did not go through the lane.
async function perchMode() {
  if (mode() === 'perch') return;
  plan['/hub/sessions'] = [[200, []]];
  assert.equal(await detect(), 'perch');
  calls.length = 0;
}
// A call that is stuck behind the lane fails the test instead of hanging it.
const within = (p, ms = 1000) => Promise.race([p, new Promise((_, no) => setTimeout(() => no(new Error('still waiting after ' + ms + ' ms: behind the lane?')), ms).unref())]);
const forget = (...paths) => { for (const p of paths) L.answer(p).value = null; };
async function holdTheLane() {
  parked['/hub/sessions'] = null;
  plan['/hub/sessions'] = [[200, []]];
  const busy = hubPoll('sessions');
  await settle();
  assert.deepEqual(calls, ['/hub/sessions'], 'the lane is in use');
  return { busy, free: async () => { parked['/hub/sessions'](); await busy; await settle(); } };
}

test('opening the loaded tab asks for everything even while the lane is held, each call with a timeout', async () => {
  await perchMode();
  const POLICY = 'meta/policy?mode=auto';
  forget('meta/connectors', 'meta/home', 'meta/tools', POLICY);
  plan['/hub/meta/connectors'] = [[200, { mneme: true, melete: false }]];
  plan['/hub/meta/home'] = [[200, { default_mode: 'auto', source: 'minerva.toml' }]];
  plan['/hub/meta/tools'] = [[200, [{ name: 'read' }]]];
  plan['/hub/meta/policy?mode=auto'] = [[200, { mode: 'auto', file: '~/p.rn', rows: 'yours: x\n', guard: { on: false, file: null, why: 'not built yet' } }]];
  const lane = await holdTheLane();
  await within(L.fetchOpen(POLICY));
  assert.deepEqual(calls.slice(1), ['/hub/meta/connectors', '/hub/meta/home', '/hub/meta/tools', '/hub/meta/policy?mode=auto'], 'all four went out behind the parked poll, one after the other');
  for (const u of calls.slice(1)) assert.ok(signals[u], u + ' carries a timeout: a perch that never answers must not leave it "in flight" for ever');
  assert.equal(L.answer('meta/connectors').value.data.mneme, true);
  assert.equal(L.answer(POLICY).value.data.guard.why, 'not built yet');
  await lane.free();
});

test('unfolding the plugin list is a call outside the lane too', async () => {
  await perchMode();
  forget('meta/plugins');
  plan['/hub/meta/plugins'] = [[200, { text: 'browser 0.1.0' }]];
  const lane = await holdTheLane();
  await within(L.fetchPlugins());
  assert.deepEqual(calls.slice(1), ['/hub/meta/plugins']);
  assert.ok(signals['/hub/meta/plugins']);
  assert.equal(L.answer('meta/plugins').value.data.text, 'browser 0.1.0');
  await lane.free();
});

test('the diff tab\'s branch at open goes out at once; its periodic refresh waits its turn in the lane', async () => {
  await perchMode();
  forget(D.gitPath('c1'), D.gitPath('c2'));
  plan['/hub/meta/git?session=c1'] = [[200, { branch: 'main' }]];
  plan['/hub/meta/git?session=c2'] = [[200, { branch: 'dev' }]];
  const lane = await holdTheLane();
  await within(D.refreshGit('c1'));
  assert.deepEqual(calls.slice(1), ['/hub/meta/git?session=c1'], 'at open: not behind the lane');
  assert.ok(signals['/hub/meta/git?session=c1']);
  const tick = D.refreshGit('c2', true);
  await settle();
  assert.deepEqual(calls.slice(1), ['/hub/meta/git?session=c1'], 'the periodic read is queued behind the parked poll');
  await lane.free();
  await tick;
  assert.deepEqual(calls.slice(1), ['/hub/meta/git?session=c1', '/hub/meta/git?session=c2'], 'and goes out when the lane is free');
  assert.equal(L.answer(D.gitPath('c2')).value.data.branch, 'dev');
});

// ---------------------------------------------------------------- what the files must not do
test('the loaded tab says nothing is "not available yet", has no effort row, and leaves the registered sections to app.js', () => {
  for (const file of ['../panes/loaded.js', '../panes/diff.js']) {
    const text = code(file);
    assert.ok(!/not available yet/.test(text), file + ' says "not available yet"');
    assert.ok(!/\bSoon\b/.test(text), file + ' uses Soon, which says it');
    assert.ok(!/loadedSections|registry\.js/.test(text), file + ' draws the registered sections (app.js\'s Loaded wrapper already does)');
  }
  assert.ok(!/effort/i.test(code('../panes/loaded.js')), 'the effort row is gone');
  assert.equal((src('../app.js').match(/loadedSections\.list\.map/g) || []).length, 1, 'app.js draws the sections once');
});

test('the tabs make no raw fetch, and the tab effects reach the perch only through pull, fetchOpen, fetchPlugins and refreshGit', () => {
  const text = code('../panes/loaded.js') + code('../panes/diff.js');
  assert.ok(!/\bfetch\(/.test(text), 'no raw fetch');
  assert.equal((text.match(/\bhubPoll\(/g) || []).length, 1, 'one place decides lane or not');
  assert.equal((text.match(/\bhubGet\(/g) || []).length, 1);
  assert.ok(/refreshGit\(id, true\)/.test(code('../panes/diff.js')), 'the 15 s refresh is the one lane call');
});
