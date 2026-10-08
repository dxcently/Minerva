// The graph tab's runs: what the perch says about jev runs (`/hub/runs`, perch/src/runs/README.md) held as signals, the polls that keep them
// current and the four things a person can do to a run (start, answer, cancel, abandon). view.js draws them; nothing here makes markup.
//
// The top half is pure and tested (tests/runs.test.mjs); the bottom half holds the state and does the asking.
//
//   Polls, every one through core/lane.js (one request in flight across the page):
//     the shown run     its detail (`runs/<rid>`), then its log from the cursor (`runs/<rid>/log?after=N`): every 1 s while it runs, every 5 s
//                       while it is parked, and not at all once it has ended (the pass that sees the end is the last). Only while the tab is
//                       drawn and the page is visible
//     health, the list  every 10 s, the same way
//   Actions are a person's and go straight out (hubPost): they never wait in the lane.
//
//   The cursor is the id of the last record held (view(run).lastId); the first poll sends no `after`. A 200 with an empty body is "nothing new"
//   and never an error; the same `after` always gets the same lines, so a lost response loses nothing. jevrun.js skips any id it holds, so
//   an overlapping poll adds nothing. The path never carries any other query: a cache-buster is a 400.
//
//   Nothing refuses a run over what jev's /health says (operator ruling 2026-10-07): `runGate` and `formGate` do not read health at all, and the
//   tests hold them to it with the health signal set to each decision-log state. The perch's only 409 on a start is "one run at a time", naming
//   the holder; a service that is down is the perch's 503 with its own sentence.
//
//   Not a poll, so not in the lane: `graphDoc` reads one graph file with hubGet, from a run's first look or the form's choice. It is cached per
//   name and sha in `docs`, so a graph is read once, and it never repeats on a timer.
//
// For W2b-2 (the steps) these are exported: answerRun(rid, text), answerPayload(index, labels), STOP, parkGate(row, v), acting, notice.
// For W3 (the turn graph): picked (null, 'turns' or a rid) and currentRid().
import { signal } from '../core/ui.js';
import { hubGet, hubPost } from '../core/api.js';
import { hubPoll, hubPollText } from '../core/lane.js';
import { clean } from '../renderers/jev.js';
import { newRun, feedText, view, classifyAnswer } from './jevrun.js';

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const own = (o, k) => (isObj(o) && Object.hasOwn(o, k) ? o[k] : undefined);

// ---------------------------------------------------------------- states and polling
export const ENDED = new Set(['done', 'failed', 'cancelled', 'abandoned', 'refused', 'lost']);
export const HOLDING = new Set(['running', 'parked']); // the two that hold the one shared browser
export const RUNNING_MS = 1000;
export const PARKED_MS = 5000;
export const HEALTH_MS = 10000;
export const RETRY_MAX_MS = 10000;

// How long to wait before the next look at a run: null once it has ended (there is nothing more to learn). Before the first answer the state is
// unknown and a run is assumed to be moving.
export function pollMs(state) {
  if (ENDED.has(state)) return null;
  return state === 'parked' ? PARKED_MS : RUNNING_MS;
}

// After `fails` passes in a row that got no answer: at least the usual interval, doubling from 2 s, never past 10 s.
export const retryMs = (base, fails) => (fails <= 0 ? base : Math.min(RETRY_MAX_MS, Math.max(base, 2000 * 2 ** (Math.min(fails, 4) - 1))));

// ---------------------------------------------------------------- paths
// Ids and graph names go into a path, so each is checked first: the perch's ids are `r` and digits, a graph is a bare name.
export const ridOk = (rid) => typeof rid === 'string' && /^r\d{1,9}$/.test(rid);
export const graphOk = (name) => typeof name === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(name);
export const detailPath = (rid) => (ridOk(rid) ? `runs/${rid}` : null);
// `after` is the cursor: null for the first poll (no query at all: after=0 would skip record 0), else a whole number.
export const logPath = (rid, after) => (ridOk(rid) ? `runs/${rid}/log${Number.isInteger(after) && after >= 0 ? `?after=${after}` : ''}` : null);
export const graphPath = (name) => (graphOk(name) ? `runs/graphs/${name}` : null); // never a query

// ---------------------------------------------------------------- what the perch said
// The sentence for an answer that was not a 200: the perch's own (`{ error, why }` are one sentence under both names), else what happened.
export function said(r) {
  if (!r || r.status === 0) return 'the perch did not answer';
  const d = r.data;
  const e = isObj(d) ? (typeof d.error === 'string' && d.error ? d.error : typeof d.why === 'string' ? d.why : '') : '';
  if (e) return clean(e, 240);
  if (r.status === 401) return "the perch refused this page's login (it restarted?)";
  if (r.status === 404) return 'this perch has no such route (an older perch?)';
  return `the perch answered ${r.status}`;
}

// ---------------------------------------------------------------- the log and its cursor
// The cursor: the id of the last record the reducer holds, or null before any (the first poll sends no `after`).
export function afterOf(run) {
  const id = view(run).lastId;
  return Number.isInteger(id) && id >= 0 ? id : null;
}

// Take one answer of the log route into the reducer. -> { ok, fresh, gone?, stop?, sentence }.
//   200      its records (an empty body is "nothing new"); jevrun skips an id it holds, so the cursor only moves forward
//   404      the perch does not know the run          413, 400   it will not get better: stop asking
//   others   (no answer, eidolon failed or was slow)  the same `after` next time
export function foldLog(run, r) {
  if (r && r.status === 200) {
    const before = afterOf(run);
    feedText(run, typeof r.data === 'string' ? r.data : '');
    const now = afterOf(run);
    return { ok: true, fresh: now !== null && (before === null || now > before), sentence: '' };
  }
  const status = r ? r.status : 0;
  return { ok: false, fresh: false, gone: status === 404, stop: status === 404 || status === 413 || status === 400, sentence: said(r) };
}

// ---------------------------------------------------------------- answering a park
// The interpreter takes `stop`, `stop: <reason>`, the exact label of an option, or its number (0-based, as the prompt prints it); the perch
// takes 1 KiB with no control character (state.rs answer_ok). The text to send for the option at `index`: its number, but if another option's
// label is that very number (the exact label wins over the number) its own label instead. null when no text picks that option for sure.
export const STOP = 'stop';
export const ANSWER_MAX = 1024;
export function answerOk(text) {
  if (typeof text !== 'string' || text.trim() === '') return 'write an answer: the number of an option, its exact label, or stop';
  if (new TextEncoder().encode(text).length > ANSWER_MAX) return 'an answer is 1 KiB at most';
  if (/[\u0000-\u001f\u007f-\u009f]/.test(text)) return 'an answer has no control characters';
  return '';
}
export function answerPayload(index, labels) {
  if (!Array.isArray(labels) || !Number.isInteger(index) || index < 0 || index >= labels.length) return null;
  for (const text of [String(index), labels[index]]) {
    if (typeof text !== 'string' || answerOk(text)) continue;
    const c = classifyAnswer(text, labels); // the interpreter's own reading of it
    if ((c.kind === 'index' || c.kind === 'label') && c.index === index) return text;
  }
  return null;
}

// May the park of `row` (the perch's detail of the run) be answered now? -> { ok, why }. eidolon names a step it cannot replay (a journaled
// failure: F0 c, g, j); that sentence is the reason, whether the journal says so (`v.unresumable`) or a refused try already did (`row.refused`).
export function parkGate(row, v) {
  const state = row && row.state;
  if (state !== 'parked') return { ok: false, why: state === 'running' ? 'a process of that run is still going' : `the run is ${state || 'not known'}, not waiting for an answer` };
  if (v && v.unresumable && v.unresumable.reason) return { ok: false, why: v.unresumable.reason };
  if (row.refused) return { ok: false, why: row.refused };
  return { ok: true, why: '' };
}

// ---------------------------------------------------------------- the new-run form
// The fields come from the graph's `meta.jev.input` (a JSON schema): one text field per string parameter, and per integer or number. Anything
// else is listed in `skipped`; a required one of those means the form cannot start the graph.
export function fieldsOf(graph) {
  const input = own(own(own(graph, 'meta'), 'jev'), 'input');
  const props = own(input, 'properties');
  const required = Array.isArray(own(input, 'required')) ? input.required.filter((s) => typeof s === 'string') : [];
  const fields = [], skipped = [];
  if (isObj(props)) {
    for (const name of Object.keys(props)) {
      if (!/^[A-Za-z_][A-Za-z0-9_]{0,39}$/.test(name)) continue;
      const p = props[name];
      const type = isObj(p) && typeof p.type === 'string' ? p.type : 'string';
      const f = { name, type, required: required.includes(name), description: isObj(p) ? clean(p.description, 160) : '' };
      if (type === 'string' || type === 'integer' || type === 'number') fields.push(f); else skipped.push(f);
    }
  }
  return { fields, skipped };
}

// The args object for the fields as typed: an empty optional field is left out, an empty required one is `missing`, a number that is not
// one is `bad`.
export function argsOf(fields, values) {
  const args = {}, missing = [], bad = [];
  for (const f of fields) {
    const typed = isObj(values) && typeof values[f.name] === 'string' ? values[f.name].trim() : '';
    if (typed === '') { if (f.required) missing.push(f.name); continue; }
    if (f.type === 'string') args[f.name] = typed;
    else if (f.type === 'integer' ? /^-?\d{1,15}$/.test(typed) : /^-?\d{1,15}(\.\d{1,15})?$/.test(typed)) args[f.name] = Number(typed);
    else bad.push(f.name);
  }
  return { args, missing, bad };
}

// The one shared browser: the perch's own sentence for a second start, said here too when the list already shows the holder.
export const busySentence = (rid) => `run ${rid} is still going or parked, and there is one shared browser: finish it, cancel it or abandon it first`;
export const holderOf = (rows) => (Array.isArray(rows) ? rows.find((r) => isObj(r) && HOLDING.has(r.state) && ridOk(r.rid)) || null : null);

// May a run be started now? -> { ok, why }. The reasons are all about the form or the one shared browser. What jev's /health says
// (`decisions_log` or anything else), and whether a service is up, is deliberately not an input: the services line shows it, the perch
// judges a start and says why in its own sentence (a 503 names the start command).
export function runGate(form) {
  const f = form || {};
  if (f.starting) return { ok: false, why: 'starting…' };
  if (f.holder) return { ok: false, why: busySentence(f.holder.rid), holder: f.holder };
  if (!f.fields) return { ok: false, why: f.loading ? 'reading the graph…' : 'choose a graph' };
  const need = (f.skipped || []).find((s) => s.required);
  if (need) return { ok: false, why: `this graph needs ${need.name} (${need.type}), which this form cannot take` };
  const { missing, bad } = argsOf(f.fields, f.values);
  if (missing.length) return { ok: false, why: `write ${missing.join(', ')}` };
  if (bad.length) return { ok: false, why: `${bad.join(', ')} must be a number` };
  return { ok: true, why: '' };
}

// The form as the page holds it, through the gate: the graph chosen (its fields once read), the values typed for it, the action in flight and the
// holder of the browser. Those are the only inputs. The services and the decision log are not, and the page passes no health to this.
const valuesOf = (form) => (form && form.values && form.values[form.graph]) || {};
export function formGate({ chosen, form, acting, holder }) {
  const ch = chosen || null;
  return runGate({ fields: ch && ch.state === 'ok' ? ch.fields : null, skipped: ch ? ch.skipped : [], loading: !!ch && ch.state === 'loading',
    values: valuesOf(form), holder, starting: acting === 'start' });
}
// `[ run ]`: starts the form's graph when its gate is open and does nothing otherwise. -> the gate, as it was when asked.
export function startFromForm({ chosen, form, acting, holder }) {
  const gate = formGate({ chosen, form, acting, holder });
  if (gate.ok) startRun(form.graph, argsOf(chosen.fields, valuesOf(form)).args);
  return gate;
}

// A start or an action the perch refused: the sentence as it said it, and the holder when it is the one-at-a-time 409 ({run}).
export function refusalOf(r) {
  const holder = r && r.status === 409 && isObj(r.data) && ridOk(r.data.run) ? r.data.run : null;
  return { text: said(r), cls: 'o', holder };
}

// ================================================================ the state: signals, the polls, the actions
export const health = signal(null);    // the last answer of runs/health: { status, data, at, stale? } (stale: why a refresh failed, the good answer kept)
export const runList = signal(null);   // the last answer of runs: { rows, ok, sentence, at } (rows: the last good list)
export const graphList = signal(null); // the last answer of runs/graphs: { graphs, ok, sentence, at }
export const picked = signal(null);    // what the tab shows: null (nothing chosen), 'turns' (the chat's turn graph) or a run id
export const model = signal(null);     // the shown run as its last pass left it: { rid, v, row, logErr, detailErr, gone, at }
export const acting = signal('');      // the action in flight: 'start' | 'answer' | 'cancel' | 'abandon', else ''
export const notice = signal(null);    // what the perch refused to a person's action: { text, cls, holder }
export const form = signal({ open: false, graph: '', values: {} }); // the new-run form: values by graph name, then field
export const chosen = signal(null);    // the form's graph once read: { name, state: loading | ok | error, fields, skipped, sentence }

// The run the tab shows, or null. Reading it in a render subscribes.
export const currentRid = () => { const p = picked.value; return ridOk(p) ? p : null; };

const visible = () => typeof document === 'undefined' || document.visibilityState === 'visible';

// ---------------------------------------------------------------- graphs
const docs = new Map(); // name -> { sha256, doc }: the graph files as the perch gave them, used as the installed graph of a run
const cached = (name, sha) => { const hit = docs.get(name); return hit && (!sha || hit.sha256 === sha) ? hit.doc : null; };
export async function graphDoc(name, sha) {
  if (!graphOk(name)) return null;
  const hit = cached(name, sha);
  if (hit) return hit;
  const r = await hubGet(graphPath(name), 8000);
  if (r.status !== 200 || !isObj(r.data) || !isObj(r.data.graph)) return null;
  docs.set(name, { sha256: typeof r.data.sha256 === 'string' ? r.data.sha256 : '', doc: r.data.graph });
  return r.data.graph;
}

export async function loadGraphs() {
  const r = await hubGet('runs/graphs', 8000);
  const ok = r.status === 200 && Array.isArray(r.data);
  const graphs = ok ? r.data.filter((g) => isObj(g) && graphOk(g.name)) : [];
  graphList.value = { graphs, ok, sentence: ok ? '' : said(r), at: Date.now() };
  return graphList.peek();
}

// Choose the form's graph and read its file for the fields. A second choice made while the first is being read wins.
export async function chooseGraph(name) {
  form.value = { ...form.peek(), graph: name };
  if (!graphOk(name)) { chosen.value = null; return; }
  const sha = ((graphList.peek() || {}).graphs || []).find((g) => g.name === name);
  const have = cached(name, sha && sha.sha256);
  const shown = (doc) => (doc ? { name, state: 'ok', ...fieldsOf(doc), sentence: '' }
    : { name, state: 'error', fields: null, skipped: [], sentence: `the graph ${name} could not be read from the perch` });
  if (have) { chosen.value = shown(have); return; } // no flash of "reading" for a file already held
  chosen.value = { name, state: 'loading', fields: null, skipped: [], sentence: '' };
  const doc = await graphDoc(name, sha && sha.sha256);
  if (form.peek().graph === name) chosen.value = shown(doc);
}

export async function openForm() {
  form.value = { ...form.peek(), open: true };
  notice.value = null;
  const got = await loadGraphs();
  const cur = form.peek().graph;
  const names = got.graphs.map((g) => g.name);
  if (names.length) await chooseGraph(names.includes(cur) ? cur : names[0]); // read again: the file may have changed since
}
export const closeForm = () => { form.value = { ...form.peek(), open: false }; };
export function setField(graph, name, text) {
  const f = form.peek();
  form.value = { ...f, values: { ...f.values, [graph]: { ...(f.values[graph] || {}), [name]: text } } };
}

// ---------------------------------------------------------------- health and the list
export async function loadHealth() {
  const r = await hubPoll('runs/health', 4000);
  const was = health.peek();
  const good = was && was.status === 200 ? { ...was, stale: null } : null;
  health.value = r.status === 200 && isObj(r.data) ? { status: 200, data: r.data, at: Date.now(), stale: null }
    : good ? { ...good, stale: said(r) } : { status: r.status, data: r.data, at: Date.now(), stale: null };
}

// The list. `lane` is for the periodic read; the one a person's action wants is a plain call. The run that holds the shared browser, when
// nothing is chosen yet, becomes the chosen one (and stays: a run that ends is not swapped for the turn graph under the person's eyes).
export async function loadRuns(lane = true) {
  const r = lane ? await hubPoll('runs', 6000) : await hubGet('runs', 6000);
  const ok = r.status === 200 && Array.isArray(r.data);
  const prev = runList.peek();
  const rows = ok ? r.data.filter((x) => isObj(x) && ridOk(x.rid)) : prev ? prev.rows : [];
  runList.value = { rows, ok, sentence: ok ? '' : said(r), at: Date.now() };
  if (!ok) return;
  if (picked.peek() === null) { const h = holderOf(rows); if (h) picked.value = h.rid; }
  const n = notice.peek();
  if (n && n.holder && !rows.some((x) => x.rid === n.holder && HOLDING.has(x.state))) notice.value = null; // the holder it named has ended
}

// A run's row in the list follows its detail, so the list does not wait for the next 10 s to say a run ended.
function followRow(row) {
  const l = runList.peek();
  if (!l || !isObj(row) || !ridOk(row.rid)) return;
  const at = l.rows.findIndex((x) => x.rid === row.rid);
  if (at < 0) { loadRuns(true); return; }
  const fresh = Object.fromEntries(Object.entries(row).filter(([k]) => k !== 'out' && k !== 'err' && k !== 'question' && k !== 'report' && k !== 'refused'));
  if (!['state', 'ended_ms', 'why', 'stopping', 'resumes'].every((k) => l.rows[at][k] === fresh[k])) {
    runList.value = { ...l, rows: l.rows.map((x, i) => (i === at ? fresh : x)) };
  }
  const n = notice.peek();
  if (n && n.holder === row.rid && !HOLDING.has(row.state)) notice.value = null;
}

// ---------------------------------------------------------------- one run
const models = new Map(); // rid -> the page's hold on that run (least recently shown last); the reducer in it keeps the cursor
const MODELS_KEPT = 8;
function modelOf(rid) {
  let m = models.get(rid);
  if (m) models.delete(rid);
  else m = { rid, run: null, row: null, logErr: '', detailErr: '', gone: false, stopped: false, fails: 0 };
  models.set(rid, m);
  for (const k of models.keys()) { if (models.size <= MODELS_KEPT) break; if (k !== rid) models.delete(k); }
  return m;
}

function publish(m) {
  model.value = { rid: m.rid, v: m.run ? view(m.run) : null, row: m.row, logErr: m.logErr, detailErr: m.detailErr, gone: m.gone, at: Date.now() };
}

// One look at a run: its detail, then (the order matters: an end seen first means the log read after it is the whole of it) its log from the
// cursor. `io` is { json(path), text(path), graph(name) } so a test can answer for the perch. -> { next }: ms to the next look, null for never.
export async function pass(m, io) {
  const d = await io.json(detailPath(m.rid));
  if (d.status === 404) { m.gone = true; m.detailErr = 'the perch does not know this run'; return { next: null }; }
  if (d.status !== 200 || !isObj(d.data)) { m.fails++; m.detailErr = said(d); return { next: retryMs(RUNNING_MS, m.fails) }; }
  m.gone = false;
  m.detailErr = '';
  m.row = d.data;
  followRow(d.data);
  const state = typeof d.data.state === 'string' ? d.data.state : '';
  if (!m.run) {
    let doc = null;
    try { doc = await io.graph(d.data.graph); } catch { doc = null; } // no installed graph: the drawing then comes from the journal's own pages
    m.run = newRun({ graph: doc });
  }
  let asked = true;
  if (!m.stopped) {
    const f = foldLog(m.run, await io.text(logPath(m.rid, afterOf(m.run))));
    m.logErr = f.ok ? '' : f.sentence;
    if (f.stop) m.stopped = true;
    if (f.gone) m.gone = true;
    asked = f.ok || f.stop;
  }
  if (!asked) { m.fails++; return { next: retryMs(pollMs(state) ?? RUNNING_MS, m.fails) }; }
  m.fails = 0;
  return { next: m.gone ? null : pollMs(state) };
}

const live = {
  json: (path) => hubPoll(path, 6000),
  text: (path) => hubPollText(path, 16000), // eidolon may take 15 s over a large log, and the perch says so
  graph: (name) => graphDoc(name),
};

// Watch one run: a pass now, then one every `next` ms. The tab calls watch when it draws and unwatch when it goes.
let loop = null;
export function watch(rid) {
  if (loop && loop.rid === rid) return;
  unwatch();
  if (!ridOk(rid)) return;
  const m = modelOf(rid);
  publish(m);
  const me = loop = { rid, timer: 0, busy: false, again: false, dead: false };
  me.go = async () => {
    clearTimeout(me.timer);
    me.timer = 0;
    if (me.dead) return;
    if (me.busy) { me.again = true; return; }
    me.busy = true;
    let next;
    try {
      if (visible()) { next = (await pass(m, live)).next; if (loop === me) publish(m); } else next = null; // hidden: nothing is asked; poke() looks when it shows
    } catch { m.fails++; next = retryMs(RUNNING_MS, m.fails); } finally { me.busy = false; }
    if (me.dead) return;
    if (me.again) { me.again = false; next = 0; }
    if (next !== null) me.timer = setTimeout(me.go, next);
  };
  me.go();
}
export function unwatch() {
  if (!loop) return;
  loop.dead = true;
  clearTimeout(loop.timer);
  loop = null;
}
// Look now (after an action, or when the page comes back into view). A pass already going is followed by another.
export const poke = () => { if (loop) loop.go(); };

// ---------------------------------------------------------------- what a person does
// Each resolves { ok, sentence }. One at a time: a second click while one is going does nothing.
async function act(name, call) {
  if (acting.peek()) return { ok: false, sentence: 'another action is still going' };
  acting.value = name;
  notice.value = null;
  try { return await call(); } finally { acting.value = ''; }
}

export const startRun = (graph, args) => act('start', async () => {
  const r = await hubPost('runs', { graph, args: isObj(args) ? args : {} });
  if (r.status === 202 && isObj(r.data) && ridOk(r.data.rid)) {
    picked.value = r.data.rid;
    closeForm();
    loadRuns(false);
    return { ok: true, sentence: '', rid: r.data.rid };
  }
  const n = refusalOf(r);
  notice.value = n;
  if (n.holder) await loadRuns(false); // the list now shows the holder, and whether it is parked
  return { ok: false, sentence: n.text };
});

export const answerRun = (rid, text) => act('answer', async () => {
  if (!ridOk(rid)) return { ok: false, sentence: 'that is not a run' };
  const bad = answerOk(text);
  if (bad) return { ok: false, sentence: bad };
  const r = await hubPost(`runs/${rid}/answer`, { text });
  poke(); // running now, or whatever it is instead
  if (r.status === 202) return { ok: true, sentence: '' };
  notice.value = { text: said(r), cls: 'o', holder: null };
  return { ok: false, sentence: said(r) };
});

export const cancelRun = (rid) => act('cancel', async () => {
  if (!ridOk(rid)) return { ok: false, sentence: 'that is not a run' };
  const r = await hubPost(`runs/${rid}/cancel`);
  poke();
  if (r.status === 200) return { ok: true, sentence: '' };
  if (r.status === 202) return { ok: true, sentence: 'the cancel was sent and the run is still stopping' }; // the child outlived the perch's 9 s
  notice.value = { text: said(r), cls: 'o', holder: null };
  return { ok: false, sentence: said(r) };
});

export const abandonRun = (rid) => act('abandon', async () => {
  if (!ridOk(rid)) return { ok: false, sentence: 'that is not a run' };
  const r = await hubPost(`runs/${rid}/abandon`);
  poke();
  loadRuns(false);
  if (r.status === 200) return { ok: true, sentence: '' };
  notice.value = { text: said(r), cls: 'o', holder: null };
  return { ok: false, sentence: said(r) };
});

// ---------------------------------------------------------------- the tab
// What the tab asks: the services and the list (refresh), at open, every HEALTH_MS while the page is visible, and again when it comes back into
// view. Both wait their turn in the lane. Returns what closes it (the tab's effect cleanup); the shown run's own loop is watch/unwatch.
export function refresh() {
  loadHealth();
  loadRuns(true);
}
export function openTab() {
  if (visible()) refresh();
  const timer = setInterval(() => { if (visible()) refresh(); }, HEALTH_MS);
  const back = () => { if (visible()) { refresh(); poke(); } };
  document.addEventListener('visibilitychange', back);
  return () => { clearInterval(timer); document.removeEventListener('visibilitychange', back); };
}

// For tests: forget everything held.
export function resetRuns() {
  unwatch();
  models.clear();
  docs.clear();
  for (const s of [health, runList, graphList, picked, model, notice, chosen]) s.value = null;
  acting.value = '';
  form.value = { open: false, graph: '', values: {} };
}
