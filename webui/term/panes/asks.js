// Asks: one panel for every pending ask on the page, docked above the
// composer of the pane that shows the current one (app.js picks it). Its top
// border is a tab strip, one tab per pending ask across all panes and
// sessions, oldest first; the current tab's ask is drawn below as a list,
// as the TUI's dialog: the answers, then `+ note`, then `chat about it`, one
// row highlighted (the first answer, to start). Where an ask arose, the
// transcript holds a one-line marker until it settles, then its folded line.
//
// Keys, while the panel holds focus:
//   up/down or k/j move the highlight (stopping at the ends, as the TUI
//   does), Enter takes it, y / n answer yes / no, 1-9 a question's option,
//   c chats about it, s submits; left/right or h/l (and shift+Tab) switch
//   tabs; Esc goes back to the composer and leaves the ask pending.
// A note goes with whatever is chosen next: Tab or the `+ note` row open the
// note line under the list (neither answers); in it Enter takes the
// highlighted row with the note, up/down go back to the list keeping the
// note, Esc closes the note line and drops the note. Every other key is text.
// Words of the user's own go through "chat about it", not a free-text answer.
// For LAND_MS after focus lands or the tab changes keys do nothing, and for
// LAND_MS after the tab changes or the strip shifts clicks do nothing but say
// "steady…"; a double click never answers.
// An ask that took focus by itself (nobody pressed or clicked anything) is
// unarmed: it answers no key until an arrow key, Tab or a click arms it, and
// any other key that types (Backspace too) goes back to the composer at its
// caret, focus with it. Letters never arm it: a typist's "look, can you…"
// must not switch tabs and then chat about it.
// Answers go in one batch: taking a row records it (Session.choose; the tab
// shows ✓ and focus moves to the next unanswered tab) and sends nothing; it
// can be taken again until sent. The `submit` row (or s, or :submit) sends
// every answer, back to back in queue order (app.js submitAsks), once none
// is left unanswered; Enter on the last unanswered tab submits too, and a
// lone ask is sent as soon as it is answered. "chat about it" never waits: it
// denies and stops the turn at once. A sent tab keeps ✓ and its pick until
// the ask's settle frame takes it off the strip.
import { html, K, Rendered, signal, useRef, useLayoutEffect, useMemo, useState } from '../core/ui.js';
import { render as rend, renderCall } from '../renderers/index.js';

const LAND_MS = 300;
const MODIFIERS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'AltGraph', 'Fn', 'NumLock', 'OS']);

// The panel's element, whether app.js wants it focused once drawn, and
// whether its keys answer (false after an automatic focus move).
export const panel = { el: null, want: false, armed: signal(true) };
const ARMS = new Set(['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Tab']);

export const quoted = (t) => t.split('\n').map((l) => '> ' + l).join('\n');

// What an ask is about, in one line.
export const askLine = (a) => {
  const f = a.f;
  return f.kind === 'approval' ? `${f.tool} ${rend(f.tool, 'input', f.input)}`.trim() : `? ${f.prompt || ''}`;
};

// The rows of an ask, in order. An answer's `go(note)` records it; `off`
// says why a row is greyed; `+ note` (id 'note') only opens the note line;
// `submit` sends every recorded answer and is live once none is missing.
function choices(a, s, p, act) {
  const f = a.f;
  const pick = (answer) => (note) => {
    const reply = { answer };
    if (note.trim() && !s.noNote.peek()) reply.note = note.trim();
    return s.choose(a, reply);
  };
  const chat = { id: 'chat', label: 'chat about it', go: (note) => chatAbout(a, s, p, act, note) };
  const note = { id: 'note', label: '+ note' };
  const total = act.unsent.value, left = act.queue.value.length;
  const submit = { id: 'submit', label: total > 1 ? `submit ${total} answers` : 'submit', off: left ? `${total - left} of ${total} answered` : null, go: () => act.submitAsks() };
  const rows = f.kind === 'approval' ? [
    { id: 'yes', label: 'yes', off: a.yes ? null : 'not offered', go: pick(a.yes) },
    { id: 'no', label: 'no', off: a.no ? null : 'not offered', go: pick(a.no) },
    a.always && { id: 'always', label: 'always', go: pick(a.always) },
  ].filter(Boolean) : a.options.map((o, i) => ({ id: 'o' + i, label: o.label, key: o.key, desc: o.description, go: pick(o.label) }));
  return [...rows, note, chat, ...(total > 1 ? [submit] : [])];
}

// "chat about it": deny (when there is a no to give), stop the turn, and hand
// the composer a quote of the ask to talk about.
async function chatAbout(a, s, p, act, note) {
  const f = a.f, no = f.kind === 'approval' || a.yesno ? a.no : null;
  act.letBe(s); // the turn's other asks are about to be cancelled: none takes focus meanwhile
  a.chat = true;
  if (no) await s.answer(a, { answer: no });
  s.cancel();
  const what = f.kind === 'approval' ? askLine(a) : 'question';
  const text = `${what}: ${f.prompt || ''}`.replace(/\s+/g, ' ').trim();
  p.prefill(quoted(text.length > 160 ? text.slice(0, 159) + '…' : text) + '\n\n' + note.trim());
}

// Per ask: the highlighted row, the note and whether its line is open. One
// panel draws every ask, so this lives with the ask, not the component.
const views = new WeakMap();
function viewOf(a, opts) {
  let v = views.get(a);
  if (!v) {
    v = { hi: signal(opts.findIndex((o) => !o.off)), note: signal(''), noting: signal(false) };
    views.set(a, v);
  }
  return v;
}

const cut = (t, n) => (t.length > n ? t.slice(0, n - 1) + '…' : t);

export function AskPanel({ p, act }) {
  const ref = useRef(null), field = useRef(null), strip = useRef(null), landed = useRef(0), born = useRef(0), steadyT = useRef(0);
  const steady = useMemo(() => signal(''), []);
  const [fit, setFit] = useState(0), [w, setW] = useState(0), shape = useRef('');
  const tabs = act.tabs.value, cur = act.current.value;
  const a = cur && cur.a, s = cur && cur.s;
  const opts = a ? choices(a, s, p, act) : [];
  const v = a ? viewOf(a, opts) : null;
  useLayoutEffect(() => {
    panel.el = ref.current;
    return () => { if (panel.el === ref.current) panel.el = null; clearTimeout(steadyT.current); };
  }, []);
  // A new tab, or a strip that moved, is a new thing under the pointer and the keys.
  useLayoutEffect(() => {
    born.current = performance.now();
    if (ref.current && ref.current.contains(document.activeElement)) landed.current = born.current;
  }, [a, tabs.length]);
  useLayoutEffect(() => {
    if (panel.want && ref.current) { panel.want = false; ref.current.focus({ preventScroll: true }); }
  });
  useLayoutEffect(() => {
    if (!ref.current) return undefined;
    const ro = new ResizeObserver(() => setW(ref.current ? ref.current.clientWidth : 0));
    ro.observe(ref.current);
    return () => ro.disconnect();
  }, [!!a]);
  // The strip fits the frame, as the status line's FIT: first the other tabs
  // drop to their number, then the current tab's words shorten, then go; the
  // current tab is scrolled into view if even that is too wide.
  useLayoutEffect(() => {
    const el = strip.current;
    if (!el) return;
    const now = tabs.map((x) => x.a.id + (x.a.chosen.peek() ? '✓' : '')).join() + '|' + (a && a.id) + '|' + w;
    if (shape.current !== now) { shape.current = now; if (fit) { setFit(0); return; } }
    if (fit < 3 && el.scrollWidth > el.clientWidth + 1) { setFit(fit + 1); return; }
    const on = el.querySelector('.atab.on');
    if (!on) return;
    if (on.offsetLeft + on.offsetWidth > el.scrollLeft + el.clientWidth) el.scrollLeft = on.offsetLeft + on.offsetWidth - el.clientWidth;
    else if (on.offsetLeft < el.scrollLeft) el.scrollLeft = on.offsetLeft;
  });
  // Focus lands in the same render that draws the note line, before the next key.
  useLayoutEffect(() => { if (v && v.noting.value && field.current) field.current.focus(); }, [v && v.noting.value]);
  if (!a) return null;
  const f = a.f, chosen = a.chosen.value, busy = a.busy.value, noteOff = s.noNote.value;
  const say = (text) => {
    steady.value = text;
    clearTimeout(steadyT.current);
    steadyT.current = setTimeout(() => { steady.value = ''; }, 1500);
  };
  const toList = () => ref.current.focus({ preventScroll: true });
  const openNote = () => { if (v.noting.peek()) field.current.focus(); else v.noting.value = true; };
  const closeNote = () => { v.noting.value = false; v.note.value = ''; toList(); };
  // Take row i. An answer is recorded; it is sent (with every other) when it
  // was the last one missing and taken by Enter, or when it is the only ask.
  const take = (i, enter) => {
    const o = opts[i];
    if (o.off || a.busy.peek()) return;
    if (o.id === 'note') { openNote(); return; }
    const note = v.noting.peek() ? v.note.peek() : '';
    if (o.id === 'chat' || o.id === 'submit') { o.go(note); return; }
    if (!o.go(note) || act.queue.peek().length) return;
    if (enter || act.unsent.peek() === 1) act.submitAsks();
    else v.hi.value = opts.findIndex((x) => x.id === 'submit');
  };
  const move = (d) => {
    for (let i = v.hi.peek() + d; i >= 0 && i < opts.length; i += d) if (!opts[i].off) { v.hi.value = i; return; }
  };
  const pick = (id) => {
    const i = opts.findIndex((o) => o.id === id);
    if (i >= 0) { v.hi.value = i; take(i); }
  };
  const optionId = (label) => 'o' + a.options.findIndex((o) => o.label === label);
  let armedNow = true;
  const onKey = (e) => {
    armedNow = panel.armed.peek();
    if (e.isComposing || e.keyCode === 229) return; // an IME's own Enter and keys
    if (e.ctrlKey || e.metaKey || e.altKey || MODIFIERS.has(e.key)) return;
    if (e.key === 'Tab' && e.shiftKey) return; // shift+Tab: the previous ask (app.js)
    const stop = () => { e.preventDefault(); e.stopPropagation(); };
    const k = e.key;
    if (!panel.armed.peek() && e.target !== field.current) {
      if (ARMS.has(k)) panel.armed.value = true;
      else if (k !== 'Escape') {
        stop();
        if (k.length === 1 || k === 'Backspace') act.bounce(p, k);
        return;
      }
    }
    if (performance.now() - landed.current < LAND_MS) { stop(); return; }
    if (e.target === field.current) {
      if (k === 'Escape') { stop(); closeNote(); }
      else if (k === 'Enter') {
        stop();
        if (e.repeat) return;
        if (opts[v.hi.peek()].id === 'note') { toList(); say('pick what to send the note with'); return; }
        take(v.hi.peek(), true);
      } else if (k === 'ArrowUp' || k === 'ArrowDown' || k === 'Tab') { stop(); toList(); }
      return;
    }
    stop();
    if (k === 'Escape') { act.leaveAsk(p); return; }
    if (k === 'ArrowLeft' || k === 'h' || k === 'ArrowRight' || k === 'l') { act.cycleAsk(k === 'ArrowLeft' || k === 'h' ? -1 : 1); return; }
    if (k === 'Tab') { if (armedNow) openNote(); return; }
    if (k === 'ArrowUp' || k === 'k' || k === 'ArrowDown' || k === 'j') { move(k === 'ArrowUp' || k === 'k' ? -1 : 1); return; }
    if (e.repeat) return;
    if (k === 'Enter') { take(v.hi.peek(), true); return; }
    if (k === 'c') { pick('chat'); return; }
    if (k === 's') { if (!act.submitAsks()) say(act.queue.peek().length ? 'answer every ask to submit' : 'nothing to submit'); return; }
    const yes = k === 'y' || k === 'Y', no = k === 'n' || k === 'N';
    if (f.kind === 'approval') { if (yes) pick('yes'); else if (no) pick('no'); return; }
    if (a.yesno && (yes || no)) { pick(optionId(yes ? a.yes : a.no)); return; }
    const o = a.options.find((x) => x.key === k);
    if (o) pick(optionId(o.label));
  };
  const click = (e, i) => {
    if (e.detail > 1) return; // the second click of a double click
    panel.armed.value = true;
    if (performance.now() - born.current < LAND_MS) { say('steady…'); return; }
    v.hi.value = i;
    take(i);
  };
  const others = (x) => x.s !== p.s;
  let head, body;
  if (f.kind === 'approval') {
    const sum = rend(f.tool, 'input', f.input);
    head = `approve: ${f.tool}${sum ? '  ' + sum : ''}`;
    body = html`
      <div>${f.prompt || ''}</div>
      ${f.reason && html`<div class="why">reason: ${f.reason}</div>`}
      ${f.judged && html`<div class="why">judge: ${f.judged}</div>`}
      ${f.structural && html`<div class="why">structural rule</div>`}
      ${f.yolo && html`<div><span class="warn">yolo is on</span><span class="why"> and the gate still asked</span></div>`}
      <${Rendered} value=${renderCall(f.input, f.tool, 'inputFull', f.input)} />`;
  } else {
    head = 'question';
    body = html`<div>${f.prompt || ''}</div>`;
  }
  const key = (k, label) => html` · <span class="key">${k}</span> ${label}`;
  const armed = panel.armed.value;
  const tab = (x, i) => {
    const n = `${i + 1}`, ok = x.a.chosen.value ? '✓' : '';
    if ((fit && x.a !== a) || fit >= 3) return n + ok;
    return `${n} ${cut(askLine(x.a), fit === 2 ? 8 : 22)}${others(x) && fit < 2 ? ' · ' + act.name(x.s) : ''}${ok && ' ' + ok}`;
  };
  return html`<div class=${'ask askpanel' + (busy ? ' busy' : '') + (armed ? '' : ' unarmed')} ref=${ref} tabindex="0" data-ask=${a.id} onKeyDown=${onKey}
      onClick=${() => { panel.armed.value = true; }}
      onFocusIn=${(e) => { if (!e.currentTarget.contains(e.relatedTarget)) landed.current = performance.now(); }}>
    <div class="ftitle astrip" role="tablist" ref=${strip}>${tabs.map((x, i) => html`${i > 0 && html`<span class="asep">│</span>`}<span
        class=${'atab k' + (x.a === a ? ' on' : '') + (x.a.chosen.value ? ' done' : '')} role="tab" aria-selected=${x.a === a ? 'true' : 'false'}
        title=${askLine(x.a) + (others(x) ? ' · ' + act.name(x.s) : '')}
        onMouseDown=${(e) => e.preventDefault()} onClick=${(e) => { e.stopPropagation(); act.focusAsk(x); }}>${tab(x, i)}</span>`)}</div>
    <div class="abody">
      <div class="ahead">${head}${others(cur) && html`<span class="faint">  · ${act.name(s)}</span>`}</div>
      ${body}
      <div class="keys">
        ${opts.map((o, i) => html`<span class=${'opt ' + o.id + (i === v.hi.value ? ' hi' : '') + (o.off || busy ? ' dis' : ' k') + (chosen && chosen.answer === (o.go && labelOf(a, o)) ? ' picked' : '')}
            role="button" tabindex="-1" aria-disabled=${o.off || busy ? 'true' : null} title=${o.off || o.desc || ''}
            onClick=${o.off || busy ? null : (e) => click(e, i)}>${o.key ? o.key + ' ' : ''}${o.label}${o.off && html`<span class="off"> (${o.off})</span>`}</span>`)}
        ${!armed ? html`<span class="o tabhint arm">↑↓ or tab to choose · typing goes to the composer</span>`
          : chosen ? html`<span class="faint tabhint">✓ ${chosen.answer}${chosen.note ? ` (note: ${chosen.note})` : ''} · ${busy ? 'sent, waiting for the door' : 'not sent yet: submit sends every answer'}</span>`
          : steady.value ? html`<span class="faint tabhint">${steady.value}</span>` : !v.noting.value && html`<span class="faint tabhint">tab: add a note</span>`}
      </div>
      ${v.noting.value && html`<div class="aline"><span class="faint">note ›</span><input ref=${field} type="text" spellcheck="false" autocomplete="off"
        aria-label="note sent with your choice" readOnly=${noteOff || busy} value=${v.note.value}
        placeholder=${noteOff ? 'the door takes no note' : 'sent with the choice you take next; esc drops it'}
        onInput=${(e) => { v.note.value = e.currentTarget.value; }} /></div>`}
      <div class="akeys">keys: <span class="key">↑↓ j k</span> choose${key('⏎', 'take')}${(f.kind === 'approval' || a.yesno) && html`${key('y', 'yes')}${key('n', 'no')}`}${f.kind !== 'approval' && key('1-' + Math.min(9, a.options.length), 'option')}${key('c', 'chat')}${key('tab', 'note')}${tabs.length > 1 && key('s', 'submit')}${key('←→ h l', 'asks')}${key('esc', 'composer')}</div>
      ${a.err.value && html`<div class="aerr">${a.err.value}</div>`}
    </div>
  </div>`;
}

// The answer a row sends, to mark the one that was sent.
function labelOf(a, o) {
  if (o.id === 'yes') return a.yes;
  if (o.id === 'no') return a.no;
  if (o.id === 'always') return a.always;
  return o.label;
}

// Where a pending ask arose: one line; a click takes its tab.
export function AskMark({ a, s, act }) {
  const i = act.tabs.value.findIndex((x) => x.a === a), chosen = a.chosen.value;
  return html`<div class="sys askmark" data-ask=${a.id} onMouseDown=${(e) => e.preventDefault()}>
    <${K} cls="o" on=${() => act.focusAsk({ s, a })}>${chosen ? `✓ ${chosen.answer} · ${a.busy.value ? 'sent' : 'not sent'}` : '? waiting'}${i >= 0 ? ` · ask ${i + 1}` : ''}</${K}>
    <span class="faint">  ${askLine(a)}</span>
  </div>`;
}

// A settled ask, folded to one line: what it asked and what was picked (the
// note too, when this page sent it). Opened (click or Enter) it lists every
// choice with the picked one marked, so the choices can still be read while
// the composer talks about them ("chat about it").
export function Settled({ a }) {
  const open = useMemo(() => signal(false), [a]);
  const d = a.done.value, f = a.f, approval = f.kind === 'approval';
  const what = approval ? `approve: ${askLine(a)}` : `question: ${f.prompt || ''}`;
  const picked = { answered: d.answer, cancelled: 'cancelled', elsewhere: 'answered elsewhere' }[d.how];
  const cls = d.how !== 'answered' ? 'faint' : d.answer === a.no ? 'r' : 'hi';
  const tail = [d.note && `note: ${d.note}`, a.chat && 'chat about it'].filter(Boolean).join(' · ');
  const labels = approval ? [a.yes, a.no, a.always].filter(Boolean).map((label) => ({ label })) : a.options;
  const line = (on, label, desc) => html`<div class=${on ? cls : 'faint'}>${on ? '✓ ' : '  '}${label}${desc && html`<span class="faint">  ${desc}</span>`}</div>`;
  // A click opens it without taking focus from the composer being typed in.
  return html`<div class="sys settled" onMouseDown=${(e) => e.preventDefault()}>
    <${K} on=${() => { open.value = !open.value; }}>${open.value ? '▾ ' : '▸ '}${what} → <span class=${cls}>${picked}</span>${tail && html`<span class="faint"> (${tail})</span>`}</${K}>
    ${open.value && html`<div class="choices">
      ${approval && f.prompt && html`<div>${f.prompt}</div>`}
      ${labels.map((o) => line(d.how === 'answered' && o.label === d.answer, o.label, o.description))}
      ${line(!!a.chat, 'chat about it')}
      ${d.note && html`<div class="faint">note: ${d.note}</div>`}
    </div>`}
  </div>`;
}

