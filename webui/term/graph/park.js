// A park as an inline ask, under the step that parked (graph/steps.js mounts one per park). The question and why, the numbered options with their p
// (0-based, as the prompt prints them), a click selects one, `[ answer ]` sends it and `[ stop ]` ends the run. Only the park the run waits on is
// live; an answered one says what its answer did. Both buttons are disabled with the reason: eidolon's own when the run cannot resume (a journaled
// failure, a refused try) or a child runs (the run is not parked, runs.js parkGate); a person's when nothing is picked, an answer is in flight,
// or the park has no option, or no text can name the option (answerPayload).
//
// The words are pure and tested (tests/steps.test.mjs): askGate, stopGate, answeredLine, optionsOf. Answers go through runs.js answerRun, the one
// place that sends one; this file never builds a request.
import { html, K, signal } from '../core/ui.js';
import { clean, fmt } from '../renderers/jev.js';
import { answerRun, answerPayload, parkGate, acting, STOP } from './runs.js';

const picks = signal({}); // `rid/call_id` -> the 0-based index clicked (kept while the page is open, so a poll does not lose it)

// The options as drawn: the index, p to three places (a dash where the chooser did not score), the label cut short.
export function optionsOf(park) {
  return park.options.map((o, i) => ({ i, p: typeof o.p === 'number' ? fmt(o.p) : '—', label: clean(o.label, 48) }));
}

// Whether the run lets a stop or an answer go at all: parkGate first (the run's own reason), then a person's action in flight.
export function stopGate({ row, v, busy }) {
  const g = parkGate(row, v);
  if (!g.ok) return { ok: false, why: g.why, run: true };
  if (busy) return { ok: false, why: 'an action is still going', run: false };
  return { ok: true, why: '', run: false };
}

// May the option at `index` be sent now? -> { ok, why, run }: `run` is true when the reason is the run's (eidolon's sentence), false when it is the
// person's to fix (pick an option first). The option must be nameable: answerPayload gives a text that picks it and no other.
// A park with no options has none to name, so its gate says so before the index is asked for; stop is its only answer.
export function askGate({ row, v, busy, index, labels }) {
  const s = stopGate({ row, v, busy });
  if (!s.ok) return s;
  if (labels.length === 0) return { ok: false, why: 'this park has no options: stop is the only answer', run: false };
  if (typeof index !== 'number') return { ok: false, why: 'select an option first', run: false };
  if (answerPayload(index, labels) === null) return { ok: false, why: 'no text can name this option; its number and label name others', run: false };
  return { ok: true, why: '', run: false };
}

// What an answer did, in a line: the option it picked, the stop it made, or an answer that was no option (the prompt asks again).
export function answeredLine(park) {
  if (typeof park.answer !== 'string') return null;
  const said = clean(park.answer, 40);
  if (park.kind === 'stop') return { text: `answered stop${park.stop ? `: ${clean(park.stop, 120)}` : ''}`, cls: 'o' };
  if (park.kind === 'invalid') return { text: `answered "${said}": not one of the options, asked again`, cls: 'o' };
  const at = typeof park.picked === 'number' && park.options[park.picked] ? clean(park.options[park.picked].label, 48) : '';
  return { text: `answered ${said}${at ? `: ${at}` : ''}`, cls: '' };
}

const pick = (key, i) => { picks.value = { ...picks.peek(), [key]: i }; };

export function ParkAsk({ rid, park, live, row, v }) {
  const done = answeredLine(park);
  if (!live) {
    return html`<div class="gask">
      <div class="gline faint">${clean(park.ask || park.why || 'asked', 160)}</div>
      <div class=${'gline ' + (done ? done.cls : 'faint')}>${done ? done.text : 'not answered'}</div>
    </div>`;
  }
  const key = `${rid}/${park.call_id}`;
  const labels = park.options.map((o) => o.label);
  const sel = picks.value[key];
  const index = typeof sel === 'number' && sel < labels.length ? sel : null;
  const busy = !!acting.value;
  const gate = askGate({ row, v, busy, index, labels });
  const stop = stopGate({ row, v, busy }); // askGate asks it first, so one reason line (the gate's) says why either button is off
  return html`<div class="gask">
    <div class="gline hi">${clean(park.ask || 'Which option?', 240)}</div>
    ${park.why ? html`<div class="gline faint">${clean(park.why, 160)}</div>` : null}
    ${optionsOf(park).map((o) => html`<div class=${'gline' + (o.i === index ? ' hi' : '')} key=${o.i}><${K} on=${() => pick(key, o.i)} title="select this option">${(o.i === index ? '› ' : '  ') + o.i + '  ' + o.p + '  ' + o.label}</${K}></div>`)}
    <div class="gact"><${K} on=${() => answerRun(rid, answerPayload(index, labels))} off=${!gate.ok} title=${gate.ok ? 'send the selected option' : clean(gate.why, 400)}>[ answer ]</${K}>
      <${K} on=${() => answerRun(rid, STOP)} off=${!stop.ok} title=${stop.ok ? 'end the run at this park' : clean(stop.why, 400)}>[ stop ]</${K}></div>
    ${!gate.ok ? html`<div class=${'gline ' + (gate.run ? 'o' : 'faint')}>${clean(gate.why, 400)}</div>` : null}
  </div>`;
}
