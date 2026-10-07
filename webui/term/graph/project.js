// A jev graph's JSON (eidolon-plugins/jev/graphs/<id>.json, or the text a run joined from its jev_graph pages) -> the drawing the jev_graph
// tool would have answered: { graph, description, initial, input, budget, tools, root_on, states, edges, marks }. Pure; no DOM.
//
// It mirrors eidolon-plugins/jev/tools/graph.rn (the vendored copy, jev/README.md "Drawing a run"), function for function:
//   getOr / isNull / sortedKeys / dotted   graph.rn:90-131     missing keys read as a default, objects walk in sorted key order
//   transitionsOf                          graph.rn:132-152    a string, an object or a list of either
//   targetPath                             graph.rn:153-179    `#id.a.b` is absolute (the id is dropped), a bare name is a sibling
//   actionTools                            graph.rn:180-193    the `name` of every `tool` action
//   edgeOf                                 graph.rn:194-212    { from, to (null: no target), kind, event?, guard?, reenter? }
//   chooseOf                               graph.rn:213-246    { source, options? (menu labels), floor?, ask? }
//   walk                                   graph.rn:247-315    states in sorted order, a parent before its children; per state the
//                                                              edges in the order initial, history, always, on (sorted events), onDone
//   draw                                   graph.rn:316-       the top level, and the root `on` as edges from "*"
// Where Rune would stop with an error on a malformed graph (a transition that is not an object, a target that is not text) this reads
// it as no target or skips it, so a half-wrong graph still draws.
//
// edge kinds: initial (a compound state to the child it starts in) | always | on (event) | onDone | history (a history state to where it
// resumes). `to` is null for a transition with no target (its actions run and the state stays). Root `on` is in `root_on`, from "*".

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const isStr = (v) => typeof v === 'string';
const isNull = (v) => v === null || v === undefined; // graph.rn:90
const getOr = (o, key, dflt) => (isObj(o) && Object.hasOwn(o, key) ? o[key] : dflt); // graph.rn:97
const sortedKeys = (o) => (isObj(o) ? Object.keys(o).sort() : []); // graph.rn:108; sort() is by UTF-16 unit, Rune's by byte: the same for ids
const dotted = (path) => path.join('.'); // graph.rn:119

// graph.rn:132: null -> none; "S" -> { target: "S" }; an object -> itself; a list of strings and objects
function transitionsOf(v) {
  if (isNull(v)) return [];
  if (isStr(v)) return [{ target: v }];
  if (isObj(v)) return [v];
  if (Array.isArray(v)) return v.map((t) => (isStr(t) ? { target: t } : t)).filter(isObj);
  return [];
}

// graph.rn:153: `#id.a.b` is absolute (parts[0], the graph id, is dropped); a bare name is a sibling of the declaring state
function targetPath(owner, target) {
  if (target.startsWith('#')) {
    const parts = target.slice(1).split('.').filter((p) => p !== '');
    return dotted(parts.slice(1));
  }
  return dotted([...owner.slice(0, -1), target]);
}

// graph.rn:180
function actionTools(actions, found) {
  if (!Array.isArray(actions)) return;
  for (const a of actions) {
    if (isObj(a) && getOr(a, 'type', '') === 'tool') {
      const name = getOr(getOr(a, 'params', {}), 'name', null);
      if (isStr(name)) found.push(name);
    }
  }
}

// graph.rn:194
function edgeOf(from, owner, tr, kind, event) {
  const e = { from, kind };
  if (event !== '') e.event = event;
  const t = getOr(tr, 'target', undefined);
  e.to = isStr(t) ? targetPath(owner, t) : null;
  const guard = getOr(tr, 'guard', null);
  if (isObj(guard)) e.guard = getOr(guard, 'type', '');
  if (getOr(tr, 'reenter', false) === true) e.reenter = true;
  return e;
}

// graph.rn:213
function chooseOf(st) {
  const meta = getOr(st, 'meta', {});
  const choose = getOr(meta, 'choose', null);
  if (!isObj(choose)) return null;
  const c = {};
  const from = getOr(choose, 'from', null);
  if (isStr(from)) {
    c.source = from;
  } else if (isObj(from)) {
    const ks = sortedKeys(from);
    if (ks.length > 0) {
      c.source = ks[0];
      if (ks[0] === 'menu') {
        const menu = getOr(from, 'menu', []);
        c.options = (Array.isArray(menu) ? menu : []).map((item) => getOr(item, 'label', ''));
      }
    }
  }
  const floor = getOr(meta, 'floor', null);
  if (!isNull(floor)) c.floor = floor;
  const ask = getOr(meta, 'ask', null);
  if (!isNull(ask)) c.ask = ask;
  return c;
}

// graph.rn:247
function walk(states, parent, outStates, outEdges) {
  for (const key of sortedKeys(states)) {
    const st = states[key];
    const path = [...parent, key];
    const name = dotted(path);
    let kind = 'leaf';
    const ty = getOr(st, 'type', '');
    const children = getOr(st, 'states', null);
    if (ty === 'final') kind = 'final';
    else if (ty === 'history') kind = 'history';
    else if (isObj(children)) kind = 'compound';
    const rec = { path: name, kind };
    const d = getOr(st, 'description', null);
    if (isStr(d)) rec.description = d;
    const ch = chooseOf(st);
    if (isObj(ch)) rec.choose = ch;
    const tools = [];
    actionTools(getOr(st, 'entry', null), tools);
    actionTools(getOr(st, 'exit', null), tools);
    const on = getOr(st, 'on', {});
    for (const ev of sortedKeys(on)) {
      for (const tr of transitionsOf(on[ev])) actionTools(getOr(tr, 'actions', null), tools);
    }
    if (tools.length > 0) {
      tools.sort();
      rec.tools = tools;
    }
    if (kind === 'compound') {
      const init = getOr(st, 'initial', '');
      if (isStr(init) && init !== '') {
        rec.initial = init;
        outEdges.push({ from: name, to: name + '.' + init, kind: 'initial' });
      }
    }
    if (kind === 'history' && isObj(st) && Object.hasOwn(st, 'target') && isStr(st.target)) {
      outEdges.push({ from: name, to: targetPath(path, st.target), kind: 'history' });
    }
    outStates.push(rec);
    for (const tr of transitionsOf(getOr(st, 'always', null))) outEdges.push(edgeOf(name, path, tr, 'always', ''));
    for (const ev of sortedKeys(on)) {
      for (const tr of transitionsOf(on[ev])) outEdges.push(edgeOf(name, path, tr, 'on', ev));
    }
    for (const tr of transitionsOf(getOr(st, 'onDone', null))) outEdges.push(edgeOf(name, path, tr, 'onDone', ''));
    if (kind === 'compound') walk(children, path, outStates, outEdges);
  }
}

// graph.rn:316. `doc` is the parsed graph, or its text (a JSON syntax error then throws).
export function project(doc) {
  if (isStr(doc)) doc = JSON.parse(doc);
  const jev = getOr(getOr(doc, 'meta', {}), 'jev', {});
  const states = [];
  const edges = [];
  walk(getOr(doc, 'states', {}), [], states, edges);
  const rootOn = [];
  const on = getOr(doc, 'on', {});
  for (const ev of sortedKeys(on)) {
    for (const tr of transitionsOf(on[ev])) rootOn.push(edgeOf('*', [], tr, 'on', ev));
  }
  return {
    graph: getOr(doc, 'id', ''),
    description: getOr(doc, 'description', ''),
    initial: getOr(doc, 'initial', ''),
    input: getOr(jev, 'input', {}),
    budget: getOr(jev, 'budget', {}),
    tools: getOr(jev, 'tools', []),
    root_on: rootOn,
    states,
    edges,
    marks: { tool: 'jev_mark', kinds: ['enter', 'park', 'final', 'end'] },
  };
}
