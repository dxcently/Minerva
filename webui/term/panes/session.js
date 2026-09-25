// One session pane: the transcript, the draft, inline asks, the composer.
// It draws a core/state.js Session and holds no session state of its own.
// Several panes may draw one Session (mirrors share its one stream), so
// everything tied to the DOM (scroller, ask elements, which asks are
// off-screen) lives in a per-PANE view, never on the Session.
import { html, K, HUB, Rendered, Component, signal, useRef, useLayoutEffect, useEffect, useMemo } from '../core/ui.js';
import { markdown } from '../core/markdown.js';
import { render as rend } from '../renderers/index.js';
import { icon, word, shade, CELLS } from '../core/life.js';
import { tildify, kfmt } from '../core/state.js';
import { openMenu, atPointer, under } from '../core/menu.js';

const AGENT = icon();
// After focus lands on an ask, keys wait this long; after an ask appears or
// jumps into view, clicks do. A key or click already under way when the ask
// arrived must not answer it.
const LAND_MS = 300;

// Per-pane view helpers: the scroller, ask elements, which asks are off-screen.
const views = new WeakMap();
function viewOf(p) {
  let v = views.get(p);
  if (!v) {
    v = { p, scroller: null, io: null, off: signal(new Set()), askEls: new Map(), jumpAt: 0 };
    v.observe = (el) => {
      if (!v.io && v.scroller) {
        v.io = new IntersectionObserver((ents) => {
          const off = new Set(v.off.value);
          for (const e of ents) {
            const id = Number(e.target.dataset.ask);
            if (e.isIntersecting) off.delete(id); else off.add(id);
          }
          v.off.value = off;
        }, { root: v.scroller, threshold: 0.2 });
      }
      if (v.io) v.io.observe(el);
    };
    v.unobserve = (el, id) => {
      if (v.io) v.io.unobserve(el);
      if (v.off.value.has(id)) { const off = new Set(v.off.value); off.delete(id); v.off.value = off; }
    };
    views.set(p, v);
  }
  return v;
}

// The element pane `p` draws ask `id` in, if it draws it.
export const askEl = (p, id) => viewOf(p).askEls.get(id);

// Scroll pane `p` to ask `a` and give it focus, which is what keys answer.
export function focusAsk(p, a) {
  const el = a && askEl(p, a.id);
  if (!el) return false;
  viewOf(p).jumpAt = performance.now();
  el.scrollIntoView({ block: 'nearest' });
  el.focus({ preventScroll: true });
  return true;
}

// ---------------------------------------------------------------- the pane
export function Pane({ p, rect, focused, mirror, act }) {
  const s = p.s, v = viewOf(p);
  const style = `left:${rect.x - (rect.x > 0 ? 1 : 0)}px;top:${rect.y - (rect.y > 0 ? 1 : 0)}px;` +
    `width:${rect.w + (rect.x > 0 ? 1 : 0)}px;height:${rect.h + (rect.y > 0 ? 1 : 0)}px`;
  const banner = s.banner.value;
  v.act = act;
  return html`<section class=${'pane frame' + (focused ? ' focus' : '')} style=${style} tabindex="-1"
      data-pane=${p.id} onMouseDown=${() => act.focus(p.id)}>
    <${Title} p=${p} mirror=${mirror} act=${act} />
    <${Glyphs} p=${p} act=${act} />
    <${Transcript} s=${s} v=${v} />
    ${banner && html`<div class=${'banner' + (banner.err ? ' err' : '')}>${banner.text}</div>`}
    <${Waiting} s=${s} v=${v} />
    <${Composer} s=${s} p=${p} act=${act} />
  </section>`;
}

function Title({ p, mirror, act }) {
  const s = p.s, conn = s.conn.value, hl = s.hello.value;
  const dot = { live: 'g', replaying: 'y', connecting: 'y', reconnecting: 'y', goodbye: 'faint' }[conn] || 'r';
  const open = (e, at) => { e.preventDefault(); e.stopPropagation(); act.paneMenu(p, at); };
  return html`<div class="ftitle k ptitle" role="button" tabindex="0" title="pane menu (click or right-click)" data-owner=${'pane-' + p.id}
      onClick=${(e) => open(e, under(e.currentTarget))} onContextMenu=${(e) => open(e, atPointer(e))}
      onKeyDown=${(e) => !e.repeat && (e.key === 'Enter' || e.key === ' ') && open(e, under(e.currentTarget))}>
    ${hl ? tildify(hl.cwd) : 'session'} <span class=${dot}>●</span>
    ${conn !== 'live' && html` <span class="faint">${conn}</span>`}
    ${mirror && html` <span class="faint">(mirror)</span>`} <span class="faint">▾</span>
  </div>`;
}

// Split and close, cut into the top border on the right.
function Glyphs({ p, act }) {
  const g = (ch, title, owner, on) => html`<span class="k glyph" role="button" tabindex="0" title=${title} data-owner=${owner}
    onClick=${(e) => { e.stopPropagation(); on(e.currentTarget); }}
    onKeyDown=${(e) => { if (!e.repeat && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); e.stopPropagation(); on(e.currentTarget); } }}>${ch}</span>`;
  return html`<div class="fglyphs">
    ${g('⇥', 'split right', `split-row-${p.id}`, (el) => act.splitMenu(p, 'row', under(el)))}
    ${g('⇩', 'split down', `split-col-${p.id}`, (el) => act.splitMenu(p, 'col', under(el)))}
    ${g('×', 'close the pane (⌥w  :q)', `close-${p.id}`, () => act.close(p.id))}
  </div>`;
}

// ---------------------------------------------------------------- transcript
function Transcript({ s, v }) {
  const ref = useRef(null), inner = useRef(null), stick = useRef(true);
  const chunks = s.chunks.value;
  s.stick.value;
  useLayoutEffect(() => {
    const el = ref.current;
    v.scroller = el;
    // Asks drawn before this ran (a child's layout effect runs before its
    // parent's, so a mirror opened with an ask pending registers it first)
    // are observed now that the observer can exist.
    for (const x of v.askEls.values()) v.observe(x);
    const pin = new ResizeObserver(() => { if (stick.current) el.scrollTop = el.scrollHeight; });
    pin.observe(inner.current);
    pin.observe(el);
    return () => { pin.disconnect(); if (v.io) v.io.disconnect(); v.io = null; };
  }, []);
  useLayoutEffect(() => {
    const el = ref.current;
    if (stick.current) el.scrollTop = el.scrollHeight;
  });
  const onScroll = (e) => {
    const el = e.currentTarget;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
  };
  return html`<div class="scroll" ref=${ref} onScroll=${onScroll}>
    <div class="rows" ref=${inner}>
      ${chunks.map((c) => html`<${Chunk} key=${c.key} c=${c} s=${s} v=${v} />`)}
      <${DraftView} s=${s} />
    </div>
  </div>`;
}

// A chunk of rows (core/state.js CHUNK). The class skips re-rendering when
// the chunk list changes; the body re-renders only when its own chunk's
// version signal moves, i.e. when rows were appended to it.
class Chunk extends Component {
  shouldComponentUpdate(next) { return next.c !== this.props.c; }
  render({ c, s, v }) { return html`<${ChunkBody} c=${c} s=${s} v=${v} />`; }
}
function ChunkBody({ c, s, v }) {
  c.ver.value; // subscribe
  return c.rows.map((r) => html`<${Row} key=${r.key} r=${r} s=${s} v=${v} />`);
}

// A row's own data never changes (its mutable parts are signals its inner
// component reads), so the list re-render skips every row that exists.
class Row extends Component {
  shouldComponentUpdate(next) { return next.r !== this.props.r; }
  render({ r, s, v }) { return html`<${RowBody} r=${r} s=${s} v=${v} />`; }
}

const Who = ({ who }) => (who ? html`<div class=${'who ' + (who === 'you' ? 'you' : 'bot')}>${who === 'you' ? 'you' : 'minerva'}</div>` : null);

function Fold({ label, cls, children }) {
  const open = useMemo(() => signal(false), []);
  return html`<div class=${cls}>
    <${K} on=${() => { open.value = !open.value; }}>${(open.value ? 'v ' : '> ') + label}</${K}>
    ${open.value && html`<div class="body">${children}</div>`}
  </div>`;
}

// ---------------------------------------------------------------- row menus
// A row's source as markdown: what "copy as markdown" and "quote" take.
function rowSource(r) {
  switch (r.kind) {
    case 'bot': return ((r.full && r.full.value) || r).text;
    case 'peer': return `~ ${r.from}: ${r.text}`;
    case 'pre': return '```\n' + r.text + '\n```';
    case 'verdict': return `policy: ${r.f.tool} [${r.f.outcome}] ${r.f.reason}`;
    case 'ask': return r.ask.f.prompt || '';
    case 'tool': {
      const c = r.call, out = c.out.value;
      const line = '`' + c.name + '` ' + rend(c.name, 'input', c.input);
      return out ? line + '\n```\n' + String(out.output ?? '').replace(/\n+$/, '') + '\n```' : line;
    }
    default: return r.text || r.label || '';
  }
}

const quoted = (t) => t.split('\n').map((l) => '> ' + l).join('\n');

function copy(text, act) {
  navigator.clipboard.writeText(text).then(() => act.toast('copied'), () => act.toast('the browser refused the clipboard', true));
}

// A right-click on a row. With text selected in the row, copy and quote take
// the selection; a selection elsewhere keeps the browser's own menu.
function rowMenu(e, r, v) {
  const el = e.currentTarget, got = getSelection(), sel = String(got);
  if (sel && !el.contains(got.anchorNode)) return;
  e.preventDefault();
  openMenu([
    { label: 'fork from here', off: HUB },
    { label: sel ? 'copy selection' : 'copy', on: () => copy(sel || el.innerText.trim(), v.act) },
    { label: 'copy as markdown', on: () => copy(rowSource(r), v.act) },
    { label: 'quote into composer', on: () => v.p.prefill(quoted(sel || rowSource(r)) + '\n\n') },
  ], atPointer(e), { title: 'row' });
}

function RowBody({ r, s, v }) {
  const cm = (e) => rowMenu(e, r, v);
  switch (r.kind) {
    case 'you':
      return html`<div class="row" onContextMenu=${cm}><${Who} who=${r.who} /><div class="body">${r.text}</div>
        ${r.images > 0 && html`<div class="body faint">[${r.images} image${r.images > 1 ? 's' : ''}]</div>`}</div>`;
    case 'bot': {
      // A reply an `error` cut short is filled in place by its assistant-message.
      const full = r.full && r.full.value, x = full || r, cut = full ? null : r.cut;
      return html`<div class="row" onContextMenu=${cm}><${Who} who=${r.who} />
        ${x.thinking && html`<${Fold} cls="think" label=${`thinking (${x.thinking.split('\n').length} lines)`}>${x.thinking}</${Fold}>`}
        ${x.redacted > 0 && html`<div class="think">(${x.redacted} redacted thinking block${x.redacted > 1 ? 's' : ''})</div>`}
        ${x.text && html`<div class="body">${cut ? x.text : markdown(x.text)}</div>`}
        ${cut && html`<div class="sys y">(${cut})</div>`}</div>`;
    }
    case 'sys': return html`<div class=${'row sys ' + (r.cls || '')} onContextMenu=${cm}>${r.text}</div>`;
    case 'verdict': {
      const f = r.f, bad = ['refused', 'declined', 'yolo'].includes(f.outcome);
      return html`<div class=${'row sys' + (bad ? ' err' : '')} onContextMenu=${cm}>policy: ${f.tool} [${f.outcome}] ${f.reason}${f.note ? ' · ' + f.note : ''}
        <span class="dim"> (not matched to a call)</span></div>`;
    }
    case 'settle': return html`<div class="row settle" onContextMenu=${cm}>${r.text}</div>`;
    case 'fold': return html`<div class="row" onContextMenu=${cm}><${Fold} cls="sys" label=${r.label}>${r.text}</${Fold}></div>`;
    case 'peer': return html`<div class="row peer" onContextMenu=${cm}><span class="b">~ ${r.from}</span>  ${r.text}</div>`;
    case 'pre': return html`<div class="row" onContextMenu=${cm}><pre>${r.text}</pre></div>`;
    case 'tool': return html`<${ToolRow} c=${r.call} s=${s} v=${v} cm=${cm} />`;
    case 'ask':
      return html`<div class="row" onContextMenu=${cm}>${r.ask.done.value ? html`<${Settled} a=${r.ask} />` : html`<${AskBlock} a=${r.ask} s=${s} v=${v} />`}</div>`;
    default: return null;
  }
}

// ---------------------------------------------------------------- tool lines
const VD = { refused: 'r', declined: 'r', yolo: 'r', judged: 'y', approved: 'g' };

function ToolRow({ c, s, v, cm }) {
  const st = c.st.value, out = c.out.value, vd = c.verdict.value, a = c.ask.value, settled = c.settled.value;
  const open = useMemo(() => signal(null), []);
  const shown = open.value ?? (out && out.isError);
  const sum = useMemo(() => rend(c.name, 'input', c.input), [c.input]);
  const body = useMemo(() => (out ? rend(c.name, 'output', out.output, out.isError) : null), [out]);
  const toggle = () => { if (out) open.value = !shown; };
  return html`<div class="row tool" onContextMenu=${cm}>
    <div class=${'tline' + (out ? ' k' : '')} onClick=${toggle}>
      <span class="arrow">-></span><span class="name">${c.name}</span>
      <span class="sum" title=${sum}>${sum}</span>
      ${c.origin !== 'model' && html`<span class="faint">(${c.origin})</span>`}
      ${vd && html`<span class=${'vd ' + (VD[vd.outcome] || 'faint')} title=${vd.reason + (vd.note ? ' · ' + vd.note : '')}>[${vd.outcome}]</span>`}
      ${c.fired.value && html`<span class="vd">[fired]</span>`}
      <span class=${'st ' + st}>${{ run: 'running', ok: 'ok ▸', err: 'error ▸', stop: 'stopped' }[st]}</span>
      <span class="tm">${c.time.value}</span>
    </div>
    ${shown && body && html`<${Rendered} value=${body} />`}
    ${a && html`<${AskBlock} a=${a} s=${s} v=${v} />`}
    ${settled && html`<${Settled} a=${settled} />`}
  </div>`;
}

// ---------------------------------------------------------------- asks
// An ask is a line of choices with one highlighted (the first, yes, to start).
// The page's oldest pending ask can take focus by itself (app.js steer), and
// keys answer the ask that holds focus:
//   left/right move the highlight, Enter takes it, y / n answer yes / no and
//   1-9 a question's option, Esc goes back to the composer and leaves the ask
//   queued.
// A note goes with whatever is chosen next: Tab, down or the `+ note` choice
// open the note line (none of them answers); in it Enter takes the highlighted
// choice with the note, up goes back to the choices keeping the note, Esc
// closes the note line and drops the note.
// Words of the user's own go through "chat about it", not a free-text answer.
// For LAND_MS after focus lands keys do nothing, and for LAND_MS after the
// ask appears, jumps into view, or an ask above it settles, clicks do nothing
// but say "steady…"; a double click never answers.
// The highlight and the note belong to this pane; the answer settles every
// pane (ask-settled).
const MODIFIERS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'AltGraph', 'Fn', 'NumLock', 'OS']);

// The choices on an ask, in order. `go(note)` answers; `off` says why a
// choice is greyed; `+ note` (id 'note') only opens the note line.
function choices(a, s, v) {
  const f = a.f;
  const send = (answer) => (note) => {
    const reply = { answer };
    if (note.trim() && !s.noNote.peek()) reply.note = note.trim();
    return s.answer(a, reply);
  };
  const chat = { id: 'chat', label: 'chat about it', go: (note) => chatAbout(a, s, v, note) };
  const note = { id: 'note', label: '+ note' };
  if (f.kind === 'approval') {
    return [
      { id: 'yes', label: 'yes', off: a.yes ? null : 'not offered', go: send(a.yes) },
      { id: 'always', label: 'always', off: a.always ? null : 'needs eidolon PR', go: send(a.always) },
      { id: 'no', label: 'no', off: a.no ? null : 'not offered', go: send(a.no) },
      chat,
      { id: 'fork', label: 'fork', off: HUB },
      note,
    ];
  }
  return [
    ...a.options.map((o, i) => ({ id: 'o' + i, label: o.label, key: o.key, desc: o.description, go: send(o.label) })),
    chat,
    note,
  ];
}

// "chat about it": deny (when there is a no to give), stop the turn, and hand
// the composer a quote of the ask to talk about.
async function chatAbout(a, s, v, note) {
  const f = a.f, no = f.kind === 'approval' || a.yesno ? a.no : null;
  v.act.letBe(s); // the turn's other asks are about to be cancelled: none takes focus meanwhile
  a.chat = true;
  if (no) await s.answer(a, { answer: no });
  s.cancel();
  const what = f.kind === 'approval' ? `${f.tool} ${rend(f.tool, 'input', f.input)}`.trim() : 'question';
  const text = `${what}: ${f.prompt || ''}`.replace(/\s+/g, ' ').trim();
  v.p.prefill(quoted(text.length > 160 ? text.slice(0, 159) + '…' : text) + '\n\n' + note.trim());
}

function AskBlock({ a, s, v }) {
  const ref = useRef(null), field = useRef(null), landed = useRef(0), born = useRef(0), steadyT = useRef(0);
  const f = a.f, busy = a.busy.value, opts = choices(a, s, v);
  const hi = useMemo(() => signal(opts.findIndex((o) => !o.off)), [a]);
  const note = useMemo(() => signal(''), [a]);
  const noting = useMemo(() => signal(false), [a]); // the note line is open
  const steady = useMemo(() => signal(''), [a]);
  const noteOff = s.noNote.value;
  useLayoutEffect(() => {
    const el = ref.current;
    born.current = performance.now();
    v.askEls.set(a.id, el);
    v.observe(el);
    return () => {
      v.unobserve(el, a.id);
      if (v.askEls.get(a.id) === el) v.askEls.delete(a.id);
      clearTimeout(steadyT.current);
      v.jumpAt = performance.now(); // this block leaving moves what is under the pointer
    };
  }, [a]);
  const say = (text) => {
    steady.value = text;
    clearTimeout(steadyT.current);
    steadyT.current = setTimeout(() => { steady.value = ''; }, 1500);
  };
  const openNote = () => {
    if (noting.peek()) field.current.focus(); else noting.value = true;
  };
  // Focus lands in the same render that draws the line, before the next key.
  useLayoutEffect(() => { if (noting.value && field.current) field.current.focus(); }, [noting.value]);
  const closeNote = () => {
    noting.value = false;
    note.value = '';
    ref.current.focus({ preventScroll: true });
  };
  const take = (i) => {
    const o = opts[i];
    if (o.off || a.busy.peek()) return;
    if (o.id === 'note') { openNote(); return; }
    o.go(noting.peek() ? note.peek() : '');
  };
  const move = (d) => {
    for (let j = 1; j < opts.length; j++) {
      const i = (hi.peek() + d * j + opts.length * j) % opts.length;
      if (!opts[i].off) { hi.value = i; return; }
    }
  };
  const pick = (id) => {
    const i = opts.findIndex((o) => o.id === id);
    if (i >= 0) { hi.value = i; take(i); }
  };
  const optionId = (label) => 'o' + a.options.findIndex((o) => o.label === label);
  const onKey = (e) => {
    if (e.isComposing || e.keyCode === 229) return; // an IME's own Enter and keys
    if (e.ctrlKey || e.metaKey || e.altKey || MODIFIERS.has(e.key)) return;
    if (e.key === 'Tab' && e.shiftKey) return; // shift+Tab: the previous ask (app.js)
    const stop = () => { e.preventDefault(); e.stopPropagation(); };
    if (performance.now() - landed.current < LAND_MS) { stop(); return; }
    const k = e.key, typing = e.target === field.current;
    if (typing) {
      if (k === 'Escape') { stop(); closeNote(); }
      else if (k === 'Enter') {
        stop();
        if (e.repeat) return;
        if (opts[hi.peek()].id === 'note') { ref.current.focus({ preventScroll: true }); say('pick what to send the note with'); return; }
        take(hi.peek());
      } else if (k === 'ArrowUp' || k === 'Tab') { stop(); ref.current.focus({ preventScroll: true }); }
      return;
    }
    stop();
    if (k === 'Escape') { v.act.leaveAsk(v.p); return; }
    if (k === 'Tab' || k === 'ArrowDown') { openNote(); return; }
    if (k === 'ArrowLeft' || k === 'ArrowRight') { move(k === 'ArrowLeft' ? -1 : 1); return; }
    if (e.repeat) return;
    if (k === 'Enter') { take(hi.peek()); return; }
    const yes = k === 'y' || k === 'Y', no = k === 'n' || k === 'N';
    if (f.kind === 'approval') { if (yes) pick('yes'); else if (no) pick('no'); return; }
    if (a.yesno && (yes || no)) { pick(optionId(yes ? a.yes : a.no)); return; }
    const o = a.options.find((x) => x.key === k);
    if (o) pick(optionId(o.label));
  };
  const click = (e, i) => {
    if (e.detail > 1) return; // the second click of a double click
    if (performance.now() - Math.max(born.current, v.jumpAt) < LAND_MS) { say('steady…'); return; }
    hi.value = i;
    take(i);
  };
  let title, body;
  if (f.kind === 'approval') {
    const sum = rend(f.tool, 'input', f.input);
    title = `approve: ${f.tool}${sum ? '  ' + sum : ''}`;
    body = html`
      <div>${f.prompt || ''}</div>
      ${f.reason && html`<div class="why">reason: ${f.reason}</div>`}
      ${f.judged && html`<div class="why">judge: ${f.judged}</div>`}
      ${f.structural && html`<div class="why">structural rule</div>`}
      ${f.yolo && html`<div><span class="warn">yolo is on</span><span class="why"> and the gate still asked</span></div>`}
      <${Rendered} value=${rend(f.tool, 'inputFull', f.input)} />`;
  } else {
    title = 'question';
    body = html`<div>${f.prompt || ''}</div>`;
  }
  const key = (k, label) => html` · <span class="key">${k}</span> ${label}`;
  return html`<div class=${'ask' + (a.fresh ? ' fresh' : '') + (busy ? ' busy' : '')} ref=${ref} tabindex="0"
      data-ask=${a.id} onKeyDown=${onKey}
      onFocusIn=${(e) => { if (!e.currentTarget.contains(e.relatedTarget)) landed.current = performance.now(); }}>
    <div class="ftitle">${title}</div>
    ${body}
    <div class="keys">
      ${opts.map((o, i) => html`<span class=${'opt ' + o.id + (i === hi.value ? ' hi' : '') + (o.off ? ' dis' : ' k')}
          role="button" tabindex="-1" aria-disabled=${o.off ? 'true' : null} title=${o.off || o.desc || ''}
          onClick=${o.off ? null : (e) => click(e, i)}>${o.key ? o.key + ' ' : ''}${o.label}${o.off && html`<span class="off"> (${o.off})</span>`}</span>`)}
      ${steady.value ? html`<span class="faint">${steady.value}</span>` : !noting.value && html`<span class="faint tabhint">tab: add a note</span>`}
    </div>
    ${noting.value && html`<div class="aline"><span class="faint">note ›</span><input ref=${field} type="text" spellcheck="false" autocomplete="off"
      aria-label="note sent with your choice" readOnly=${noteOff} value=${note.value}
      placeholder=${noteOff ? 'the door takes no note' : 'sent with the choice you take next; esc drops it'}
      onInput=${(e) => { note.value = e.currentTarget.value; }} /></div>`}
    <div class="akeys">keys: <span class="key">←→</span> choose${key('⏎', 'take')}${key('tab ↓', 'note')}${(f.kind === 'approval' || a.yesno) && html`${key('y', 'yes')}${key('n', 'no')}`}${f.kind !== 'approval' && key('1-' + Math.min(9, a.options.length), 'option')}${key('esc', 'composer')}</div>
    ${a.err.value && html`<div class="aerr">${a.err.value}</div>`}
  </div>`;
}

// A settled ask, folded to one line: what it asked and what was picked (the
// note too, when this page sent it). Opened (click or Enter) it lists every
// choice with the picked one marked, so the choices can still be read while
// the composer talks about them ("chat about it").
function Settled({ a }) {
  const open = useMemo(() => signal(false), [a]);
  const d = a.done.value, f = a.f, approval = f.kind === 'approval';
  const what = approval ? `approve: ${f.tool} ${rend(f.tool, 'input', f.input)}`.trim() : `question: ${f.prompt || ''}`;
  const picked = { answered: d.answer, cancelled: 'cancelled', elsewhere: 'answered elsewhere' }[d.how];
  const cls = d.how !== 'answered' ? 'faint' : d.answer === a.no ? 'r' : approval ? 'g' : 'y';
  const tail = [d.note && `note: ${d.note}`, a.chat && 'chat about it'].filter(Boolean).join(' · ');
  const labels = approval ? [a.yes, a.always, a.no].filter(Boolean).map((label) => ({ label })) : a.options;
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

function Waiting({ s, v }) {
  const off = v.off.value;
  const asks = s.asks.value.filter((a) => off.has(a.id));
  if (!asks.length) return null;
  return html`<div class="waiting"><span>${asks.length} waiting</span><span class="faint">·</span>
    <${K} on=${() => focusAsk(v.p, asks[0])}>jump</${K}></div>`;
}

// ---------------------------------------------------------------- draft
// Streamed text goes straight into one text node with appendData, so each
// delta costs its own length (review #10), and nothing else re-renders.
function Stream({ d, which }) {
  const ref = useRef(null);
  useLayoutEffect(() => {
    const node = document.createTextNode((which === 'think' ? d.think : d.text).join(''));
    ref.current.replaceChildren(node);
    const sink = (t) => node.appendData(t);
    const set = which === 'think' ? d.thinkSinks : d.sinks; // one sink per pane showing the draft
    set.add(sink);
    return () => { set.delete(sink); };
  }, [d]);
  return html`<span ref=${ref}></span>`;
}

function DraftView({ s }) {
  const d = s.draft.value;
  const open = useMemo(() => signal(false), [d]);
  if (!d) return null;
  const lines = d.thinkLines.value;
  const tools = d.tools.value;
  return html`<div class="row draft" key=${d.key}>
    ${s.lastWho !== 'bot' && html`<div class="who bot">minerva</div>`}
    ${lines > 0 && html`<div class="think"><${K} on=${() => { open.value = !open.value; }}>${(open.value ? 'v' : '>') + ` thinking (${lines} lines)`}</${K}>
      <div class="body" hidden=${!open.value}><${Stream} d=${d} which="think" /></div></div>`}
    <div class="body"><${Stream} d=${d} which="text" /><span class="caret"></span></div>
    ${tools.map((t) => html`<div class="tool"><div class="tline"><span class="arrow">-></span><span class="name">${t.name}</span>
      <span class="sum faint">…${t.bytes.value ? ' ' + kfmt(t.bytes.value) + ' B' : ''}</span><span class="st run"></span></div></div>`)}
  </div>`;
}

// ---------------------------------------------------------------- composer
// Square double gold frame, flat tint. While a turn runs the frame's border
// pulses and the Life strip runs in its title, as the TUI draws it in the
// prompt frame's title (crates/tui/ui/default.rn:494-521).
function Pulse({ s }) {
  if (s.running.value !== true) return html`<span class="agent" title="at rest">${AGENT}</span>`;
  const strip = s.strip.value;
  const ms = s.clock.value, known = s.t0 != null;
  const secs = Math.floor(ms / 1000);
  const clock = secs >= 60 ? `${Math.floor(secs / 60)}m${secs % 60}s` : `${secs}.${Math.floor(ms / 100) % 10}s`;
  const ctx = s.ctx.value;
  const gauge = [known ? clock : null, ctx != null ? `⇡ ${kfmt(ctx)}` : null].filter(Boolean).join(' · ');
  return html`<span class="life" aria-hidden="true">${[...strip].map((c, x) => html`<span style=${'color:' + shade(x, CELLS)}>${c}</span>`)}</span>
    <span class="word">  ${word(s.finished.value)}</span>${gauge && html`<span class="gauge"> ${gauge}</span>`}`;
}

function Composer({ s, p, act }) {
  const ta = useRef(null);
  const text = useMemo(() => signal(''), []);
  const run = s.running.value, bye = s.bye || s.conn.value === 'goodbye';
  useEffect(() => {
    // A blurred textarea keeps its selection, so focus() puts the caret back where it was.
    p.focusComposer = () => ta.current && ta.current.focus();
    // Text put in front of what is already typed, the caret at the end.
    p.prefill = (text) => {
      const el = ta.current;
      el.value = text + el.value;
      autosize();
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    };
  }, [p]);
  const autosize = () => {
    const el = ta.current;
    el.style.height = 'auto';
    el.style.height = el.scrollHeight + 'px';
    if (text.value !== el.value) text.value = el.value;
  };
  const go = async (how) => {
    const el = ta.current;
    if (how === 'stop') { s.cancel(); return; }
    if (!el.value.trim()) { el.focus(); return; }
    const sent = el.value;
    if (await s.submit(how, sent) && el.value === sent) { el.value = ''; autosize(); }
  };
  const onKey = (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !e.ctrlKey) {
      e.preventDefault();
      go(e.altKey ? 'queue' : run === true ? 'steer' : 'send');
    } else if (e.key === 'Tab' && !e.shiftKey) {
      // Tab from the composer goes to the oldest pending ask on the page.
      if (act.focusHead()) e.preventDefault();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      ta.current.blur();
      ta.current.closest('.pane').focus();
    } else if (e.key === 'c' && e.ctrlKey && !ta.current.value && run !== false) {
      e.preventDefault();
      s.cancel();
    }
  };
  const enter = run === true ? 'steer' : 'send';
  const w = (how, label, on, keyHint) => html`<${K} off=${!on || bye} on=${() => go(how)} cls="verb">${label}${keyHint && html`<span class="kh"> ${keyHint}</span>`}</${K}>`;
  const sep = html`<span class="faint"> · </span>`;
  return html`<div class=${'composer' + (run === true ? ' running' : '')}>
    <div class="ftitle"><span class="mode">${run === true ? 'steer' : 'message'}</span> <${Pulse} s=${s} /></div>
    <textarea ref=${ta} rows="1" spellcheck="false" placeholder=${bye ? 'this session said goodbye' : 'say something to minerva'}
      aria-label="message" onInput=${autosize} onKeyDown=${onKey} onFocus=${() => act.focus(p.id)}></textarea>
    <div class="fbot">
      ${w('send', 'send', run !== true, enter === 'send' ? '⏎' : '')}${sep}${w('steer', 'steer', run !== false, enter === 'steer' ? '⏎' : '')}${sep}${w('queue', 'queue', run !== false, '⌥⏎')}${sep}${w('stop', 'stop', run !== false, '^c')}
      ${s.queued.value > 0 && html`${sep}<span class="y">${s.queued.value} queued</span>`}
      ${text.value.startsWith(':') && html`${sep}<span class="cwarn">: goes to the model; commands live in the status line</span>`}
    </div>
  </div>`;
}
