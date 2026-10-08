// jev's view of one choice, folded under `[ jev's view ]` in the step that chose (graph/steps.js mounts one per step with a jev_choose). It shows
// what jevlike reads: the first 192 bytes of the context and the first 32 bytes of each option (CUT in graph/jevrun.js, from interp.rn). Options
// that agree in those bytes are one to jevlike, so each is flagged `tie after the cut: margin 0`: the margin between them is 0 whatever the
// scorer answers.
//
// The words are pure and tested (tests/steps.test.mjs): viewOf (the cut lines and the flags) and contextLine. Text from the model is run through
// `clean` and drawn as a text node. The fold is the one signal here; it is kept per step, so a poll does not close what a person opened.
import { html, K, signal } from '../core/ui.js';
import { clean } from '../renderers/jev.js';
import { CUT } from './jevrun.js';

export const TIE = 'tie after the cut: margin 0';
const opened = signal({}); // step key -> true while its view is unfolded

// The view of one choice, from the step's `jev` (jevrun.js): the context and each option as jevlike reads them, and which options tie.
// null when the step has no choice.
export function viewOf(jev) {
  if (!jev || typeof jev !== 'object') return null;
  const cuts = Array.isArray(jev.options_cut) ? jev.options_cut : [];
  const clipped = Array.isArray(jev.options_clipped) ? jev.options_clipped : [];
  const tied = new Set();
  for (const pair of Array.isArray(jev.collisions) ? jev.collisions : []) {
    if (Array.isArray(pair)) for (const i of pair) tied.add(i);
  }
  return {
    context: { text: clean(jev.context_cut, 400), bytes: typeof jev.context_bytes === 'number' ? jev.context_bytes : null, clipped: !!jev.context_clipped },
    options: cuts.map((t, i) => ({ i, text: clean(t, 40), clipped: !!clipped[i], tie: tied.has(i) })),
    ties: tied.size,
  };
}

// The line above the context: how much of it jevlike reads, e.g. `the first 192 of 216 bytes`, or the whole of it.
export function contextLine(ctx) {
  const bytes = ctx.bytes;
  if (ctx.clipped && bytes !== null) return `the context as jevlike reads it: the first ${CUT.context} of ${bytes} bytes`;
  return `the context as jevlike reads it: all ${bytes === null ? '?' : bytes} bytes`;
}

export function JevView({ id, jev }) {
  const v = viewOf(jev);
  if (!v) return null;
  const on = !!opened.value[id];
  const toggle = () => { opened.value = { ...opened.peek(), [id]: !opened.peek()[id] }; };
  return html`<div class="gjev">
    <div class="gline"><${K} on=${toggle} title="what jevlike reads of this choice: the context and each option, cut as it cuts them">${on ? "[ hide jev's view ]" : "[ jev's view ]"}</${K}></div>
    ${on ? html`<div class="gline faint">${contextLine(v.context)}</div>
      <div class="gline gctx">${v.context.text}${v.context.clipped ? '…' : ''}</div>
      ${v.options.map((o) => html`<div class="gline" key=${o.i}><span class="faint">${o.i} </span>${o.text}${o.clipped ? '…' : ''}${o.tie ? html`<span class="o">  ${TIE}</span>` : null}</div>`)}`
      : null}
  </div>`;
}
