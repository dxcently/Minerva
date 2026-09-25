// One session pane: the transcript, the draft, inline asks, the composer.
// It draws a core/state.js Session and holds no session state of its own.
// Several panes may draw one Session (mirrors share its one stream), so
// everything tied to the DOM (scroller, ask elements, which asks are
// off-screen) lives in a per-PANE view, never on the Session.
import { html, K, Rendered, Component, signal, useRef, useLayoutEffect, useEffect, useMemo } from '../core/ui.js';
import { markdown } from '../core/markdown.js';
import { render as rend } from '../renderers/index.js';
import { icon, word, shade, CELLS } from '../core/life.js';
import { tildify, kfmt } from '../core/state.js';

const AGENT = icon();
const HOVER_MS = 300;   // a mouse pointer must rest on an ask this long before a click arms allow
const CONFIRM_MS = 400; // and the confirming click must come at least this long after the arming one

// Per-pane view helpers: the scroller, ask elements, which asks are off-screen.
const views = new WeakMap();
function viewOf(p) {
  let v = views.get(p);
  if (!v) {
    v = { scroller: null, io: null, off: signal(new Set()), askEls: new Map() };
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

// Focus the ask block `a` in pane `p` (keys reach an ask only while it holds focus).
export function focusAsk(p, a) {
  const el = a && askEl(p, a.id);
  if (!el) return false;
  el.scrollIntoView({ block: 'nearest' });
  el.focus({ preventScroll: true });
  return true;
}

// ---------------------------------------------------------------- the pane
export function Pane({ p, rect, focused, menuOpen, act }) {
  const s = p.s, v = viewOf(p);
  const style = `left:${rect.x - (rect.x > 0 ? 1 : 0)}px;top:${rect.y - (rect.y > 0 ? 1 : 0)}px;` +
    `width:${rect.w + (rect.x > 0 ? 1 : 0)}px;height:${rect.h + (rect.y > 0 ? 1 : 0)}px`;
  const banner = s.banner.value;
  return html`<section class=${'pane frame' + (focused ? ' focus' : '')} style=${style} tabindex="-1"
      data-pane=${p.id} onMouseDown=${() => act.focus(p.id)}>
    <${Title} p=${p} act=${act} />
    ${menuOpen && html`<${PaneMenu} p=${p} act=${act} />`}
    <${Transcript} s=${s} v=${v} />
    ${banner && html`<div class=${'banner' + (banner.err ? ' err' : '')}>${banner.text}</div>`}
    <${Waiting} s=${s} v=${v} />
    <${Composer} s=${s} p=${p} act=${act} />
  </section>`;
}

function Title({ p, act }) {
  const s = p.s, conn = s.conn.value, hl = s.hello.value;
  const dot = { live: 'g', replaying: 'y', connecting: 'y', reconnecting: 'y', goodbye: 'faint' }[conn] || 'r';
  const open = (e) => { e.preventDefault(); e.stopPropagation(); act.menu(p.id); };
  return html`<div class="ftitle k ptitle" role="button" tabindex="0" title="pane menu (click or right-click)"
      onClick=${open} onContextMenu=${open} onKeyDown=${(e) => !e.repeat && (e.key === 'Enter' || e.key === ' ') && open(e)}>
    ${hl ? tildify(hl.cwd) : 'session'} <span class=${dot}>●</span>
    ${conn !== 'live' && html` <span class="faint">${conn}</span>`}
    ${p.mirror && html` <span class="faint">(mirror)</span>`} <span class="faint">▾</span>
  </div>`;
}

function PaneMenu({ p, act }) {
  const item = (label, fn, hint) => html`<div class="it"><${K} on=${() => { act.menu(null); fn(); }}>${label}</${K}>
    <span class="faint">${hint}</span></div>`;
  return html`<div class="pmenu frame" onMouseDown=${(e) => e.stopPropagation()}>
    ${item('mirror', () => act.mirror(p.id), ':vsplit')}
    ${item('reconnect', () => p.s.reconnect(), ':reconnect')}
    ${item('close', () => act.close(p.id), '⌥w  :q')}
  </div>`;
}

// ---------------------------------------------------------------- transcript
function Transcript({ s, v }) {
  const ref = useRef(null), inner = useRef(null), stick = useRef(true);
  const chunks = s.chunks.value;
  s.stick.value; // subscribe: caught-up asks for the bottom
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
  // A click in the transcript off an ask arms nothing: the click moves focus
  // to the pane (tabindex -1), so no ask block holds it.
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

function RowBody({ r, s, v }) {
  switch (r.kind) {
    case 'you':
      return html`<div class="row"><${Who} who=${r.who} /><div class="body">${r.text}</div>
        ${r.images > 0 && html`<div class="body faint">[${r.images} image${r.images > 1 ? 's' : ''}]</div>`}</div>`;
    case 'bot': {
      // A reply an `error` cut short is filled in place by its assistant-message.
      const full = r.full && r.full.value, x = full || r, cut = full ? null : r.cut;
      return html`<div class="row"><${Who} who=${r.who} />
        ${x.thinking && html`<${Fold} cls="think" label=${`thinking (${x.thinking.split('\n').length} lines)`}>${x.thinking}</${Fold}>`}
        ${x.redacted > 0 && html`<div class="think">(${x.redacted} redacted thinking block${x.redacted > 1 ? 's' : ''})</div>`}
        ${x.text && html`<div class="body">${cut ? x.text : markdown(x.text)}</div>`}
        ${cut && html`<div class="sys y">(${cut})</div>`}</div>`;
    }
    case 'sys': return html`<div class=${'row sys ' + (r.cls || '')}>${r.text}</div>`;
    case 'verdict': {
      const f = r.f, bad = ['refused', 'declined', 'yolo'].includes(f.outcome);
      return html`<div class=${'row sys' + (bad ? ' err' : '')}>policy: ${f.tool} [${f.outcome}] ${f.reason}${f.note ? ' · ' + f.note : ''}
        <span class="dim"> (not matched to a call)</span></div>`;
    }
    case 'settle': return html`<div class="row settle">${r.text}</div>`;
    case 'fold': return html`<div class="row"><${Fold} cls="sys" label=${r.label}>${r.text}</${Fold}></div>`;
    case 'peer': return html`<div class="row peer"><span class="b">~ ${r.from}</span>  ${r.text}</div>`;
    case 'pre': return html`<div class="row"><pre>${r.text}</pre></div>`;
    case 'tool': return html`<${ToolRow} c=${r.call} s=${s} v=${v} />`;
    case 'ask': {
      const done = r.ask.done.value;
      if (done) return done.line ? html`<div class=${'row sys ' + done.cls}>${done.line}</div>` : null;
      return html`<div class="row"><${AskBlock} a=${r.ask} s=${s} v=${v} /></div>`;
    }
    default: return null;
  }
}

// ---------------------------------------------------------------- tool lines
const VD = { refused: 'r', declined: 'r', yolo: 'r', judged: 'y', approved: 'g' };

function ToolRow({ c, s, v: view }) {
  const st = c.st.value, out = c.out.value, v = c.verdict.value, a = c.ask.value, settled = c.settled.value;
  const open = useMemo(() => signal(null), []);
  const shown = open.value ?? (out && out.isError);
  const sum = useMemo(() => rend(c.name, 'input', c.input), [c.input]);
  const body = useMemo(() => (out ? rend(c.name, 'output', out.output, out.isError) : null), [out]);
  const toggle = () => { if (out) open.value = !shown; };
  return html`<div class="row tool">
    <div class=${'tline' + (out ? ' k' : '')} onClick=${toggle}>
      <span class="arrow">-></span><span class="name">${c.name}</span>
      <span class="sum" title=${sum}>${sum}</span>
      ${c.origin !== 'model' && html`<span class="faint">(${c.origin})</span>`}
      ${v && html`<span class=${'vd ' + (VD[v.outcome] || 'faint')} title=${v.reason + (v.note ? ' · ' + v.note : '')}>[${v.outcome}]</span>`}
      ${c.fired.value && html`<span class="vd">[fired]</span>`}
      <span class=${'st ' + st}>${{ run: 'running', ok: 'ok ▸', err: 'error ▸', stop: 'stopped' }[st]}</span>
      <span class="tm">${c.time.value}</span>
    </div>
    ${shown && body && html`<${Rendered} value=${body} />`}
    ${a && html`<${AskBlock} a=${a} s=${s} v=${view} />`}
    ${settled && html`<div class=${'sys ' + settled.cls}>${settled.line}</div>`}
  </div>`;
}

// ---------------------------------------------------------------- asks
// Keys reach an ask only while its block holds focus (Tab or a click puts it
// there), and the block shows that with a mark. Only this block's handler
// acts on keys inside it (K ignores keys inside an ask, core/ui.js).
// Approve by key: exactly `y` (or `Y`), then Enter; `n` denies at once. Held
// keys (e.repeat) are ignored, and a bare modifier (Shift, Caps Lock...)
// neither arms nor disarms. Enter or Space never completes a click confirm:
// on an armed [confirm allow?] they disarm it.
// Approve by click: a first click arms [allow] -> [confirm allow?] for 3 s,
// and a second click at least CONFIRM_MS after the arming one allows,
// whatever e.detail says (so a double-click, which lands sooner, never
// confirms, and slow repeated clicks are not refused). With a mouse, the
// pointer must also have rested on the ask HOVER_MS before a click arms (an
// ask drawn under a moving pointer is not armed by a click already under
// way). Touch, pen and pointerless clicks (a screen reader's activate) have
// no hover to rest, so they arm on the first tap. A click that does nothing
// yet (still in the rest window, or too soon to confirm) says "steady…".
// [deny] is one click.
// The arm belongs to this block, so to this pane: a mirror's [allow] is not
// armed by this one, and closing the pane drops its arm.
// A question whose options are exactly yes/no (a.yesno, core confirm())
// takes the same two steps for yes; other questions answer on one key/click.
const MODIFIERS = new Set(['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'AltGraph', 'Fn', 'NumLock', 'OS']);
const LAPSE_MS = 3000;

function AskBlock({ a, s, v }) {
  const ref = useRef(null), hover = useRef(null), down = useRef(null), timer = useRef(0), steadyT = useRef(0);
  const arm = useMemo(() => signal(null), [a]);       // null | { how: 'key' | 'click', at }
  const steady = useMemo(() => signal(false), [a]);
  const f = a.f, busy = a.busy.value, armed = arm.value, how = armed && armed.how;
  const two = f.kind === 'approval' || a.yesno;
  useLayoutEffect(() => {
    const el = ref.current;
    v.askEls.set(a.id, el);
    v.observe(el);
    return () => {
      v.unobserve(el, a.id);
      if (v.askEls.get(a.id) === el) v.askEls.delete(a.id);
      clearTimeout(timer.current); clearTimeout(steadyT.current);
    };
  }, [a]);
  const setArm = (h) => {
    clearTimeout(timer.current);
    arm.value = h ? { how: h, at: performance.now() } : null;
    if (h === 'click') timer.current = setTimeout(() => { arm.value = null; }, LAPSE_MS);
  };
  const say = () => {
    steady.value = true;
    clearTimeout(steadyT.current);
    steadyT.current = setTimeout(() => { steady.value = false; }, 900);
  };
  const onKey = (e) => {
    if (e.repeat || e.ctrlKey || e.metaKey || e.altKey || MODIFIERS.has(e.key)) return;
    const stop = () => { e.preventDefault(); e.stopPropagation(); };
    const cur = arm.peek();
    if (two) {
      if (e.key === 'y' || e.key === 'Y') { stop(); setArm('key'); return; }
      if (e.key === 'n' || e.key === 'N') { stop(); setArm(null); s.answer(a, a.no); return; }
      if (e.key === 'Enter' && cur && cur.how === 'key') { stop(); setArm(null); s.answer(a, a.yes); return; }
    } else {
      const o = a.options.find((x) => x.key === e.key);
      if (o) { stop(); s.answer(a, o.label); return; }
    }
    if (e.key === 'Escape') { stop(); setArm(null); ref.current.closest('.pane').focus(); return; }
    if (e.key === 'Enter' || e.key === ' ') { stop(); if (cur) setArm(null); return; }
    if (e.key !== 'Tab' && cur) setArm(null);
  };
  const allow = (e) => {
    const now = performance.now();
    const cur = arm.peek();
    if (cur && cur.how === 'click') {
      if (now - cur.at >= CONFIRM_MS) { setArm(null); s.answer(a, a.yes); } else say();
      return;
    }
    // The pointer that made this click: the click's own pointerType where the
    // browser gives one, else the last pointerdown on this ask; '' = none.
    const d = down.current;
    const type = e.pointerType != null ? e.pointerType : d && now - d.at < 1500 ? d.type : '';
    if (type === 'mouse' && (hover.current == null || now - hover.current < HOVER_MS)) { say(); return; }
    setArm('click');
  };
  const yesWord = f.kind === 'approval' ? 'allow' : a.yes;
  let title, body, keys, hint;
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
  if (two) {
    keys = html`
      <${K} cls=${'allow' + (how === 'click' ? ' armed' : '')} on=${allow}>${how === 'click' ? `[confirm ${yesWord}?]` : `[${yesWord}]`}${steady.value && html`<span class="faint steady"> steady…</span>`}</${K}>
      <${K} cls="deny" on=${() => { setArm(null); s.answer(a, a.no); }}>[${f.kind === 'approval' ? 'deny' : a.no}]</${K}>`;
    hint = how === 'key' ? html`<span class="armed">⏎ to confirm ${yesWord}</span> <span class="faint">· any other key cancels</span>`
      : html`<span class="key">y</span> ${yesWord} · <span class="key">n</span> ${f.kind === 'approval' ? 'deny' : a.no}`;
  } else {
    keys = a.options.map((o) => html`<${K} cls="opt" on=${() => s.answer(a, o.label)}>
      [${o.key ? o.key + ' ' : ''}${o.label}]${o.description && html`<span class="desc">  ${o.description}</span>`}</${K}>`);
    hint = html`<span class="key">1-${Math.min(9, a.options.length)}</span> answer`;
  }
  return html`<div class=${'ask' + (a.fresh ? ' fresh' : '') + (busy ? ' busy' : '')} ref=${ref} tabindex="0"
      data-ask=${a.id} onKeyDown=${onKey}
      onPointerDown=${(e) => { down.current = { type: e.pointerType, at: performance.now() }; }}
      onPointerEnter=${(e) => { if (e.pointerType === 'mouse') hover.current = performance.now(); }}
      onPointerMove=${(e) => { if (e.pointerType === 'mouse' && hover.current == null) hover.current = performance.now(); }}
      onPointerLeave=${(e) => { if (e.pointerType === 'mouse') hover.current = null; }}
      onBlur=${(e) => {
        const cur = arm.peek();
        if (cur && cur.how === 'key' && !e.currentTarget.contains(e.relatedTarget)) setArm(null);
      }}>
    <div class="ftitle">${title}</div>
    ${body}
    <div class="keys">${keys}</div>
    <div class="akeys">keys: ${hint}</div>
    ${a.err.value && html`<div class="aerr">${a.err.value}</div>`}
  </div>`;
}

function Waiting({ s, v }) {
  const off = v.off.value;
  const asks = s.asks.value.filter((a) => off.has(a.id));
  if (!asks.length) return null;
  return html`<div class="waiting"><span>${asks.length} waiting</span><span class="faint">·</span>
    <${K} on=${() => { const el = v.askEls.get(asks[0].id); if (el) { el.scrollIntoView({ block: 'center' }); el.focus({ preventScroll: true }); } }}>jump</${K}></div>`;
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
  useEffect(() => { p.focusComposer = () => ta.current && ta.current.focus(); }, [p]);
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
    } else if (e.key === 'Tab' && !e.shiftKey && s.asks.value.length) {
      // Tab from the composer goes to the oldest pending ask, if this pane draws it.
      if (focusAsk(p, s.asks.value[0])) e.preventDefault();
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
