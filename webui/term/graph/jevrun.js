// A jev run, read from its journal: the records of `eidolon log --json` in, the decision steps a graph tab marks and lists out. Pure: no DOM,
// no fetch, no clock; every string that came from a model, a page or a tool stays a string (nothing here builds markup from it).
//
//   newRun({ graph?, cut?, hostCap? })   an empty reducer; graph is the installed graph (object or JSON text), used only when the run's own is
//                                        not in its journal; cut is { context: 192, option: 32 }, the bytes jevlike reads of each
//   feed(run, records)                   take records (parsed objects, or one ndjson line each); ids at or below the last one are skipped, so a
//                                        poll that overlaps the last one, or feeding twice, changes nothing. Returns run.
//   feedText(run, ndjson)                feed the body of GET /hub/runs/<rid>/log?after=N; a line that is not JSON is skipped
//   view(run)                            the model (same object until a record is taken); view(run).lastId is the next ?after=
//   reduce(records, opts)                newRun + feed + view
//
// Feeding records one at a time and all at once give the same view: the reducer is a state machine over the records in order, and view() is a
// function of its state. A step is one visit to a state, opened by the run's own jev_mark enter; the journal never repeats a step, so a
// resumed run just carries on.
//
// What the journal does and does not say (F0, tests/fixtures/jev/README.md):
//   - WorkflowEnd is Completed also for a graph that ended in its own `failed` state: the end is read from the last jev_mark (final / end),
//     never from the door. status 'failed' with end.door 'Completed' is that case.
//   - A step with no ToolResult at the end is a cut run, not a failure. With no WorkflowEnd the status stays 'running' and view.open names
//     that step (in flight, or cut: the caller knows from the perch's own state which). WorkflowEnd Failed with such a step is 'cancelled'.
//   - SIGTERM can end a run Failed (cancelled) or Parked (the in-flight tool comes back 'cancelled while the tool was running'): view.interrupted.
//   - Resume is refused when any ToolResult is_error: view.unresumable names the first, with eidolon's own sentence (while parked, cut or cancelled).
//   - Call ids (w<run>-<n>, w<run>-park<n>) restart per run: a second WorkflowStart in the feed starts the model again.
//
// view(run) = {
//   rev, lastId, meta { run, plugin, workflow, version, hash, ts_ms, graph, start, goal, args },
//   status  running | parked | arrived | failed | stopped | exhausted | cancelled | done
//   open    null | { call_id, index, tool, ts_ms }   the step with no result yet
//   waiting null | { step, park }                    the park nobody has answered
//   unresumable null | { run, step, call_id, tool, error, reason }, interrupted
//   active (the last jev_mark's state), visited [states], trail [every state entered, the report's `path`], path [headings, output.path]
//   marks { current, visited, final, failed } for drawGraph, drawing (project() of the graph, one object per graph), graph { source journal |
//   installed | none, label, text, doc, pages, error }
//   steps [ { n, decision, state, id, call_id, ts_ms, duration_ms, waited_ms, open, page, arrival, jev, menu, parks, park, escalations, pick,
//             stopped, action, actions, errors } ]
//   end     null | { status, door, state, outcome, reason, cause, exhausted, decisions, asked, escalations, title }
//   budgets { decisions, actions, escalations, host }  each { used, cap, left };  spend [ { id, ts_ms, key, call_id, usage } ] (expected: none)
// }
// pick.how is 'chooser' (the scorer was sure), 'answer' (a person), 'rule' (the label equals the goal; no scorer call) or 'single' (one option left).
import { project } from './project.js';

export const HOST_STEP_CAP = 200; // eidolon caps a workflow run at 200 steps: every tool call, scorer call and mark counts one
export const FALLBACK_LABEL = 'drawn from the installed graph; may differ from the one this run used';
export const CUT = { context: 192, option: 32 }; // the bytes of the context and of each option jevlike reads (ckpt context_tokens, option_tokens)

const KEEP = 65536; // the longest tool text kept per result
const SHOW = 2000; // the longest tool text a step carries
const NOT_ACTIONS = new Set(['jev_mark', 'jev_graph', 'jev_choose', 'jev_entail']);
const CANCELLED = /^cancelled (while the tool was running|before the tool ran)$/;

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const str = (v) => (typeof v === 'string' ? v : null);
const fin = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const numOr = (v, d) => (typeof v === 'number' && Number.isFinite(v) ? v : d);
const clip = (s, n = KEEP) => (s.length > n ? s.slice(0, n) : s);
const own = (o, k) => (isObj(o) && Object.hasOwn(o, k) ? o[k] : undefined);
const fold = (s) => s.trim().toLowerCase(); // the `fold` of an equals guard (interp guards.rn values_equal)
const last = (xs, pred) => {
  for (let i = xs.length - 1; i >= 0; i--) if (pred(xs[i])) return xs[i];
  return null;
};
function json(s) {
  if (typeof s !== 'string') return undefined;
  try { return JSON.parse(s); } catch { return undefined; }
}
const transitions = (v) => (typeof v === 'string' ? [{ target: v }] : isObj(v) ? [v] : Array.isArray(v) ? v.map((t) => (typeof t === 'string' ? { target: t } : t)).filter(isObj) : []);

// ---- bytes: what jevlike reads ---------------------------------------------------------------------------------------------------------

const enc = new TextEncoder();
const dec = new TextDecoder('utf-8');

// The first n bytes of text, as text: a character the cut falls inside is dropped whole, so the result is always valid UTF-8. `key` is the raw
// n bytes (a string of byte values), which is what two options must differ in for the scorer to tell them apart.
export function cutBytes(text, n) {
  const s = String(text), b = enc.encode(s);
  let key = '';
  for (let i = 0; i < Math.min(n, b.length); i++) key += String.fromCharCode(b[i]);
  if (b.length <= n) return { text: s, bytes: b.length, clipped: false, key };
  let end = n;
  while (end > 0 && (b[end] & 0xc0) === 0x80) end--; // b[end] continues a character that began before it: the cut is inside one
  return { text: dec.decode(b.subarray(0, end)), bytes: b.length, clipped: true, key };
}

// ---- a browser snapshot: the head block and the tree (jev/lib/obs.rn parse_head, parse_refs, build_obs) --------------------------------------

const HEAD_KEY = /^[a-z][a-z_]*$/;
const PROP = /^\/(?<key>[A-Za-z][A-Za-z0-9_-]*):\s*(?<value>.*)$/;
const QUOTED = /^'(?<head>(?:''|[^'])*)'\s*:\s*(?<rest>.*)$/;
const ELEMENT = /^(?<role>[A-Za-z][A-Za-z0-9_-]*)(?:\s+"(?<name>(?:[^"\\]|\\.)*)")?(?<attrs>(?:\s*\[[A-Za-z]+=[^\]]*\])*)\s*:?\s*(?<text>.*)$/;
const ATTR = /\[(?<k>[A-Za-z]+)=(?<v>[^\]]*)\]/g;
const LANDMARKS = new Set(['banner', 'navigation', 'main', 'complementary', 'contentinfo', 'search', 'region']);

// `url: …\ntitle: …\n(key: value\n)*\n<tree>` -> { head, body, declared, got, truncated, h1 }, or null for any other text.
// truncated: the head's `chars` is the tree's length as the browser counted it; any other length arrived shortened.
export function parseSnapshot(raw) {
  if (typeof raw !== 'string') return null;
  const lines = raw.split('\n');
  if (lines.length < 3 || !lines[0].startsWith('url: ') || !lines[1].startsWith('title: ')) return null;
  const head = Object.create(null);
  head.url = lines[0].slice(5);
  head.title = lines[1].slice(7);
  for (let i = 2; i < lines.length; i++) {
    const l = lines[i];
    if (l === '') {
      const body = lines.slice(i + 1).join('\n');
      const c = str(head.chars);
      const declared = c !== null && /^[+-]?\d+$/.test(c.trim()) ? Number(c.trim()) : null;
      const got = declared === null ? null : [...body].length;
      let h1 = str(head.heading) || null;
      if (!h1) {
        const h = parseRefs(body, true).find((r) => r.role === 'heading' && r.level === 1 && r.name);
        h1 = h ? h.name : null;
      }
      return { head, body, declared, got, truncated: declared !== null && got !== declared, h1 };
    }
    const at = l.indexOf(': ');
    if (at < 0 || !HEAD_KEY.test(l.slice(0, at))) return null;
    head[l.slice(0, at)] = l.slice(at + 2);
  }
  return null;
}

// The accessibility tree -> [{ ref, role, name, level, url, landmark, text }] in order. Only elements with a ref are kept unless `all`
// (an element with none is never an option and never clicked).
export function parseRefs(tree, all = false) {
  const out = [], stack = [];
  for (const line of String(tree).split('\n')) {
    const stripped = line.trim();
    if (stripped === '') continue;
    let indent = 0;
    while (indent < line.length && line[indent] === ' ') indent++;
    if (!stripped.startsWith('- ')) continue;
    const content = line.slice(indent + 2);
    while (stack.length && stack[stack.length - 1].indent >= indent) stack.pop();
    const parent = stack.length ? stack[stack.length - 1].rec : null;
    const pm = PROP.exec(content);
    if (pm) {
      if (pm.groups.key === 'url' && parent) parent.url = pm.groups.value.trim();
      stack.push({ indent, rec: null });
      continue;
    }
    let src = content, override = null;
    if (content.startsWith("'")) {
      const qm = QUOTED.exec(content);
      if (qm) {
        src = qm.groups.head.replaceAll("''", "'");
        const rest = qm.groups.rest.trim();
        override = rest.length >= 2 && rest.startsWith('"') && rest.endsWith('"') ? rest.slice(1, -1) : rest;
      }
    }
    const em = ELEMENT.exec(src);
    if (!em) {
      stack.push({ indent, rec: null });
      continue;
    }
    const attrs = new Map();
    for (const a of (em.groups.attrs || '').matchAll(ATTR)) attrs.set(a.groups.k, a.groups.v);
    let landmark = null;
    for (let i = stack.length - 1; i >= 0; i--) {
      const anc = stack[i].rec;
      if (anc && LANDMARKS.has(anc.role)) { landmark = anc.role; break; }
    }
    const text = override === null ? (em.groups.text || '').trim() : override;
    const lv = attrs.has('level') && /^[+-]?\d+$/.test(attrs.get('level')) ? Number(attrs.get('level')) : null;
    const rec = { ref: attrs.has('ref') ? attrs.get('ref') : null, role: em.groups.role, name: em.groups.name === undefined ? null : em.groups.name,
      level: lv, url: null, landmark, text: text === '' ? null : text };
    if (all || (rec.ref !== null && rec.ref !== '')) out.push(rec);
    stack.push({ indent, rec });
  }
  return out;
}

// ---- a park's answer (interp.rn park_pick) ---------------------------------------------------------------------------------------------

// What the interpreter makes of an answer: `stop` or `stop: why` ends the run; else the exact label, else a number inside the menu; else it asks
// again. -> { kind: stop | label | index | invalid, index, reason }
export function classifyAnswer(answer, labels) {
  const a = String(answer).trim();
  if (a === 'stop' || a.startsWith('stop:')) {
    let reason = 'stopped at a park';
    if (a.length > 5) {
      const r = a.slice(5).trim();
      if (r !== '') reason = r;
    }
    return { kind: 'stop', index: null, reason };
  }
  const j = labels.indexOf(a);
  if (j >= 0) return { kind: 'label', index: j, reason: null };
  if (/^[+-]?\d+$/.test(a)) {
    const n = parseInt(a, 10) || 0; // -0 is 0
    if (n >= 0 && n < labels.length) return { kind: 'index', index: n, reason: null };
  }
  return { kind: 'invalid', index: null, reason: null };
}

// The prompt's first lines (interp.rn park_pick): [`'x' is not one of the options.`] `jev <graph>: <why>` [the graph's ask] `state: …` …
function parsePrompt(prompt) {
  const lines = String(prompt).split('\n');
  let retry = null, why = null, ask = null;
  const bad = /^'(.*)' is not one of the options\.$/.exec(lines[0] || '');
  if (bad) retry = bad[1];
  let at = -1;
  for (let i = 0; i < Math.min(lines.length, 4); i++) {
    const m = /^jev [^\s:]+: (.*)$/.exec(lines[i]);
    if (m) { why = m[1]; at = i; break; }
  }
  if (at >= 0) {
    const stateAt = lines.findIndex((l, i) => i > at && l.startsWith('state: '));
    if (stateAt > at + 1) ask = lines.slice(at + 1, stateAt).join('\n');
  }
  let cause = null;
  if (why !== null) {
    if (why.startsWith('chooser unavailable')) cause = 'unavailable';
    else if (/^top \S+ under floor /.test(why)) cause = 'floor';
    else if (/^top \S+, margin \S+ under /.test(why)) cause = 'margin';
    else if (why.startsWith('the menu was empty')) cause = 'empty';
  }
  return { retry, why, ask, cause };
}

// ---- a graph: floor, margin, threshold and budgets as the interpreter reads them (interp.rn graph_default, graph_budget) -----------------

function walkState(doc, path) {
  let node = own(doc, 'states'), st = null;
  for (const seg of String(path).split('.')) {
    st = own(node, seg);
    if (!isObj(st)) return null;
    node = own(st, 'states');
  }
  return st;
}

function factsOf(doc) {
  if (!isObj(doc)) return null;
  const jev = own(own(doc, 'meta'), 'jev');
  const defaults = own(jev, 'defaults'), budget = own(jev, 'budget');
  const meta = (p, key) => own(own(walkState(doc, p), 'meta'), key);
  return {
    floor: (p) => numOr(meta(p, 'floor'), numOr(own(defaults, 'floor'), 0.5)),
    margin: (p) => numOr(meta(p, 'margin'), numOr(own(defaults, 'margin'), 0.1)),
    // the first `entails` guard among the state's `always` transitions
    threshold: (p) => {
      for (const tr of transitions(own(walkState(doc, p), 'always'))) {
        const g = own(tr, 'guard');
        if (own(g, 'type') === 'entails') return numOr(own(own(g, 'params'), 'threshold'), numOr(own(defaults, 'threshold'), 0.6));
      }
      return null;
    },
    choose: (p) => {
      const c = meta(p, 'choose');
      return isObj(c) ? c : null;
    },
    max: numOr(own(defaults, 'max'), 64),
    cap: { steps: numOr(own(budget, 'steps'), 50), actions: numOr(own(budget, 'actions'), 100), escalations: numOr(own(budget, 'escalations'), 10) },
  };
}

// ---- the reducer -------------------------------------------------------------------------------------------------------------------------

function installed(g) {
  if (typeof g === 'string') {
    const doc = json(g);
    return isObj(doc) ? { doc, text: g } : null;
  }
  return isObj(g) ? { doc: g, text: null } : null;
}

export function newRun(opts = {}) {
  const cut = isObj(opts.cut) ? opts.cut : {};
  const run = {
    opts: { cut: { context: numOr(cut.context, CUT.context), option: numOr(cut.option, CUT.option) }, hostCap: numOr(opts.hostCap, HOST_STEP_CAP),
      installed: installed(opts.graph) },
    count: 0, lastId: -1, cache: null, drawn: null,
  };
  blank(run);
  return run;
}

// everything one run of the journal fills in; a second WorkflowStart starts over
function blank(run) {
  Object.assign(run, {
    start: null, calls: new Map(), order: [], visits: [], cur: null, marks: [], mark: null, finalMark: null, endMark: null, trail: [],
    parks: new Map(), parkList: [], end: null, parkedAt: null, spend: [], hostSteps: 0, actionCount: 0, firstError: null, interrupted: false,
    pages: { items: [], done: false, error: null, text: null, graph: null }, graphDoc: null, graphId: null, lastTs: null,
  });
}

export function feed(run, records) {
  for (const rec of Array.isArray(records) ? records : []) take(run, typeof rec === 'string' ? json(rec) : rec);
  return run;
}

export function feedText(run, text) {
  return feed(run, String(text || '').split('\n').filter((l) => l.trim() !== ''));
}

function take(run, rec) {
  if (!isObj(rec)) return;
  if (typeof rec.id === 'number') {
    if (rec.id <= run.lastId) return;
    run.lastId = rec.id;
  }
  run.count++;
  const ts = fin(rec.ts_ms);
  if (ts !== null) run.lastTs = ts;
  if (!isObj(rec.kind)) return;
  const name = Object.keys(rec.kind)[0], body = rec.kind[name];
  if (!isObj(body)) return;
  switch (name) {
    case 'WorkflowStart': onStart(run, rec, body, ts); break;
    case 'WorkflowStep': onStep(run, rec, body, ts); break;
    case 'PolicyVerdict': {
      const c = run.calls.get(body.tool_use_id);
      if (c) c.verdicts.push(str(body.outcome));
      break;
    }
    case 'ToolResult': onResult(run, rec, body, ts); break;
    case 'AskUser': onAsk(run, rec, body, ts); break;
    case 'WorkflowEnd': onEnd(run, body, ts); break;
    case 'ModelSpend':
      run.spend.push({ id: fin(rec.id), ts_ms: ts, key: str(body.key), call_id: str(body.call_id), usage: isObj(body.usage) ? body.usage : null });
      break;
    default: // SessionStart, ApiRequest and whatever eidolon adds later
  }
  if (run.cur && ts !== null && ts > run.cur.lastTs) run.cur.lastTs = ts;
}

function onStart(run, rec, body, ts) {
  if (run.start) {
    run.runs = (run.runs || 1) + 1;
    blank(run);
  }
  const args = isObj(body.args) ? body.args : json(body.args);
  const flat = (v) => typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean';
  const a = isObj(args) ? Object.fromEntries(Object.entries(args).filter(([, v]) => flat(v))) : {}; // fromEntries: a key `__proto__` stays a key
  run.start = { run: fin(rec.id), ts, plugin: str(body.plugin), workflow: str(body.workflow), version: str(body.version), hash: str(body.hash), args: a };
}

function onStep(run, rec, body, ts) {
  const id = str(body.call_id), tool = str(body.tool) || '?';
  if (id === null || run.calls.has(id)) return; // a step the journal already holds (it never repeats one)
  const call = { rec: fin(rec.id), ts, run: fin(body.run), index: fin(body.index), call_id: id, tool, verdicts: [], result: null, visit: null,
    input: isObj(body.input) ? body.input : (json(body.input) ?? null), head: null, snap: null, links: null, refs: null };
  if (!isObj(call.input)) call.input = null;
  run.calls.set(id, call);
  run.order.push(call);
  run.hostSteps++;
  if (!NOT_ACTIONS.has(tool)) run.actionCount++;
  if (tool === 'jev_mark') return onMark(run, call, ts);
  if (tool === 'jev_graph') {
    if (!run.graphId && call.input && typeof call.input.graph === 'string') run.graphId = call.input.graph;
    return;
  }
  if (run.cur) {
    run.cur.calls.push(call);
    call.visit = run.cur;
  }
}

// jev_mark's input is the record of where the run is, whether or not its result came back an error (a cancelled run's marks do)
function onMark(run, call, ts) {
  const m = call.input || {};
  const mark = { rec: call.rec, ts, kind: str(m.kind), state: str(m.state), step: fin(m.step), actions: fin(m.actions), outcome: str(m.outcome) };
  if (!run.graphId && typeof m.graph === 'string') run.graphId = m.graph;
  run.marks.push(mark);
  run.mark = mark;
  if (mark.kind === 'enter' && mark.state !== null) {
    if (run.cur) run.cur.closeTs = run.cur.closeTs ?? ts;
    const v = { n: run.visits.length + 1, state: mark.state, rec: call.rec, call_id: call.call_id, ts, lastTs: ts, closeTs: null, markStep: mark.step,
      parkStep: null, calls: [], parks: [] };
    run.visits.push(v);
    run.cur = v;
    call.visit = v;
    run.trail.push(mark.state);
  } else if (mark.kind === 'final' && mark.state !== null) {
    if (run.cur) run.cur.closeTs = run.cur.closeTs ?? ts;
    run.finalMark = mark;
    run.trail.push(mark.state);
  } else if (mark.kind === 'park') {
    if (run.cur) run.cur.parkStep = mark.step;
  } else if (mark.kind === 'end') {
    if (run.cur) run.cur.closeTs = run.cur.closeTs ?? ts;
    run.endMark = mark;
  }
}

function onResult(run, rec, body, ts) {
  const call = run.calls.get(body.tool_use_id);
  if (!call || call.result) return;
  const text = str(body.content) ?? (body.content === undefined || body.content === null ? '' : JSON.stringify(body.content));
  const bad = body.is_error === true;
  call.result = { rec: fin(rec.id), ts, content: call.tool === 'jev_graph' ? text : clip(text), is_error: bad };
  if (bad) {
    if (!run.firstError) run.firstError = call;
    if (CANCELLED.test(text.trim())) run.interrupted = true;
  }
  if (call.tool === 'jev_graph') return onPage(run, call);
  if (NOT_ACTIONS.has(call.tool) || !text.startsWith('url: ')) return;
  const sn = parseSnapshot(text);
  if (!sn) return;
  call.head = sn.head;
  call.snap = sn;
  if (!bad) {
    call.links = parseRefs(sn.body);
    call.refs = new Map();
    for (const r of call.links) if (!call.refs.has(r.ref)) call.refs.set(r.ref, r);
    call.result.content = ''; // the tree is not kept: the links and the head are what a step needs
  }
  sn.body = '';
}

// The graph is the pages jev_graph handed the workflow, joined (interp.rn read_graph): `jev-page next=<line>\n<body>`, next 0 on the last.
function onPage(run, call) {
  const p = run.pages;
  if (p.done || p.error) return;
  if (call.result.is_error) { p.error = call.result.content; return; }
  const text = call.result.content, nl = text.indexOf('\n');
  const head = nl < 0 ? text : text.slice(0, nl), body = nl < 0 ? '' : text.slice(nl + 1);
  if (!head.startsWith('jev-page next=')) { p.error = 'a page of the graph had no jev-page header'; return; }
  const t = head.slice(14).trim();
  if (!/^[+-]?\d+$/.test(t)) { p.error = 'a page of the graph had a bad jev-page header: ' + head; return; }
  const next = Number(t), from = fin(call.input && call.input.from) ?? (p.items.length ? p.items[p.items.length - 1].next : 1);
  if (next !== 0 && next <= from) { p.error = "the graph's pages do not advance"; return; }
  p.items.push({ from, next, body });
  if (next !== 0) return;
  p.done = true;
  p.text = p.items.map((i) => i.body).join('');
  const doc = json(p.text);
  if (isObj(doc)) run.graphDoc = doc;
  else p.error = 'the graph is not JSON';
}

function onAsk(run, rec, body, ts) {
  const id = str(body.call_id);
  if (id === null) return;
  let p = run.parks.get(id);
  if (!p) {
    p = { call_id: id, n: run.parkList.length, prompt: str(body.prompt) ?? '', ts, visit: run.cur, answer: null, answerTs: null, waited: null };
    run.parks.set(id, p);
    run.parkList.push(p);
    if (run.cur) run.cur.parks.push(p);
  }
  if (typeof body.answer === 'string' && p.answer === null) {
    p.answer = body.answer;
    p.answerTs = ts;
    p.waited = ts !== null && (run.parkedAt ?? p.ts) !== null ? ts - (run.parkedAt ?? p.ts) : null;
    run.parkedAt = null;
    if (run.end && run.end.status === 'Parked') run.end = null; // the answer resumes the run: it is running until the next WorkflowEnd
  }
}

function onEnd(run, body, ts) {
  const status = str(body.status);
  run.end = { ts, status };
  if (status === 'Parked') run.parkedAt = ts;
  else if (run.cur) run.cur.closeTs = run.cur.closeTs ?? ts;
}

// ---- the view ----------------------------------------------------------------------------------------------------------------------------

export function view(run) {
  if (run.cache && run.cache.rev === run.count) return run.cache.view;
  const v = build(run);
  run.cache = { rev: run.count, view: v };
  return v;
}

export function reduce(records, opts) {
  return view(feed(newRun(opts), records));
}

const SNAPSHOT_TOOL = /(^|_)snapshot$/;

// the graph the run used (its own pages, joined), else the installed one, labelled
function graphOf(run) {
  const p = run.pages, inst = run.opts.installed;
  if (run.graphDoc) return { source: 'journal', label: null, text: p.text, doc: run.graphDoc, pages: p.items.length, error: null };
  if (inst) return { source: 'installed', label: FALLBACK_LABEL, text: inst.text, doc: inst.doc, pages: p.items.length, error: p.error };
  return { source: 'none', label: null, text: null, doc: null, pages: p.items.length, error: p.error };
}

// project() once per graph object, so the drawing keeps its identity and draw.js's layout memo holds across polls
function drawingOf(run, g) {
  if (!g.doc) return null;
  if (!run.drawn || run.drawn.doc !== g.doc) {
    let drawing = null;
    try { drawing = project(g.doc); } catch { drawing = null; }
    run.drawn = { doc: g.doc, drawing };
  }
  if (!run.drawn.drawing && !g.error) g.error = 'the graph could not be drawn';
  return run.drawn.drawing;
}

// The final states a graph ends in when something went wrong: where its ERROR events lead, and the names the six graphs use for them.
function failing(drawing) {
  const set = new Set();
  if (drawing) for (const e of [...drawing.edges, ...drawing.root_on]) if (e.kind === 'on' && e.event === 'ERROR' && typeof e.to === 'string') set.add(e.to);
  return (state) => set.has(state) || /^(failed|aborted|error)$/.test(state.slice(state.lastIndexOf('.') + 1));
}

function build(run) {
  const g = graphOf(run);
  const drawing = drawingOf(run, g);
  const facts = factsOf(g.doc);
  const s = run.start;
  const goal = s && typeof s.args.goal === 'string' ? s.args.goal : null;
  const ctx = { run, facts, goal, cut: run.opts.cut, visited: [], path: [] };
  const steps = run.visits.map((v, i) => stepOf(ctx, v, i));

  const fm = run.finalMark, em = run.endMark, E = run.end;
  const over = !!(fm || em || (E && E.status !== 'Parked'));
  const openCall = last(run.order, (c) => c.result === null);
  const isFail = failing(drawing);
  let status;
  if (fm) status = isFail(fm.state) ? 'failed' : 'arrived';
  else if (em) status = em.outcome === 'exhausted' ? 'exhausted' : em.outcome === 'stopped' ? 'stopped' : 'failed';
  else if (!E) status = 'running';
  else if (E.status === 'Parked') status = 'parked';
  else if (E.status === 'Failed') status = openCall ? 'cancelled' : 'failed';
  else status = 'done';

  const marksMax = (key) => run.marks.reduce((m, x) => (x[key] !== null && x[key] > m ? x[key] : m), 0);
  const cap = facts ? facts.cap : { steps: null, actions: null, escalations: null };
  const gauge = (used, c) => ({ used, cap: c, left: c === null ? null : Math.max(0, c - used) });
  const budgets = {
    decisions: gauge(Math.max(marksMax('step'), ...steps.map((x) => x.decision ?? 0)), cap.steps),
    actions: gauge(run.actionCount, cap.actions),
    escalations: gauge(run.parkList.length, cap.escalations),
    host: gauge(run.hostSteps, run.opts.hostCap),
  };

  const active = run.mark ? run.mark.state : null;
  const visited = [...new Set(run.marks.filter((m) => m.kind === 'enter' && m.state !== null).map((m) => m.state))];
  const failedFinal = fm && isFail(fm.state);
  const marks = {
    current: !over && active !== null ? [active] : [],
    visited,
    final: fm && !failedFinal ? [fm.state] : [],
    failed: failedFinal ? [fm.state] : !fm && (status === 'exhausted' || status === 'failed') && active !== null ? [active] : [],
  };

  const wp = status === 'parked' ? last(run.parkList, (p) => p.answer === null) : null;
  const wstep = wp && wp.visit ? steps[wp.visit.n - 1] : null;
  const waiting = wstep ? { step: wstep.n, park: wstep.parks.find((p) => p.call_id === wp.call_id) || null } : null;

  const fe = run.firstError;
  const unresumable = fe && (status === 'running' || status === 'parked' || status === 'cancelled') ? unresumableOf(run, fe) : null;
  const open = openCall && !(E && E.status === 'Parked') ? { call_id: openCall.call_id, index: openCall.index, tool: openCall.tool, ts_ms: openCall.ts } : null;

  let end = null;
  if (status !== 'running' && status !== 'parked') {
    const lastVisit = run.visits.length ? run.visits[run.visits.length - 1] : null;
    const stop = last(steps, (x) => x.stopped);
    const why = status === 'failed' ? failReason(lastVisit, run) : { reason: null, cause: null };
    end = {
      status, door: E ? E.status : null, state: fm ? fm.state : em ? em.state : active, outcome: fm ? 'reached' : em ? em.outcome : null,
      reason: status === 'stopped' ? (stop ? stop.stopped.reason : null) : status === 'cancelled' ? 'the run was cancelled' : why.reason,
      cause: why.cause, exhausted: status === 'exhausted' ? exhaustedBy(budgets) : null,
      decisions: budgets.decisions.used, asked: steps.filter((x) => x.parks.length > 0).length, escalations: budgets.escalations.used,
      title: ctx.path.length ? ctx.path[ctx.path.length - 1] : null,
    };
  }

  const args = s ? s.args : {};
  const firstCall = run.order.length ? run.order[0] : null;
  return {
    rev: run.count, lastId: run.lastId,
    meta: { run: s ? s.run : firstCall ? firstCall.run : null, plugin: s ? s.plugin : null, workflow: s ? s.workflow : null, version: s ? s.version : null,
      hash: s ? s.hash : null, ts_ms: s ? s.ts : null, graph: run.graphId ?? (s ? s.workflow : null), start: typeof args.start === 'string' ? args.start : null,
      goal, args },
    status, open, waiting, unresumable, interrupted: run.interrupted,
    active, visited, trail: [...run.trail], path: ctx.path, marks, drawing, graph: g,
    steps, end, budgets,
    spend: run.spend.map((x) => ({ ...x })),
  };
}

// eidolon's own sentence for a resume it refuses (F0 c, g, j), rebuilt from the first failed result in the journal
function unresumableOf(run, c) {
  const s = run.start, err = c.result.content;
  const no = s && s.run !== null ? s.run : c.run;
  const again = s && s.plugin && s.workflow ? ` (\`eidolon workflow run ${s.plugin} ${s.workflow}\`)` : '';
  const step = c.index ?? (Number(c.call_id.slice(c.call_id.lastIndexOf('-') + 1)) || 0);
  return { run: no, step, call_id: c.call_id, tool: c.tool, error: err,
    reason: `refusing run #${no}: step ${step} (${c.call_id} ${c.tool}) is journaled as a failure — ${err}. The memo replays a journaled result `
      + `verbatim, so resuming cannot change it: start a new run${again}` };
}

// What put a graph in its failed state: the snapshot that came short (interp.rn run_tool_action) or the call that failed, in the last visit
function failReason(v, run) {
  if (v) {
    const t = last(v.calls, (c) => c.snap && !c.result.is_error && c.snap.truncated);
    if (t) {
      return { reason: `observation from '${t.tool}' was truncated in transport: ${t.snap.got} of ${t.snap.declared} declared characters arrived`, cause: 'truncated' };
    }
    const e = last(v.calls, (c) => c.result && c.result.is_error && c.tool !== 'jev_choose' && c.tool !== 'jev_entail');
    if (e) return { reason: clip(e.result.content, SHOW), cause: 'failed' };
  }
  const any = last(run.order, (c) => c.result && c.result.is_error && c.tool !== 'jev_mark');
  return { reason: any ? clip(any.result.content, SHOW) : null, cause: any ? 'failed' : null };
}

// which budget an `end exhausted` mark ran into: the one at its cap (steps, actions, escalations), else a state's visits
function exhaustedBy(b) {
  for (const k of ['decisions', 'actions', 'escalations']) if (b[k].cap !== null && b[k].used >= b[k].cap) return k === 'decisions' ? 'steps' : k;
  return 'visits';
}

// ---- one step ------------------------------------------------------------------------------------------------------------------------------

function stepOf(ctx, v, i) {
  const { run, facts } = ctx;
  const calls = v.calls;
  const entailCall = last(calls, (c) => c.tool === 'jev_entail');
  const chooseCall = last(calls, (c) => c.tool === 'jev_choose');
  const snapCall = last(calls, (c) => c.snap && !c.result.is_error) || last(calls, (c) => c.snap);
  const actions = calls.filter((c) => !NOT_ACTIONS.has(c.tool) && !c.snap && !SNAPSHOT_TOOL.test(c.tool)).map((c) => actionOf(c, snapCall));
  const action = actions.length ? actions[actions.length - 1] : null;
  const click = last(actions, (a) => a.ref !== null);

  const page = pageOf(snapCall);
  const pageOk = page !== null && page.ok;
  if (pageOk && page.heading) ctx.path.push(page.heading);
  const arrival = pageOk && (ctx.goal !== null || entailCall) ? arrivalOf(ctx, v, page, entailCall) : null;

  let menu = null;
  if (chooseCall) menu = chooserMenu(ctx, v, chooseCall);
  else if (pageOk && click) menu = plainMenu(ctx, v, click, snapCall);
  else if (pageOk && (v.parks.length > 0 || (action && /(^|_)back$/.test(action.tool)))) menu = blankMenu('empty', 'empty');
  const labels = menu ? menu.options.map((o) => o.label) : [];

  const parks = v.parks.map((p) => parkOf(p, labels, menu));
  const answered = last(parks, (p) => p.answer !== null);
  const stopped = answered && answered.kind === 'stop' ? { reason: answered.stop } : null;

  // how it was decided: the scorer when it was sure, a person when it was not, else the rule or the lone option; the click says what happened
  let how = null, index = null, label = null, ref = null;
  if (menu && menu.source === 'chooser') {
    if (answered && answered.picked !== null) { how = 'answer'; index = answered.picked; } else if (menu.sure) { how = 'chooser'; index = menu.best_index; }
  } else if (menu && (menu.source === 'rule' || menu.source === 'single')) {
    how = menu.source;
  }
  if (click) {
    ref = click.ref;
    label = click.label;
    const at = label === null ? -1 : labels.indexOf(label);
    index = at >= 0 ? at : null;
    if (how === null && menu && menu.source === 'chooser') how = 'chooser';
  } else if (index !== null && menu.options[index]) {
    label = menu.options[index].label;
  }
  const pick = how !== null && (click || label !== null) ? { label, index, how, ref } : null;

  if (pageOk && click) ctx.visited.push(page.heading);
  const decision = v.parkStep ?? (menu ? (v.markStep ?? 0) + 1 : null);
  const end = v.closeTs ?? v.lastTs;
  const over = !!(run.finalMark || run.endMark || (run.end && run.end.status !== 'Parked'));
  return {
    n: v.n, decision, state: v.state, id: v.rec, call_id: v.call_id, ts_ms: v.ts, duration_ms: end !== null && v.ts !== null ? end - v.ts : null,
    waited_ms: parks.reduce((t, p) => t + (p.waited_ms ?? 0), 0), open: i === run.visits.length - 1 && !over,
    page, arrival, jev: chooseCall ? jevOf(ctx, chooseCall, menu) : null, menu, parks, park: parks.length ? parks[parks.length - 1] : null,
    escalations: parks.length, pick, stopped, action, actions,
    errors: calls.filter((c) => c.result && c.result.is_error).map((c) => ({ call_id: c.call_id, tool: c.tool, text: clip(c.result.content, SHOW) })),
  };
}

function pageOf(c) {
  if (!c) return null;
  const sn = c.snap, bad = c.result.is_error;
  return { url: str(sn.head.url), title: str(sn.head.title), heading: sn.h1, chars: sn.declared,
    truncated: !bad && sn.truncated ? { got: sn.got, declared: sn.declared } : null, error: bad ? clip(c.result.content, SHOW) : null,
    ok: !bad && !sn.truncated };
}

// A tool action's input (`ref`) and result. The fake browser answers 'clicked e3 -> <url>', the real one a JSON {ref, url, title, confined}.
function actionOf(c, snapCall) {
  const ref = c.input && typeof c.input.ref === 'string' ? c.input.ref : null;
  const rec = ref !== null && snapCall && snapCall.refs ? snapCall.refs.get(ref) : null;
  const r = c.result;
  let result = null;
  if (r && !r.is_error) {
    const text = clip(r.content, SHOW), j = text.startsWith('{') ? json(r.content) : undefined;
    const m = /->\s*(\S+)/.exec(text) || /^opened\s+(\S+)/.exec(text);
    result = { text, url: isObj(j) ? str(j.url) : m ? m[1] : null, title: isObj(j) ? str(j.title) : null };
  }
  return { call_id: c.call_id, tool: c.tool, input: c.input, ref, refOk: ref === null ? null : !!rec, label: rec ? (rec.name ?? '') : null, ts_ms: c.ts,
    duration_ms: r && r.ts !== null && c.ts !== null ? r.ts - c.ts : null, pending: !r, result, error: r && r.is_error ? clip(r.content, SHOW) : null };
}

// the `always` guards of a reading state: the heading equals the goal (exactly), else jev_entail's entailment reaches the threshold
function arrivalOf(ctx, v, page, c) {
  const goal = ctx.goal, heading = page.heading;
  const equals = { heading, goal, hit: goal !== null && heading !== null ? heading === goal : null };
  const entail = c ? entailOf(ctx, v, c) : null;
  return { equals, entail, error: entail ? entail.error : null, hit: equals.hit === true || (entail !== null && entail.hit === true) };
}

function entailOf(ctx, v, c) {
  const threshold = ctx.facts ? ctx.facts.threshold(v.state) : null;
  const h = c.input && Array.isArray(c.input.hypotheses) ? c.input.hypotheses[0] : null;
  const out = { pending: !c.result, score: null, threshold, hit: null, label: null, hypothesis: typeof h === 'string' ? h : null, error: null };
  if (!c.result) return out;
  const fail = (e) => { out.error = e; out.hit = false; return out; };
  if (c.result.is_error) return fail(clip(c.result.content, SHOW));
  const j = json(c.result.content);
  if (j === undefined) return fail("the scorer's answer was not JSON");
  const rs = own(j, 'results'), r = Array.isArray(rs) ? rs[0] : undefined;
  if (!isObj(r)) return fail('the scorer answered no result');
  out.score = numOr(own(own(r, 'scores'), 'entailment'), 0);
  out.label = str(r.label);
  out.hit = threshold === null ? null : out.score >= threshold;
  return out;
}

// jev's view: jevlike reads the first 192 bytes of the context and the first 32 of each option, so options that agree there are one to it
function jevOf(ctx, c, menu) {
  const input = c.input || {};
  const options = Array.isArray(input.options) ? input.options.map((o) => (typeof o === 'string' ? o : String(o))) : [];
  const cx = cutBytes(str(input.context) ?? '', ctx.cut.context);
  const cuts = options.map((o) => cutBytes(o, ctx.cut.option));
  const first = new Map(), collisions = [];
  cuts.forEach((x, j) => {
    if (!first.has(x.key)) first.set(x.key, j);
    else if (options[first.get(x.key)] !== options[j]) collisions.push([first.get(x.key), j]);
  });
  const top = menu ? menu.best_index : null;
  return { context_cut: cx.text, context_bytes: cx.bytes, context_clipped: cx.clipped, options_cut: cuts.map((x) => x.text),
    options_clipped: cuts.map((x) => x.clipped), collisions, top_tie: top !== null && collisions.some((p) => p[0] === top || p[1] === top) };
}

// ---- a step's menu -------------------------------------------------------------------------------------------------------------------------

function blankMenu(source, verdict) {
  return { source, options: [], reconstructed: false, pending: false, best: null, best_index: null, top1: null, top2: null, floor: null, margin: null,
    floorOk: null, marginOk: null, sure: null, verdict, asked_for: null, unavailable: null };
}

// jev_choose: the options it was given, the p it answered, and the interpreter's own test (choose_phase): top >= floor and top - runner-up >= margin
function chooserMenu(ctx, v, c) {
  const input = c.input || {};
  const labels = Array.isArray(input.options) ? input.options.map((o) => (typeof o === 'string' ? o : String(o))) : [];
  const m = blankMenu('chooser', 'pending');
  m.options = labels.map((label) => ({ label, p: null }));
  const f = ctx.facts;
  if (f) { m.floor = f.floor(v.state); m.margin = f.margin(v.state); }
  if (!c.result) { m.pending = true; return m; }
  let probs = null;
  if (c.result.is_error) {
    m.unavailable = clip(c.result.content, SHOW);
  } else {
    const j = json(c.result.content);
    if (j === undefined) {
      m.unavailable = "the chooser's answer was not JSON";
    } else {
      const table = own(j, 'probs');
      probs = labels.map((l) => numOr(isObj(table) && Object.hasOwn(table, l) ? table[l] : 0, 0));
      m.best = str(own(j, 'best'));
    }
  }
  if (m.unavailable !== null) { m.verdict = 'unavailable'; return m; }
  if (probs.length === 0) { m.verdict = 'empty'; return m; }
  m.options.forEach((o, i) => { o.p = probs[i]; });
  let best = 0;
  for (let k = 1; k < probs.length; k++) if (probs[k] > probs[best]) best = k; // the highest, the first of equals
  let top2 = 0;
  for (let k = 0; k < probs.length; k++) if (k !== best && probs[k] > top2) top2 = probs[k];
  m.best_index = best;
  m.top1 = probs[best];
  m.top2 = top2;
  if (!f) { m.verdict = 'scored'; return m; }
  m.floorOk = m.top1 >= m.floor;
  m.marginOk = m.top1 - m.top2 >= m.margin;
  m.sure = m.floorOk && m.marginOk;
  m.verdict = m.sure ? 'sure' : 'asked';
  m.asked_for = m.sure ? null : (m.top1 < m.floor ? 'floor' : 'margin');
  return m;
}

// No jev_choose before the click: a `prefer` rule took the label that equals the goal, or one option was left. The menu is rebuilt from the
// snapshot by the state's `meta.choose` (options.rn build_options) when the graph says how, else it is just the option taken.
function plainMenu(ctx, v, click, snapCall) {
  const how = click.label !== null && ctx.goal !== null && fold(click.label) === fold(ctx.goal) ? 'rule' : 'single';
  const m = blankMenu(how, how);
  const cfg = ctx.facts ? ctx.facts.choose(v.state) : null;
  const rebuilt = cfg && snapCall && snapCall.links ? rebuild(cfg, snapCall.links, ctx.visited, ctx.facts.max) : null;
  if (rebuilt && click.label !== null && rebuilt.includes(click.label)) {
    m.options = rebuilt.map((label) => ({ label, p: null }));
    m.reconstructed = true;
  } else if (click.label !== null) {
    m.options = [{ label: click.label, p: null }];
  }
  return m;
}

// choose.from.refs -> the labels: roles, within, named, url; duplicates collapsed; `exclude` of {{context.visited}}; capped at max
function rebuild(cfg, links, visited, dmax) {
  const from = cfg.from, refs = isObj(from) ? from.refs : null;
  if (!isObj(refs)) return null;
  const roles = Array.isArray(refs.roles) ? refs.roles : ['link', 'button'];
  const within = typeof refs.within === 'string' ? refs.within : null;
  let re = null;
  if (typeof refs.url === 'string') {
    try { re = new RegExp(refs.url); } catch { return null; }
  }
  const seen = new Set(), out = [];
  for (const r of links) {
    if (!roles.includes(r.role) || (within !== null && r.landmark !== within)) continue;
    if (refs.named !== false && (r.name === null || r.name === '')) continue;
    if (re && (!r.url || !re.test(r.url))) continue;
    const label = r.name ?? '';
    if (!seen.has(label)) { seen.add(label); out.push(label); }
  }
  const ex = typeof cfg.exclude === 'string' && /^\{\{\s*context\.visited\s*\}\}$/.test(cfg.exclude) ? new Set(visited) : null;
  const kept = ex ? out.filter((l) => !ex.has(l)) : out;
  return kept.slice(0, numOr(cfg.max, dmax));
}

// ---- a park ----------------------------------------------------------------------------------------------------------------------------------

function parkOf(p, labels, menu) {
  const info = parsePrompt(p.prompt);
  const a = p.answer === null ? null : classifyAnswer(p.answer, labels);
  return { call_id: p.call_id, n: p.n, prompt: p.prompt, ts_ms: p.ts, why: info.why, cause: info.cause, ask: info.ask, retry: info.retry,
    options: menu ? menu.options.map((o) => ({ label: o.label, p: o.p })) : [],
    answer: p.answer, answer_ts_ms: p.answerTs, waited_ms: p.waited, kind: a ? a.kind : null, invalid: a ? a.kind === 'invalid' : false,
    picked: a && (a.kind === 'label' || a.kind === 'index') ? a.index : null, stop: a && a.kind === 'stop' ? a.reason : null };
}
