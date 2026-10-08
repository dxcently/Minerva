// The graph tab's run view, top to bottom: the services line, the runs list and `[ new run ]`, then for the run shown the drawing with its
// current state lit, a status sentence, the path, the budgets left, `[ cancel ]` (or `[ abandon ]` for a park), the model spend and the
// steps. index.js (the tab) holds the polls and hands this file what they found; runs.js has the state.
//
// The top half is words, pure and tested (tests/runs.test.mjs): the services line, a run's row, its status, path, budgets and spend. They return
// strings and plain objects, never markup. Every string that came from the perch or a service is run through `clean` (control characters, bidi
// overrides and zero-width marks out, cut short) and drawn as a text node: no innerHTML, and no remote word is a class name or a key.
//
//   servicesParts(resp)   the line under the tab's top: jev ● up · jevlike jev-base-v0.pt (weights present; loads on first call) · openjev …
//   rowLine(row, now)     one run of the list; statusParts(row, v, now), pathText, budgetParts, spendLine for the run shown
//
// The bottom half is the components. They take their data as props (no signals read, no hooks), so a test can call one and read the tree.
import { html, K } from '../core/ui.js';
import { openMenu, under } from '../core/menu.js';
import { clean } from '../renderers/jev.js';
import { drawGraph } from './draw.js';
import { said, HOLDING } from './runs.js';
import { Steps } from './steps.js'; // the run's steps (steps.js; W2b-2 fills it)

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

// ================================================================ words
// 12s, 3m, 2h, 4d: how long, from milliseconds.
export function span(ms) {
  const s = typeof ms === 'number' && Number.isFinite(ms) ? Math.max(0, ms) / 1000 : null;
  if (s === null) return '?';
  if (s < 60) return Math.floor(s) + 's';
  if (s < 3600) return Math.floor(s / 60) + 'm';
  if (s < 86400) return Math.floor(s / 3600) + 'h';
  return Math.floor(s / 86400) + 'd';
}

// ---------------------------------------------------------------- the services line
export const OPENJEV_LOAD = '~24 s'; // what loading openjev's weights cost when it was measured: said so a person is not surprised by the first call

// GET /hub/runs/health -> the parts of one line, in the order a person reads them:
//   jev ● up · jevlike jev-base-v0.pt (weights present; loads on first call) · openjev weights present (loads on first call, ~24 s) · decision log off · browser ● up
// { key, text, cls, detail }: `detail` marks the two that say what the models cost to load (the tab folds them away by default). A field an old
// jev lacks is said as unknown, never guessed; a service that is down shows its start command as text (nothing in the page starts one).
export function servicesParts(resp) {
  if (!resp) return [{ key: 'wait', text: 'services: asking the perch…', cls: 'faint', detail: false }];
  if (resp.status !== 200 || !isObj(resp.data)) return [{ key: 'unknown', text: `services: unknown (${said(resp)})`, cls: 'o', detail: false }];
  const jev = isObj(resp.data.jev) ? resp.data.jev : null, br = isObj(resp.data.browser) ? resp.data.browser : null;
  const parts = [];
  const lead = (key, label, s) => {
    if (!s) return { key, text: `${label}: unknown`, cls: 'faint', detail: false };
    if (!s.configured) return { key, text: `${label}: not configured`, cls: 'faint', detail: false };
    if (s.up) return { key, text: `${label} ● up`, cls: 'up', detail: false };
    const why = clean(s.why, 100), start = clean(s.start, 120);
    return { key, text: `${label} ○ down${why ? ` (${why})` : ''}${start ? `, start it: ${start}` : ''}`, cls: 'r', detail: false };
  };
  parts.push(lead('jev', 'jev', jev));
  if (jev && jev.configured && jev.up) {
    const ckpt = typeof jev.ckpt === 'string' && jev.ckpt ? clean(jev.ckpt, 80) : 'checkpoint: unknown';
    const loaded = isObj(jev.loaded) ? jev.loaded : {};
    const like = jev.jevlike === true ? (loaded.jevlike === true ? 'weights present; loaded' : loaded.jevlike === false ? 'weights present; loads on first call' : 'weights present')
      : jev.jevlike === false ? 'no weights at that path' : 'weights: unknown';
    parts.push({ key: 'jevlike', text: `jevlike ${ckpt} (${like})`, cls: jev.jevlike === false ? 'o' : '', detail: true });
    const dtype = typeof jev.openjev_dtype === 'string' && jev.openjev_dtype ? clean(jev.openjev_dtype, 16) : '';
    if (jev.openjev === true) {
      const state = loaded.openjev === true ? 'loaded' : loaded.openjev === false ? `loads on first call, ${OPENJEV_LOAD}` : '';
      const note = [dtype, state].filter(Boolean).join(', ');
      parts.push({ key: 'openjev', text: `openjev weights present${note ? ` (${note})` : ''}`, cls: '', detail: true });
    } else if (jev.openjev === false) parts.push({ key: 'openjev', text: 'openjev: no weights found', cls: 'o', detail: true });
    else parts.push({ key: 'openjev', text: 'openjev: weights unknown', cls: 'faint', detail: true });
  }
  // The decision log. Shown, never a gate (operator ruling 2026-10-07): a run starts whatever this says, and `[ run ]` stays enabled for each.
  //   'off' -> off; a path -> on, with the path; a missing field (an old jev, or no jev answer) -> unknown.
  const log = jev ? jev.decisions_log : undefined;
  if (log === 'off') parts.push({ key: 'log', text: 'decision log off', cls: '', detail: false });
  else if (typeof log === 'string' && log.trim() !== '') parts.push({ key: 'log', text: `decision log on: ${clean(log, 200)}`, cls: 'o', detail: false });
  else parts.push({ key: 'log', text: 'decision log: unknown', cls: 'faint', detail: false });
  parts.push(lead('browser', 'browser', br));
  if (resp.stale) parts.push({ key: 'stale', text: `(not refreshed: ${resp.stale})`, cls: 'faint', detail: false });
  return parts;
}
export const servicesText = (resp) => servicesParts(resp).map((p) => p.text).join(' · ');

// ---------------------------------------------------------------- a run in the list
const STATE_CLS = { running: 'hi', parked: 'o', done: 'up', failed: 'r', cancelled: 'o', abandoned: 'faint', refused: 'r', lost: 'r' };

// What the run was started with, in a few words: `start Ancient_Egypt, goal Felidae`.
export function argsText(args) {
  if (!isObj(args)) return '';
  return Object.entries(args).slice(0, 3).map(([k, v]) => `${clean(k, 16)} ${clean(typeof v === 'string' ? v : JSON.stringify(v), 28)}`).join(', ');
}

// One run of the list: { rid, graph, state, cls, when, title }. `when` is how long it has been going, or how long ago it ended.
export function rowLine(row, now) {
  const state = typeof row.state === 'string' ? clean(row.state, 16) : '?';
  const going = HOLDING.has(row.state);
  const when = going ? span(now - row.started_ms) : typeof row.ended_ms === 'number' ? `${span(now - row.ended_ms)} ago` : '';
  const why = typeof row.why === 'string' && row.why ? clean(row.why, 120) : '';
  return { rid: row.rid, graph: clean(row.graph, 40), state, cls: Object.hasOwn(STATE_CLS, row.state) ? STATE_CLS[row.state] : 'faint', when,
    title: [argsText(row.args), why].filter(Boolean).join(' · ') };
}

// ---------------------------------------------------------------- the run shown
// The perch's state in words, with its reason. A cancelled run is `cancelled`, never `failed`, whatever eidolon said on its way out (it says
// failed or parked for the two ways a SIGTERM can end; the perch knows the page asked). `v` is the run model (graph/jevrun.js view) or null.
export function statusParts(row, v, now = Date.now()) {
  if (!isObj(row)) return [{ text: 'state unknown', cls: 'faint' }];
  const out = [];
  const why = typeof row.why === 'string' && row.why ? clean(row.why, 240) : '';
  switch (row.state) {
    case 'running': {
      out.push({ text: 'running', cls: 'hi' });
      if (typeof row.started_ms === 'number') out.push({ text: ` ${span(now - row.started_ms)}`, cls: 'faint' });
      if (row.stopping) out.push({ text: ' · stopping: the cancel was sent', cls: 'o' });
      else if (v && v.active) out.push({ text: ` · in ${clean(v.active, 40)}`, cls: '' });
      break;
    }
    case 'parked': {
      out.push({ text: 'parked: waiting for an answer', cls: 'o' });
      const w = v && v.waiting && v.waiting.park && v.waiting.park.why ? clean(v.waiting.park.why, 120) : '';
      if (w) out.push({ text: ` · ${w}`, cls: '' });
      if (typeof row.refused === 'string' && row.refused) out.push({ text: ` · eidolon would not resume it: ${clean(row.refused, 240)}`, cls: 'r' });
      break;
    }
    case 'done': {
      out.push({ text: 'done', cls: 'up' });
      const s = v && v.status;
      if (s === 'arrived') out.push({ text: ' · arrived', cls: '' });
      else if (s === 'failed') out.push({ text: ' · the graph ended in its failed state', cls: 'r' }); // the door says completed; the journal says otherwise
      else if (s === 'stopped') out.push({ text: ' · stopped at a park', cls: 'o' });
      else if (s === 'exhausted') out.push({ text: ` · ran out of ${v.end && v.end.exhausted ? clean(v.end.exhausted, 20) : 'budget'}`, cls: 'o' });
      break;
    }
    case 'failed': out.push({ text: 'failed', cls: 'r' }, ...(why ? [{ text: `: ${why}`, cls: '' }] : [])); break;
    case 'cancelled': out.push({ text: 'cancelled', cls: 'o' }, { text: ` · ${why || 'the page asked and the run was stopped'}`, cls: '' }); break;
    case 'abandoned': out.push({ text: 'abandoned', cls: 'faint' }, { text: ' · a parked run given up; nothing was started', cls: '' }); break;
    case 'refused': out.push({ text: 'refused', cls: 'r' }, ...(why ? [{ text: `: ${why}`, cls: '' }] : [])); break;
    case 'lost': out.push({ text: 'lost', cls: 'r' }, ...(why ? [{ text: `: ${why}`, cls: '' }] : [])); break;
    default: out.push({ text: clean(row.state, 24) || 'state unknown', cls: 'faint' });
  }
  return out;
}

// The pages the run has been through, `Cat → Felidae → …` (the dots while it is still going). Long paths keep their last few.
export function pathText(path, going, keep = 5) {
  const xs = (Array.isArray(path) ? path : []).filter((p) => typeof p === 'string' && p).map((p) => clean(p, 28));
  if (!xs.length) return '';
  const shown = xs.length > keep + 1 ? ['…', ...xs.slice(-keep)] : xs;
  return shown.join(' → ') + (going ? ' → …' : '');
}

// What is left of each budget (the graph's own three, then eidolon's cap on steps): { label, left, cap, text, cls }.
export function budgetParts(budgets) {
  if (!isObj(budgets)) return [];
  const names = [['decisions', 'decisions'], ['actions', 'actions'], ['escalations', 'asks'], ['host', 'host steps']];
  return names.filter(([k]) => isObj(budgets[k])).map(([k, label]) => {
    const b = budgets[k];
    const text = b.left === null ? `${label} ${b.used} used` : `${label} ${b.left} left`;
    const cls = b.cap === null ? 'faint' : b.left === 0 ? 'r' : b.left <= b.cap * 0.2 ? 'o' : '';
    return { label, left: b.left, cap: b.cap, used: b.used, text, cls, title: b.cap === null ? `${b.used} used` : `${b.used} of ${b.cap} used` };
  });
}

// Spend on a model: a run on the mock provider has none, so any is a surprise and is red.
export function spendLine(spend) {
  const n = Array.isArray(spend) ? spend.length : 0;
  return n ? { text: `model spend: ${plural(n, 'record', 'records')}, none was expected`, cls: 'r' } : { text: 'model spend: none', cls: 'faint' };
}

// ================================================================ components
export const LIST_ROWS = 4; // runs shown before `[ all N ]`

const Text = ({ parts }) => parts.map((p, i) => html`<span class=${p.cls || null} title=${p.title || null} key=${i}>${p.text}</span>`);
const Dotted = ({ parts }) => parts.map((p, i) => html`${i ? html`<span class="faint"> · </span>` : null}<span class=${p.cls || null} title=${p.title || null} key=${p.key || i}>${p.text}</span>`);

// The services line. The two lines about the models' weights are folded away until `[ details ]` (the header stays short in a narrow panel).
export function Services({ resp, open, onToggle }) {
  const parts = servicesParts(resp);
  const lead = parts.filter((p) => !p.detail), more = parts.filter((p) => p.detail);
  return html`<div class="gsvc">
    <div class="gline"><${Dotted} parts=${lead} />${more.length ? html` <${K} on=${onToggle} title="how the models stand: weights present, loaded or not">${open ? '[ less ]' : '[ details ]'}</${K}>` : null}</div>
    ${open && more.map((p) => html`<div class=${'gline gdet ' + p.cls} key=${p.key}>${p.text}</div>`)}
  </div>`;
}

// The runs, newest first; the one shown is marked. A click shows that run.
export function RunList({ rows, shown, now, all, onPick, onAll }) {
  if (!rows.length) return html`<div class="faint">no runs yet</div>`;
  const seen = all ? rows : rows.slice(0, LIST_ROWS);
  return html`<div class="glist">
    ${seen.map((row) => {
      const r = rowLine(row, now);
      return html`<${K} on=${() => onPick(r.rid)} cls=${'grow-row' + (r.rid === shown ? ' gsel' : '')} title=${r.title || 'show this run'} key=${r.rid}>
        <span class="gmark">${r.rid === shown ? '▸' : ' '}</span><span class="gid">${r.rid}</span><span class="grow">${r.graph}</span>
        <span class=${r.cls}>${r.state}</span><span class="faint gwhen">${r.when}</span>
      </${K}>`;
    })}
    ${rows.length > LIST_ROWS ? html`<div><${K} on=${onAll}>${all ? '[ fewer ]' : `[ all ${rows.length} ]`}</${K}></div>` : null}
  </div>`;
}

// The new-run form: the graph (a menu of what the perch offers), one text field per parameter the graph takes, and `[ run ]`.
// `gate` is formGate's answer (greyed for the form, a holder, or an action in flight); what the services say is not part of it.
export function NewRun({ list, form, chosen, gate, rows, busy, onPickGraph, onField, onRun, onClose, onAbandon }) {
  const names = list && list.ok ? list.graphs.map((g) => g.name) : [];
  const ready = chosen && chosen.state === 'ok' ? chosen : null;
  const values = (form.values && form.values[form.graph]) || {};
  const key = (e) => {
    if (e.key === 'Enter') { e.preventDefault(); if (gate.ok) onRun(); } else if (e.key === 'Escape') { e.preventDefault(); onClose(); }
  };
  const menu = (e) => openMenu(names.map((n) => ({ label: n, tick: n === form.graph, on: () => onPickGraph(n) })), under(e.currentTarget), { title: 'graph', owner: 'run-graph' });
  const holder = gate.holder ? (rows || []).find((r) => r.rid === gate.holder.rid) : null;
  let focused = false;
  return html`<div class="gform">
    <div class="gline"><span class="hi">new run</span> <${K} on=${onClose}>[ close ]</${K}></div>
    ${!list ? html`<div class="faint">asking the perch for its graphs…</div>`
      : !list.ok ? html`<div class="o">${list.sentence}</div>`
      : !names.length ? html`<div class="faint">this perch offers no graph: it was started without --run-graphs</div>`
      : html`<div class="gfield"><span class="faint gk">graph</span><span data-owner="run-graph"><${K} on=${menu} title="the graphs this perch offers">[ ${form.graph || 'choose'} ▾ ]</${K}></span></div>
        ${chosen && chosen.state === 'loading' ? html`<div class="faint">reading the graph…</div>` : null}
        ${chosen && chosen.state === 'error' ? html`<div class="o">${chosen.sentence}</div>` : null}
        ${ready && ready.fields.map((f) => {
          const first = !focused && !(values[f.name] || '');
          if (first) focused = true;
          return html`<label class="gfield" key=${f.name} title=${f.description || null}><span class="faint gk">${f.name}</span>
            <input type="text" spellcheck="false" autocomplete="off" maxlength="512" aria-label=${f.name} placeholder=${f.required ? 'required' : 'optional'}
              value=${values[f.name] || ''} onInput=${(e) => onField(f.name, e.currentTarget.value)} onKeyDown=${key}
              ref=${(el) => { if (el && !el.dataset.fx) { el.dataset.fx = '1'; if (first) el.focus(); } }} /></label>`;
        })}
        ${ready && ready.skipped.length ? html`<div class="faint">not set from here: ${ready.skipped.map((s) => `${s.name} (${s.type})`).join(', ')}</div>` : null}`}
    <div class="gact"><${K} on=${onRun} off=${!gate.ok || busy} title=${gate.ok ? 'start this run' : gate.why}>[ run ]</${K}></div>
    ${gate.holder ? html`<div class="o gline">${gate.why}</div>
        ${holder && holder.state === 'parked' ? html`<div class="gact"><${K} on=${() => onAbandon(holder.rid)} off=${busy} title="give the parked run up; nothing is started">[ abandon ]</${K}> <span class="faint">${holder.rid}</span></div>` : null}`
      : !gate.ok && gate.why && gate.why !== 'choose a graph' ? html`<div class="faint gline">${gate.why}</div>` : null}
  </div>`;
}

// The run shown: its drawing with the current state lit, then the words. `m` is the model signal's value for this run (or null before the
// first answer), `listed` the run's row from the list (to draw something at once), `acting` the action in flight.
export function RunView({ rid, m, listed, now, acting, onCancel, onAbandon }) {
  const row = m && m.row ? m.row : listed;
  if (!row) return html`<div class="faint">${m && m.gone ? m.detailErr : m && m.detailErr ? `the run could not be read: ${m.detailErr}` : 'reading the run…'}</div>`;
  const v = m ? m.v : null;
  const going = row.state === 'running';
  const stopping = going && !!row.stopping;
  const path = v ? pathText(v.path, going) : '';
  const budgets = v ? budgetParts(v.budgets) : [];
  const args = argsText(row.args);
  const g = v ? v.graph : null;
  return html`<div class="grunv">
    <div class="gline"><span class="hi">${rid}</span> ${clean(row.graph, 40)}${args ? html` <span class="faint">${args}</span>` : null}</div>
    ${v && v.drawing ? html`<div class="gwrap">${drawGraph(v.drawing, v.marks)}</div>`
      : v ? html`<div class="faint gline">${g && g.error ? g.error : 'no graph to draw: it is not installed on this perch and the journal holds no page of it yet'}</div>` : null}
    ${g && g.label ? html`<div class="faint gline">${g.label}</div>` : null}
    <div class="gline gstat"><${Text} parts=${statusParts(row, v, now)} /></div>
    ${path ? html`<div class="gline"><span class="faint">path </span>${path}</div>` : null}
    ${budgets.length ? html`<div class="gline"><span class="faint">left </span><${Dotted} parts=${budgets} /></div>` : null}
    ${going ? html`<div class="gact"><${K} on=${onCancel} off=${stopping || acting === 'cancel'} title="stop the run; its journal is kept">${stopping || acting === 'cancel' ? '[ cancelling… ]' : '[ cancel ]'}</${K}></div>` : null}
    ${row.state === 'parked' ? html`<div class="gact"><${K} on=${onAbandon} off=${!!acting} title="give the parked run up; nothing is started">[ abandon ]</${K}></div>` : null}
    ${v ? html`<div class=${'gline ' + spendLine(v.spend).cls}>${spendLine(v.spend).text}</div>` : null}
    ${m && m.detailErr && !m.gone ? html`<div class="o gline">the run could not be read: ${m.detailErr}</div>` : null}
    ${m && m.logErr ? html`<div class="o gline">its log could not be read: ${m.logErr}</div>` : null}
    <${Steps} rid=${rid} v=${v} row=${row} />
  </div>`;
}
