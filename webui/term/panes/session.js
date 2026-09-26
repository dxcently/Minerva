// One session pane: the transcript, the draft, the ask panel (when the
// current ask is this pane's, panes/asks.js), the composer. It draws a
// core/state.js Session and holds no session state of its own. Several panes
// may draw one Session (mirrors share its one stream), so what is tied to the
// DOM lives in a per-PANE view, never on the Session.
import { html, K, HUB, Rendered, Component, signal, effect, useRef, useLayoutEffect, useEffect, useMemo, useState } from '../core/ui.js';
import { IMAGE_TYPES, MAX_IMAGES, looksImage, readImage, readText, tooBig } from '../core/attach.js';
import { markdown } from '../core/markdown.js';
import { render as rend, renderCall } from '../renderers/index.js';
import { icon, word, shade, CELLS } from '../core/life.js';
import { tildify, kfmt, hhmm, stateOf, STATE_CLS, MODES } from '../core/state.js';
import { openMenu, atPointer, under } from '../core/menu.js';
import { AskPanel, AskMark, Settled, quoted } from './asks.js';

const AGENT = icon();

// Per-pane view helpers: what the row menus reach (the pane and the actions).
const views = new WeakMap();
function viewOf(p) {
  let v = views.get(p);
  if (!v) { v = { p }; views.set(p, v); }
  return v;
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
    ${act.panelPane.value === p.id && html`<${AskPanel} p=${p} act=${act} />`}
    <${Outbox} p=${p} />
    <${Composer} s=${s} p=${p} act=${act} />
  </section>`;
}

// Session name, its menu, the project chip, the connection dot.
function Title({ p, mirror, act }) {
  const s = p.s, conn = s.conn.value, hl = s.hello.value;
  const dot = { live: 'hi', replaying: 'y', connecting: 'y', reconnecting: 'y', goodbye: 'faint' }[conn] || 'r';
  const open = (e, at) => { e.preventDefault(); e.stopPropagation(); act.paneMenu(p, at); };
  return html`<div class="ftitle k ptitle" role="button" tabindex="0" title=${'pane menu (click or right-click)' + (hl ? '\n' + hl.cwd : '')} data-owner=${'pane-' + p.id}
      onClick=${(e) => open(e, under(e.currentTarget))} onContextMenu=${(e) => open(e, atPointer(e))}
      onKeyDown=${(e) => !e.repeat && (e.key === 'Enter' || e.key === ' ') && open(e, under(e.currentTarget))}>
    <span class="b">${act.name(s)}</span>${mirror && html` <span class="faint">(mirror)</span>`} <span class="faint">▾</span>
    ${hl && html` <span class="proj">[${act.project(hl.cwd)}]</span>`} <span class=${dot}>●</span>
    ${conn !== 'live' && html` <span class="faint">${conn}</span>`}
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
  const chunks = s.chunks.value, jumps = s.jumpToEnd.value;
  useLayoutEffect(() => { stick.current = true; }, [jumps]);
  useLayoutEffect(() => {
    const el = ref.current;
    const pin = new ResizeObserver(() => { if (stick.current) el.scrollTop = el.scrollHeight; });
    pin.observe(inner.current);
    pin.observe(el);
    return () => pin.disconnect();
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
      ${!chunks.length && !s.draft.value && html`<${Start} s=${s} v=${v} />`}
      ${chunks.map((c) => html`<${Chunk} key=${c.key} c=${c} s=${s} v=${v} />`)}
      <${DraftView} s=${s} />
    </div>
  </div>`;
}

// A session nobody has spoken in opens on the start screen, as the TUI's
// does (default.rn:253-285): the owl, what the session runs on (read live),
// and enough keys to begin; `?` has the rest.
function Start({ s, v }) {
  const hl = s.hello.value;
  if (!hl) return null;
  const line = (k, val) => html`<div><span class="faint">  ${k}  </span>${val}</div>`;
  const key = (k, what) => html`<div>  <span class="kk">${k}</span>  ${what}</div>`;
  return html`<div class="start">
    <div class="owl big">${'   ,___,\n   ['}<span class="eyes">O.o</span>${']    '}<span class="name">minerva</span>${'\n   /)__)    '}<span class="faint">Make it, Break it, Hack it.</span>${'\n  ---"-"---'}</div>
    ${line('session ', v.act.name(s))}
    ${line('model   ', html`<span class="c b">${hl.model || '?'}</span>`)}
    ${line('persona ', hl.persona ? html`<span class="m b">${hl.persona}</span>` : 'none')}
    ${line('gate    ', hl.yolo ? html`<span class="r b">YOLO</span>` : 'asks')}
    ${line('cwd     ', tildify(hl.cwd))}
    <div class="gap"></div>
    ${key('i', 'write a message · ⏎ sends · ⇧⏎ is a newline · esc leaves the composer')}
    ${key(':', 'a command · tab completes')}
    ${key('tab', 'the oldest waiting ask · h l between asks')}
    ${key('?', 'the key reference')}
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

// The speaker's name on the first row of a run, and beside it, dim, what is
// known of the run: its time (the record's, else when it arrived; none for a
// replayed record without one) and, for the agent, what its turns cost.
function Who({ r, at = r.at }) {
  if (!r.who) return null;
  const m = r.meta && r.meta.value;
  const bits = [at != null ? hhmm(at) : null];
  if (m && (m.in || m.out)) bits.push(`${kfmt(m.in)} in`, `${kfmt(m.out)} out`);
  if (m && m.ms) bits.push(secs(m.ms));
  const info = bits.filter(Boolean).join(' · ');
  return html`<div class=${'who ' + (r.who === 'you' ? 'you' : 'bot')}>${r.who === 'you' ? 'you' : 'minerva'}${info && html`<span class="wt" title=${at != null ? new Date(at).toLocaleString() : ''}>  ${info}</span>`}</div>`;
}
const secs = (ms) => (ms >= 60000 ? `${Math.floor(ms / 60000)}m${Math.round((ms % 60000) / 1000)}s` : `${Math.round(ms / 100) / 10}s`);

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
    case 'day': return html`<div class="row day" role="separator">${r.text}</div>`;
    case 'you':
      return html`<div class="row you" onContextMenu=${cm}><${Who} r=${r} /><div class="body">${r.text}</div>
        ${r.images > 0 && html`<div class="body faint">[${r.images} image${r.images > 1 ? 's' : ''}]</div>`}</div>`;
    case 'bot': {
      // A reply an `error` cut short is filled in place by its assistant-message.
      const full = r.full && r.full.value, x = full || r, cut = full ? null : r.cut;
      return html`<div class="row" onContextMenu=${cm}><${Who} r=${r} />
        ${x.thinking && html`<${Fold} cls="think" label=${`thinking (${x.thinking.split('\n').length} lines)`}>${x.thinking}</${Fold}>`}
        ${x.redacted > 0 && html`<div class="think">(${x.redacted} redacted thinking block${x.redacted > 1 ? 's' : ''})</div>`}
        ${x.text && html`<div class="body">${cut ? x.text : markdown(x.text)}</div>`}
        ${cut && html`<div class="sys y">(${cut})</div>`}</div>`;
    }
    case 'sys': return html`<div class=${'row sys ' + (r.cls || '')} onContextMenu=${cm}>${r.text}</div>`;
    case 'verdict': {
      const f = r.f, bad = ['refused', 'declined'].includes(f.outcome);
      return html`<div class=${'row sys' + (bad ? ' err' : '')} onContextMenu=${cm}>policy: ${f.tool} [${f.outcome}] ${f.reason}${f.note ? ' · ' + f.note : ''}
        <span class="dim"> (not matched to a call)</span></div>`;
    }
    case 'settle': return html`<div class="row settle" onContextMenu=${cm}>${r.text}</div>`;
    case 'fold': return html`<div class="row" onContextMenu=${cm}><${Fold} cls="sys" label=${r.label}>${r.text}</${Fold}></div>`;
    case 'peer': return html`<div class="row peer" onContextMenu=${cm}><span class="b">~ ${r.from}</span>  ${r.text}</div>`;
    case 'pre': return html`<div class="row" onContextMenu=${cm}><pre>${r.text}</pre></div>`;
    case 'tool': return html`<${ToolRow} c=${r.call} s=${s} v=${v} cm=${cm} />`;
    case 'ask':
      return html`<div class="row" onContextMenu=${cm}>${r.ask.done.value ? html`<${Settled} a=${r.ask} />` : html`<${AskMark} a=${r.ask} s=${s} act=${v.act} />`}</div>`;
    default: return null;
  }
}

// ---------------------------------------------------------------- tool lines
const VD = { refused: 'r', declined: 'r' };

function ToolRow({ c, s, v, cm }) {
  const st = c.st.value, out = c.out.value, vd = c.verdict.value, a = c.ask.value, settled = c.settled.value;
  const open = useMemo(() => signal(null), []);
  const shown = open.value ?? (out && out.isError);
  const sum = useMemo(() => rend(c.name, 'input', c.input), [c.input]);
  const body = useMemo(() => (out ? renderCall(c.input, c.name, 'output', out.output, out.isError) : null), [out]);
  const preview = useMemo(() => renderCall(c.input, c.name, 'preview', c.input), [c.input]);
  const toggle = () => { if (out) open.value = !shown; };
  return html`<div class="row tool" onContextMenu=${cm}>
    <div class=${'tline' + (out ? ' k' : '')} onClick=${toggle}>
      <span class="arrow">-></span><span class="name">${c.name}</span>
      <span class="sum" title=${sum}>${sum}</span>
      ${c.origin !== 'model' && html`<span class="faint">(${c.origin})</span>`}
      ${vd && html`<span class=${'vd ' + (VD[vd.outcome] || '')} title=${vd.reason + (vd.note ? ' · ' + vd.note : '')}>[${vd.outcome}]</span>`}
      ${c.fired.value && html`<span class="vd">[fired]</span>`}
      <span class=${'st ' + st}>${{ run: 'running', ok: 'ok ▸', err: 'error ▸', stop: 'stopped' }[st]}</span>
      <span class="tm">${c.time.value}</span>
    </div>
    ${preview && html`<${Rendered} value=${preview} />`}
    ${shown && body && html`<${Rendered} value=${body} />`}
    ${a && html`<${AskMark} a=${a} s=${s} act=${v.act} />`}
    ${settled && html`<${Settled} a=${settled} />`}
  </div>`;
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
  const first = s.lastWho !== 'bot';
  return html`<div class="row draft" key=${d.key}>
    ${first && html`<${Who} r=${{ who: 'bot', at: d.at }} />`}
    ${lines > 0 && html`<div class="think"><${K} on=${() => { open.value = !open.value; }}>${(open.value ? 'v' : '>') + ` thinking (${lines} lines)`}</${K}>
      <div class="body" hidden=${!open.value}><${Stream} d=${d} which="think" /></div></div>`}
    <div class="body"><${Stream} d=${d} which="text" /><span class="caret"></span></div>
    ${tools.map((t) => html`<div class="tool"><div class="tline"><span class="arrow">-></span><span class="name">${t.name}</span>
      <span class="sum faint">…${t.bytes.value ? ' ' + kfmt(t.bytes.value) + ' B' : ''}</span><span class="st run"></span></div></div>`)}
  </div>`;
}

// ---------------------------------------------------------------- composer
// The composer is the TUI's prompt frame (crates/tui/ui/default.rn:226-521):
// one square frame in the session's state colour (stateOf: the status
// line's badge wears the same), titled on the left, the agent on the right
// of its top border. While a turn runs the agent grows into the Life strip,
// with the word and the gauge to its left, and the frame pulses. The strip
// has a fixed slot of CELLS cells, each one cell wide, so nothing moves as
// it steps whatever font draws the braille. The bottom border holds the
// menus (attach, model, effort, mode, mic) on the left, the verbs on the right.
function Pulse({ s }) {
  if (s.running.value !== true) return html`<span class="slot rest" title="at rest">${cells(AGENT)}</span>`;
  const ms = s.clock.value, known = s.t0 != null;
  const secs = Math.floor(ms / 1000);
  const clock = secs >= 60 ? `${Math.floor(secs / 60)}m${secs % 60}s` : `${secs}.${Math.floor(ms / 100) % 10}s`;
  const ctx = s.ctx.value;
  const gauge = [known ? clock : null, ctx != null ? `⇡ ${kfmt(ctx)}` : null].filter(Boolean).join(' · ');
  return html`<span class="word">${word(s.finished.value)}</span>${gauge && html`<span class="gauge"> ${gauge}</span>`}<span class="faint"> </span><span
    class="slot life" aria-hidden="true">${cells(s.strip.value, true)}</span>`;
}
const cells = (t, shaded) => [...t].map((c, x) => html`<span class="cell" style=${shaded ? 'color:' + shade(x, CELLS) : null}>${c}</span>`);

// ---------------------------------------------------------------- the outbox
// Messages queued (⌥⏎) while a turn runs are held here, in the pane, not on
// the door: the door's own queue (core follow_up) has no route to edit or
// drop a message, and these stay editable until the model reads them. When
// the turn ends (turn-state idle) the first is sent, which starts the next
// turn; the rest wait for that one to end, in order. Each row: its text (one
// line; a click shows it whole), `edit` (back into the composer; ⏎ puts it
// back in its place, Esc leaves it as it was), `steer ↑` (send it now, into
// the running turn) and `×` (drop it). ↑ in an empty composer edits the last.
let qSeq = 0;
export const enqueue = (p, text, images = []) => { p.queue.value = [...p.queue.peek(), { id: ++qSeq, text, images }]; };

function Outbox({ p }) {
  const list = p.queue.value, ed = p.editing.value;
  const open = useMemo(() => signal(new Set()), []);
  if (!list.length) return null;
  const flip = (id) => { const o = new Set(open.peek()); if (!o.delete(id)) o.add(id); open.value = o; };
  const drop = (id) => { p.queue.value = p.queue.peek().filter((m) => m.id !== id); };
  const steer = async (m) => {
    const at = p.queue.peek().indexOf(m);
    drop(m.id);
    if (!(await p.s.submit('steer', m.text, m.images))) {
      const q = [...p.queue.peek()];
      q.splice(Math.min(at, q.length), 0, m);
      p.queue.value = q;
    }
  };
  const one = (t) => t.replace(/\s+/g, ' ').trim();
  return html`<div class="outbox" onMouseDown=${(e) => e.preventDefault()}>
    <div class="ohead faint">queued · sent in order when the turn ends</div>
    ${list.map((m, i) => {
      const editing = ed && ed.id === m.id, whole = open.value.has(m.id);
      return html`<div class=${'qm' + (editing ? ' editing' : '')} data-q=${m.id}>
        <span class="faint">${i + 1} </span>${m.images.length > 0 && html`<span class="c">[img${m.images.length > 1 ? '×' + m.images.length : ''}] </span>`}<${K} cls=${'qt' + (whole ? ' whole' : '')} title=${whole ? 'fold' : 'show it whole'} on=${() => flip(m.id)}>${whole ? m.text : one(m.text)}</${K}>
        <span class="qv">${editing ? html`<span class="o">editing in the composer</span>`
          : html`<${K} cls="qe" title="back into the composer; ⏎ puts it back here" on=${() => p.editQueued && p.editQueued(m.id)}>edit</${K}><span class="faint"> · </span><${K}
              cls="qs" title="send it now, into the running turn" on=${() => steer(m)}>steer ↑</${K}><span class="faint"> · </span><${K} cls="qx" title="drop it" on=${() => drop(m.id)}>×</${K}>`}</span>
      </div>`;
    })}
  </div>`;
}

// The composer's bottom border: a line of menus (switching what is loaded
// needs the hub: the door restarts between turns) and the verbs. It fits the
// frame as the status line does: the verbs drop their keys, the labels drop
// to glyphs, every menu goes into one `⋯`, then only send and stop stay.
const RESTART = 'needs the hub (restarts the door between turns)';
const MIC = 'needs local dictation (hub + whisper.cpp)';
let attSeq = 0;

function Composer({ s, p, act }) {
  const ta = useRef(null), root = useRef(null), bar = useRef(null), fileIn = useRef(null);
  const text = useMemo(() => signal(''), []);
  const atts = useMemo(() => signal([]), []); // images going with the next message
  const [fit, setFit] = useState(0), [cw, setCw] = useState(0);
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
  const put = (t) => { const el = ta.current; el.value = t; autosize(); el.focus(); el.setSelectionRange(t.length, t.length); };
  // Editing a queued message: what was typed waits aside and comes back after.
  useEffect(() => {
    p.editQueued = (id) => {
      const m = p.queue.peek().find((x) => x.id === id);
      if (!m) return;
      const cur = p.editing.peek();
      p.editing.value = { id, stash: cur ? cur.stash : ta.current.value };
      put(m.text);
    };
  }, [p]);
  const endEdit = (save) => {
    const e = p.editing.peek(), t = ta.current.value;
    if (save) p.queue.value = p.queue.peek().flatMap((m) => (m.id !== e.id ? [m] : t.trim() || m.images.length ? [{ ...m, text: t }] : []));
    p.editing.value = null;
    put(e.stash);
  };
  // The turn ended: send the first held message. One at a time: the next
  // waits for the turn this one starts to end (`sent` until running is seen).
  useEffect(() => {
    let sent = false, t = 0;
    const stop = effect(() => {
      const run = s.running.value, q = p.queue.value, e = p.editing.value, live = s.conn.value === 'live';
      if (run === true) { sent = false; clearTimeout(t); return; }
      if (run !== false || sent || !q.length || !live || s.bye || (e && e.id === q[0].id)) return;
      const m = q[0];
      sent = true;
      t = setTimeout(() => { sent = false; }, 5000); // no turn-state came: try again
      p.queue.value = q.slice(1);
      s.submit('send', m.text, m.images).then((ok) => { if (!ok) { p.queue.value = [m, ...p.queue.peek()]; } });
    });
    return () => { stop(); clearTimeout(t); };
  }, [p, s]);
  const go = async (how) => {
    const el = ta.current;
    if (how === 'stop') { s.cancel(); return; }
    if (p.editing.peek()) { endEdit(true); return; }
    const imgs = atts.peek();
    if (!el.value.trim() && !imgs.length) { el.focus(); return; }
    const sent = el.value, big = tooBig(sent, imgs);
    if (big) { act.toast(big, true); return; }
    // Queued while a turn runs: held here, editable until it is sent.
    if (how === 'queue' && s.running.peek() === true) { enqueue(p, sent, imgs); el.value = ''; autosize(); atts.value = []; return; }
    if (await s.submit(how, sent, imgs) && el.value === sent) { el.value = ''; autosize(); atts.value = []; }
  };
  // ---- attachments: + attach, a paste, a drop
  const pick = (kind) => {
    const el = fileIn.current;
    el.accept = kind === 'image' ? IMAGE_TYPES.join(',') : '';
    el.dataset.kind = kind;
    el.value = '';
    el.click();
  };
  const insert = (t) => {
    const el = ta.current, a = el.selectionStart, b = el.selectionEnd;
    el.setRangeText((a > 0 && el.value[a - 1] !== '\n' ? '\n' : '') + t, a, b, 'end');
    autosize();
  };
  const take = async (files, kind) => {
    for (const f of files) {
      try {
        if (kind === 'image' || (kind !== 'text' && looksImage(f))) {
          if (atts.peek().length >= MAX_IMAGES) throw new Error(`at most ${MAX_IMAGES} images go with one message`);
          const a = await readImage(f);
          atts.value = [...atts.peek(), { ...a, id: ++attSeq }];
        } else insert(await readText(f));
      } catch (err) { act.toast(err.message, true); }
    }
    ta.current.focus();
  };
  const onPaste = (e) => {
    const fs = [...((e.clipboardData && e.clipboardData.files) || [])].filter(looksImage);
    if (fs.length) { e.preventDefault(); take(fs, 'image'); }
  };
  const files = (e) => e.dataTransfer && [...e.dataTransfer.types].includes('Files');
  // ---- the bottom border fits: 0 all, 1 verbs without keys (css), 2 glyphs, 3 one menu, 4 fewer verbs (css)
  useLayoutEffect(() => {
    const ro = new ResizeObserver(() => setCw(root.current ? root.current.clientWidth : 0));
    ro.observe(root.current);
    return () => ro.disconnect();
  }, []);
  const hl = s.hello.value, yolo = !!(hl && hl.yolo), model = (hl && hl.model) || '?';
  useLayoutEffect(() => { setFit(0); }, [cw, model, yolo, run, !!p.editing.value]);
  useLayoutEffect(() => {
    const el = bar.current;
    if (el && fit < 4 && el.scrollWidth > el.clientWidth + 1) setFit(fit + 1);
  });
  const onKey = (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !e.ctrlKey) {
      e.preventDefault();
      go(e.altKey ? 'queue' : run === true ? 'steer' : 'send');
    } else if (e.key === 'Escape' && p.editing.peek()) {
      e.preventDefault();
      endEdit(false);
    } else if (e.key === 'ArrowUp' && !ta.current.value && !p.editing.peek() && p.queue.peek().length) {
      e.preventDefault();
      const q = p.queue.peek();
      p.editQueued(q[q.length - 1].id);
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
  // [verb, key, drawn in a narrow pane]
  const verbs = [
    run !== true && ['send', '⏎', true],
    run !== false && ['steer', enter === 'steer' ? '⏎' : '', enter === 'steer'],
    run !== false && ['queue', '⌥⏎', false],
    run !== false && ['stop', '^c', true],
  ].filter(Boolean);
  const sep = html`<span class="faint"> · </span>`;
  const menus = [
    { id: 'attach', full: '+ attach', short: '+', items: () => [
      { label: 'image…', hint: 'png jpeg gif webp · 4 MiB', on: () => pick('image') },
      { label: 'file…', hint: 'text, inlined', on: () => pick('text') },
      { label: 'folder…', off: `${HUB}: a session's folder is fixed at launch` },
    ] },
    { id: 'model', full: `model ${model}`, short: model.split(/[/:]/).pop(), items: () => [
      { label: model, tick: true, off: RESTART }, { label: 'another model…', off: RESTART }] },
    { id: 'effort', full: 'effort ?', short: '◔', items: () => ['low', 'medium', 'high'].map((l) => ({ label: l, off: RESTART })) },
    { id: 'mode', full: `mode ${yolo ? 'YOLO' : 'gated'}`, short: yolo ? 'YOLO' : '⚑', items: () => MODES.map((m) => ({ label: m, tick: m === 'yolo' && yolo, off: RESTART })) },
    { id: 'mic', full: '◉ mic', short: '◉', off: MIC },
  ];
  const menuAt = (m) => (e) => openMenu(m.items(), under(e.currentTarget), { title: m.full, owner: `cm-${m.id}-${p.id}` });
  const cmenus = fit >= 3
    ? html`<${K} cls="cm cm-all" title="attach, model, effort, mode, mic" on=${(e) => openMenu(menus.map((m) => (m.off ? { label: m.full, off: m.off } : { label: m.full, sub: m.items() })),
        under(e.currentTarget), { title: 'composer', owner: `cm-all-${p.id}` })}>⋯ ▾</${K}>`
    : menus.map((m, i) => html`${i > 0 && sep}${m.off ? html`<span class=${'dis cm cm-' + m.id} title=${m.off}>${fit >= 2 ? m.short : m.full}</span>`
      : html`<${K} cls=${'cm cm-' + m.id} title=${m.full} on=${menuAt(m)}>${fit >= 2 ? m.short : m.full} ▾</${K}>`}`);
  const st = stateOf(s), ed = p.editing.value;
  const edN = ed ? p.queue.value.findIndex((m) => m.id === ed.id) + 1 : 0;
  return html`<div class=${'composer st-' + STATE_CLS[st] + (run === true ? ' running' : '') + (ed ? ' editing' : '')} ref=${root}
      onDragOver=${(e) => { if (files(e)) e.preventDefault(); }} onDrop=${(e) => { if (files(e) && e.dataTransfer.files.length) { e.preventDefault(); take([...e.dataTransfer.files]); } }}>
    <div class="ftitle">message${ed && html`<span class="o"> · editing queued ${edN}</span>`}</div>
    <div class="ftitle fr"><${Pulse} s=${s} /></div>
    ${atts.value.length > 0 && html`<div class="chips">${atts.value.map((a) => html`<span class="chip" title=${`${a.media_type}, ${kfmt(a.size)} bytes`}><span class="c">[img]</span> ${a.name} <${K}
        cls="cx" title="remove it" on=${() => { atts.value = atts.peek().filter((x) => x !== a); }}>×</${K}></span>`)}</div>`}
    <textarea ref=${ta} rows="1" spellcheck="false" aria-label="message"
      placeholder=${bye ? 'this session said goodbye' : `Write a message… (⏎ ${enter} · ⌥⏎ queue · ^c stop)`}
      onInput=${autosize} onKeyDown=${onKey} onPaste=${onPaste} onFocus=${() => act.focus(p.id)}></textarea>
    <input type="file" ref=${fileIn} multiple hidden aria-hidden="true" tabindex="-1" onChange=${(e) => take([...e.currentTarget.files], e.currentTarget.dataset.kind)} />
    ${text.value.startsWith(':') && html`<div class="cwarn">: goes to the model; commands live in the status line</div>`}
    <div class=${'fbar fit' + fit} ref=${bar}><span class="cmenus">${cmenus}</span><span class="grow"></span>
      <span class="verbs">${ed ? html`<${K} cls="verb" on=${() => endEdit(true)}><span class="kh">⏎ </span>save</${K}>${sep}<${K} cls="verb" on=${() => endEdit(false)}><span class="kh">esc </span>cancel</${K}>`
        : html`${s.queued.value > 0 && html`<span class="hi">${s.queued.value} queued</span>${sep}`}${verbs.map(([how, kh, narrow], i) => html`<span class=${narrow ? '' : 'wide'}>${i > 0 && sep}<${K} off=${bye} on=${() => go(how)} cls="verb">${kh && html`<span class="kh">${kh} </span>`}${how}</${K}></span>`)}`}</span>
    </div>
  </div>`;
}
