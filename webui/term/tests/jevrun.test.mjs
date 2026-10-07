// The run model (graph/jevrun.js): a jev run's journal in, the decision steps out. Pure: no DOM. Input is the twelve real wiki-hop journals of
// tests/fixtures/jev (cases a-l, captured by F0 from `eidolon workflow` with the mock provider), the door's own reports and refusals beside them
// in each case.json, and, for what no case holds (a collision after the byte cut, a stop, a hostile label, a graph of several pages), journals
// built here record by record in the shape the README of that folder lists.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { newRun, feed, feedText, view, reduce, cutBytes, parseSnapshot, parseRefs, classifyAnswer, FALLBACK_LABEL, HOST_STEP_CAP, CUT } from '../graph/jevrun.js';
import { drawGraph } from '../graph/draw.js';

const dir = (c) => new URL(`./fixtures/jev/${c}/`, import.meta.url);
const journalOf = (c) => readFileSync(new URL('journal.ndjson', dir(c)), 'utf8').trim().split('\n').map((l) => JSON.parse(l));
const caseOf = (c) => JSON.parse(readFileSync(new URL('case.json', dir(c)), 'utf8'));
const meta = JSON.parse(readFileSync(new URL('./fixtures/jev/meta.json', import.meta.url), 'utf8'));
const stdout = (c) => caseOf(c).invocations.map((i) => { try { return JSON.parse(i.stdout); } catch { return null; } });
const reportOf = (c) => { // the door's last report: a JSON string inside the door's JSON line
  const o = [...stdout(c)].reverse().find((x) => x && x.report);
  return o ? JSON.parse(o.report) : null;
};
const CASES = [...'abcdefghijkl'];
const J = Object.fromEntries(CASES.map((c) => [c, journalOf(c)]));
const V = Object.fromEntries(CASES.map((c) => [c, reduce(J[c])]));
const step = (v, n) => v.steps[n - 1];
// the journal as the log held it when the nth invocation of a case had ended
const upto = (c, n) => J[c].slice(0, caseOf(c).invocations[n - 1].records);

// ---- the twelve cases: the status each ends in, and the door's own report agreeing with what is read from the steps ------------------------

const STATUS = { a: 'arrived', b: 'arrived', c: 'parked', d: 'cancelled', e: 'failed', f: 'arrived', g: 'parked', h: 'arrived', i: 'failed', j: 'parked', k: 'arrived', l: 'parked' };

test('all twelve cases: the status each journal ends in', () => {
  for (const c of CASES) {
    assert.equal(V[c].status, STATUS[c], `case ${c}`);
    assert.equal(V[c].lastId, J[c].length - 1, `case ${c}: the cursor is the last record id`);
    assert.equal(V[c].meta.workflow, 'wiki-hop');
    assert.equal(V[c].meta.plugin, 'jev');
    assert.equal(V[c].meta.run, 1);
    assert.deepEqual([V[c].meta.start, V[c].meta.goal], [caseOf(c).args.start, caseOf(c).args.goal], `case ${c}: start and goal are WorkflowStart's args, parsed twice`);
  }
});

test('the trail, the path, the decisions, the actions, the escalations, the chooser and rule picks are what the door reported', () => {
  let n = 0;
  for (const c of CASES) {
    const r = reportOf(c), v = V[c];
    if (!r) continue;
    n++;
    assert.deepEqual(v.trail, r.path, `case ${c}: the states entered (the report's path)`);
    assert.deepEqual(v.path, r.output.path, `case ${c}: the article headings (the report's output.path)`);
    assert.equal(v.budgets.decisions.used, r.steps, `case ${c}: decisions`);
    assert.equal(v.budgets.actions.used, r.actions, `case ${c}: actions`);
    assert.equal(v.budgets.escalations.used, r.escalations, `case ${c}: escalations`);
    assert.equal(v.steps.filter((s) => s.menu && s.menu.source === 'chooser').length, r.chooser_calls, `case ${c}: chooser calls`);
    assert.equal(v.steps.filter((s) => s.menu && s.menu.source === 'rule').length, r.rule_picks, `case ${c}: rule picks`);
    assert.equal(v.end.escalations, r.escalations);
    assert.equal(v.end.decisions, r.steps);
  }
  assert.equal(n, 7, 'a, b, e, f, h, i and k have a report');
});

test('a: arrived; the path is right; each pick equals the click ref mapped through the snapshot', () => {
  const v = V.a;
  assert.equal(v.status, 'arrived');
  assert.deepEqual(v.path, ['Ancient Egypt', 'Egypt', 'Felidae']);
  assert.deepEqual(v.steps.map((s) => [s.state, s.page && s.page.heading]), [['open', null], ['reading', 'Ancient Egypt'], ['reading', 'Egypt'], ['reading', 'Felidae']]);
  assert.deepEqual(v.end, { status: 'arrived', door: 'Completed', state: 'arrived', outcome: 'reached', reason: null, cause: null, exhausted: null,
    decisions: 2, asked: 0, escalations: 0, title: 'Felidae' });
  assert.deepEqual(v.marks, { current: [], visited: ['open', 'reading'], final: ['arrived'], failed: [] });
  assert.equal(v.active, 'arrived');
  assert.equal(v.open, null);
  assert.equal(v.waiting, null);
  assert.equal(v.unresumable, null);
  assert.deepEqual(v.spend, []);
});

// Read independently of the module: the label of `link "<label>" [ref=<ref>]` in the snapshot a click follows
test('every pick, in all twelve cases, is the label of the clicked ref in the snapshot before the click', () => {
  let clicks = 0;
  for (const c of CASES) {
    let tree = '';
    const results = new Map(J[c].filter((r) => r.kind.ToolResult).map((r) => [r.kind.ToolResult.tool_use_id, r.kind.ToolResult.content]));
    for (const r of J[c]) {
      const s = r.kind.WorkflowStep;
      if (!s) continue;
      if (s.tool === 'browser_snapshot' && results.has(s.call_id)) tree = results.get(s.call_id);
      if (s.tool !== 'browser_click') continue;
      clicks++;
      const ref = JSON.parse(s.input).ref;
      const label = new RegExp(`link "([^"]*)" \\[ref=${ref}\\]`).exec(tree)[1];
      const st = V[c].steps.find((x) => x.action && x.action.call_id === s.call_id);
      assert.equal(st.pick.label, label, `case ${c} ${s.call_id}`);
      assert.equal(st.pick.ref, ref);
      assert.equal(st.action.label, label);
      assert.equal(st.action.refOk, true);
      assert.equal(st.menu.options[st.pick.index].label, label, `case ${c}: the index points at the label in the menu`);
    }
  }
  assert.equal(clicks, 12, 'a, b, h and k: 2 each, f: 3, i: 1');
});

test('a: a chooser pick with p, best, top two, floor and margin from the run\'s own graph; then a rule pick', () => {
  const m = step(V.a, 2).menu;
  assert.equal(m.source, 'chooser');
  assert.deepEqual(m.options, [{ label: 'Egypt', p: 0.9 }, { label: 'Cat', p: 0.1 }]);
  assert.deepEqual([m.best, m.best_index, m.top1, m.top2], ['Egypt', 0, 0.9, 0.1]);
  assert.deepEqual([m.floor, m.margin, m.floorOk, m.marginOk, m.sure, m.verdict, m.asked_for], [0.35, 0.05, true, true, true, 'sure', null]);
  assert.deepEqual(step(V.a, 2).pick, { label: 'Egypt', index: 0, how: 'chooser', ref: 'e3' });
  assert.equal(step(V.a, 2).decision, 1);
  const r = step(V.a, 3);
  assert.equal(r.menu.source, 'rule');
  assert.equal(r.menu.verdict, 'rule');
  assert.deepEqual(r.menu.options.map((o) => o.label), ['Cat', 'Felidae'], 'rebuilt from the snapshot: Ancient Egypt is where it has been');
  assert.equal(r.menu.reconstructed, true);
  assert.deepEqual([r.menu.top1, r.menu.floor, r.menu.options[0].p], [null, null, null], 'no scorer was asked: no p, no floor');
  assert.deepEqual(r.pick, { label: 'Felidae', index: 1, how: 'rule', ref: 'e4' });
  assert.equal(r.decision, 2);
  assert.equal(step(V.a, 4).decision, null, 'arriving takes no decision');
  assert.equal(step(V.a, 4).menu, null);
});

test('arrival: the heading against the goal, then entail against the threshold of the run\'s own graph', () => {
  const a = step(V.a, 2).arrival;
  assert.deepEqual(a.equals, { heading: 'Ancient Egypt', goal: 'Felidae', hit: false });
  assert.deepEqual(a.entail, { pending: false, score: 0.1, threshold: 0.8, hit: false, label: 'neutral', hypothesis: 'This is the encyclopedia article about Felidae.', error: null });
  assert.equal(a.hit, false);
  assert.equal(a.error, null);
  const z = step(V.a, 4).arrival;
  assert.deepEqual(z.equals, { heading: 'Felidae', goal: 'Felidae', hit: true });
  assert.equal(z.entail, null, 'heading equals the goal: jev_entail is not asked');
  assert.equal(z.hit, true);
  assert.equal(step(V.a, 1).arrival, null, 'open reads no page');
});

test('the graph is rebuilt from the jev_graph pages in the journal: its sha matches the PIN, and floor, margin and threshold come from it', () => {
  for (const c of CASES) {
    const g = V[c].graph;
    assert.equal(g.source, 'journal', `case ${c}`);
    assert.equal(g.label, null);
    assert.equal(g.pages, 1);
    assert.equal(g.error, null);
    assert.equal(createHash('sha256').update(g.text).digest('hex'), meta.graphs[caseOf(c).graph.variant], `case ${c}: the sha of the joined pages is the fixture's graph sha`);
    assert.equal(g.doc.id, 'wiki-hop');
    assert.equal(V[c].drawing.graph, 'wiki-hop');
    assert.equal(V[c].drawing.states.length, 4);
  }
  assert.equal(caseOf('e').graph.variant, 'pre', 'e ran the graph from before P1');
  assert.deepEqual(V.a.budgets.decisions, { used: 2, cap: 30, left: 28 });
  assert.deepEqual(V.a.budgets.actions, { used: 6, cap: 70, left: 64 });
  assert.deepEqual(V.a.budgets.escalations, { used: 0, cap: 30, left: 30 });
  assert.deepEqual(V.a.budgets.host, { used: 15, cap: HOST_STEP_CAP, left: 185 });
  assert.equal(V.a.budgets.host.used, J.a.filter((r) => r.kind.WorkflowStep).length);
});

// ---- b: a park, answered ---------------------------------------------------------------------------------------------------------------

test('b: a park on the margin, then the answer: pick.how is answer, and the time the person took is kept apart', () => {
  const s = step(V.b, 2);
  assert.equal(s.menu.verdict, 'asked');
  assert.deepEqual([s.menu.floorOk, s.menu.marginOk, s.menu.asked_for], [true, false, 'margin']);
  assert.equal(s.parks.length, 1);
  const p = s.park;
  assert.equal(p.call_id, 'w1-park0');
  assert.equal(p.why, 'top 0.500, margin 0.000 under 0.050');
  assert.equal(p.cause, 'margin');
  assert.equal(p.ask, 'Which link leads toward the article "Felidae"?');
  assert.deepEqual(p.options, [{ label: 'Egypt', p: 0.5 }, { label: 'Cat', p: 0.5 }]);
  assert.deepEqual([p.answer, p.kind, p.picked, p.invalid, p.retry], ['0', 'index', 0, false, null]);
  assert.deepEqual(s.pick, { label: 'Egypt', index: 0, how: 'answer', ref: 'e3' });
  assert.equal(s.decision, 1);
  assert.equal(s.escalations, 1);
  const ends = J.b.find((r) => r.kind.WorkflowEnd && r.kind.WorkflowEnd.status === 'Parked').ts_ms;
  const said = J.b.find((r) => r.kind.AskUser && r.kind.AskUser.answer === '0').ts_ms;
  assert.equal(p.waited_ms, said - ends);
  assert.equal(s.waited_ms, said - ends);
  assert.ok(s.duration_ms >= s.waited_ms);
  assert.deepEqual([V.b.end.asked, V.b.end.escalations, V.b.end.decisions], [1, 1, 2]);
});

test('b, cut where the first invocation ended: parked, waiting on that park; one record later the answer has it running again', () => {
  const v = reduce(upto('b', 1));
  assert.equal(v.status, 'parked');
  assert.equal(v.end, null);
  assert.equal(v.waiting.step, 2);
  assert.equal(v.waiting.park.call_id, 'w1-park0');
  assert.equal(v.waiting.park.answer, null);
  assert.equal(v.waiting.park, step(v, 2).park, 'the park of the step, not a copy of it');
  assert.equal(step(v, 2).pick, null, 'nobody has picked yet');
  assert.equal(step(v, 2).open, true, 'the run is still in this step, waiting');
  assert.deepEqual(v.marks.current, ['reading']);
  assert.equal(v.unresumable, null);
  assert.equal(v.open, null);
  const r = reduce(J.b.slice(0, upto('b', 1).length + 1));
  assert.equal(r.status, 'running');
  assert.equal(r.waiting, null);
  assert.equal(step(r, 2).park.answer, '0');
  assert.deepEqual(step(r, 2).pick, { label: 'Egypt', index: 0, how: 'answer', ref: null }, 'the answer picks it before the click is journaled');
});

// ---- c, g, j, l: a run that cannot be resumed ---------------------------------------------------------------------------------------------

test('c, g, j: unresumable names the first failed step, in eidolon\'s own words, byte for byte', () => {
  const named = { c: [5, 'w1-5', 'jev_entail'], g: [5, 'w1-5', 'jev_entail'], j: [6, 'w1-6', 'jev_choose'] };
  for (const [c, [n, id, tool]] of Object.entries(named)) {
    const u = V[c].unresumable, refused = stdout(c)[1];
    assert.equal(refused.status, 'refused');
    assert.equal(u.reason, refused.error, `case ${c}: the sentence the door refused with`);
    assert.deepEqual([u.run, u.step, u.call_id, u.tool], [1, n, id, tool], `case ${c}`);
    assert.equal(V[c].status, 'parked');
    assert.ok(V[c].waiting, `case ${c}: it is also waiting on a person`);
  }
});

test('c: the chooser is unavailable: the menu says so with p unknown, the park says why, jev_entail errored too', () => {
  const s = step(V.c, 2);
  assert.equal(s.menu.verdict, 'unavailable');
  assert.equal(s.menu.unavailable, 'stub: the model is not loaded');
  assert.deepEqual(s.menu.options, [{ label: 'Egypt', p: null }, { label: 'Cat', p: null }]);
  assert.deepEqual([s.menu.top1, s.menu.best_index, s.menu.sure], [null, null, null]);
  assert.equal(s.park.cause, 'unavailable');
  assert.equal(s.park.why, 'chooser unavailable: stub: the model is not loaded');
  assert.equal(s.arrival.entail.error, 'stub: the model is not loaded');
  assert.equal(s.arrival.entail.score, null);
  assert.equal(s.arrival.entail.hit, false);
  assert.equal(s.arrival.error, 'stub: the model is not loaded');
  assert.equal(s.arrival.hit, false);
  assert.deepEqual(s.errors.map((e) => e.tool), ['jev_entail', 'jev_choose']);
  assert.equal(s.decision, 1);
});

test('g: an entail error and the run goes on: the chooser works, the margin parks, and the park cannot be resumed', () => {
  const s = step(V.g, 2);
  assert.equal(s.arrival.entail.error, 'stub: the entail model is not loaded');
  assert.equal(s.menu.verdict, 'asked');
  assert.equal(s.park.cause, 'margin');
  assert.equal(V.g.status, 'parked');
  assert.match(V.g.unresumable.reason, /^refusing run #1: step 5 \(w1-5 jev_entail\) is journaled as a failure — stub: the entail model is not loaded\./);
});

test('j: jev_choose errors: the refusal names the jev_choose step, not the entail one', () => {
  const s = step(V.j, 2);
  assert.equal(s.arrival.entail.error, null);
  assert.equal(s.arrival.entail.score, 0.1);
  assert.equal(V.j.unresumable.tool, 'jev_choose');
  assert.equal(s.menu.unavailable, 'stub: the chooser model is not loaded');
});

test('l: SIGTERM, the program wins: the chooser came back cancelled, the run parked, and says it was interrupted; the park cannot resume', () => {
  const v = V.l;
  assert.equal(v.status, 'parked');
  assert.equal(v.interrupted, true);
  assert.equal(step(v, 2).menu.unavailable, 'cancelled while the tool was running');
  assert.equal(step(v, 2).park.why, 'chooser unavailable: cancelled while the tool was running');
  assert.deepEqual([v.unresumable.call_id, v.unresumable.tool], ['w1-6', 'jev_choose']);
  assert.match(v.unresumable.reason, /is journaled as a failure — cancelled while the tool was running\./);
  for (const c of CASES) if (c !== 'l') assert.equal(V[c].interrupted, false, `case ${c} was not interrupted`);
});

// ---- d: cancelled -----------------------------------------------------------------------------------------------------------------------

test('d: SIGTERM, the cancel token wins: Failed, with the in-flight step still open; the model says cancelled', () => {
  const v = V.d;
  assert.equal(v.status, 'cancelled');
  assert.equal(v.end.door, 'Failed');
  assert.equal(v.end.reason, 'the run was cancelled');
  assert.deepEqual(v.open, { call_id: 'w1-6', index: 6, tool: 'jev_choose', ts_ms: J.d.find((r) => r.kind.WorkflowStep && r.kind.WorkflowStep.call_id === 'w1-6').ts_ms });
  assert.equal(step(v, 2).menu.pending, true, 'jev_choose never answered');
  assert.equal(step(v, 2).menu.verdict, 'pending');
  assert.equal(step(v, 2).pick, null);
  assert.equal(v.unresumable, null, 'nothing failed in it');
  assert.deepEqual(v.marks.current, []);
  assert.equal(v.active, 'reading');
});

// ---- e, i: failed, whatever the door said -------------------------------------------------------------------------------------------------

test('e: failed even though the door said completed: the last jev_mark is final failed, the reason is the cut snapshot, the exit was 0', () => {
  const v = V.e;
  assert.equal(caseOf('e').invocations[0].exit, 0);
  assert.equal(stdout('e')[0].status, 'completed');
  assert.equal(v.status, 'failed');
  assert.equal(v.end.door, 'Completed');
  assert.equal(v.end.state, 'failed');
  assert.equal(v.end.outcome, 'reached', 'the graph reached a final state; it is the state that is a failure');
  const r = reportOf('e');
  assert.equal(r.final_state, 'failed');
  assert.equal(v.end.reason, r.output.error);
  assert.equal(v.end.cause, r.output.reason);
  assert.equal(v.end.cause, 'truncated');
  assert.deepEqual(v.marks, { current: [], visited: ['open', 'reading'], final: [], failed: ['failed'] });
  const s = step(v, 2);
  assert.deepEqual(s.page.truncated, { got: 2059, declared: 13578 });
  assert.equal(s.page.ok, false);
  assert.equal(s.menu, null, 'no menu from a page that came short');
  assert.equal(s.arrival, null);
  assert.deepEqual(v.path, [], 'a page that came short is not on the path');
});

test('i: failed after a click to a page that does not exist: the reason is the tool\'s own error text', () => {
  const v = V.i, r = reportOf('i');
  assert.equal(v.status, 'failed');
  assert.equal(v.end.door, 'Completed');
  assert.equal(v.end.reason, r.output.error);
  assert.equal(v.end.cause, r.output.reason);
  assert.equal(v.end.cause, 'failed');
  assert.match(v.end.reason, /awk: fatal: cannot open file/);
  assert.equal(v.unresumable, null, 'a tool failed, but the run is over: there is nothing to resume, so no refusal to explain');
  const s = step(v, 3);
  assert.equal(s.page.ok, false);
  assert.equal(s.page.heading, 'Registered cat breed number 002', 'the head of an error result still says which page');
  assert.match(s.page.error, /awk: fatal/);
  assert.equal(s.menu, null);
  assert.deepEqual(v.marks.failed, ['failed']);
  assert.deepEqual(v.path, ['List of cat breeds']);
});

test('i: a park under the floor over 64 options, answered by the label of an option', () => {
  const s = step(V.i, 2);
  assert.equal(s.menu.options.length, 64);
  assert.equal(s.park.cause, 'floor');
  assert.equal(s.park.why, 'top 0.015 under floor 0.350');
  assert.deepEqual([s.menu.floorOk, s.menu.marginOk, s.menu.asked_for], [false, false, 'floor']);
  assert.equal(s.menu.top1, 0.015625);
  assert.equal(s.menu.best_index, 0);
  assert.equal(s.park.answer, 'Registered cat breed number 002');
  assert.deepEqual([s.park.kind, s.park.picked], ['label', 1]);
  assert.deepEqual(s.pick, { label: 'Registered cat breed number 002', index: 1, how: 'answer', ref: 'e4' });
  assert.equal(s.jev.collisions.length, 0, 'the longest label is 31 bytes: no two share a cut');
});

// ---- f: picks without a menu ---------------------------------------------------------------------------------------------------------------

test('f: single and rule picks, no jev_choose at all', () => {
  const v = V.f;
  assert.deepEqual(v.steps.filter((s) => s.pick).map((s) => [s.pick.label, s.pick.how]), [['Cat', 'single'], ['Ancient Egypt', 'single'], ['Egypt', 'rule']]);
  assert.deepEqual(v.steps.filter((s) => s.menu).map((s) => s.menu.options.length), [1, 1, 1]);
  assert.ok(v.steps.every((s) => s.jev === null), 'no chooser call, so no jev view');
  assert.deepEqual(v.steps.map((s) => s.decision), [null, 1, 2, 3, null]);
  assert.equal(v.end.title, 'Egypt');
  assert.equal(v.end.asked, 0);
  assert.equal(step(v, 3).menu.reconstructed, true, 'Felidae was visited: only Ancient Egypt is left on the Cat page');
});

// ---- h: an invalid answer ---------------------------------------------------------------------------------------------------------------

test('h: an invalid answer re-parks and uses an escalation; then 0', () => {
  const s = step(V.h, 2);
  assert.equal(s.parks.length, 2);
  assert.equal(s.escalations, 2);
  const [one, two] = s.parks;
  assert.deepEqual([one.call_id, one.answer, one.kind, one.invalid, one.picked, one.retry], ['w1-park0', 'banana', 'invalid', true, null, null]);
  assert.deepEqual([two.call_id, two.answer, two.kind, two.invalid, two.picked, two.retry], ['w1-park1', '0', 'index', false, 0, 'banana']);
  assert.equal(two.why, one.why, 'the same menu, asked again');
  assert.match(two.prompt, /^'banana' is not one of the options\.\njev wiki-hop: /);
  assert.deepEqual(s.pick, { label: 'Egypt', index: 0, how: 'answer', ref: 'e3' });
  assert.deepEqual(V.h.budgets.escalations, { used: 2, cap: 30, left: 28 });
  assert.deepEqual([V.h.end.escalations, V.h.end.asked, V.h.end.decisions], [2, 1, 2], 'two escalations, one decision asked of a person');
  assert.ok(one.waited_ms > 0 && two.waited_ms > 0);
  assert.equal(s.waited_ms, one.waited_ms + two.waited_ms);
});

test('h, cut where the second invocation ended: parked again, on the second park, which says banana was refused', () => {
  const v = reduce(upto('h', 2));
  assert.equal(v.status, 'parked');
  assert.equal(v.waiting.park.call_id, 'w1-park1');
  assert.equal(v.waiting.park.retry, 'banana');
  assert.equal(step(v, 2).parks[0].invalid, true);
  assert.equal(v.budgets.escalations.used, 2);
  assert.equal(step(v, 2).pick, null, 'an invalid answer picks nothing');
});

// ---- k: a run cut, then resumed -----------------------------------------------------------------------------------------------------------

test('k: killed in jev_choose it has no end: running, that step open and not a failure; after the resume it arrives', () => {
  const v = reduce(upto('k', 1));
  assert.equal(caseOf('k').invocations[0].exit, 137);
  assert.equal(v.status, 'running');
  assert.equal(v.end, null);
  assert.deepEqual([v.open.call_id, v.open.tool], ['w1-6', 'jev_choose']);
  assert.equal(v.unresumable, null, 'nothing in it failed: resume continues it');
  assert.equal(step(v, 2).menu.pending, true);
  assert.equal(step(v, 2).open, true);
  assert.deepEqual(v.marks.current, ['reading']);
  const all = V.k;
  assert.equal(all.status, 'arrived');
  assert.equal(all.open, null);
  assert.deepEqual(step(all, 2).pick, { label: 'Egypt', index: 0, how: 'chooser', ref: 'e3' }, 'the resumed step has its result once');
  assert.deepEqual(all.path, ['Ancient Egypt', 'Egypt', 'Felidae']);
  assert.equal(all.budgets.host.used, J.k.filter((r) => r.kind.WorkflowStep).length, 'a step with two verdicts is still one step');
});

// ---- the same view, however the records arrive ------------------------------------------------------------------------------------------

const lcg = (seed) => () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;

test('feeding records one at a time equals feeding them all at once, in every case', () => {
  for (const c of CASES) {
    const one = newRun();
    for (const r of J[c]) feed(one, [r]);
    assert.deepEqual(view(one), V[c], `case ${c}`);
  }
});

test('in chunks of any size, with each poll overlapping the last, and with records handed over twice: the same view', () => {
  const rnd = lcg(11);
  for (const c of CASES) {
    const run = newRun();
    for (let at = 0; at < J[c].length;) {
      const n = 1 + Math.floor(rnd() * 9);
      feed(run, J[c].slice(Math.max(0, at - Math.floor(rnd() * 5)), at + n)); // starts a few records back: those are already taken
      at += n;
    }
    assert.deepEqual(view(run), V[c], `case ${c}`);
    const before = view(run);
    feed(run, J[c]);
    feed(run, J[c].slice(7, 19));
    assert.equal(view(run), before, `case ${c}: nothing new, so the very same view`);
  }
});

test('feedText takes the body of a log poll: ndjson, a blank line and a line that is not JSON skipped; records may come as lines to feed', () => {
  const lines = J.h.map((r) => JSON.stringify(r));
  const text = lines.slice(0, 20).join('\n') + '\n\n{not json\n' + lines.slice(20).join('\n') + '\n';
  assert.deepEqual(view(feedText(newRun(), text)), V.h);
  assert.deepEqual(view(feed(newRun(), lines)), V.h);
  assert.deepEqual(view(feedText(newRun(), '')), view(newRun()));
  assert.equal(view(newRun()).status, 'running');
  assert.equal(view(newRun()).lastId, -1);
});

test('the view is one object until a record is taken, and its drawing keeps its identity while the graph does', () => {
  const run = newRun();
  feed(run, J.a.slice(0, 20));
  const v1 = view(run);
  assert.equal(view(run), v1);
  feed(run, J.a.slice(20, 21));
  const v2 = view(run);
  assert.notEqual(v2, v1);
  assert.equal(v2.drawing, v1.drawing, 'one project() per graph: draw.js memoizes its layout on it');
  assert.equal(v2.graph.doc, v1.graph.doc);
  feed(run, J.a.slice(21));
  assert.equal(view(run).drawing, v1.drawing);
});

test('at every jev_mark of every case, the decisions and actions counted are the counters the mark carries', () => {
  let marks = 0;
  for (const c of CASES) {
    J[c].forEach((r, k) => {
      const s = r.kind.WorkflowStep;
      if (!s || s.tool !== 'jev_mark') return;
      const m = JSON.parse(s.input), v = reduce(J[c].slice(0, k + 1));
      assert.equal(v.budgets.actions.used, m.actions, `case ${c} record ${r.id}: actions`);
      assert.equal(v.budgets.decisions.used, m.step, `case ${c} record ${r.id}: decisions`);
      assert.equal(v.active, m.state);
      marks++;
    });
  }
  assert.equal(marks, CASES.flatMap((c) => J[c]).filter((r) => r.kind.WorkflowStep && r.kind.WorkflowStep.tool === 'jev_mark').length);
  assert.equal(marks, 51);
});

test('every prefix of every journal reduces: nothing throws, the cursor is the last id, the record count is the revision', () => {
  for (const c of CASES) {
    for (let k = 0; k <= J[c].length; k++) {
      const v = reduce(J[c].slice(0, k));
      assert.equal(v.lastId, k - 1);
      assert.equal(v.rev, k);
      assert.ok(['running', 'parked', 'arrived', 'failed', 'cancelled', 'stopped', 'exhausted', 'done'].includes(v.status));
      assert.ok(v.steps.every((s, i) => s.n === i + 1));
    }
  }
});

test('records the model does not know are ignored, so a newer eidolon does not break it', () => {
  const extra = [];
  J.a.forEach((r) => {
    extra.push(r, { kind: { FutureThing: { x: 1 } } }, { kind: { ApiRequest: { consumer: 'plugin:jev', service: 'jev' } } }, { kind: 'odd' }, 7, null);
  });
  extra.forEach((r, i) => { if (r && typeof r === 'object' && r.kind && !('id' in r)) r.ts_ms = 5; });
  const v = reduce(extra);
  for (const k of ['status', 'steps', 'path', 'trail', 'end', 'budgets', 'marks']) assert.deepEqual(v[k], V.a[k], k);
});

test('no case journaled a ModelSpend; a ModelSpend that is there shows up in spend', () => {
  for (const c of CASES) assert.deepEqual(V[c].spend, [], `case ${c}`);
  const usage = { input: 3, output: 4 };
  const v = reduce([...J.a, { id: 51, ts_ms: 1, kind: { ModelSpend: { key: 'mock', call_id: 'w1-3', usage } } }]);
  assert.deepEqual(v.spend, [{ id: 51, ts_ms: 1, key: 'mock', call_id: 'w1-3', usage }]);
});

// ---- the drawing it feeds ---------------------------------------------------------------------------------------------------------------

function* nodes(v) {
  if (Array.isArray(v)) { for (const x of v) yield* nodes(x); return; }
  if (!v || typeof v !== 'object' || v.type === undefined) return;
  yield v;
  yield* nodes(v.props && v.props.children);
}
const klass = (v) => String((v.props && v.props.class) || '').split(/\s+/);
const box = (svg, id) => [...nodes(svg)].find((v) => v.type === 'g' && klass(v).includes('gn') && v.props['data-id'] === id);

test('marks and drawing go to drawGraph as they are: a failed graph lights failed, an arrived one arrived, a parked run its state', () => {
  const e = drawGraph(V.e.drawing, V.e.marks);
  assert.ok(klass(e).includes('live'));
  assert.ok(klass(box(e, 'failed')).includes('failed'));
  assert.ok(!klass(box(e, 'arrived')).includes('final'));
  const a = drawGraph(V.a.drawing, V.a.marks);
  assert.ok(klass(box(a, 'arrived')).includes('final'));
  assert.ok(klass(box(a, 'reading')).includes('visited'));
  const c = drawGraph(V.c.drawing, V.c.marks);
  assert.ok(klass(box(c, 'reading')).includes('current'));
  assert.ok(!klass(box(c, 'arrived')).includes('final'));
  const d = drawGraph(V.d.drawing, V.d.marks);
  assert.ok(![...nodes(d)].some((v) => klass(v).includes('current')), 'a cancelled run is nowhere any more');
});

// ---- no DOM, no network, no markup --------------------------------------------------------------------------------------------------------

test('the module touches no DOM and no network and builds no markup: a scan of its source', () => {
  const src = readFileSync(new URL('../graph/jevrun.js', import.meta.url), 'utf8').replace(/\r\n/g, '\n');
  for (const bad of ['document.', 'window.', 'innerHTML', 'outerHTML', 'insertAdjacentHTML', 'fetch(', 'eval(', 'new Function', 'XMLHttpRequest', 'localStorage',
    'setTimeout', 'setInterval', 'Date.now', 'new Date', 'Math.random', 'dangerouslySetInnerHTML', '<script', 'html`']) {
    assert.ok(!src.includes(bad), `jevrun.js has ${bad}`);
  }
  assert.deepEqual([...src.matchAll(/^import .* from '(.*)';$/gm)].map((m) => m[1]), ['./project.js']);
});

// ---- the pieces -----------------------------------------------------------------------------------------------------------------------------

test('cutBytes: the first n bytes as text, a character the cut falls inside dropped whole', () => {
  assert.deepEqual(cutBytes('abc', 8), { text: 'abc', bytes: 3, clipped: false, key: 'abc' });
  const a = cutBytes('x'.repeat(31) + 'é' + 'tail', 32); // é is bytes 31 and 32: the cut is between them
  assert.deepEqual([a.text, a.clipped, a.bytes, a.key.length], ['x'.repeat(31), true, 31 + 2 + 4, 32]);
  const b = cutBytes('x'.repeat(30) + 'é' + 'tail', 32); // é is bytes 30 and 31: it fits
  assert.equal(b.text, 'x'.repeat(30) + 'é');
  const c = cutBytes('a'.repeat(190) + '€' + 'z', 192); // € is three bytes, 190 to 192
  assert.deepEqual([c.text.length, c.clipped, c.bytes], [190, true, 194]);
  const d = cutBytes('a'.repeat(30) + '😀' + 'z', 32); // four bytes, 30 to 33
  assert.equal(d.text, 'a'.repeat(30));
  const mixed = 'ab€é😀z€é😀'.repeat(6);
  for (let n = 0; n <= 80; n++) {
    const r = cutBytes(mixed, n);
    assert.ok(mixed.startsWith(r.text), `n=${n}: a prefix`);
    assert.ok(!r.text.includes('�'), `n=${n}: valid UTF-8`);
    assert.ok(Buffer.byteLength(r.text) <= n && Buffer.byteLength(r.text) > n - 4, `n=${n}: at most n bytes, at most 3 short`);
    assert.equal(r.bytes, Buffer.byteLength(mixed));
  }
  assert.equal(cutBytes('', 5).text, '');
  assert.equal(cutBytes(12345, 3).text, '123', 'not a string: made one');
});

test('parseSnapshot: the head block and the tree; anything else is not a snapshot; a body that came short is flagged', () => {
  const tree = '- main [ref=e1]:\n  - heading "Cat" [level=1] [ref=e2]';
  const ok = parseSnapshot(`url: https://x.test/wiki/Cat\ntitle: Cat - Test\nheading: Cat\nscope: main\nchars: ${tree.length}\n\n${tree}`);
  assert.deepEqual([ok.head.url, ok.head.title, ok.head.heading, ok.declared, ok.got, ok.truncated, ok.h1], ['https://x.test/wiki/Cat', 'Cat - Test', 'Cat', tree.length, tree.length, false, 'Cat']);
  const short = parseSnapshot(`url: u\ntitle: t\nchars: 999\n\n${tree}`);
  assert.deepEqual([short.declared, short.got, short.truncated], [999, tree.length, true]);
  assert.equal(short.h1, 'Cat', 'no heading line: the first level 1 heading of the tree');
  assert.equal(parseSnapshot('opened https://x.test\n'), null);
  assert.equal(parseSnapshot('url: u\ntitle: t\nno blank line follows'), null);
  assert.equal(parseSnapshot('url: u\ntitle: t\nBad-Key: x\n\nbody'), null, 'head keys are lower case words');
  assert.equal(parseSnapshot('title: t\nurl: u\n\nbody'), null, 'url first, then title');
  assert.equal(parseSnapshot(null), null);
  assert.equal(parseSnapshot('url: u\ntitle: t\nchars: 3\n\nabc').truncated, false);
  assert.equal(parseSnapshot('url: u\ntitle: t\nchars: 2\n\n😀😀').truncated, false, 'characters, not UTF-16 units: two emoji is two');
  assert.equal(parseSnapshot('url: u\ntitle: t\nchars: 2\n\n😀').truncated, true);
  assert.equal(parseSnapshot('url: u\ntitle: t\nchars: 1\n\n😀').truncated, false);
});

test('parseRefs: element, name, ref, url, landmark; escapes kept as written; the quoted form; no ref no record', () => {
  const tree = [
    '- banner [ref=e1]:', '  - link "Menu" [ref=e2]:', '    - /url: /menu',
    '- main [ref=e3]:', '  - heading "Cat" [level=1] [ref=e4]',
    '  - link "Say \\"hi\\"" [ref=e5]:', '    - /url: /wiki/Hi',
    "  - 'link \"a: b\" [ref=e6]': tail", '  - link "No ref"', '  - text: plain',
  ].join('\n');
  const r = parseRefs(tree);
  assert.deepEqual(r.map((x) => [x.ref, x.role, x.name, x.url, x.landmark]), [
    ['e1', 'banner', null, null, null], ['e2', 'link', 'Menu', '/menu', 'banner'], ['e3', 'main', null, null, null], ['e4', 'heading', 'Cat', null, 'main'],
    ['e5', 'link', 'Say \\"hi\\"', '/wiki/Hi', 'main'], ['e6', 'link', 'a: b', null, 'main'],
  ]);
  assert.equal(r.find((x) => x.ref === 'e4').level, 1);
  assert.equal(r.find((x) => x.ref === 'e6').text, 'tail');
  assert.equal(parseRefs(tree, true).length, 8, 'with `all`, the ones with no ref too');
  assert.deepEqual(parseRefs(''), []);
});

test('classifyAnswer: stop first, then the exact label, then a number in the menu, else invalid; the answer is trimmed', () => {
  const L = ['Egypt', 'Cat'];
  assert.deepEqual(classifyAnswer('0', L), { kind: 'index', index: 0, reason: null });
  assert.deepEqual(classifyAnswer(' 1 ', L), { kind: 'index', index: 1, reason: null });
  assert.deepEqual(classifyAnswer('+1', L), { kind: 'index', index: 1, reason: null });
  assert.ok(Object.is(classifyAnswer('-0', L).index, 0), '-0 is index 0, not minus zero');
  assert.deepEqual(classifyAnswer('Cat', L), { kind: 'label', index: 1, reason: null });
  assert.equal(classifyAnswer('cat', L).kind, 'invalid', 'the label is exact');
  assert.equal(classifyAnswer('2', L).kind, 'invalid');
  assert.equal(classifyAnswer('-1', L).kind, 'invalid');
  assert.equal(classifyAnswer('1.0', L).kind, 'invalid');
  assert.equal(classifyAnswer('banana', L).kind, 'invalid');
  assert.equal(classifyAnswer('', L).kind, 'invalid');
  assert.equal(classifyAnswer('0', []).kind, 'invalid', 'an empty menu has nothing to number');
  assert.deepEqual(classifyAnswer('1', ['1', '0']), { kind: 'label', index: 0, reason: null }, 'a label beats a number');
  assert.deepEqual(classifyAnswer('0', ['1', '0']), { kind: 'label', index: 1, reason: null });
  assert.deepEqual(classifyAnswer('stop', L), { kind: 'stop', index: null, reason: 'stopped at a park' });
  assert.deepEqual(classifyAnswer('stop: too risky', L), { kind: 'stop', index: null, reason: 'too risky' });
  assert.deepEqual(classifyAnswer('stop:', L), { kind: 'stop', index: null, reason: 'stopped at a park' });
  assert.deepEqual(classifyAnswer('stop:   ', L), { kind: 'stop', index: null, reason: 'stopped at a park' });
  assert.deepEqual(classifyAnswer('stop', ['stop']).kind, 'stop', 'stop is read before labels');
  assert.equal(classifyAnswer('stopper', L).kind, 'invalid');
});

// ---- journals built here, for what the twelve do not hold ---------------------------------------------------------------------------------

const GRAPH = readFileSync(new URL('../../../eidolon-plugins/jev/graphs/wiki-hop.json', import.meta.url), 'utf8').replace(/\r\n/g, '\n');
const SNAP = { max: 100, roles: ['link'], within: 'main' };

// records in the shape F0's README lists: ids dense, call ids w<run>-<n>, a park w<run>-park<n>
function journal() {
  const recs = [];
  let id = 0, ts = 1_700_000_000_000, n = 0, run = 0, parks = 0, ask = null;
  const add = (kind, body) => { recs.push({ id, parent: id === 0 ? null : id - 1, ts_ms: (ts += 7), kind: { [kind]: body } }); id++; };
  const j = {
    recs,
    add,
    session() { add('SessionStart', { model: 'mock', cwd: '/tmp/x', system: null }); return j; },
    start(args = { start: 'Alpha', goal: 'Omega' }) {
      run = id; n = 0; parks = 0;
      add('WorkflowStart', { plugin: 'jev', workflow: 'wiki-hop', version: '', hash: 'h', args: typeof args === 'string' ? args : JSON.stringify(args) });
      return j;
    },
    call(tool, input, out, bad = false) {
      const call_id = `w${run}-${n}`;
      add('WorkflowStep', { run, index: n, call_id, tool, input: JSON.stringify(input) });
      add('PolicyVerdict', { tool_use_id: call_id, tool, reason: 'r', structural: false, outcome: 'Approved', note: 'n' });
      if (out !== undefined) add('ToolResult', { tool_use_id: call_id, content: out, is_error: bad });
      n++;
      return j;
    },
    mark(kind, state, step, actions, extra = {}) { return j.call('jev_mark', { actions, graph: 'wiki-hop', kind, state, step, ...extra }, `marked wiki-hop ${kind} ${state}`); },
    pages(text, size = 1700) {
      const chunks = [];
      for (let i = 0; i < text.length; i += size) chunks.push(text.slice(i, i + size));
      chunks.forEach((body, i) => j.call('jev_graph', { from: i + 1, graph: 'wiki-hop' }, `jev-page next=${i === chunks.length - 1 ? 0 : i + 2}\n${body}`));
      return j;
    },
    park(prompt) { ask = `w${run}-park${parks++}`; add('AskUser', { call_id: ask, prompt, answer: null }); return j; },
    answer(text, prompt) { add('AskUser', { call_id: ask, prompt, answer: text }); return j; },
    end(status) { add('WorkflowEnd', { run, status }); return j; },
  };
  return j;
}

const tree = (heading, links) => {
  const body = ['- main [ref=e1]:', `  - heading "${heading}" [level=1] [ref=e2]`,
    ...links.flatMap(([name, ref, url]) => [`  - link "${name}" [ref=${ref}]:`, `    - /url: ${url}`])].join('\n');
  return `url: https://x.test/wiki/${heading}\ntitle: ${heading} - Test\nheading: ${heading}\nscope: main\nchars: ${[...body].length}\n\n${body}`;
};
const entail = (score) => JSON.stringify({ results: [{ label: score >= 0.8 ? 'entailment' : 'neutral', hypothesis: 'This is the encyclopedia article about Omega.',
  scores: { neutral: 1 - score, entailment: score, contradiction: 0 } }] });
const entailIn = (heading) => ({ hypotheses: ['This is the encyclopedia article about Omega.'], premise: `Title: ${heading} - Test. Heading: ${heading}.` });
const probsOut = (probs, best) => JSON.stringify({ probs, best });
const evenly = (labels) => probsOut(Object.fromEntries(labels.map((l) => [l, 1 / labels.length])), labels[0]);
const PARK_END = 'Answer with the number or the exact label, or `stop` (or `stop: <reason>`) to end the run.';
const promptOf = (why, labels, ps, step = 1) => `jev wiki-hop: ${why}\nWhich link leads toward the article "Omega"?\nstate: reading (step ${step})\n`
  + labels.map((l, i) => `  ${i}  ${ps[i].toFixed(3)}  ${l}`).join('\n') + '\n' + PARK_END;

// the journal of a run that has opened Alpha: WorkflowStart, the graph in three pages, the open state
function base(args) {
  return journal().session().start(args).pages(GRAPH).mark('enter', 'open', 0, 0)
    .call('browser_open', { confine: ['https://x.test'], url: 'https://x.test/wiki/Alpha' }, 'opened https://x.test/wiki/Alpha\n');
}
// a reading state up to the chooser (options, probs: the jev_choose call; null for none)
function reading(j, heading, links, options, probs, step, actions) {
  j.mark('enter', 'reading', step, actions).call('browser_snapshot', SNAP, tree(heading, links)).call('jev_entail', entailIn(heading), entail(0.1));
  if (options) j.call('jev_choose', { context: `Target article: Omega\nCurrent article: ${heading}\n`, options }, probs);
  return j;
}
const BG = [['Beta', 'e3', '/wiki/Beta'], ['Gamma', 'e4', '/wiki/Gamma']];

test('a graph of several pages is joined into the text the run used, and drawn', () => {
  const v = reduce(base().recs);
  assert.deepEqual([v.graph.pages, v.graph.source, v.graph.label, v.graph.error], [3, 'journal', null, null]);
  assert.equal(v.graph.text, GRAPH);
  assert.equal(v.drawing.states.length, 4);
  assert.equal(v.meta.run, 1);
  assert.equal(v.meta.graph, 'wiki-hop');
  assert.equal(v.hostSteps, undefined);
  assert.equal(v.budgets.host.used, 3 + 2, 'three pages, a mark and the open');
});

test('a graph that cannot be read from the journal is not drawn from it: the installed one is, and the label says so', () => {
  const first = (extra) => journal().session().start().call('jev_graph', { from: 1, graph: 'wiki-hop' }, ...extra);
  const bad = {
    'a page that promises another, the journal ending first': [first([`jev-page next=2\n${GRAPH.slice(0, 100)}`]), null],
    'a page with no header': [first(['{"id":"wiki-hop"}']), 'a page of the graph had no jev-page header'],
    'a header that is no number': [first(['jev-page next=abc\nx']), 'a page of the graph had a bad jev-page header: jev-page next=abc'],
    'pages that do not advance': [first(['jev-page next=1\nx']), "the graph's pages do not advance"],
    'text that is not JSON': [first(['jev-page next=0\n{nope']), 'the graph is not JSON'],
    'a jev_graph that failed': [first(['jev_graph: no such graph', true]), 'jev_graph: no such graph'],
  };
  for (const [what, [j, error]] of Object.entries(bad)) {
    const none = reduce(j.recs);
    assert.deepEqual([none.graph.source, none.graph.doc, none.drawing, none.graph.error], ['none', null, null, error], what);
    const inst = reduce(j.recs, { graph: GRAPH });
    assert.deepEqual([inst.graph.source, inst.graph.label, inst.graph.error], ['installed', FALLBACK_LABEL, error], what);
  }
  assert.equal(FALLBACK_LABEL, 'drawn from the installed graph; may differ from the one this run used');
});

test('with no jev_graph pages in the journal at all, the installed graph (text or object) gives floor, margin and threshold, labelled', () => {
  const noPages = J.a.filter((r) => !(r.kind.WorkflowStep && r.kind.WorkflowStep.tool === 'jev_graph') && !(r.kind.PolicyVerdict && r.kind.PolicyVerdict.tool === 'jev_graph')
    && !(r.kind.ToolResult && r.kind.ToolResult.tool_use_id === 'w1-0'));
  assert.equal(noPages.length, J.a.length - 3);
  for (const installed of [GRAPH, JSON.parse(GRAPH)]) {
    const v = reduce(noPages, { graph: installed });
    assert.deepEqual([v.graph.source, v.graph.label, v.graph.pages], ['installed', FALLBACK_LABEL, 0]);
    assert.equal(v.graph.text, typeof installed === 'string' ? GRAPH : null);
    assert.equal(v.drawing.states.length, 4);
    assert.deepEqual(step(v, 2).menu.floor, 0.35);
    assert.equal(step(v, 2).arrival.entail.threshold, 0.8);
    assert.deepEqual(step(v, 2).pick, step(V.a, 2).pick);
    assert.deepEqual(v.budgets.decisions, { used: 2, cap: 30, left: 28 });
  }
  assert.equal(reduce(noPages, { graph: 'not json' }).graph.source, 'none');
  assert.equal(reduce(noPages, { graph: null }).graph.source, 'none');
});

test('with no graph at all there is no floor to test: the menu is scored, the pick still follows the click, and the caps are unknown', () => {
  const j = journal().session().start().mark('enter', 'reading', 0, 0).call('browser_snapshot', SNAP, tree('Alpha', BG))
    .call('jev_choose', { context: 'c', options: ['Beta', 'Gamma'] }, probsOut({ Beta: 0.9, Gamma: 0.1 }, 'Beta')).call('browser_click', { ref: 'e4' }, 'clicked e4 -> u\n');
  const v = reduce(j.recs);
  const s = step(v, 1);
  assert.deepEqual([s.menu.floor, s.menu.margin, s.menu.floorOk, s.menu.sure, s.menu.verdict], [null, null, null, null, 'scored']);
  assert.deepEqual(s.pick, { label: 'Gamma', index: 1, how: 'chooser', ref: 'e4' }, 'what was clicked, whatever the scorer thought');
  assert.deepEqual(v.budgets.decisions, { used: 1, cap: null, left: null });
  assert.equal(s.arrival.entail, null, 'no jev_entail was asked');
  assert.deepEqual([s.arrival.equals.hit, s.arrival.hit], [false, false]);
});

test('a state\'s own floor and margin come before the defaults, the defaults before 0.5 and 0.1; the threshold is the entails guard\'s; compound states are walked', () => {
  const doc = { id: 'g', initial: 'reading', meta: { jev: { defaults: { floor: 0.2, margin: 0.02, threshold: 0.7 }, budget: { steps: 5, actions: 6, escalations: 7 } } },
    states: { reading: { meta: { floor: 0.9 }, always: [{ guard: { type: 'equals' }, target: 'a' }, { guard: { type: 'entails', params: { threshold: 0.55 } }, target: 'a' }] },
      investigate: { states: { identify: { meta: { floor: 0.77, margin: 0 }, always: { guard: { type: 'entails', params: {} }, target: 'x' } } } }, a: { type: 'final' } } };
  const run = (state, probs) => {
    const j = journal().session().start().mark('enter', state, 0, 0).call('browser_snapshot', SNAP, tree('Alpha', BG)).call('jev_entail', entailIn('Alpha'), entail(0.6))
      .call('jev_choose', { context: 'c', options: ['Beta', 'Gamma'] }, probsOut(probs, 'Beta'));
    return step(reduce(j.recs, { graph: doc }), 1);
  };
  const r = run('reading', { Beta: 0.6, Gamma: 0.1 });
  assert.deepEqual([r.menu.floor, r.menu.margin, r.arrival.entail.threshold, r.arrival.entail.hit], [0.9, 0.02, 0.55, true]);
  assert.equal(r.menu.verdict, 'asked');
  assert.equal(r.menu.asked_for, 'floor');
  const c = run('investigate.identify', { Beta: 0.8, Gamma: 0.8 });
  assert.deepEqual([c.menu.floor, c.menu.margin, c.arrival.entail.threshold, c.arrival.entail.hit], [0.77, 0, 0.7, false]);
  assert.equal(c.menu.verdict, 'sure', 'margin 0: a tie is enough');
  const v = reduce(journal().session().start().mark('enter', 'x', 0, 0).recs, { graph: doc });
  assert.deepEqual([v.budgets.decisions.cap, v.budgets.actions.cap, v.budgets.escalations.cap], [5, 6, 7]);
  const bare = reduce(journal().session().start().mark('enter', 'x', 0, 0).recs, { graph: { id: 'g', states: { x: {} } } });
  assert.deepEqual([bare.budgets.decisions.cap, bare.budgets.actions.cap, bare.budgets.escalations.cap], [50, 100, 10], 'the interpreter\'s own defaults');
  const f = reduce(journal().session().start().mark('enter', 'x', 0, 0).call('browser_snapshot', SNAP, tree('Alpha', BG))
    .call('jev_choose', { context: 'c', options: ['Beta', 'Gamma'] }, probsOut({ Beta: 0.45, Gamma: 0.3 }, 'Beta')).recs, { graph: { id: 'g', states: { x: {} } } });
  assert.deepEqual([step(f, 1).menu.floor, step(f, 1).menu.margin, step(f, 1).menu.asked_for], [0.5, 0.1, 'floor']);
});

test('the cut the scorer reads is a setting: CUT, and newRun({ cut })', () => {
  assert.deepEqual(CUT, { context: 192, option: 32 });
  const j = reading(base(), 'Alpha', BG, ['Betabeta', 'Betaalph'], evenly(['Betabeta', 'Betaalph']), 0, 1);
  const by = (o) => step(reduce(j.recs, o), 2).jev;
  assert.deepEqual(by({}).collisions, []);
  assert.deepEqual(by({ cut: { option: 4 } }).collisions, [[0, 1]]);
  assert.deepEqual(by({ cut: { option: 4, context: 6 } }).options_cut, ['Beta', 'Beta']);
  assert.equal(by({ cut: { context: 6 } }).context_cut, 'Target');
});

// ---- jev's view: what the scorer reads --------------------------------------------------------------------------------------------------

function jevOf(options, context) {
  const j = base().mark('enter', 'reading', 0, 1).call('browser_snapshot', SNAP, tree('Alpha', options.map((o, i) => [o, 'e' + (3 + i), '/wiki/' + i])))
    .call('jev_entail', entailIn('Alpha'), entail(0.1)).call('jev_choose', { context, options }, evenly(options));
  return step(reduce(j.recs), 2).jev;
}

test('jev\'s view: the context cut at 192 bytes, each option at 32; options that agree there are flagged, with the pair', () => {
  const L1 = 'The quick brown fox jumps over the lazy dog (north)', L2 = 'The quick brown fox jumps over the lazy dog (south)';
  const ctx = 'Target article: Omega\nCurrent article: Alpha\n' + 'z'.repeat(300);
  const s = jevOf([L1, L2, 'Short'], ctx);
  assert.equal(s.context_cut, ctx.slice(0, 192));
  assert.deepEqual([s.context_bytes, s.context_clipped], [Buffer.byteLength(ctx), true]);
  assert.deepEqual(s.options_cut, [L1.slice(0, 32), L1.slice(0, 32), 'Short']);
  assert.deepEqual(s.options_clipped, [true, true, false]);
  assert.deepEqual(s.collisions, [[0, 1]]);
  assert.equal(s.top_tie, true, 'the best of them is one of the pair: the scorer cannot tell its top two apart');
  const short = jevOf(['Beta', 'Gamma'], 'abc');
  assert.deepEqual([short.context_cut, short.context_clipped, short.collisions, short.top_tie], ['abc', false, [], false]);
});

test('the collision test compares the 32 bytes the scorer reads, not the text: multi-byte characters, a difference just inside or just outside', () => {
  const x = 'x'.repeat(31);
  assert.deepEqual(jevOf([x + 'é', x + 'è'], 'c').collisions, [[0, 1]], 'é and è share their first byte: the same 32 bytes');
  assert.deepEqual(jevOf([x + 'é', x + 'è'], 'c').options_cut, [x, x]);
  assert.deepEqual(jevOf([x + 'a', x + 'b'], 'c').collisions, [], '32 bytes, the last one differs');
  assert.deepEqual(jevOf([x + 'ab', x + 'ac'], 'c').collisions, [[0, 1]], 'the 33rd byte differs: outside the cut');
  const p = 'x'.repeat(10);
  assert.deepEqual(jevOf([p + 'q' + 'y'.repeat(40), p + 'r' + 'y'.repeat(40)], 'c').collisions, [], 'both clipped, but they differ inside the cut');
  const three = jevOf(['k'.repeat(40) + '1', 'k'.repeat(40) + '2', 'k'.repeat(40) + '3', 'other'], 'c');
  assert.deepEqual(three.collisions, [[0, 1], [0, 2]], 'each later one is paired with the first it agrees with');
  assert.equal(jevOf(['plain one', 'plain two', 'k'.repeat(40) + '1', 'k'.repeat(40) + '2'], 'c').top_tie, false, 'the best is not in the pair');
});

// ---- stop, exhausted, and the rest of the ends --------------------------------------------------------------------------------------------

function parked(answer) {
  const labels = ['Beta', 'Gamma'], prompt = promptOf('top 0.500, margin 0.000 under 0.050', labels, [0.5, 0.5]);
  const j = reading(base(), 'Alpha', BG, labels, probsOut({ Beta: 0.5, Gamma: 0.5 }, 'Beta'), 0, 1).mark('park', 'reading', 1, 2).park(prompt).end('Parked');
  if (answer !== undefined) j.answer(answer, prompt);
  return j;
}

test('a person says stop: the run ends stopped, with their reason; no pick; not a failure', () => {
  for (const [answer, reason] of [['stop: too risky', 'too risky'], ['stop', 'stopped at a park']]) {
    const asked = reduce(parked(answer).recs);
    assert.equal(asked.status, 'running', 'answered, the interpreter has yet to end it');
    assert.deepEqual(step(asked, 2).stopped, { reason });
    const v = reduce(parked(answer).mark('end', 'reading', 1, 2, { outcome: 'stopped' }).end('Failed').recs);
    assert.equal(v.status, 'stopped');
    assert.deepEqual([v.end.outcome, v.end.reason, v.end.door, v.end.cause], ['stopped', reason, 'Failed', null]);
    assert.equal(step(v, 2).pick, null);
    assert.deepEqual([step(v, 2).park.kind, step(v, 2).park.stop], ['stop', reason]);
    assert.deepEqual(v.marks, { current: [], visited: ['open', 'reading'], final: [], failed: [] });
    assert.equal(v.open, null);
  }
});

test('a budget runs out: exhausted, naming the budget that is at its cap', () => {
  const end = (j) => reduce(j.mark('end', 'reading', 4, 1, { outcome: 'exhausted' }).end('Failed').recs);
  const visits = end(base().mark('enter', 'reading', 4, 1));
  assert.deepEqual([visits.status, visits.end.exhausted, visits.end.outcome, visits.marks.failed], ['exhausted', 'visits', 'exhausted', ['reading']]);
  const steps = reduce(base().mark('enter', 'reading', 30, 1).mark('end', 'reading', 30, 1, { outcome: 'exhausted' }).end('Failed').recs);
  assert.deepEqual([steps.status, steps.end.exhausted, steps.budgets.decisions], ['exhausted', 'steps', { used: 30, cap: 30, left: 0 }]);
  const j = base().mark('enter', 'reading', 4, 1);
  for (let i = 0; i < 69; i++) j.call('browser_open', { url: 'u' }, 'opened u\n');
  const actions = end(j);
  assert.deepEqual([actions.status, actions.end.exhausted, actions.budgets.actions], ['exhausted', 'actions', { used: 70, cap: 70, left: 0 }]);
});

test('an end mark of outcome error is a failure; a Failed end with nothing open and no end mark is a failure too, with the last error as the reason', () => {
  const e = reduce(base().mark('enter', 'reading', 0, 1).call('browser_back', {}, 'page closed', true).mark('end', 'reading', 0, 2, { outcome: 'error' }).end('Failed').recs);
  assert.deepEqual([e.status, e.end.outcome, e.end.reason, e.end.cause], ['failed', 'error', 'page closed', 'failed']);
  const f = reduce(base().mark('enter', 'reading', 0, 1).call('jev_choose', { context: 'c', options: ['a', 'b'] }, 'jev_choose was refused: denied by policy: x', true).end('Failed').recs);
  assert.equal(f.status, 'failed');
  assert.equal(f.end.reason, 'jev_choose was refused: denied by policy: x');
  const done = reduce(base().end('Completed').recs);
  assert.equal(done.status, 'done', 'a completed run that left no jev mark: not claimed to have arrived');
});

test('a second WorkflowStart in the feed is a second run: the model starts again, the cursor goes on', () => {
  const j = reading(base(), 'Alpha', BG, null, null, 0, 1).mark('final', 'arrived', 0, 2).end('Completed');
  const first = reduce(j.recs);
  assert.equal(first.status, 'arrived');
  j.start({ start: 'Gamma', goal: 'Delta' }).pages(GRAPH).mark('enter', 'open', 0, 0);
  const v = reduce(j.recs);
  assert.deepEqual([v.meta.run, v.meta.start, v.meta.goal, v.status, v.steps.length, v.trail], [first.lastId + 1, 'Gamma', 'Delta', 'running', 1, ['open']]);
  assert.equal(v.lastId, j.recs.length - 1);
  assert.equal(v.graph.source, 'journal');
  assert.deepEqual(v.path, []);
  assert.equal(v.end, null);
});

// ---- the rest of what a step can be --------------------------------------------------------------------------------------------------

test('arrival by entailment: the score reaches the threshold though the heading is not the goal', () => {
  const j = base().mark('enter', 'reading', 0, 1).call('browser_snapshot', SNAP, tree('Alpha', BG)).call('jev_entail', entailIn('Alpha'), entail(0.93)).mark('final', 'arrived', 0, 2).end('Completed');
  const v = reduce(j.recs);
  const a = step(v, 2).arrival;
  assert.deepEqual([a.equals.hit, a.entail.hit, a.entail.score, a.entail.label, a.hit], [false, true, 0.93, 'entailment', true]);
  assert.equal(v.status, 'arrived');
  assert.equal(step(v, 2).menu, null, 'it arrived: nothing was chosen');
  const exact = reduce(base().mark('enter', 'reading', 0, 1).call('browser_snapshot', SNAP, tree('omega', BG)).call('jev_entail', entailIn('omega'), entail(0.1)).recs);
  assert.equal(step(exact, 2).arrival.equals.hit, false, 'the equals guard is exact: Omega is not omega');
});

test('a scorer that answers badly is the chooser being unavailable, or the entail guard not passing, with what it said', () => {
  const j = base().mark('enter', 'reading', 0, 1).call('browser_snapshot', SNAP, tree('Alpha', BG)).call('jev_entail', entailIn('Alpha'), 'oops')
    .call('jev_choose', { context: 'c', options: ['Beta', 'Gamma'] }, 'not json at all');
  const s = step(reduce(j.recs), 2);
  assert.equal(s.arrival.entail.error, "the scorer's answer was not JSON");
  assert.equal(s.menu.unavailable, "the chooser's answer was not JSON");
  assert.equal(s.menu.verdict, 'unavailable');
  const none = step(reduce(base().mark('enter', 'reading', 0, 1).call('browser_snapshot', SNAP, tree('Alpha', BG)).call('jev_entail', entailIn('Alpha'), '{"results":[]}').recs), 2);
  assert.equal(none.arrival.entail.error, 'the scorer answered no result');
  assert.equal(none.arrival.hit, false);
});

test('a probability the scorer did not give, or gave as text, counts as 0, as the interpreter counts it', () => {
  const j = reading(base(), 'Alpha', [['A', 'e3', '/a'], ['B', 'e4', '/b'], ['C', 'e5', '/c']], ['A', 'B', 'C'], '{"probs":{"A":0.8,"B":"high"},"best":"A"}', 0, 1);
  const m = step(reduce(j.recs), 2).menu;
  assert.deepEqual(m.options.map((o) => o.p), [0.8, 0, 0]);
  assert.deepEqual([m.best_index, m.top1, m.top2, m.sure], [0, 0.8, 0, true]);
  const first = reading(base(), 'Alpha', BG, ['Beta', 'Gamma'], probsOut({ Beta: 0.4, Gamma: 0.4 }, 'Gamma'), 0, 1);
  const f = step(reduce(first.recs), 2).menu;
  assert.deepEqual([f.best, f.best_index], ['Gamma', 0], 'the service said Gamma; the interpreter takes the first of equals, and so does the model');
});

test('the real browser answers a click with JSON; a ref the snapshot never had is flagged; an empty menu is a back', () => {
  const click = (ref, out) => step(reduce(reading(base(), 'Alpha', [['Beta', 'e3', '/wiki/Beta']], null, null, 0, 1).call('browser_click', { ref }, out).recs), 2);
  const real = click('e3', JSON.stringify({ confined: ['https://x.test'], ref: 'e3', title: 'Beta - Test', url: 'https://x.test/wiki/Beta' }));
  assert.deepEqual(real.action.result, { text: real.action.result.text, url: 'https://x.test/wiki/Beta', title: 'Beta - Test' });
  assert.deepEqual([real.menu.source, real.pick, real.action.refOk, real.action.label], ['single', { label: 'Beta', index: 0, how: 'single', ref: 'e3' }, true, 'Beta']);
  const lost = click('e99', 'clicked e99 -> https://x.test/wiki/Nowhere\n');
  assert.deepEqual([lost.action.refOk, lost.action.label, lost.pick.label, lost.pick.index], [false, null, null, null]);
  assert.equal(lost.action.result.url, 'https://x.test/wiki/Nowhere');
  const bad = click('e3', 'ref e3 is not from the last snapshot');
  assert.equal(bad.action.error, null, 'a result that says so is not an error unless the journal says it is');
  const failed = step(reduce(reading(base(), 'Alpha', [['Beta', 'e3', '/wiki/Beta']], null, null, 0, 1).call('browser_click', { ref: 'e3' }, 'click e3: no such element', true).recs), 2);
  assert.deepEqual([failed.action.error, failed.action.result], ['click e3: no such element', null]);
  assert.deepEqual(failed.errors.map((e) => e.tool), ['browser_click']);
  const empty = step(reduce(reading(base(), 'Alpha', [], null, null, 0, 1).call('browser_back', {}, 'went back\n').recs), 2);
  assert.deepEqual([empty.menu.source, empty.menu.verdict, empty.menu.options, empty.pick, empty.decision, empty.action.tool], ['empty', 'empty', [], null, 1, 'browser_back']);
  const pending = step(reduce(reading(base(), 'Alpha', BG, null, null, 0, 1).call('browser_click', { ref: 'e3' }).recs), 2);
  assert.deepEqual([pending.action.pending, pending.action.result, pending.open], [true, null, true]);
});

test('a park on an empty menu has nothing to number: only stop is an answer', () => {
  const prompt = 'jev wiki-hop: the menu was empty and nothing handled EMPTY\nstate: reading (step 1)\nNothing here can be picked: answer `stop` (or `stop: <reason>`) to end the run.';
  const v = reduce(reading(base(), 'Alpha', [], null, null, 0, 1).mark('park', 'reading', 1, 2).park(prompt).end('Parked').answer('0', prompt).recs);
  const p = step(v, 2).park;
  assert.deepEqual([p.cause, p.why, p.ask, p.options, p.kind, p.invalid], ['empty', 'the menu was empty and nothing handled EMPTY', null, [], 'invalid', true]);
  assert.equal(step(v, 2).menu.source, 'empty');
});

// ---- model text is data -------------------------------------------------------------------------------------------------------------------

test('model text stays data: markup, __proto__, constructor and a newline in a label, a script for an answer, a page that is a heading of markup', () => {
  const evil = '<img src=x onerror=alert(1)>', page = '<svg onload=alert(2)>';
  const links = [[evil, 'e3', '/wiki/E'], ['__proto__', 'e4', '/wiki/P'], ['constructor', 'e5', '/wiki/C'], ['toString', 'e6', '/wiki/T']];
  const labels = [evil, '__proto__', 'constructor', 'toString', 'line\nbreak'];
  const probs = '{"probs":{"<img src=x onerror=alert(1)>":0.6,"__proto__":0.2,"constructor":0.1,"toString":0.05,"line\\nbreak":0.05},"best":"<img src=x onerror=alert(1)>"}';
  const prompt = promptOf('top 0.600, margin 0.400', labels, [0.6, 0.2, 0.1, 0.05, 0.05]).replace('Omega', '<b>Omega</b>');
  const j = reading(base('{"start":"A","goal":"<b>Z</b>","__proto__":{"polluted":1},"n":3}'), page, links, labels, probs, 0, 1)
    .mark('park', 'reading', 1, 2).park(prompt).end('Parked').answer('<script>alert(3)</script>', prompt).call('browser_click', { ref: 'e3' }, 'clicked e3 -> <u>\n');
  const v = reduce(j.recs);
  const s = step(v, 2);
  assert.deepEqual(s.menu.options, [{ label: evil, p: 0.6 }, { label: '__proto__', p: 0.2 }, { label: 'constructor', p: 0.1 }, { label: 'toString', p: 0.05 }, { label: 'line\nbreak', p: 0.05 }]);
  assert.deepEqual([s.menu.best, s.menu.best_index, s.menu.sure], [evil, 0, true]);
  assert.deepEqual(s.pick, { label: evil, index: 0, how: 'chooser', ref: 'e3' });
  assert.deepEqual([s.park.answer, s.park.kind, s.park.invalid], ['<script>alert(3)</script>', 'invalid', true]);
  assert.equal(s.park.prompt, prompt);
  assert.equal(s.page.heading, page);
  assert.deepEqual(v.path, [page]);
  assert.equal(s.action.result.url, '<u>');
  assert.equal(v.meta.goal, '<b>Z</b>');
  assert.deepEqual(Object.keys(v.meta.args).sort(), ['goal', 'n', 'start'], 'an object value (a __proto__ key) is not carried');
  for (const o of [v.meta.args, s.menu, s.menu.options[1], s, v]) assert.equal(Object.getPrototypeOf(o), Object.prototype);
  assert.equal({}.polluted, undefined);
  assert.equal(Object.prototype.polluted, undefined);
  assert.deepEqual(JSON.parse(JSON.stringify(s)), s, 'plain data all the way down');
});

// @@MORE@@
