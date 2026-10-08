// The graph tab. Under the perch it is the jev runs (graph/runs.js holds them, graph/view.js draws them): a services line, the runs, `[ new run ]`,
// and under them the run shown (its drawing with the current state lit, its status, path and budgets, its steps) or, with no run shown, the
// chat's turn graph. Under a bare door there are no runs and the tab is the turn graph alone. It gets { s, sid } like every tab.
//
// What polls, and when: the tab asks for the services and the runs when it opens and every 10 s while the page is visible, and watches the run
// shown (runs.js: every 1 s while it runs, every 5 s while it is parked, never once it has ended). Closing the tab stops all of it; what was
// read is kept, so the tab draws at once when it opens again.
import { html, K, useEffect, signal } from '../core/ui.js';
import { mode } from '../core/api.js';
import { known } from '../core/hub.js';
import * as R from './runs.js';
import { Services, RunList, NewRun, RunView } from './view.js';
import { TurnGraph } from './turns.js'; // the chat's turn graph (turns.js; W3 fills it), { s, sid } like every tab

const details = signal(false); // the services line's `[ details ]`
const allRuns = signal(false); // the list's `[ all N ]`

function RunsPane({ s, sid }) {
  const rid = R.currentRid();
  useEffect(() => R.openTab(), []);
  useEffect(() => { R.watch(rid); return R.unwatch; }, [rid]);

  const list = R.runList.value, resp = R.health.value, form = R.form.value, notice = R.notice.value, m = R.model.value, acting = R.acting.value;
  const ch = R.chosen.value, graphs = R.graphList.value;
  const rows = list ? list.rows : [], now = Date.now();
  const holder = R.holderOf(rows);
  const gate = R.formGate({ chosen: ch, form, acting, holder });
  const run = () => R.startFromForm({ chosen: ch, form, acting, holder });

  return html`<div class="grun">
    <${Services} resp=${resp} open=${details.value} onToggle=${() => { details.value = !details.peek(); }} />
    <div class="gline gbar"><span class="hi">runs</span>
      ${list && !list.ok ? null : html` <${K} on=${() => (form.open ? R.closeForm() : R.openForm())} title="start a jev run on this perch">[ new run ]</${K}>`}
      ${rid ? html` <${K} on=${() => { R.picked.value = 'turns'; }} title="the chat's turn graph instead of the run">[ turn graph ]</${K}>` : null}</div>
    ${list && !list.ok ? html`<div class="o gline">the runs could not be read: ${list.sentence}</div>` : null}
    ${form.open ? html`<${NewRun} list=${graphs} form=${form} chosen=${ch} gate=${gate} rows=${rows} busy=${!!acting}
      onPickGraph=${R.chooseGraph} onField=${(name, text) => R.setField(form.graph, name, text)} onRun=${run} onClose=${R.closeForm} onAbandon=${R.abandonRun} />` : null}
    ${notice && !(form.open && holder) ? html`<div class=${'gline ' + notice.cls}>${notice.text}</div>` : null}
    <${RunList} rows=${rows} shown=${rid} now=${now} all=${allRuns.value} onPick=${(r) => { R.picked.value = r; }} onAll=${() => { allRuns.value = !allRuns.peek(); }} />
    ${rid ? html`<${RunView} rid=${rid} m=${m && m.rid === rid ? m : null} listed=${rows.find((r) => r.rid === rid)} now=${now} acting=${acting}
      onCancel=${() => R.cancelRun(rid)} onAbandon=${() => R.abandonRun(rid)} />`
      : html`<${TurnGraph} s=${s} sid=${sid} />`}
  </div>`;
}

export function GraphTab(props) {
  known.value; // redraw once the mode is known
  return mode() === 'perch' ? html`<${RunsPane} ...${props} />` : html`<${TurnGraph} ...${props} />`;
}
