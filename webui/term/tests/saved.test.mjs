// A saved chat (core/saved.js) over jev's real wiki-hop journals (tests/fixtures/jev, cases a to l): a workflow run is drawn as a start line, a tool row
// per step with its result beside it, an ask row that holds a park's answer, and an end line; nothing says "not shown here" about a run, no tool row is
// named "?", and a record with nothing to name draws nothing (the baseline drew "[record #undefined: unknown, not shown here]"). Pure: no DOM, the
// stubs core/state.js needs at import (matchMedia, a fetch that never answers) are the ones tests/registry.test.mjs uses. Rows are read from a real Session.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

globalThis.matchMedia = () => ({ matches: false, addEventListener() {}, addListener() {} });
globalThis.fetch = () => new Promise(() => {});
const { Session } = await import('../core/state.js');
const saved = await import('../core/saved.js');

// As LF: a checkout with core.autocrlf has CRLF sources.
const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
const RLO = String.fromCharCode(0x202e); // right-to-left override: the character that makes a line read backwards
const INVISIBLE = new RegExp('[' + [[0x200b, 0x200f], [0x2028, 0x202e], [0x2060, 0x206f], [0xfeff, 0xfeff]].map(([a, b]) => String.fromCharCode(a) + '-' + String.fromCharCode(b)).join('') + ']');
const CASES = 'abcdefghijkl'.split('');
const journal = (c) => src(`./fixtures/jev/${c}/journal.ndjson`);
const records = (c) => journal(c).trim().split('\n').map((l) => JSON.parse(l));
const kindOf = (r) => Object.keys(r.kind)[0];

// Draw a log into a fresh Session and read its rows back.
function drawn(text) {
  const s = new Session('saved-test', { toast() {} });
  const n = saved.draw(s, text);
  const rows = s.chunks.value.flatMap((k) => k.rows);
  return { s, n, rows, sys: rows.filter((r) => r.kind === 'sys'), tools: rows.filter((r) => r.kind === 'tool').map((r) => r.call), asks: rows.filter((r) => r.kind === 'ask') };
}
const closing = (d) => d.s.close();

test('no Workflow record, ApiRequest or PolicyVerdict is "not shown here", whatever the case; no line says undefined', () => {
  for (const c of CASES) {
    const d = drawn(journal(c));
    for (const r of d.sys) {
      assert.ok(!/not shown here/.test(r.text), `case ${c}: ${r.text}`);
      assert.ok(!/undefined/.test(r.text), `case ${c}: ${r.text}`);
    }
    closing(d);
  }
});

test('every step is a tool row named for its tool, from origin script, its input parsed once more; none is named "?"', () => {
  for (const c of CASES) {
    const recs = records(c);
    const steps = recs.filter((r) => kindOf(r) === 'WorkflowStep').map((r) => r.kind.WorkflowStep);
    const d = drawn(journal(c));
    assert.deepEqual(d.tools.map((t) => t.name), steps.map((s) => s.tool), `case ${c}: the tool rows are the steps, in order`);
    for (const [i, t] of d.tools.entries()) {
      assert.notEqual(t.name, '?', `case ${c}`);
      assert.equal(t.id, steps[i].call_id);
      assert.equal(t.origin, 'script');
      assert.equal(typeof t.input, 'object', `case ${c} step ${i}: the input is the object the JSON string held`);
      assert.deepEqual(t.input, JSON.parse(steps[i].input));
    }
    closing(d);
  }
});

test('every jev_choose (and every step with a result) is paired with its ToolResult by call id', () => {
  for (const c of CASES) {
    const recs = records(c);
    const result = new Map(recs.filter((r) => kindOf(r) === 'ToolResult').map((r) => [r.kind.ToolResult.tool_use_id, r.kind.ToolResult]));
    const d = drawn(journal(c));
    let chooses = 0;
    for (const t of d.tools) {
      const want = result.get(t.id);
      if (!want) continue; // a step cut off: below
      assert.ok(t.done, `case ${c}: ${t.id} ${t.name} has its result`);
      assert.equal(t.out.value.output, want.content, `case ${c}: ${t.id} holds its own result`);
      assert.equal(t.out.value.isError, !!want.is_error);
      if (t.name === 'jev_choose') chooses++;
    }
    const wanted = recs.filter((r) => kindOf(r) === 'WorkflowStep' && r.kind.WorkflowStep.tool === 'jev_choose' && result.has(r.kind.WorkflowStep.call_id)).length;
    assert.equal(chooses, wanted, `case ${c}: every jev_choose with a result is paired`);
    closing(d);
  }
  // The cases that have a jev_choose at all: a, b, c, g, h, i, j, k, l (e and f never reach the chooser, d is cut off in it).
  const d = drawn(journal('a'));
  const choose = d.tools.find((t) => t.name === 'jev_choose');
  assert.deepEqual(JSON.parse(choose.out.value.output), { probs: { Egypt: 0.9, Cat: 0.1 }, best: 'Egypt' });
  closing(d);
});

test('a run is a start line, then its steps, then an end line (case a)', () => {
  const d = drawn(journal('a'));
  const texts = d.sys.map((r) => r.text);
  const at = texts.findIndex((t) => t.startsWith('▶'));
  assert.equal(texts[at], '▶ jev wiki-hop · start=Ancient_Egypt goal=Felidae');
  assert.equal(texts[texts.length - 1], '■ jev wiki-hop · completed · arrived');
  assert.equal(d.sys[d.sys.length - 1].cls, 'faint');
  assert.equal(d.tools.length, 15);
  assert.equal(d.tools[0].name, 'jev_graph');
  assert.equal(d.asks.length, 0);
  closing(d);
});

test('a park is an ask row that holds its answer: the option by index (case b), by label (case i), and nothing is left waiting', () => {
  const b = drawn(journal('b'));
  assert.equal(b.asks.length, 1, 'the question and the answer record are one row');
  const ask = b.asks[0].ask;
  assert.equal(ask.id, 'w1-park0');
  assert.match(ask.f.prompt, /^jev wiki-hop: top 0\.500, margin 0\.000 under 0\.050 · Which link leads toward the article "Felidae"\? · state: reading \(step 1\)$/);
  assert.deepEqual(ask.f.options, [{ label: '0  Egypt', description: '0.500' }, { label: '1  Cat', description: '0.500' }]);
  assert.deepEqual(ask.done.value, { how: 'answered', answer: '0  Egypt', note: undefined }, 'answered `0` is option 0, Egypt');
  assert.equal(b.s.asks.value.length, 0, 'no ask is waiting in a saved chat');
  // The park ends a run's first stretch and says so in its own line: it was answered, so the run went on.
  assert.ok(b.sys.some((r) => r.text === '■ jev wiki-hop · parked, then resumed with the answer'));
  assert.ok(!b.sys.some((r) => /waiting for an answer/.test(r.text)));
  assert.equal(b.sys[b.sys.length - 1].text, '■ jev wiki-hop · completed · arrived');
  closing(b);

  const i = drawn(journal('i'));
  assert.equal(i.asks.length, 1);
  assert.equal(i.asks[0].ask.f.options.length, 64, 'the whole menu of 64');
  assert.equal(i.asks[0].ask.done.value.answer, '1  Registered cat breed number 002', 'answered by the option label');
  assert.match(i.asks[0].ask.f.prompt, /top 0\.015 under floor 0\.350/);
  closing(i);
});

test('an answer that is no option (case h) reads so, and the second park is its own row with its answer', () => {
  const d = drawn(journal('h'));
  assert.equal(d.asks.length, 2);
  assert.equal(d.asks[0].ask.done.value.answer, 'banana (not an option)');
  assert.equal(d.asks[1].ask.id, 'w1-park1');
  assert.match(d.asks[1].ask.f.prompt, /^'banana' is not one of the options\. · jev wiki-hop: top 0\.500/);
  assert.equal(d.asks[1].ask.done.value.answer, '0  Egypt');
  assert.equal(d.s.asks.value.length, 0);
  assert.equal(d.sys.filter((r) => r.text === '■ jev wiki-hop · parked, then resumed with the answer').length, 2, 'each park ended a stretch');
  closing(d);
});

test('a park nobody answered (cases c, g, j, l) is an ask row that says there is no answer, and the end line says parked', () => {
  for (const c of ['c', 'g', 'j', 'l']) {
    const d = drawn(journal(c));
    assert.equal(d.asks.length, 1, `case ${c}`);
    assert.equal(d.asks[0].ask.done.value.answer, '(no answer in this log)', `case ${c}`);
    assert.equal(d.s.asks.value.length, 0, `case ${c}: a saved chat has no pending ask`);
    assert.equal(d.sys[d.sys.length - 1].text, '■ jev wiki-hop · parked, waiting for an answer', `case ${c}`);
    closing(d);
  }
  const c = drawn(journal('c'));
  assert.match(c.asks[0].ask.f.prompt, /chooser unavailable/);
  closing(c);
});

test('the end line is the graph\'s own end: case e completed in the door\'s words and failed in the graph\'s, case d failed', () => {
  const e = drawn(journal('e'));
  const last = e.sys[e.sys.length - 1];
  assert.match(last.text, /^■ jev wiki-hop · failed: it reached its failed state/);
  assert.equal(last.cls, 'err');
  closing(e);
  const d = drawn(journal('d'));
  assert.equal(d.sys[d.sys.length - 1].text, '■ jev wiki-hop · failed');
  assert.equal(d.sys[d.sys.length - 1].cls, 'err');
  closing(d);
});

test('a step with no result (a run cut off or cancelled) is stopped, not a spinner that never ends', () => {
  const d = drawn(journal('d'));
  const last = d.tools[d.tools.length - 1];
  assert.equal(last.name, 'jev_choose');
  assert.equal(last.done, false);
  assert.equal(last.st.value, 'stop');
  assert.equal(d.s.open.value.length, 0);
  closing(d);
  // k is killed in jev_choose: its first 24 records end there, with no WorkflowEnd.
  const cut = journal('k').trim().split('\n').slice(0, 24).join('\n');
  const k = drawn(cut);
  assert.equal(k.tools[k.tools.length - 1].st.value, 'stop');
  assert.ok(!k.sys.some((r) => r.text.startsWith('■')), 'no end line for a run with no end');
  closing(k);
});

test('a tool the program cancelled is an error row (case l)', () => {
  const d = drawn(journal('l'));
  const choose = d.tools.find((t) => t.name === 'jev_choose');
  assert.equal(choose.st.value, 'err');
  assert.match(choose.out.value.output, /cancelled while the tool was running/);
  closing(d);
});

test('a record with nothing to name draws nothing, and one that is named but unknown never says "#undefined"', () => {
  const log = [
    '{"id": 1, "ts_ms": 5, "kind": {"SessionStart": {"model": "mock", "cwd": "/w"}}}',
    '{"id": 2}',                                   // no kind
    '{"id": 3, "kind": {}}',                       // an empty kind
    '{"id": 4, "kind": null}',
    '{"kind": ""}',
    '{"id": 5, "kind": {"SomethingNew": {"x": 1}}}',   // named, unknown, with an id
    '{"kind": {"SomethingNew": {"x": 1}}}',         // named, unknown, no id
    '{"id": 6, "kind": "SomethingBare"}',
    '{"id": 7, "kind": {"ApiRequest": {"consumer": "plugin:jev", "service": "jev"}}}',
    '{"id": 8, "kind": {"WorkflowBudget": {"run": 1}}}',
  ].join('\n');
  const d = drawn(log);
  assert.deepEqual(d.sys.map((r) => r.text), [
    'started · mock · /w',
    '[record #5: SomethingNew, not shown here]',
    '[record: SomethingNew, not shown here]',
    '[record #6: SomethingBare, not shown here]',
  ]);
  closing(d);
});

test('framesOf: the parts of the mapping, record by record', () => {
  const rec = (kind, id = 1) => ({ id, ts_ms: 10, kind });
  const [start] = saved.framesOf(rec({ WorkflowStart: { plugin: 'jev', workflow: 'wiki-hop', version: '', hash: 'x', args: '{"start":"A","goal":"B"}' } }));
  assert.deepEqual([start.type, start.text], ['x-line', '▶ jev wiki-hop · start=A goal=B']);
  const [step] = saved.framesOf(rec({ WorkflowStep: { run: 1, index: 2, call_id: 'w1-2', tool: 'jev_mark', input: '{"kind":"enter","state":"s"}' } }));
  assert.deepEqual([step.type, step.id, step.name, step.input, step.origin], ['tool-call-started', 'w1-2', 'jev_mark', { kind: 'enter', state: 's' }, 'script']);
  const [res] = saved.framesOf(rec({ ToolResult: { tool_use_id: 'w1-2', content: 'marked', is_error: false } }));
  assert.deepEqual([res.type, res.id, res.output, res.is_error], ['tool-call-finished', 'w1-2', 'marked', false]);
  // An input that is not JSON is kept as the text it is; an AskUser alone (no state) answers from itself.
  assert.equal(saved.framesOf(rec({ WorkflowStep: { run: 1, index: 0, call_id: 'w1-0', tool: 'x', input: 'not json' } }))[0].input, 'not json');
  const ask = saved.framesOf(rec({ AskUser: { call_id: 'q1', prompt: 'ok?', answer: 'yes' } }));
  assert.deepEqual(ask.map((f) => f.type), ['ask', 'ask-settled']);
  assert.equal(ask[0].prompt, 'ok?');
  assert.equal(ask[1].answer, 'yes');
});

test('parkOf reads the interpreter\'s menu and nothing else', () => {
  const p = saved.parkOf('jev wiki-hop: top 0.500, margin 0.000 under 0.050\nAsk?\nstate: reading (step 1)\n  0  0.500  Egypt\n  1  0.500  Cat\nAnswer with the number.');
  assert.deepEqual(p.head, ['jev wiki-hop: top 0.500, margin 0.000 under 0.050', 'Ask?', 'state: reading (step 1)']);
  assert.deepEqual(p.options, [{ index: '0', p: '0.500', label: 'Egypt' }, { index: '1', p: '0.500', label: 'Cat' }]);
  // Indexes must run on from 0: a line that only looks like an option in a label is not one, and a prompt with no menu has none.
  assert.equal(saved.parkOf('q\n  5  0.1  x').options.length, 0);
  assert.deepEqual(saved.parkOf('what now?').options, []);
  assert.deepEqual(saved.parkOf(null), { head: [], options: [] });
});

test('hostile text in a run is text: it stays in strings the page draws as text nodes, and is cut', () => {
  const evil = '<img src=x onerror=alert(1)>' + RLO + '</b>';
  const log = [
    JSON.stringify({ id: 1, ts_ms: 1, kind: { WorkflowStart: { plugin: evil, workflow: 'w', version: '', hash: 'x', args: JSON.stringify({ goal: evil.repeat(40) }) } } }),
    JSON.stringify({ id: 2, ts_ms: 2, kind: { AskUser: { call_id: 'w1-park0', prompt: `jev w: why ${evil}\n  0  0.5  ${evil}\n  1  0.5  Cat`, answer: evil } } }),
  ].join('\n');
  const d = drawn(log);
  const start = d.sys.find((r) => r.text.startsWith('▶'));
  assert.ok(!start.text.includes(RLO), 'no bidi override in a line');
  assert.ok(start.text.length < 200, 'cut');
  const ask = d.asks[0].ask;
  assert.ok(!ask.f.options[0].label.includes(RLO));
  assert.ok(!ask.done.value.answer.includes(RLO));
  closing(d);
});

test('wiring: saved.js has no markup sink, and no literal bidi or line-separator character sits in the sources', () => {
  for (const rel of ['../core/saved.js', '../renderers/jev.js', '../renderers/index.js']) {
    const text = src(rel);
    assert.ok(!/innerHTML|dangerouslySetInnerHTML|insertAdjacentHTML|document\.write|\beval\(/.test(text), `${rel}: no markup sink`);
    assert.ok(!INVISIBLE.test(text), `${rel}: no invisible character in the source`);
  }
});
