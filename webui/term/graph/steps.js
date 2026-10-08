// The run's steps: the foot of the run view (view.js mounts `Steps` with the run's model `v` and the perch's detail `row`). One frame per step, in
// the order the run took them: the page (its heading and url, when it began and how long it took), the arrival check (heading against the goal,
// the entailment against its threshold from the run's graph), the bars of the choice (ProbBars, the top 8, the rest behind `… N more`), jev's view
// of the choice (context.js, folded), the verdict (why the step went the way it did), each park as an inline ask (park.js) and the action (what
// was clicked and where it went). Then the end, from the run's last jev_mark (the door's WorkflowEnd when no jev_mark came): arrived, failed (the
// journal's own final, even when the door said completed), exhausted, stopped or cancelled, each with its reason. A run the perch ended while its
// journal still reads parked (cancelled or abandoned at a park) gets the perch's end instead (rowEnd). A run with no step gets its end line alone.
//
// The words are pure and tested (tests/steps.test.mjs): tookText, arrivalOf, verdictOf, actionOf, endOf, rowEnd, barsOf. Every string that came from
// a model, a page or a tool goes through `clean` and is drawn as a text node. The last SHOWN frames are drawn; `[ all N steps ]` shows the rest.
import { html, K, signal } from '../core/ui.js';
import { ProbBars, clean } from '../renderers/jev.js';
import { JevView } from './context.js';
import { ParkAsk } from './park.js';

export const SHOWN = 8; // frames drawn before `[ all N steps ]`
export const BARS = 8;  // bars drawn before `… N more`

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
const f2 = (p) => (typeof p === 'number' && Number.isFinite(p) ? p.toFixed(2) : '?');

const showAll = signal(false);   // every frame drawn, not the last SHOWN
const wide = signal({});         // step key -> true: every bar of that step drawn

// 24 ms, 640 ms, 1.5 s, 42 s, 2m 5s: how long a step took, or how far into the run it began.
export function tookText(ms) {
  if (typeof ms !== 'number' || !Number.isFinite(ms)) return '';
  const a = Math.max(0, ms);
  if (a < 1000) return `${Math.round(a)} ms`;
  if (a < 60000) return `${(a / 1000).toFixed(1)} s`;
  const s = Math.round(a / 1000);
  return `${Math.floor(s / 60)}m ${s % 60}s`;
}

// The arrival check, as parts: `heading X = goal` or `heading X ≠ goal` (the page's heading against the goal, exactly), then the entailment
// against its threshold (`entail 0.12 < 0.80`), or what made the check fail (`entail failed: …`). A check that never ran says nothing.
export function arrivalOf(a) {
  if (!isObj(a)) return [];
  const out = [];
  const eq = a.equals;
  if (isObj(eq) && typeof eq.goal === 'string' && typeof eq.heading === 'string') {
    const h = clean(eq.heading, 40), g = clean(eq.goal, 40);
    out.push(eq.hit === true ? { text: `heading ${h} = ${g}`, cls: 'up' } : { text: `heading ${h} ≠ ${g}`, cls: '' });
  }
  const e = a.entail;
  if (isObj(e)) {
    if (e.error) out.push({ text: `entail failed: ${clean(e.error, 120)}`, cls: 'r' });
    else if (typeof e.score === 'number') {
      const thr = typeof e.threshold === 'number' ? e.threshold : null;
      out.push(thr === null ? { text: `entail ${f2(e.score)}`, cls: '' }
        : { text: `entail ${f2(e.score)} ${e.hit ? '≥' : '<'} ${f2(thr)}`, cls: e.hit ? 'up' : '' });
    } else if (e.pending) out.push({ text: 'entail…', cls: 'faint' });
  }
  return out;
}

// Why the step went the way it did, in one line. A chooser that was sure picks by its scores; one that was not asks a person (a park follows).
// `ended` says the run has ended, so a chooser still pending will not answer.
export function verdictOf(s, ended = false) {
  const m = s && s.menu;
  if (!isObj(m)) return null;
  if (m.source === 'rule') return { text: 'picked by rule (label equals goal)', cls: '' };
  if (m.source === 'single') return { text: 'only one option', cls: '' };
  if (typeof m.unavailable === 'string') return { text: `chooser unavailable: ${clean(m.unavailable, 120)} → asked a person`, cls: 'o' };
  if (m.pending) return { text: ended ? 'jev_choose did not answer before the run ended' : 'waiting for jev_choose', cls: 'faint' };
  if (m.verdict === 'empty') return { text: 'the menu was empty → asked a person', cls: 'o' };
  if (typeof m.top1 !== 'number') return null;
  const margin = m.top1 - (typeof m.top2 === 'number' ? m.top2 : 0);
  const head = `top ${f2(m.top1)}` + (typeof m.floor === 'number' && typeof m.floorOk === 'boolean' ? ` ${m.floorOk ? '≥' : '<'} floor ${f2(m.floor)}` : '');
  const mid = `margin ${f2(margin)}` + (typeof m.margin === 'number' && typeof m.marginOk === 'boolean' ? ` ${m.marginOk ? '≥' : '<'} ${f2(m.margin)}` : '');
  if (typeof m.sure !== 'boolean') return { text: `${head} · ${mid}`, cls: '' };
  return m.sure ? { text: `${head} · ${mid} → picked by the scorer`, cls: 'up' } : { text: `${head} · ${mid} → asked a person`, cls: 'o' };
}

// The action, in words: a click names the link it clicked and where it went; anything else says what it did, and where.
export function actionOf(a) {
  if (!isObj(a)) return null;
  const clicked = typeof a.ref === 'string';
  const verb = clicked ? (typeof a.label === 'string' ? `clicked "${clean(a.label, 40)}"` : `clicked ${clean(a.ref, 12)}`)
    : clean(String(a.tool || 'action').replace(/^browser_/, ''), 16);
  if (a.error) return { text: `${verb} failed: ${clean(a.error, 160)}`, cls: 'r' };
  if (a.pending) return { text: `${verb}…`, cls: 'faint' };
  if (clicked && a.refOk === false) return { text: `clicked ${clean(a.ref, 12)}, which is not on the page`, cls: 'o' };
  const to = isObj(a.result) ? (typeof a.result.title === 'string' && a.result.title ? a.result.title : a.result.url) : null;
  return { text: typeof to === 'string' && to ? `${verb} → ${clean(to, 120)}` : verb, cls: '' };
}

const EXHAUSTED = { steps: 'decision steps', actions: 'actions', escalations: 'asks', visits: 'visits to a state' };

// The end, from the run's last jev_mark (view.end). `arrived` names the page and counts the decisions and the asks; a re-ask (an answer that was
// no option, or a second ask at the same park) is counted too. `failed` gives the journal's reason, with the door's word beside it when it said
// completed (exit 0 does not make a failed final a success).
export function endOf(v) {
  const e = v && v.end;
  if (!isObj(e)) return null;
  const reason = typeof e.reason === 'string' && e.reason ? clean(e.reason, 240) : '';
  switch (e.status) {
    case 'arrived': {
      const to = typeof e.title === 'string' && e.title ? `arrived at "${clean(e.title, 40)}"` : 'arrived';
      const reasks = Math.max(0, (e.escalations || 0) - (e.asked || 0));
      const text = `${to} in ${plural(e.decisions || 0, 'decision', 'decisions')}, ${e.asked || 0} asked${reasks ? `, ${reasks} re-asked` : ''}`;
      return { text, cls: 'up' };
    }
    case 'failed':
      return { text: `failed: ${reason || 'the graph ended in its failed state'}`, cls: 'r', note: e.door === 'Completed' ? 'the door said completed' : '' };
    case 'exhausted':
      return { text: `exhausted: out of ${EXHAUSTED[e.exhausted] || 'budget'}`, cls: 'o' };
    case 'stopped':
      return { text: `stopped: ${reason || 'stopped at a park'}`, cls: 'o' };
    case 'cancelled':
      return { text: `cancelled: ${reason || 'the page asked and the run was stopped'}`, cls: 'o' };
    default:
      return { text: clean(e.status, 24) || 'ended', cls: 'faint' };
  }
}

// The end of a run the journal does not end: the perch ended it while the journal still reads parked. A cancel that loses the race to the program
// leaves the journal parked (its in-flight tool came back cancelled: v.interrupted), and an abandon leaves it parked for good. The journal's own
// end (endOf) always wins; a run with neither gets no end line. `row` is the perch's row for the run (its state).
export function rowEnd(row, v) {
  if (endOf(v)) return null;
  const state = isObj(row) ? row.state : null;
  if (state === 'cancelled') return { text: v && v.interrupted ? 'cancelled: the run was cancelled while its tool was running' : 'cancelled: the run was cancelled', cls: 'o' };
  if (state === 'abandoned') return { text: 'abandoned: no answer was sent at this park', cls: 'o' };
  return null;
}

// The bars of a step's choice: its options with their p, and the pick (the scorer's best unless a person answered). null when the step has no
// scored choice (a rule, a single option, a chooser that did not answer).
export function barsOf(s) {
  const m = s && s.menu;
  if (!isObj(m) || m.source !== 'chooser' || !Array.isArray(m.options) || !m.options.length) return null;
  if (m.options.some((o) => typeof o.p !== 'number')) return null;
  const best = typeof m.best_index === 'number' && m.options[m.best_index] ? m.options[m.best_index].label : undefined;
  const pick = s.pick && (s.pick.how === 'chooser' || s.pick.how === 'answer') ? s.pick.label : best;
  return { rows: m.options.map((o) => ({ label: o.label, p: o.p })), pick, floor: typeof m.floor === 'number' ? m.floor : null,
    margin: typeof m.margin === 'number' ? m.margin : null };
}

// Parts joined with a dot, each with its class: the arrival line.
const Parts = ({ parts }) => parts.map((p, i) => html`${i ? html`<span class="faint"> · </span>` : null}<span class=${p.cls || null}>${p.text}</span>`);

// One step: its frame. The lines are as the plan orders them, and each is absent when it has nothing to say.
function Frame({ rid, s, v, row }) {
  const key = `${rid}/${s.n}`;
  const pg = isObj(s.page) ? s.page : null;
  const at = typeof s.ts_ms === 'number' && typeof v.meta.ts_ms === 'number' ? `+${tookText(s.ts_ms - v.meta.ts_ms)}` : '';
  const took = tookText(s.duration_ms);
  const head = pg && pg.heading ? clean(pg.heading, 60) : clean(s.state, 24);
  const arrival = arrivalOf(s.arrival);
  const verdict = verdictOf(s, !!v.end);
  const act = actionOf(s.action);
  const bars = barsOf(s);
  const isWide = !!wide.value[key];
  const toggleWide = () => { wide.value = { ...wide.peek(), [key]: !wide.peek()[key] }; };
  return html`<div>
    <div class="gline"><span class="faint">${s.n} </span><span class="hi">${head}</span>${at || took ? html` <span class="faint">${[at, took].filter(Boolean).join(' · ')}</span>` : null}</div>
    ${pg && pg.url ? html`<div class="gline faint">${clean(pg.url, 120)}</div>` : null}
    ${pg && pg.error ? html`<div class="gline r">page: ${clean(pg.error, 160)}</div>` : null}
    ${pg && pg.truncated ? html`<div class="gline o">page cut short: ${pg.truncated.got} of ${pg.truncated.declared} characters arrived</div>` : null}
    ${arrival.length ? html`<div class="gline"><${Parts} parts=${arrival} /></div>` : null}
    ${bars ? html`<div>
      <${ProbBars} rows=${bars.rows} pick=${bars.pick} floor=${bars.floor} margin=${bars.margin} max=${isWide ? Infinity : BARS} more=${toggleWide} />
      ${isWide ? html`<div class="gline"><${K} on=${toggleWide}>[ top ${BARS} ]</${K}></div>` : null}
    </div>` : null}
    ${s.jev ? html`<${JevView} id=${key} jev=${s.jev} />` : null}
    ${verdict ? html`<div class=${'gline ' + verdict.cls}>${verdict.text}</div>` : null}
    ${s.parks.map((p) => html`<${ParkAsk} key=${p.call_id} rid=${rid} park=${p} row=${row} v=${v}
      live=${!!(v.waiting && v.waiting.park && v.waiting.park.call_id === p.call_id)} />`)}
    ${act ? html`<div class=${'gline ' + act.cls}>${act.text}</div>` : null}
  </div>`;
}

// The run's steps and its end. A run with no step (failed, cancelled or abandoned before its first jev_mark enter) draws its end line alone, under
// no frame. A run with neither a step nor an end (one still going, or none read yet) draws nothing.
export function Steps({ rid, v, row }) {
  if (!v) return null;
  const steps = Array.isArray(v.steps) ? v.steps : [];
  const e = endOf(v) || rowEnd(row, v);
  if (!steps.length && !e) return null;
  const full = showAll.value;
  const shown = full || steps.length <= SHOWN ? steps : steps.slice(-SHOWN);
  const toggle = () => { showAll.value = !showAll.peek(); };
  return html`<div class="gsteps">
    ${steps.length > SHOWN ? html`<div class="gline"><${K} on=${toggle} title="the steps before the last ${SHOWN}">${full ? `[ last ${SHOWN} ]` : `[ all ${steps.length} steps ]`}</${K}></div>` : null}
    ${shown.map((s) => html`<${Frame} key=${s.n} rid=${rid} s=${s} v=${v} row=${row} />`)}
    ${e ? html`<div class=${'gline ' + e.cls}>${e.text}${e.note ? html`<span class="faint"> · ${e.note}</span>` : null}</div>` : null}
  </div>`;
}
