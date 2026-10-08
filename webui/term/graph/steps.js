// The steps of the run shown: the last part of the run view. W2b-1 leaves this stub so the view has its slot; W2b-2 owns this file and replaces the
// body, not the name or the props. The run's model comes in as `v` (graph/jevrun.js view), the perch's detail of the run as `row`.
import { html } from '../core/ui.js';
import { clean } from '../renderers/jev.js';

export function Steps({ v }) {
  const steps = v ? v.steps : [];
  if (!steps.length) return null;
  const shown = steps.slice(-8);
  return html`<div class="gsteps">
    ${steps.length > shown.length ? html`<div class="faint">… ${steps.length - shown.length} earlier</div>` : null}
    ${shown.map((s) => html`<div class="gstep" key=${s.n}><span class="faint">${s.n} </span>${clean(s.state, 24)}${s.page && s.page.heading ? html`<span class="faint"> ${clean(s.page.heading, 40)}</span>` : null}</div>`)}
  </div>`;
}
