// jev's tools, drawn where they are called: in a chat and in a saved run (core/saved.js maps a workflow run's steps to tool rows, so a run
// is read the way a chat is). A renderer is the shape generic.js describes; this file has three of them and one component other folders import:
//
//   jev_choose   input: the options in a line.   output: ProbBars of {probs, best}, sorted, the pick marked, the margin under them
//   jev_entail   input: premise vs hypothesis.   output: per hypothesis, the three scores as ProbBars
//   jev_mark     input: where a run is (enter, park, final, end).   output: one line
//   ProbBars({ rows, floor?, margin?, pick?, max?, more?, head?, width?, cls? })   the bars; graph/ draws its decision steps with it
//
// Everything here comes from a model or a web page (an option is a link's text; a run's journal holds whatever the page said), so it is only
// ever drawn as a text node, cut short, and cleaned of what could make a line lie (control characters, bidi overrides, zero-width marks).
// The pure parts (clean, rowsOf, rank, barsModel, the three styles, parseChoose, parseEntail, markOf) are exported for tests/probbars.test.mjs.
import { html, K, useState } from '../core/ui.js';
import generic from './generic.js';

export const WIDTH = 16; // cells of a bar
export const TOP = 8;    // rows drawn before "… N more"

// ---------------------------------------------------------------- text
// Control characters become a space; zero-width and bidi marks (U+200B-200F, U+2028-202E, U+2060-206F, U+FEFF) go.
const CTRL = /[\u0000-\u001f\u007f-\u009f]/g;
const MARKS = /[\u200b-\u200f\u2028-\u202e\u2060-\u206f\ufeff]/g;
// One line of at most `n` code points (the last one `…` when cut). Anything that is not a string is taken as String(v), nothing as ''.
export function clean(v, n = 48) {
  const s = (typeof v === 'string' ? v : v == null ? '' : String(v)).replace(MARKS, '').replace(CTRL, ' ').replace(/ {2,}/g, ' ').trim();
  const cps = [...s];
  return cps.length > n ? (n > 1 ? cps.slice(0, n - 1).join('') : '') + '…' : s;
}

const num = (p) => (typeof p === 'number' && Number.isFinite(p) ? p : null);
const clamp01 = (p) => Math.min(1, Math.max(0, p));
export const fmt = (p) => p.toFixed(3);

// ---------------------------------------------------------------- the rows
// `probs` is {label: p} (jev answers an object). The labels of `order` (the call's options) come first in that order, then any other in the
// object's own order: JS puts integer-like keys first, so a tie between "2020" and "1999" would otherwise be broken by the engine, not by the
// options. A p that is not a finite number is no row. `Map` keeps a label like "__proto__" a label.
export function rowsOf(probs, order) {
  if (!probs || typeof probs !== 'object' || Array.isArray(probs)) return [];
  const have = new Map();
  for (const [label, p] of Object.entries(probs)) if (num(p) != null) have.set(label, p);
  const out = [];
  if (Array.isArray(order)) {
    for (const o of order) if (typeof o === 'string' && have.has(o)) { out.push({ label: o, p: have.get(o) }); have.delete(o); }
  }
  for (const [label, p] of have) out.push({ label, p });
  return out;
}

// Highest p first; equal p keep the order they came in (so the first of equals is the top, as an argmax picks it). Rows without a finite p and
// without a string label are dropped. The sort is by (p, position), not left to the engine's stability.
export function rank(rows) {
  const ok = (Array.isArray(rows) ? rows : []).filter((r) => r && typeof r.label === 'string' && num(r.p) != null);
  return ok.map((r, i) => ({ label: r.label, p: r.p, i })).sort((a, b) => b.p - a.p || a.i - b.i).map(({ label, p }) => ({ label, p }));
}

// Top minus runner-up of ranked rows; null with fewer than two.
export const marginOf = (ranked) => (ranked.length >= 2 ? ranked[0].p - ranked[1].p : null);

// ---------------------------------------------------------------- the bar
// A bar is not made of glyphs: the page's own font (VT323) has no block characters, so they would come from whatever monospace font the browser
// falls back to, a cell and a half wide, and a row of them would not be `width` cells. It is a box `width` cells long (--cw is the measured cell)
// with a filled box inside, as wide as p; the floor, when there is one, is a thin line in the same place in every row, so a bar that crosses it is
// over the floor. Every number in the style is ours, formatted here; no text from a model or a page ever reaches an attribute.
const pct = (f) => `${(clamp01(f) * 100).toFixed(2)}%`;
export const trackStyle = (width) => `display:inline-block;position:relative;vertical-align:middle;width:calc(${width} * var(--cw));height:calc(var(--ch) * 0.4);background:var(--line)`;
export const fillStyle = (frac, color) => `position:absolute;left:0;top:0;bottom:0;width:${pct(frac)};min-width:${frac > 0 ? '1px' : '0'};background:${color}`;
export const tickStyle = (frac) => `position:absolute;top:-30%;bottom:-30%;left:${pct(frac)};width:1px;background:var(--orange)`;

// What ProbBars draws, as data. `pick` is a label (the pick drawn is that row; without one, the top row, the first of equals). When the pick is
// not among the first `max` rows it is added after them, so a person's answer is never hidden by the cut. `frac` is p as a fraction of the bar
// (0 to 1); `color` is the bar's: the pick bright, the rest faint, a bar under the floor orange.
export function barsModel({ rows, floor, margin, pick, max = TOP, width = WIDTH } = {}) {
  const ranked = rank(rows);
  const top = ranked.length ? ranked[0].p : null;
  const at = typeof pick === 'string' ? ranked.findIndex((r) => r.label === pick) : -1;
  const picked = at >= 0 ? at : ranked.length ? 0 : -1;
  const cap = Number.isFinite(max) ? Math.max(1, Math.floor(max)) : ranked.length;
  const cells = Number.isInteger(width) && width >= 4 && width <= 64 ? width : WIDTH;
  const fl = num(floor) != null ? clamp01(floor) : null;
  const keep = ranked.map((_, i) => i < cap || i === picked);
  const lines = ranked.flatMap((r, i) => (keep[i] ? [{
    label: r.label, p: r.p, frac: clamp01(r.p), picked: i === picked, mark: i === picked ? '>' : ' ', ptext: fmt(r.p).padStart(5),
    color: fl != null && r.p < fl ? 'var(--orange)' : i === picked ? 'var(--bright)' : 'var(--faint)',
  }] : []));
  const m = marginOf(ranked), mt = num(margin);
  const parts = [];
  if (m != null) parts.push(`margin ${fmt(m)}${mt != null ? ` ${m >= mt ? '≥' : '<'} ${fmt(mt)}` : ''}`);
  if (fl != null && top != null) parts.push(top >= fl ? `floor ${fmt(fl)} (the tick)` : `top ${fmt(top)} < floor ${fmt(fl)} (the tick)`);
  const low = (m != null && mt != null && m < mt) || (fl != null && top != null && top < fl);
  return { lines, hidden: ranked.length - lines.length, total: ranked.length, top, margin: m, floor: fl, width: cells, foot: parts.join(' · '), low };
}

// ---------------------------------------------------------------- ProbBars
// One <pre>, a line per row: `> [bar] 0.512  label`, the pick bright (and marked with a > the way a tool line's -> is ASCII, so the columns stay
// one cell wide in the page's font), then `… N more` (a click when `more` is given), then the margin line (orange when it is under its bound, or
// the top under the floor). No hooks: a caller that wants "show all" holds the state (Choice below does) and passes `max` and `more`. `head` is a
// line above the rows. `cls` is the <pre>'s class: the default is the tool output's (a rule on the left, faint), graph/ passes its own.
export function ProbBars(props) {
  const { head, more, cls } = props || {};
  const m = barsModel(props || {});
  // The line ends are values, not template text: htm trims a newline off the edge of static text.
  const nl = '\n';
  const track = trackStyle(m.width);
  return html`<pre class=${cls ?? 'tout pbars'}>${head ? html`<span class="faint">${head + nl}</span>` : null}${
    m.lines.map((r) => html`<span class=${r.picked ? 'hi' : ''}>${r.mark + ' '}<span class="pbar" style=${track}><span style=${fillStyle(r.frac, r.color)}></span>${
      m.floor != null ? html`<span style=${tickStyle(m.floor)}></span>` : null}</span>${` ${r.ptext}  ${clean(r.label, 48)}\n`}</span>`)}${
    m.hidden > 0 ? (more ? html`<${K} cls="more" on=${more}>${`… ${m.hidden} more`}</${K}>${nl}` : html`<span class="faint">${`… ${m.hidden} more\n`}</span>`) : null}${
    m.foot ? html`<span class=${m.low ? 'o' : 'faint'}>${m.foot}</span>` : null}</pre>`;
}

// ---------------------------------------------------------------- what the tools send
const parseJson = (v) => { if (typeof v !== 'string') return v; try { return JSON.parse(v); } catch { return null; } };
const obj = (v) => (v && typeof v === 'object' && !Array.isArray(v) ? v : null);

// jev_choose's result `{"probs": {label: p}, "best": label}` (a string), the rows in the call's option order when `input` has them.
// null when it is not that (an error text, an older jev): the caller falls back to the generic output.
export function parseChoose(output, input) {
  const o = obj(parseJson(output));
  const rows = o ? rowsOf(obj(o.probs), input && input.options) : [];
  return rows.length ? { rows, best: typeof o.best === 'string' ? o.best : null } : null;
}

// jev_entail's `{"results": [{"label", "hypothesis", "scores": {neutral, entailment, contradiction}}]}`. Each result: the hypothesis, the
// service's label, the score rows in the fixed order entailment, neutral, contradiction (so a tie is broken the same way every time).
const CLASSES = ['entailment', 'neutral', 'contradiction'];
export function parseEntail(output) {
  const o = obj(parseJson(output));
  if (!o || !Array.isArray(o.results)) return null;
  const out = [];
  for (const r of o.results) {
    const x = obj(r), sc = x && obj(x.scores);
    const rows = sc ? rowsOf(sc, CLASSES) : [];
    if (rows.length) out.push({ hypothesis: typeof x.hypothesis === 'string' ? x.hypothesis : '', label: typeof x.label === 'string' ? x.label : null, rows });
  }
  return out.length ? out : null;
}

// jev_mark's input `{graph, kind, state, step, actions, outcome?}`: the facts of one mark, or null when it has no kind and state.
export function markOf(input) {
  const x = obj(input);
  if (!x || typeof x.kind !== 'string' || typeof x.state !== 'string') return null;
  const n = (v) => (Number.isInteger(v) && v >= 0 ? v : null);
  return { graph: typeof x.graph === 'string' ? x.graph : '', kind: x.kind, state: x.state, step: n(x.step), actions: n(x.actions), outcome: typeof x.outcome === 'string' ? x.outcome : '' };
}

// ---------------------------------------------------------------- jev_choose
const optionsOf = (input) => (input && Array.isArray(input.options) ? input.options.filter((o) => typeof o === 'string') : null);
// The wiki-hop graph's context starts `Target article: X` and `Current article: Y`: the page it is on and where it wants to get. Other
// contexts have no such header and get none.
function hop(context) {
  if (typeof context !== 'string') return '';
  const t = /^Target article: (.*)$/m.exec(context), c = /^Current article: (.*)$/m.exec(context);
  return t && c ? `${clean(c[1], 28)} → ${clean(t[1], 28)}` : '';
}

// The bars of one choice. Holds the one bit of state there is: whether the rest is shown.
function Choice({ rows, best }) {
  const [all, setAll] = useState(false);
  return html`<${ProbBars} rows=${rows} pick=${best} max=${all ? Infinity : TOP} more=${() => setAll(true)} />`;
}

const choose = {
  ...generic,
  input(input, ctx) {
    const o = optionsOf(input);
    if (!o) return generic.input(input, ctx);
    const head = hop(input.context);
    return `${head ? head + ' · ' : ''}${o.length} option${o.length === 1 ? '' : 's'}: ${o.slice(0, 6).map((x) => clean(x, 24)).join(', ')}${o.length > 6 ? ', …' : ''}`;
  },
  output(output, isError, ctx) {
    const r = isError ? null : parseChoose(output, ctx.input);
    return r ? ctx.html`<${Choice} rows=${r.rows} best=${r.best} />` : generic.output(output, isError, ctx);
  },
  // The bars are the point of the call: a row that can draw them is drawn open (renderers/index.js autoOpen).
  autoOpen: (output, isError) => !isError && parseChoose(output) != null,
};

// ---------------------------------------------------------------- jev_entail
const HYP = 6; // hypotheses drawn before "… N more"
const entail = {
  ...generic,
  input(input, ctx) {
    const hs = input && Array.isArray(input.hypotheses) ? input.hypotheses.filter((h) => typeof h === 'string') : null;
    if (!hs || !hs.length || typeof input.premise !== 'string') return generic.input(input, ctx);
    return `${clean(input.premise, 48)} vs ${clean(hs[0], 56)}${hs.length > 1 ? ` (+${hs.length - 1} more)` : ''}`;
  },
  output(output, isError, ctx) {
    const rs = isError ? null : parseEntail(output);
    if (!rs) return generic.output(output, isError, ctx);
    const more = rs.length - HYP;
    return ctx.html`<div>${rs.slice(0, HYP).map((r) => ctx.html`<${ProbBars} rows=${r.rows} pick=${r.label} max=${3} head=${clean(r.hypothesis, 72) || null} />`)}${
      more > 0 ? ctx.html`<pre class="tout"><span class="faint">… ${more} more hypotheses</span></pre>` : null}</div>`;
  },
  autoOpen: (output, isError) => !isError && parseEntail(output) != null,
};

// ---------------------------------------------------------------- jev_mark
// enter: a state was entered; park: the run waits on a person here; final: a final state (`failed` is one); end: it ended without one.
const KINDS = new Set(['enter', 'park', 'final', 'end']);
const steps = (m) => [m.step != null ? `step ${m.step}` : '', m.actions != null ? `${m.actions} action${m.actions === 1 ? '' : 's'}` : ''].filter(Boolean);
export function markLine(m) {
  const where = m.kind === 'end' ? (m.outcome ? `end ${clean(m.outcome, 24)}` : 'end') : `${clean(m.kind, 12)} ${clean(m.state, 40)}`;
  return [where, ...(m.kind === 'end' && m.state ? [clean(m.state, 40)] : []), ...steps(m)].join(' · ');
}
const mark = {
  ...generic,
  input(input, ctx) {
    const m = markOf(input);
    return m ? markLine(m) : generic.input(input, ctx);
  },
  // Said once more as a sentence, in the color of what it means: a park is waiting on someone, `failed` and an ended run did not arrive.
  output(output, isError, ctx) {
    const m = isError ? null : markOf(ctx.input);
    if (!m || !KINDS.has(m.kind)) return generic.output(output, isError, ctx);
    const st = clean(m.state, 40), at = m.step != null ? ` (step ${m.step})` : '';
    const [cls, text] = m.kind === 'park' ? ['o', `parked at ${st}${at}: waiting for a person`]
      : m.kind === 'final' ? [st.split('.').pop() === 'failed' ? 'r' : 'hi', `finished at ${st}${at}`]
      : m.kind === 'end' ? ['r', `ended${m.outcome ? ': ' + clean(m.outcome, 24) : ''}${at}`]
      : ['faint', `at ${st}${at}`];
    return ctx.html`<pre class="tout"><span class=${cls}>${text}</span></pre>`;
  },
};

export default { jev_choose: choose, jev_entail: entail, jev_mark: mark };
