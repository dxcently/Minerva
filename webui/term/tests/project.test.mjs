// The graph drawing (graph/project.js, graph/draw.js): a jev graph's JSON -> the drawing jev_graph gives, and that drawing -> boxes and
// lines under dagre -> an SVG tree. Pure: no DOM, the tree is read as vnodes. The six graphs are the vendored eidolon-plugins/jev/graphs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { project } from '../graph/project.js';
import { layout, drawGraph, CW } from '../graph/draw.js';

const dir = new URL('../../../eidolon-plugins/jev/graphs/', import.meta.url);
const ids = readdirSync(dir).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5)).sort();
const text = (id) => readFileSync(new URL(id + '.json', dir), 'utf8');
const drawn = (id) => project(text(id));
const sig = (e) => [e.kind, e.from + '>' + e.to, e.event, e.guard, e.reenter ? 'reenter' : ''].filter(Boolean).join(' ');
const lay = (g) => layout(g.states, g.edges, { initial: g.initial, rootOn: g.root_on });

test('there are six graphs', () => {
  assert.deepEqual(ids, ['ctf-juice-recon', 'find-related', 'triage-botforge', 'triage-linux', 'triage-wsl', 'wiki-hop']);
});

// ---- project.js ----------------------------------------------------------------------------------------------------------------------

test('wiki-hop: exactly 4 states, open and reading leaves, arrived and failed final', () => {
  const g = drawn('wiki-hop');
  assert.deepEqual(g.states.map((s) => [s.path, s.kind]).sort(), [['arrived', 'final'], ['failed', 'final'], ['open', 'leaf'], ['reading', 'leaf']]);
  assert.equal(g.initial, 'open');
  assert.equal(g.graph, 'wiki-hop');
});

test('wiki-hop: open->reading always; reading->arrived twice, by guard; reading->reading on PICK and EMPTY; *->failed on ERROR', () => {
  const g = drawn('wiki-hop');
  assert.deepEqual(g.edges.map(sig), [
    'always open>reading',
    'always reading>arrived equals',
    'always reading>arrived entails',
    'on reading>reading EMPTY reenter',
    'on reading>reading PICK reenter',
  ]);
  assert.deepEqual(g.root_on.map(sig), ['on *>failed ERROR']);
  assert.equal(g.root_on[0].from, '*');
});

// What the real jev_graph tool answered for each graph (tools/graph.rn through `eidolon workflow run`, mock provider, 2026-10-07), read as one
// line per state and per edge. If a graph changes, this is the line to change on purpose; if project.js drifts, this is the one that notices.
const LINUX = {
  initial: 'investigate',
  states: 'aborted:final investigate:compound investigate.done:final investigate.hist:history investigate.identify:leaf investigate.judge:leaf investigate.network:leaf investigate.processes:leaf recover:leaf report:final',
  edges: [
    'initial investigate>investigate.identify', 'onDone investigate>report', 'history investigate.hist>investigate.identify',
    'always investigate.identify>investigate.network entails', 'always investigate.identify>investigate.network count', 'on investigate.identify>null PICK',
    'on investigate.judge>investigate.done CLEAN', 'on investigate.judge>investigate.done PICK',
    'always investigate.network>investigate.processes entails', 'always investigate.network>investigate.processes count', 'on investigate.network>null PICK',
    'always investigate.processes>investigate.judge', 'on recover>aborted ABORT', 'on recover>investigate.hist CONTINUE',
  ],
  root: ['on *>recover ERROR'],
};
const REAL = {
  'ctf-juice-recon': {
    initial: 'headers',
    states: 'failed:final headers:leaf report:final robots:leaf',
    edges: ['always headers>robots', 'always robots>report'],
    root: ['on *>failed ERROR'],
  },
  'find-related': {
    initial: 'docs',
    states: 'code:leaf design:leaf docs:leaf elsewhere:leaf excerpt:leaf graphs:leaf inspect:leaf nothing:final report:final scripts:leaf',
    edges: [
      'always code>scripts', 'always design>elsewhere', 'always docs>code', 'always elsewhere>inspect', 'always excerpt>report', 'always graphs>design',
      'always inspect>excerpt', 'always scripts>graphs',
    ],
    root: ['on *>nothing ERROR'],
  },
  'triage-botforge': {
    initial: 'investigate',
    states: LINUX.states,
    edges: [
      'initial investigate>investigate.identify', 'onDone investigate>report', 'history investigate.hist>investigate.identify',
      'always investigate.identify>investigate.network count', 'on investigate.identify>null PICK',
      'on investigate.judge>investigate.done CLEAN', 'on investigate.judge>investigate.done PICK',
      'always investigate.network>investigate.processes count', 'on investigate.network>null PICK',
      'always investigate.processes>investigate.judge', 'on recover>aborted ABORT', 'on recover>investigate.hist CONTINUE',
    ],
    root: LINUX.root,
  },
  'triage-linux': LINUX,
  'triage-wsl': LINUX,
  'wiki-hop': {
    initial: 'open',
    states: 'arrived:final failed:final open:leaf reading:leaf',
    edges: ['always open>reading', 'always reading>arrived equals', 'always reading>arrived entails', 'on reading>reading EMPTY reenter', 'on reading>reading PICK reenter'],
    root: ['on *>failed ERROR'],
  },
};

for (const id of Object.keys(REAL)) {
  test(`${id}: the states, edges and root on are what jev_graph answers`, () => {
    const g = drawn(id), want = REAL[id];
    assert.equal(g.graph, id);
    assert.equal(g.initial, want.initial);
    assert.equal(g.states.map((s) => s.path + ':' + s.kind).join(' '), want.states, 'states, in sorted order, a parent before its children');
    assert.deepEqual(g.edges.map(sig), want.edges);
    assert.deepEqual(g.root_on.map(sig), want.root);
  });

  test(`${id}: every edge joins states that exist, and a compound state starts somewhere`, () => {
    const g = drawn(id);
    const paths = new Set(g.states.map((s) => s.path));
    assert.equal(paths.size, g.states.length, 'a path is listed once');
    for (const e of [...g.edges, ...g.root_on]) {
      assert.ok(e.from === '*' || paths.has(e.from), `${sig(e)}: from`);
      assert.ok(e.to === null || paths.has(e.to), `${sig(e)}: to`);
    }
    for (const s of g.states.filter((x) => x.kind === 'compound')) {
      assert.ok(paths.has(s.path + '.' + s.initial), `${s.path} starts in ${s.initial}`);
      assert.ok(g.edges.some((e) => e.kind === 'initial' && e.from === s.path && e.to === s.path + '.' + s.initial));
    }
  });
}

test('a graph given as text is the same as the parsed graph; text that is not JSON throws', () => {
  assert.deepEqual(project(text('wiki-hop')), project(JSON.parse(text('wiki-hop'))));
  assert.throws(() => project('{ not json'), SyntaxError);
});

test('what a state carries: description, tools sorted, the chooser (menu labels, floor, ask)', () => {
  const g = drawn('triage-linux');
  const by = Object.fromEntries(g.states.map((s) => [s.path, s]));
  assert.equal(by['investigate.identify'].description, 'What is this box?');
  assert.deepEqual(by['investigate.identify'].choose, {
    source: 'menu', options: ['os release', 'kernel and arch', 'hostname and uptime', 'who is logged in'], ask: 'Which command answers what this box is?',
  });
  assert.deepEqual(by['investigate.judge'].choose, { source: 'lines', floor: 0.6, ask: 'Which process looks out of place, if any?' });
  assert.deepEqual(by['recover'].choose, { source: 'transitions', floor: 0.7, ask: 'A command failed. Continue the investigation or stop?' });
  assert.deepEqual(by['investigate.processes'].tools, ['jev_processes'], 'an entry action that calls a tool');
  assert.deepEqual(by['investigate.identify'].tools, ['{{event.option.tool}}'], 'an `on` action; an `always` has no actions to count');
  const w = drawn('wiki-hop');
  assert.deepEqual(w.states.find((s) => s.path === 'reading').tools, ['browser_back', 'browser_click', 'browser_snapshot'], 'entry and on, sorted');
  assert.deepEqual(w.states.find((s) => s.path === 'reading').choose, { source: 'refs', floor: 0.35, ask: 'Which link leads toward the article "{{context.goal}}"?' });
  assert.deepEqual(w.budget, { steps: 30, actions: 70, escalations: 30 });
  assert.deepEqual(w.input.required, ['start', 'goal']);
});

test('transitions: a string, an object or a list; # is absolute, a bare name is a sibling; events in sorted order', () => {
  const g = project({
    id: 'g',
    initial: 'p',
    on: { STOP: { target: 'x', guard: { type: 'equals' } }, ERROR: 'x', BOTH: ['x', { target: '#g.p.q' }] },
    states: {
      x: { type: 'final' },
      p: {
        initial: 'q',
        onDone: 'x',
        states: {
          r: { type: 'final' },
          q: {
            always: [{ guard: { type: 'count' }, target: 'r' }, 'r'],
            on: { ZED: { target: '#g.x' }, ALPHA: { target: 'r', reenter: true }, MID: 'q', STAY: { actions: [] } },
          },
        },
      },
    },
  });
  assert.deepEqual(g.states.map((s) => s.path + ':' + s.kind), ['p:compound', 'p.q:leaf', 'p.r:final', 'x:final']);
  assert.deepEqual(g.edges.map(sig), [
    'initial p>p.q',
    'onDone p>x',
    'always p.q>p.r count',
    'always p.q>p.r',
    'on p.q>p.r ALPHA reenter',
    'on p.q>p.q MID',
    'on p.q>null STAY',
    'on p.q>x ZED',
  ]);
  assert.deepEqual(g.root_on.map(sig), ['on *>x BOTH', 'on *>p.q BOTH', 'on *>x ERROR', 'on *>x STOP equals']);
});

test('tools: those of entry, exit and `on` actions only (an `always` action is not counted, as in jev_graph), by name, not deduplicated', () => {
  const call = (name) => ({ type: 'tool', params: { name } });
  const g = project({
    id: 't',
    states: {
      a: {
        entry: [call('z_entry'), { type: 'assign', params: { name: 'not_a_tool' } }, call('b_dup')],
        exit: [call('b_dup')],
        always: [{ target: 'a', actions: [call('x_always')] }],
        on: { GO: { target: 'a', actions: [call('m_on')] }, ALSO: [{ actions: [call('a_on')] }] },
      },
    },
  });
  assert.deepEqual(g.states[0].tools, ['a_on', 'b_dup', 'b_dup', 'm_on', 'z_entry']);
});

test('a history state points where it resumes; an empty or odd graph does not throw', () => {
  const g = project({ id: 'h', states: { a: { initial: 'b', states: { b: {}, h: { type: 'history', target: 'b' } } } } });
  assert.deepEqual(g.edges.map(sig), ['initial a>a.b', 'history a.h>a.b']);
  const e = project({});
  assert.deepEqual([e.graph, e.initial, e.states, e.edges, e.root_on, e.tools], ['', '', [], [], [], []]);
  assert.doesNotThrow(() => project({ states: { a: 'x', b: { always: [1, null, { target: 5 }], on: { E: 3 } } }, on: 'x' }));
  assert.doesNotThrow(() => project(null));
});

// ---- draw.js: layout -----------------------------------------------------------------------------------------------------------------

const overlap = (a, b) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
const inside = (a, f) => a.x >= f.x && a.y >= f.y && a.x + a.w <= f.x + f.w && a.y + a.h <= f.y + f.h;

test('layout of wiki-hop: 4 states and the "*" box at distinct positions, no overlap, every line routed', () => {
  const g = drawn('wiki-hop'), l = lay(g);
  assert.ok(!l.fallback, l.error);
  assert.equal(l.skipped, 0);
  assert.deepEqual(l.nodes.map((n) => n.id).sort(), ['*', 'arrived', 'failed', 'open', 'reading']);
  assert.equal(new Set(l.nodes.map((n) => n.x + ',' + n.y)).size, 5, 'distinct positions');
  for (const a of l.nodes) {
    assert.ok([a.x, a.y, a.w, a.h].every(Number.isFinite) && a.w > 0 && a.h > 0, a.id);
    assert.ok(a.x >= 0 && a.y >= 0 && a.x + a.w <= l.width && a.y + a.h <= l.height, `${a.id} is inside the drawing`);
    for (const b of l.nodes) if (a !== b) assert.ok(!overlap(a, b), `${a.id} overlaps ${b.id}`);
  }
  // 5 edges: 3 lines (open>reading, reading>arrived once for both guards, *>failed) and 2 loops written inside reading
  assert.deepEqual(l.edges.map((e) => e.from + '>' + e.to + ' ' + e.label).sort(), ['*>failed ERROR', 'open>reading ', 'reading>arrived equals · entails']);
  for (const e of l.edges) {
    assert.ok(e.points.length >= 2 && e.points.every((p) => Number.isFinite(p.x) && Number.isFinite(p.y)), `${e.from}>${e.to} has points`);
    const a = l.nodes.find((n) => n.id === e.from), b = l.nodes.find((n) => n.id === e.to);
    assert.ok(Math.abs(e.points[0].y - (a.y + a.h)) <= 1 && e.points[0].x >= a.x && e.points[0].x <= a.x + a.w, `${e.from}>${e.to} leaves the bottom of its box`);
    const z = e.points.at(-1);
    assert.ok(Math.abs(z.y - b.y) <= 1 && z.x >= b.x && z.x <= b.x + b.w, `${e.from}>${e.to} ends on the top of its box`);
    assert.ok(e.points.every((p, i) => i === 0 || p.x === e.points[i - 1].x || p.y === e.points[i - 1].y), 'right angles only');
  }
  const reading = l.nodes.find((n) => n.id === 'reading');
  assert.deepEqual(reading.rows, ['↻ EMPTY', '↻ PICK']);
  assert.ok(reading.h > l.nodes.find((n) => n.id === 'open').h, 'a box with loops is taller');
  assert.ok(l.nodes.find((n) => n.id === 'open').label.startsWith('▸ '), 'the state the graph starts in');
  assert.ok(!reading.label.startsWith('▸'));
  const rank = (id) => l.nodes.find((n) => n.id === id).y;
  assert.ok(rank('open') < rank('reading') && rank('reading') < rank('arrived'), 'top to bottom');
});

test('layout of the triage graphs: a frame around investigate, the way out of it cut at its edge, parallel guards on one line', () => {
  for (const id of ['triage-linux', 'triage-wsl', 'triage-botforge']) {
    const l = lay(drawn(id));
    assert.ok(!l.fallback, `${id}: ${l.error}`);
    const frame = l.nodes.find((n) => n.id === 'investigate');
    assert.ok(frame.cluster);
    const kids = l.nodes.filter((n) => n.parent === 'investigate');
    assert.deepEqual(kids.map((n) => n.path).sort(), ['investigate.done', 'investigate.hist', 'investigate.identify', 'investigate.judge', 'investigate.network', 'investigate.processes']);
    for (const k of kids) assert.ok(inside(k, frame), `${k.id} is inside the frame`);
    for (const n of l.nodes.filter((x) => !x.cluster)) {
      for (const m of l.nodes.filter((x) => !x.cluster && x !== n)) assert.ok(!overlap(n, m), `${id}: ${n.id} overlaps ${m.id}`);
      if (!n.parent) assert.ok(!overlap(n, frame), `${id}: ${n.id} sits in the frame`);
    }
    const done = l.edges.find((e) => e.kind === 'onDone');
    assert.deepEqual([done.from, done.to, done.label], ['investigate', 'report', 'done']);
    const p = done.points[0];
    assert.ok(p.x >= frame.x && p.x <= frame.x + frame.w && Math.abs(p.y - (frame.y + frame.h)) <= 1, `${id}: onDone leaves at the bottom of the frame, not from the box inside`);
    const hist = l.edges.find((e) => e.kind === 'history');
    assert.deepEqual([hist.from, hist.to], ['investigate.hist', 'investigate.identify']);
    for (const e of l.edges) assert.ok(e.points.every((p, i) => i === 0 || p.x === e.points[i - 1].x || p.y === e.points[i - 1].y), `${id}: ${e.from}>${e.to} turns at right angles`);
    assert.ok(l.edges.some((e) => e.points.some((p) => p.x !== e.points[0].x)), 'at least one line does turn: the check above is not vacuous');
    const idNet = l.edges.find((e) => e.from === 'investigate.identify' && e.to === 'investigate.network');
    assert.equal(idNet.label, id === 'triage-botforge' ? 'count' : 'entails · count');
    assert.equal(idNet.items.length, id === 'triage-botforge' ? 1 : 2);
    assert.ok(l.nodes.find((n) => n.id === 'investigate.identify').label.startsWith('▸ '), 'where investigate starts');
    assert.ok(frame.title.startsWith('▸ '), 'where the graph starts');
    // the loop rows: PICK in identify and network
    assert.deepEqual(l.nodes.find((n) => n.id === 'investigate.network').rows, ['↻ PICK']);
  }
});

test('layout of every graph: nothing dropped, no fallback, one line per pair of states, every loop written in a box, narrow enough for 34 cells', () => {
  for (const id of ids) {
    const g = drawn(id), l = lay(g);
    assert.ok(!l.fallback, `${id}: ${l.error}`);
    assert.equal(l.skipped, 0, id);
    const all = [...g.edges, ...g.root_on].filter((e) => e.kind !== 'initial');
    const looped = (e) => e.to === null || e.to === e.from;
    const pairs = new Set(all.filter((e) => !looped(e)).map((e) => e.from + '>' + e.to));
    assert.equal(l.edges.length, pairs.size, `${id}: lines`);
    assert.equal(l.nodes.reduce((n, b) => n + b.rows.length, 0), all.filter(looped).length, `${id}: loop rows`);
    for (const e of l.edges) assert.ok(e.points.length >= 2, `${id}: ${e.from}>${e.to}`);
    assert.equal(new Set(l.nodes.map((n) => n.x + ',' + n.y)).size, l.nodes.length, `${id}: distinct positions`);
    assert.ok(l.width <= 34 * CW * 1.25, `${id} is ${l.width} wide: more than a quarter over the narrow inspector (34 cells)`);
  }
});

test('layout takes any states and edges (the turn graph is not a jev graph): own labels, a frame by kind, unknown ends skipped, an empty list', () => {
  const states = [
    { path: 't1', kind: 'compound', label: 'turn 1' },
    { path: 't1.c1', kind: 'leaf', label: 'read' },
    { path: 't1.c2', kind: 'leaf', label: 'edit' },
    { path: 'c.d', kind: 'leaf', label: 'not nested: its prefix is not a state' },
  ];
  const edges = [
    { from: 't1.c1', to: 't1.c2', kind: 'on', event: 'then' },
    { from: 't1.c2', to: 'nowhere', kind: 'on', event: 'x' },
    { from: 'nowhere', to: 'c.d', kind: 'always' },
    { from: 't1.c2', to: 'c.d', kind: 'on', label: 'asks' },
  ];
  const l = layout(states, edges);
  assert.equal(l.skipped, 2);
  assert.equal(l.nodes.find((n) => n.id === 't1').cluster, true);
  assert.equal(l.nodes.find((n) => n.id === 't1.c1').parent, 't1');
  assert.equal(l.nodes.find((n) => n.id === 'c.d').parent, null);
  assert.deepEqual(l.nodes.find((n) => n.id === 't1.c1').label, 'read');
  assert.deepEqual(l.edges.map((e) => e.label).sort(), ['asks', 'then']);
  assert.deepEqual(layout([], []), { nodes: [], edges: [], width: 0, height: 0, skipped: 0 });
  assert.doesNotThrow(() => layout(undefined, null));
});

test('a line dagre routes past the left of every box (s3 > s1 here, at x = -5) is slid back inside the drawing', () => {
  const states = [{ path: 's0', kind: 'leaf' }, { path: 's1', kind: 'final' }, { path: 's2', kind: 'leaf' }, { path: 's3', kind: 'leaf' }];
  const edges = [
    { from: 's0', to: 's1', kind: 'always' },
    { from: 's2', to: 's2', kind: 'onDone', guard: 'equals' },
    { from: 's3', to: 's0', kind: 'always', guard: 'count' },
    { from: 's3', to: 's1', kind: 'always' },
    { from: 's0', to: 's0', kind: 'always', guard: 'count' },
    { from: 's2', to: 's1', kind: 'on', event: 'PICK' },
  ];
  const l = layout(states, edges, { initial: 's1' });
  assert.ok(!l.fallback);
  for (const e of l.edges) for (const p of e.points) assert.ok(p.x >= 0 && p.y >= 0 && p.x <= l.width && p.y <= l.height, `${e.from}>${e.to} at ${p.x},${p.y} in ${l.width}x${l.height}`);
  for (const n of l.nodes) assert.ok(n.x >= 0 && n.y >= 0 && n.x + n.w <= l.width && n.y + n.h <= l.height, n.id);
  const long = l.edges.find((e) => e.from === 's3' && e.to === 's1');
  const s1 = l.nodes.find((n) => n.id === 's1');
  assert.ok(Math.abs(long.points.at(-1).y - s1.y) <= 1, 'it still ends on the top of s1 after the slide');
});

test('no point that dagre could not place reaches the drawing: a cycle through a frame (dagre 3.1.1 answers NaN for some of its points) falls back to the column', () => {
  const states = [
    { path: 's0', kind: 'compound', initial: 'a' }, { path: 's0.a', kind: 'final' }, { path: 's0.b', kind: 'compound', initial: 'a' }, { path: 's1', kind: 'final' },
  ];
  const edges = [
    { from: 's1', to: 's1', kind: 'onDone' }, { from: 's0.a', to: null, kind: 'onDone' }, { from: 's0.b', to: 's0.b', kind: 'on', event: 'PICK' },
    { from: 's0.a', to: 's1', kind: 'on', event: 'PICK', guard: 'count' }, { from: 's1', to: 's0.b', kind: 'always' },
    { from: 's0', to: 's0.b', kind: 'always', guard: 'count' }, { from: 's0.b', to: 's0', kind: 'onDone', guard: 'equals' },
    { from: 's1', to: 's0.a', kind: 'on', event: 'ERROR' }, { from: 's0', to: 's1', kind: 'always' },
  ];
  const l = layout(states, edges, { initial: 's1', rootOn: [{ from: '*', to: 's0.b', kind: 'on', event: 'ERROR' }] });
  assert.ok(l.edges.length > 0);
  for (const e of l.edges) assert.ok(e.points.every((p) => Number.isFinite(p.x) && Number.isFinite(p.y)), `${e.from}>${e.to}: ${JSON.stringify(e.points)}`);
  for (const n of l.nodes) assert.ok([n.x, n.y, n.w, n.h].every(Number.isFinite), n.id);
  assert.ok(Number.isFinite(l.width) && Number.isFinite(l.height));
});

test('the same arrays are laid out once; a different list is laid out again', () => {
  const g = drawn('wiki-hop');
  const a = layout(g.states, g.edges, { initial: g.initial, rootOn: g.root_on });
  assert.equal(layout(g.states, g.edges, { initial: g.initial, rootOn: g.root_on }), a);
  assert.notEqual(layout(g.states, g.edges.slice(), { initial: g.initial, rootOn: g.root_on }), a);
  assert.notEqual(layout(g.states, g.edges, { initial: g.initial }), a, 'without the root on there is no "*" box');
});

test('the column layout (what a graph gets if dagre throws) still has a box for every state and a line for every pair', () => {
  const g = drawn('triage-linux');
  const l = layout(g.states, g.edges, { initial: g.initial, rootOn: g.root_on, engine: 'stack' });
  assert.equal(l.fallback, true);
  assert.equal(l.nodes.length, 11);
  assert.ok(l.nodes.every((n) => !n.cluster), 'frames become boxes');
  assert.equal(l.edges.length, lay(g).edges.length);
  for (const e of l.edges) assert.ok(e.points.length >= 2);
  const svg = drawGraph(g, {}, { engine: 'stack' });
  assert.equal(boxes(svg).length, 11);
});

// ---- draw.js: the tree ---------------------------------------------------------------------------------------------------------------

// every vnode under v
function* nodes(v) {
  if (Array.isArray(v)) { for (const x of v) yield* nodes(x); return; }
  if (!v || typeof v !== 'object' || v.type === undefined) return;
  yield v;
  yield* nodes(v.props && v.props.children);
}
const klass = (v) => String((v.props && v.props.class) || '').split(/\s+/);
const boxes = (svg) => [...nodes(svg)].filter((v) => v.type === 'g' && klass(v).includes('gn'));
const frames = (svg) => [...nodes(svg)].filter((v) => v.type === 'g' && klass(v).includes('gc'));
const lines = (svg) => [...nodes(svg)].filter((v) => v.type === 'g' && klass(v).includes('ge'));
const strings = (v) => [...nodes(v)].flatMap((n) => [n.props.children].flat(Infinity)).filter((c) => typeof c === 'string');
const byId = (list, id) => list.find((v) => v.props['data-id'] === id);

test('drawGraph: an svg scaled by its viewBox, with a box per state and a frame per compound; no marks, nothing lit', () => {
  const g = drawn('triage-linux'), l = lay(g);
  const svg = drawGraph(g);
  assert.equal(svg.type, 'svg');
  assert.equal(svg.props.viewBox, `0 0 ${l.width} ${l.height}`);
  assert.equal(svg.props.width, l.width, 'natural width: the page only ever scales it down (max-width: 100%)');
  assert.equal(svg.props.height, l.height);
  assert.equal(svg.props.role, 'img');
  assert.equal(boxes(svg).length, 10, '9 states and the "*" box');
  assert.equal(frames(svg).length, 1);
  assert.equal(lines(svg).length, l.edges.length);
  assert.ok(!klass(svg).includes('live'));
  for (const v of [...boxes(svg), ...frames(svg), ...lines(svg)]) {
    for (const c of ['current', 'visited', 'final', 'failed', 'lit', 'has-current', 'has-visited']) assert.ok(!klass(v).includes(c), `${c} with no marks`);
  }
  assert.ok(klass(byId(boxes(svg), 'investigate.done')).includes('k-final'));
  assert.ok(klass(byId(boxes(svg), 'investigate.hist')).includes('k-history'));
  assert.ok(klass(byId(boxes(svg), '*')).includes('any'));
  assert.ok(klass(byId(boxes(svg), 'investigate.identify')).includes('initial'));
  for (const v of nodes(svg)) for (const k of Object.keys(v.props || {})) assert.doesNotMatch(k, /innerhtml|dangerously|^on/i, `${v.type} has ${k}`);
});

test('drawGraph: the marks of a run: current, visited, final, failed, and the lines between visited states', () => {
  const g = drawn('wiki-hop');
  const svg = drawGraph(g, { current: 'reading', visited: new Set(['open', 'reading']), final: ['arrived'], failed: ['failed'] });
  assert.ok(klass(svg).includes('live'));
  const b = boxes(svg);
  assert.ok(klass(byId(b, 'reading')).includes('current') && klass(byId(b, 'reading')).includes('visited'));
  assert.ok(klass(byId(b, 'open')).includes('visited') && !klass(byId(b, 'open')).includes('current'));
  assert.ok(klass(byId(b, 'arrived')).includes('final') && !klass(byId(b, 'arrived')).includes('visited'));
  assert.ok(klass(byId(b, 'failed')).includes('failed'));
  assert.equal(b.filter((v) => klass(v).includes('current')).length, 1, 'one state is current');
  assert.equal(lines(svg).filter((v) => klass(v).includes('lit')).length, 2, 'open>reading and reading>arrived (both ends reached: reading and the final one)');
  const only = drawGraph(g, { current: 'open' });
  assert.equal(lines(only).filter((v) => klass(v).includes('lit')).length, 0, 'a line needs both ends reached');
});

test('drawGraph: a frame is current and reached when anything inside it is; the way out is lit only once the state it leads to is reached', () => {
  const g = drawn('triage-linux');
  const run = drawGraph(g, { current: 'investigate.network', visited: ['investigate.identify', 'investigate.network'] });
  const f = byId(frames(run), 'investigate');
  assert.ok(klass(f).includes('has-current') && klass(f).includes('has-visited'));
  const out = (svg) => lines(svg).find((v) => klass(v).includes('e-onDone'));
  assert.ok(!klass(out(run)).includes('lit'));
  const done = drawGraph(g, { visited: ['investigate.done', 'report'], final: 'report' });
  assert.ok(klass(out(done)).includes('lit'), 'inside is reached, report is reached');
  assert.ok(klass(byId(boxes(done), 'report')).includes('final'));
});

test('drawGraph: names are text (no markup is built from them), an empty graph says so, garbage in does not throw', () => {
  const svg = drawGraph({ states: [{ path: 'x', kind: 'leaf', label: '<img src=x onerror=alert(1)>' }], edges: [] });
  assert.ok(strings(svg).some((s) => s.startsWith('<img src=x onerror=')), 'the label is a string child, cut to fit');
  assert.ok(![...nodes(svg)].some((v) => v.type === 'img'), 'and no element is made of it');
  assert.equal(drawGraph({ states: [], edges: [] }).props.class, 'gempty');
  assert.equal(drawGraph(null).props.class, 'gempty');
  assert.doesNotThrow(() => drawGraph({ states: [null, 3, { path: 7 }, { path: 'a' }], edges: [null, { from: 'a' }, { from: 'a', to: 'a' }] }, null));
});

test('drawGraph: a line to a state with an unknown name is left out; a label is cut at 22 characters, a title keeps the whole name', () => {
  const long = 'a-state-with-a-very-long-name-indeed';
  const svg = drawGraph({ states: [{ path: long, kind: 'leaf' }, { path: 'b', kind: 'leaf' }], edges: [{ from: long, to: 'b', kind: 'on', event: 'GO' }, { from: long, to: 'zz', kind: 'on', event: 'GO' }] });
  assert.equal(lines(svg).length, 1);
  const box = byId(boxes(svg), long);
  const shown = strings(box).filter((s) => s.startsWith('a-state') && s.endsWith('…'));
  assert.equal(shown.length, 1);
  assert.equal(shown[0].length, 22);
  assert.ok(strings(box).some((s) => s.startsWith(long)), 'the tooltip has the whole path');
});
