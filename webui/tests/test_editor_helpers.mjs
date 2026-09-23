/* Unit tests for the DOM-free helper block in webui/workspace.js.
   The block between the mw:pure-start / mw:pure-end markers is extracted from the real
   source file and evaluated here, so these assertions cover the shipped code rather than
   a copy of it. No DOM, no browser, no network: the panel itself is exercised by the parent
   in a real browser session. */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const source = readFileSync(fileURLToPath(new URL('../workspace.js', import.meta.url)), 'utf8');
const block = source.split('/* mw:pure-start */')[1]?.split('/* mw:pure-end */')[0];
assert.ok(block && block.length > 200, 'pure helper block markers missing in webui/workspace.js');

const helpers = new Function(`${block}
return { transitionsOf, nodeAtPath, resolveTargetRef, removeState, pushDraft, runStatusFor, isLiveRun, latestOrder, orderStatusLabel, scoreLabel };`)();

const results = [];
const test = (name, run) => { run(); results.push('ok   ' + name); };

test('transitionsOf normalizes strings, arrays and empty values', () => {
  assert.deepEqual(helpers.transitionsOf('b'), [{target: 'b'}]);
  assert.deepEqual(helpers.transitionsOf(['b', {target: 'c'}]), [{target: 'b'}, {target: 'c'}]);
  assert.deepEqual(helpers.transitionsOf({target: 'd'}), [{target: 'd'}]);
  assert.deepEqual(helpers.transitionsOf(undefined), []);
});

test('nodeAtPath resolves nested states and rejects unknown paths', () => {
  const doc = {id: 'g', states: {a: {states: {b: {description: 'deep'}}}}};
  assert.equal(helpers.nodeAtPath(doc, 'a.b').description, 'deep');
  assert.equal(helpers.nodeAtPath(doc, 'a.missing'), undefined);
  assert.equal(helpers.nodeAtPath(doc, 'a.b.c'), undefined);
});

test('resolveTargetRef handles graph-qualified and sibling references', () => {
  const doc = {id: 'g', states: {a: {}}};
  assert.equal(helpers.resolveTargetRef(doc, 'a', '#g.b'), 'b');
  assert.equal(helpers.resolveTargetRef(doc, 'a', 'b'), 'b');
  assert.equal(helpers.resolveTargetRef(doc, 'p.child', 'b'), 'p.b');
  assert.equal(helpers.resolveTargetRef(doc, 'a', undefined), null);
});

const graph = () => structuredClone({
  id: 'g', version: '1', initial: 'a',
  states: {
    a: {description: 'start', always: [{target: '#g.b'}]},
    b: {description: 'work'},
    done: {type: 'final'},
  },
});

test('removeState deletes a node, its attached edges, and repairs the start state', () => {
  const doc = graph();
  assert.equal(helpers.removeState(doc, 'b'), true);
  assert.equal(doc.states.b, undefined);
  assert.equal(doc.states.a.always, undefined, 'edge pointing at the removed node must be dropped');
  assert.equal(doc.states.a.description, 'start', 'unrelated nodes stay intact');
  assert.ok(doc.states.done, 'untouched sibling survives');
  assert.equal(doc.initial, 'a');
  assert.equal(helpers.removeState(doc, 'nope'), false);
});

test('removeState falls back to the first non-history state when the start is removed', () => {
  const doc = {id: 'g', initial: 'a', states: {a: {}, history1: {type: 'history'}, done: {type: 'final'}}};
  assert.equal(helpers.removeState(doc, 'a'), true);
  assert.equal(doc.initial, 'done');
});

test('removeState resolves sibling references inside nested state groups', () => {
  const doc = {id: 'g', initial: 'p', states: {p: {states: {
    c1: {},
    c2: {always: [{target: 'c1'}]},
  }}}};
  assert.equal(helpers.removeState(doc, 'p.c1'), true);
  assert.equal(doc.states.p.states.c1, undefined);
  assert.deepEqual(doc.states.p.states.c2, {}, 'sibling state c2 survives with its edge to p.c1 dropped');
});

test('removeState drops references into a removed subtree', () => {
  const doc = {id: 'g', initial: 'p', states: {
    root: {always: [{target: '#g.p'}], onDone: [{target: 'p.c'}]},
    p: {states: {c: {}}},
  }};
  assert.equal(helpers.removeState(doc, 'p'), true);
  assert.equal(doc.states.root.always, undefined);
  assert.equal(doc.states.root.onDone, undefined);
  assert.equal(doc.states.p, undefined);
});

test('undo history restores nodes and their attached edges', () => {
  const history = [], future = [];
  const before = JSON.stringify(graph());
  const doc = graph();
  helpers.removeState(doc, 'b');
  const after = JSON.stringify(doc);
  assert.equal(helpers.pushDraft(history, future, before, after), true);
  assert.equal(history.length, 1);
  const restored = JSON.parse(history.pop());
  assert.ok(restored.states.b, 'deleted node is back');
  assert.deepEqual(restored.states.a.always, [{target: '#g.b'}], 'attached edge is back');
  assert.equal(helpers.pushDraft(history, future, before, before), false, 'no-op edits add no history');
});

test('pushDraft clears redo and caps the undo stack', () => {
  const history = [], future = ['redo-me'];
  helpers.pushDraft(history, future, 'a', 'b');
  assert.deepEqual(future, []);
  for (let i = 0; i < 200; i++) helpers.pushDraft(history, future, 'draft' + i, 'draft' + (i + 1));
  assert.equal(history.length, 100, 'history stays bounded at 100 entries');
  assert.equal(history.at(-1), 'draft199', 'the most recent draft is the one kept');
});

test('runStatusFor distinguishes running, waiting, finished, stopped and failed', () => {
  assert.equal(helpers.runStatusFor(null).key, 'none');
  assert.equal(helpers.runStatusFor({status: 'running'}).key, 'running');
  assert.equal(helpers.runStatusFor({state: 'thinking'}).key, 'running', 'unknown status is running, not invented');
  assert.equal(helpers.runStatusFor({status: 'running', escalation: {}}).key, 'waiting');
  assert.equal(helpers.runStatusFor({outcome: 'reached'}).key, 'finished');
  assert.equal(helpers.runStatusFor({outcome: 'exhausted'}).key, 'finished');
  assert.equal(helpers.runStatusFor({status: 'stopped'}).key, 'stopped');
  assert.equal(helpers.runStatusFor({status: 'failed'}).key, 'failed');
  assert.equal(helpers.isLiveRun({status: 'running'}), true);
  assert.equal(helpers.isLiveRun({status: 'running', escalation: {}}), true, 'a parked run is still live and still accepts orders');
  assert.equal(helpers.isLiveRun({outcome: 'reached'}), false);
});

test('latestOrder prefers the recorded live order and reports its real status', () => {
  assert.equal(helpers.latestOrder({}), null);
  assert.equal(helpers.latestOrder({orders: []}), null);
  const orders = [{id: 'one', text: 'first', status: 'superseded'}, {id: 'two', text: 'second', status: 'pending'}];
  assert.equal(helpers.latestOrder({orders}).id, 'two');
  const only = [{id: 'three', text: 'x', status: 'superseded'}];
  assert.equal(helpers.latestOrder({orders: only}).id, 'three');
  assert.match(helpers.orderStatusLabel({status: 'pending'}), /not yet applied/);
  assert.equal(helpers.orderStatusLabel({status: 'applied'}), 'Applied by the run');
  assert.equal(helpers.orderStatusLabel({status: 'superseded'}), 'Superseded');
});

test('scoreLabel says unscored instead of inventing a confidence number', () => {
  assert.equal(helpers.scoreLabel(0), '0.0%');
  assert.equal(helpers.scoreLabel(0.4231), '42.3%');
  assert.equal(helpers.scoreLabel(undefined), 'unscored');
  assert.equal(helpers.scoreLabel(null), 'unscored');
  assert.equal(helpers.scoreLabel('rule'), 'unscored');
});

console.log(results.join('\n'));
console.log(`${results.length} checks passed`);
