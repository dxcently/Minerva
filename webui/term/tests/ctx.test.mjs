// The context limit of the status line's ctx gauge (core/ctxlimit.js): read from the note on a row of GET /hub/models, '-' when there is none.
// Pure: no DOM. The last tests read the sources that cannot load here (app.js, core/ui.js, core/state.js) to hold the small edits that went
// with it: the gauge reads the table and not a hello field the door never sends, the three stale sentences are gone, hubOff is gone.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { ctxLimit, limitTable } from '../core/ctxlimit.js';

// As LF: a checkout with core.autocrlf has CRLF sources.
const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');

test("'10k ctx' is 10000, '1M ctx' is 1000000, and a note with no ctx is null", () => {
  assert.equal(ctxLimit('10k ctx'), 10000);
  assert.equal(ctxLimit('1M ctx'), 1000000);
  assert.equal(ctxLimit('mock · free'), null);
  assert.equal(ctxLimit('1.5M ctx'), 1500000);
  assert.equal(ctxLimit('8192 ctx'), 8192, 'a bare number of tokens');
  assert.equal(ctxLimit('20K CTX'), 20000, 'case does not matter');
});

test('the notes eidolon and the perch really send: the context is the thousands before `ctx`, whatever else the note says', () => {
  // perch/src/models.rs and its http test; eidolon's own catalog line is `provider · Nk ctx · $in/out per M · reasoning · no images`.
  assert.equal(ctxLimit('mock · 10k ctx · free'), 10000);
  assert.equal(ctxLimit('mock · 20k ctx · $1/2 per M'), 20000);
  assert.equal(ctxLimit('deepseek · 1000k ctx · $0.3/1.2 per M'), 1000000);
  assert.equal(ctxLimit('zai · 1048k ctx'), 1048000);
  assert.equal(ctxLimit('ollama · 262k ctx · reasoning · no images'), 262000);
  assert.equal(ctxLimit('openai · $2/8 per M · reasoning'), null, 'a catalog that does not know the context prints none');
});

test('a note with no usable limit is null: empty, missing, not text, a zero, a word that only ends in ctx', () => {
  for (const bad of ['', undefined, null, 42, {}, [], 'ctx', 'k ctx', '0k ctx', '0 ctx', 'a10k ctx', 'x1.5M ctx', '10k ctxs', '10k context']) {
    assert.equal(ctxLimit(bad), null, JSON.stringify(bad));
  }
});

test('the table has a row per key, null where the note names none; anything that is not the rows gives an empty table', () => {
  const t = limitTable([
    { key: 'mock:alpha', name: 'Alpha', note: 'mock · 10k ctx · free' },
    { key: 'mock:beta', name: 'Beta', note: 'mock · 20k ctx · $1/2 per M' },
    { key: 'zai:glm-5.3', note: 'zai' },
    { key: 'bare:model' },
    { name: 'no key', note: '9k ctx' },
    null,
    { key: 7, note: '9k ctx' },
  ]);
  assert.deepEqual([...t], [['mock:alpha', 10000], ['mock:beta', 20000], ['zai:glm-5.3', null], ['bare:model', null]]);
  assert.equal(t.get('mock:alpha'), 10000);
  assert.equal(t.has('nope'), false, 'a model the list does not hold is not in the table (the page asks again, once a minute)');
  for (const bad of [null, undefined, 'x', {}, 3]) assert.equal(limitTable(bad).size, 0, JSON.stringify(bad));
});

test('wiring: the status line reads the limit from the table, and the ctx gauge shows `-` with no limit', () => {
  const app = src('../app.js');
  assert.ok(app.includes("import { limitTable } from './core/ctxlimit.js'"));
  assert.ok(!app.includes('context_limit'), 'the door never sends one');
  assert.ok(app.includes('limit = hl && table && table.get(hl.model)'));
  assert.ok(app.includes("limit ? kfmt(limit) : '-'"));
  assert.ok(app.includes("hubPoll('models')"), 'the list rides the request lane, as every poll does');
});

test('wiring: the three stale sentences say what is true now', () => {
  const app = src('../app.js');
  for (const stale of ['arrives with the hub (M2)', 'arrives with the hub, M2', 'arrive with the registry (M4)', 'need the park-tick frame']) {
    assert.ok(!app.includes(stale), `still says: ${stale}`);
  }
  assert.ok(app.includes("parks of wait_for calls show no live samples: eidolon's web door sends no park-tick (upstream gap 7)"));
  assert.ok(app.includes("no such command; these are the page's own commands (eidolon's web door lists none)"));
  const picker = app.slice(app.indexOf('function picker()'), app.indexOf('function longSide'));
  assert.ok(picker.includes("the picker needs Minerva's server: open it with hoot"));
  assert.ok(app.includes("ctrl+enter  new pane with a picker (needs Minerva's server: open it with hoot)"));
});

test('wiring: hubOff is gone from core/ui.js (nothing used it), and state.js names the route the saved chat really reads', () => {
  assert.ok(!src('../core/ui.js').includes('hubOff'));
  const state = src('../core/state.js');
  assert.ok(state.includes('GET /hub/sessions/<id>/transcript'));
  assert.ok(!state.includes('GET /hub/sessions/<id>/log'));
});
