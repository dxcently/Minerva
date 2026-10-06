// One session pane: the transcript, the draft, the ask panel (when the
// current ask is this pane's, panes/asks.js), the composer. It draws a
// core/state.js Session and holds no session state of its own. Several panes
// may draw one Session (mirrors share its one stream), so what is tied to the
// DOM lives in a per-PANE view, never on the Session.
import { html, K, HUB, Rendered, Component, signal, effect, useRef, useLayoutEffect, useEffect, useMemo, useState } from '../core/ui.js';
import { IMAGE_TYPES, MAX_IMAGES, looksImage, readImage, readText, tooBig } from '../core/attach.js';
import { markdown } from '../core/markdown.js';
import { resolveIn } from '../core/path.js';
import { render as rend, renderCall } from '../renderers/index.js';
import { icon, word, shade, CELLS } from '../core/life.js';
import { tildify, kfmt, hhmm, stateOf, STATE_CLS, connWord } from '../core/state.js';
import { mode, hubGet, hubPost } from '../core/api.js';
import { list, refresh, modeOf } from '../core/hub.js';
import { openMenu, closeMenu, menu, atPointer, under } from '../core/menu.js';
import { AskPanel, AskMark, Settled, quoted } from './asks.js';
import { Strip, items as imageItems, linkPaths } from './imgs.js';

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
  // A chat that is not running and can be continued (saved, stopped, exited): no composer, a visible action instead.
  const away = !!banner && (!!banner.resume || !!s.resuming);
  // A fork from a point that has had no first message: history up to its reply, and a composer that runs that one turn (ForkComposer).
  const row = list.value.find((r) => r.id === s.sid);
  const forking = !!row && !!row.pending && s.conn.value === 'kept';
  v.act = act;
  return html`<section class=${'pane frame' + (focused ? ' focus' : '')} style=${style} tabindex="-1"
      data-pane=${p.id} onMouseDown=${() => act.focus(p.id)}>
    <${Title} p=${p} mirror=${mirror} act=${act} />
    <${Glyphs} p=${p} act=${act} />
    <${Transcript} s=${s} v=${v} />
    ${banner && !forking && html`<div class=${'banner' + (banner.err ? ' err' : '')}>${banner.text}</div>`}
    ${act.panelPane.value === p.id && html`<${AskPanel} p=${p} act=${act} />`}
    <${Outbox} p=${p} />
    ${forking ? html`<${ForkComposer} s=${s} p=${p} act=${act} />` : away ? html`<${Continue} s=${s} p=${p} act=${act} />` : html`<${Composer} s=${s} p=${p} act=${act} />`}
  </section>`;
}

// Session name, its menu, the project chip, the connection dot.
function Title({ p, mirror, act }) {
  const s = p.s, conn = s.conn.value, hl = s.hello.value;
  const dot = { live: 'hi', replaying: 'y', connecting: 'y', reconnecting: 'y', goodbye: 'faint', stopped: 'faint', kept: 'faint', gone: 'faint' }[conn] || 'r';
  const open = (e, at) => { e.preventDefault(); e.stopPropagation(); act.paneMenu(p, at); };
  return html`<div class="ftitle k ptitle" role="button" tabindex="0" title=${'pane menu (click or right-click)\nid ' + s.sid + (hl ? '\n' + tildify(hl.cwd) : '')} data-owner=${'pane-' + p.id}
      onClick=${(e) => open(e, under(e.currentTarget))} onContextMenu=${(e) => open(e, atPointer(e))}
      onKeyDown=${(e) => !e.repeat && (e.key === 'Enter' || e.key === ' ') && open(e, under(e.currentTarget))}>
    <span class="b">${act.name(s)}</span>${mirror && html` <span class="faint">(mirror)</span>`} <span class="faint">▾</span>
    ${hl && act.project(hl.cwd) && html` <span class="proj">[${act.project(hl.cwd)}]</span>`} <span class=${dot}>●</span>
    ${conn !== 'live' && html` <span class="faint">${connWord(conn)}</span>`}
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

// ---------------------------------------------------------------- a chat that is not running
// Where the composer would be, for a saved, stopped or exited chat: the way back in is a visible text
// action, never a side effect of opening it (a resume can start a paid turn: hub.md section 4).
// A chat from eidolon's own sessions folder says what continuing it means.
function Continue({ s, p, act }) {
  const ref = useRef(null);
  const row = list.value.find((r) => r.id === s.sid);
  const other = !!row && row.origin === 'other', busy = !!s.resuming;
  useEffect(() => {
    p.focusComposer = () => { const el = ref.current && ref.current.querySelector('.k'); if (el) el.focus({ preventScroll: true }); };
    p.prefill = () => {};
    return () => { p.focusComposer = null; };
  }, [p]);
  return html`<div class="cont" ref=${ref}>
    <${K} cls="act" off=${busy} title="continue this chat: it may carry on the last turn" on=${() => act.resume(p)}>${busy ? '[ continuing… ]' : '[ continue this chat ]'}</${K}>
    <span class="faint"> read-only until then</span>
    ${other && html`<div class="faint note">from eidolon's own sessions folder · continuing resumes it with your normal config and writes to the same log</div>`}
  </div>`;
}


// ---------------------------------------------------------------- a fork from a point, before its first message
// The copy has no door yet (perch pending.rs): its history above ends at the reply it was forked from. The first message is run by
// eidolon itself (`resume --at`), headless, so this pane shows its work as plain text; when it ends the perch brings the door up and
// the pane goes live. A failure keeps the fork waiting, with the sentence, and the text stays here to try again.
const OUT_LINES = 12;
function ForkComposer({ s, p, act }) {
  const ta = useRef(null), fileIn = useRef(null);
  const run = useMemo(() => signal({ state: 'none', out: '', elapsed_ms: 0 }), []);
  const atts = useMemo(() => signal([]), []);
  const sending = useMemo(() => signal(false), []); // the POST is in flight: a second send would be a second run
  useEffect(() => {
    p.focusComposer = () => ta.current && ta.current.focus();
    p.prefill = (text) => { const el = ta.current; if (el) { el.value = text + el.value; el.focus(); } };
    let dead = false, timer = 0;
    const tick = async () => {
      const r = await hubGet(`sessions/${encodeURIComponent(s.sid)}/first`, 8000);
      if (dead) return;
      if (r.status === 200 && r.data) {
        run.value = r.data;
        if (r.data.state === 'done') { s.adopt(); refresh(); return; }
        // A run found after a restart may have settled the fork: the list says so (the row drops `pending`), and this pane follows it.
        if (r.data.state === 'failed') refresh();
        if (r.data.state === 'running') timer = setTimeout(tick, 1000);
      } else if (r.status === 0) timer = setTimeout(tick, 3000);
    };
    p.pollFirst = () => { clearTimeout(timer); tick(); };
    tick();
    return () => { dead = true; clearTimeout(timer); p.focusComposer = null; p.pollFirst = null; };
  }, [p, s]);
  const working = run.value.state === 'running' || sending.value;
  const go = async () => {
    const el = ta.current, imgs = atts.peek();
    if (working || sending.peek()) return;
    if (!el.value.trim()) { el.focus(); return; }
    const big = tooBig(el.value, imgs);
    if (big) { act.toast(big, true); return; }
    sending.value = true;
    let r;
    try { r = await hubPost(`sessions/${encodeURIComponent(s.sid)}/first`, { text: el.value, images: imgs.map(({ media_type, data }) => ({ media_type, data })) }); }
    finally { sending.value = false; }
    if (r.status === 202) { run.value = { ...run.peek(), state: 'running', out: '', elapsed_ms: 0, error: '' }; p.pollFirst(); }
    else act.toast(r.status === 409 ? 'that fork is already running its first message' : r.status === 0 ? 'the server is not answering' : `could not start it: ${(r.data && r.data.error) || r.status}`, true);
  };
  const take = async (files) => {
    for (const f of files) {
      try {
        if (!looksImage(f)) throw new Error(`${f.name || 'that file'} is not an image`);
        if (atts.peek().length >= MAX_IMAGES) throw new Error(`at most ${MAX_IMAGES} images go with one message`);
        atts.value = [...atts.peek(), { ...(await readImage(f)), id: ++attSeq }];
      } catch (err) { act.toast(err.message, true); }
    }
    ta.current.focus();
  };
  const onKey = (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !e.ctrlKey) { e.preventDefault(); go(); }
    else if (e.key === 'Escape') { e.preventDefault(); ta.current.blur(); ta.current.closest('.pane').focus(); }
  };
  const onPaste = (e) => {
    const fs = [...((e.clipboardData && e.clipboardData.files) || [])].filter(looksImage);
    if (fs.length) { e.preventDefault(); take(fs); }
  };
  const r = run.value, secs = Math.round((r.elapsed_ms || 0) / 1000);
  const tail = String(r.out || '').split('\n').filter((l) => l.trim()).slice(-OUT_LINES).join('\n');
  const gate = r.mode === 'yolo' || (!r.mode && r.yolo) ? 'tool approvals in this turn are answered yes (the chat was in yolo mode)' : `tool calls that need a yes in this turn are declined (the chat is in ${r.mode || 'auto'} mode): it runs with no terminal to ask you in`;
  return html`<div class=${'composer st-' + (working ? 'run' : 'idle') + (working ? ' running' : '')}
      onDragOver=${(e) => { if (e.dataTransfer && [...e.dataTransfer.types].includes('Files')) e.preventDefault(); }}
      onDrop=${(e) => { if (e.dataTransfer && e.dataTransfer.files.length) { e.preventDefault(); take([...e.dataTransfer.files]); } }}>
    <div class="ftitle">first message · fork from the reply above</div>
    ${r.state === 'failed' && html`<div class="cwarn">${r.error || 'the first message did not finish'}</div>`}
    ${working && html`<div class="hi">working… ${secs}s <span class="faint">the first message of this fork is running; it opens by itself when it ends</span></div>`}
    ${working && tail && html`<pre class="firstout">${tail}</pre>`}
    ${atts.value.length > 0 && html`<div class="chips">${atts.value.map((a) => html`<span class="chip" title=${`${a.media_type}, ${kfmt(a.size)} bytes`}><span class="c">[img]</span> ${a.name} <${K}
        cls="cx" title="remove it" on=${() => { atts.value = atts.peek().filter((x) => x !== a); }}>×</${K}></span>`)}</div>`}
    <textarea ref=${ta} rows="1" spellcheck="false" aria-label="first message" disabled=${working}
      placeholder=${'Write the first message of this fork… (⏎ send · ⇧⏎ newline)'}
      onInput=${(e) => { const el = e.currentTarget; el.style.height = 'auto'; el.style.height = el.scrollHeight + 'px'; }} onKeyDown=${onKey} onPaste=${onPaste} onFocus=${() => act.focus(p.id)}></textarea>
    <input type="file" ref=${fileIn} multiple hidden aria-hidden="true" tabindex="-1" accept=${IMAGE_TYPES.join(',')} onChange=${(e) => take([...e.currentTarget.files])} />
    <div class="faint">${gate}</div>
    <div class="fbar fit0"><span class="cmenus"><${K} cls="cm" off=${working} title="an image goes with the message" on=${() => { fileIn.current.value = ''; fileIn.current.click(); }}>+ image</${K}></span><span class="grow"></span>
      <span class="verbs"><${K} off=${working} on=${go} cls="verb"><span class="kh">⏎ </span>send</${K}></span></div>
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
// What the gate is, in the words of the start screen and the loaded tab.
const gateWords = (chat) => (chat === 'yolo' ? html`<span class="r b">YOLO</span>` : chat === 'manual' ? html`<span class="o b">manual</span> asks before anything that changes things` : chat === 'auto' ? html`<span class="b">auto</span> asks only for risky commands` : 'asks');

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
    ${line('gate    ', gateWords(modeOf(s.sid, hl)))}
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
function rowMenu(e, r, v, extra = []) {
  const el = e.currentTarget, got = getSelection(), sel = String(got);
  if (sel && !el.contains(got.anchorNode)) return;
  e.preventDefault();
  openMenu([
    ...extra,
    { label: sel ? 'copy selection' : 'copy', on: () => copy(sel || el.innerText.trim(), v.act) },
    { label: 'copy as markdown', on: () => copy(rowSource(r), v.act) },
    { label: 'quote into composer', on: () => v.p.prefill(quoted(sel || rowSource(r)) + '\n\n') },
  ], atPointer(e), { title: 'row' });
}

// `fork from here` on a reply: a new chat that keeps the history up to it (its first message is the turn that makes the branch stay).
// Not on the latest reply (that is `fork (latest)`), not on a reply cut short, not in a fork still waiting for its first message.
// A user's own row has none: after it the model would first have to answer it, a turn nobody asked for.
function forkItems(r, s, v) {
  if (mode() !== 'perch' || r.record == null || r.cut || r.record === s.lastReply) return [];
  const row = list.peek().find((x) => x.id === s.sid);
  if (row && row.pending) return [];
  return [{ label: 'fork from here', hint: 'a new chat, history up to this reply', on: () => v.act.forkFrom(s.sid, r.record, v.act.name(s)) }];
}

function RowBody({ r, s, v }) {
  const cm = (e) => rowMenu(e, r, v);
  switch (r.kind) {
    case 'day': return html`<div class="row day" role="separator">${r.text}</div>`;
    case 'you':
      return html`<div class="row you" onContextMenu=${cm}><${Who} r=${r} /><div class="body">${r.text}</div>
        ${r.images > 0 && html`<${Strip} sid=${s.sid} list=${imageItems(r)} />`}</div>`;
    case 'bot': {
      // A reply an `error` cut short is filled in place by its assistant-message.
      const full = r.full && r.full.value, x = full || r, cut = full ? null : r.cut;
      return html`<div class="row" onContextMenu=${(e) => rowMenu(e, r, v, forkItems(r, s, v))}><${Who} r=${r} />
        ${x.thinking && html`<${Fold} cls="think" label=${`thinking (${x.thinking.split('\n').length} lines)`}>${x.thinking}</${Fold}>`}
        ${x.redacted > 0 && html`<div class="think">(${x.redacted} redacted thinking block${x.redacted > 1 ? 's' : ''})</div>`}
        ${x.text && html`<div class="body">${cut ? x.text : markdown(x.text, (t) => linkPaths(t, s.sid, s.hello.value && s.hello.value.cwd))}</div>`}
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
    case 'tool': return html`<${ToolRow} c=${r.call} s=${s} v=${v} cm=${(e) => rowMenu(e, r, v, toolOpens(r.call.input, s))} />`;
    case 'ask':
      return html`<div class="row" onContextMenu=${cm}>${r.ask.done.value ? html`<${Settled} a=${r.ask} />` : html`<${AskMark} a=${r.ask} s=${s} act=${v.act} />`}</div>`;
    default: return null;
  }
}

// ---------------------------------------------------------------- tool lines
// `open in editor` and `open folder` for a call whose input names a path
// (`path` or `file_path`, a string). The path is the model's, so it is resolved
// against the session's folder (core/path.js) and offered only when it lands
// inside it; what is sent is that resolved absolute path, shown whole in the
// item's hint and title and again in the line that says it is opening.
function toolOpens(input, s) {
  const raw = input && [input.path, input.file_path].find((x) => typeof x === 'string' && x);
  if (!raw) return [];
  const hl = s.hello.value;
  const r = resolveIn(hl && hl.cwd, raw);
  const off = s.launchOff() || (r.why ? `not offered: ${r.why}: ${raw}` : null);
  const go = (w) => () => { s.hooks.toast(`opening ${r.path}`); s.launch(w, r.path); };
  return [
    { label: 'open in editor', hint: r.path, off, on: go('editor') },
    { label: 'open folder', hint: r.path, off, on: go('files') },
    { sep: true },
  ];
}

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
    ${c.imgs.value.length > 0 && html`<${Strip} sid=${s.sid} list=${c.imgs.value} fold=${true} />`}
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

// The composer's bottom border: a line of menus and the verbs. Every menu works or is not drawn: `+ attach` always; `model`
// and `mode` under the perch only (a bare door cannot change either: the perch stops the door between turns and resumes the same
// log with the new flags, POST /hub/sessions/<id>/restart). There is no effort menu (eidolon's `web` has no such flag) and no
// mic (nothing here records or transcribes). It fits the frame as the status line does: the verbs drop their keys, the labels
// drop to glyphs, every menu goes into one `⋯`, then only send and stop stay.
let attSeq = 0;

// The models eidolon lists (GET /hub/models, cached by the perch for ten minutes), kept here for one minute so reopening the
// menu is instant. `null` when the perch could not say; the menu then offers the current model and `other…` only.
let modelRows = null, modelsAt = 0, modelsErr = '';
async function loadModels() {
  if (modelRows && Date.now() - modelsAt < 60000) return modelRows;
  const r = await hubGet('models', 30000);
  if (r.status === 200 && Array.isArray(r.data)) { modelRows = r.data; modelsAt = Date.now(); modelsErr = ''; return modelRows; }
  modelsErr = (r.data && r.data.error) || 'the model list could not be read';
  return null;
}
const MODE_ROWS = [
  ['manual', 'asks before anything that changes things'],
  ['auto', "eidolon's rules: asks only for risky commands"],
  ['yolo', 'never asks'],
];

function Composer({ s, p, act }) {
  const ta = useRef(null), root = useRef(null), bar = useRef(null), fileIn = useRef(null);
  const text = useMemo(() => signal(''), []);
  const atts = useMemo(() => signal([]), []); // images going with the next message
  const [fit, setFit] = useState(0), [cw, setCw] = useState(0);
  const other = useMemo(() => signal(null), []); // the `other…` model: null, or what is typed
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
  const hl = s.hello.value, chat = modeOf(s.sid, hl), model = (hl && hl.model) || '?';
  useLayoutEffect(() => { setFit(0); }, [cw, model, chat, run, !!p.editing.value]);
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
    ] },
  ];
  // model and mode are the perch's: it restarts the chat between turns (the transcript is its log, so nothing is lost).
  const busy = run === true ? 'wait for the turn to end' : null;
  const switchTo = (change, said) => s.restart(change, said);
  const pickModel = (key) => key !== model && switchTo({ model: key }, `switched to ${key} — the chat restarted between turns, nothing lost`);
  const pickMode = async (name) => { if (await switchTo({ mode: name }, `mode is now ${name} — the chat restarted between turns, nothing lost`)) refresh(); };
  if (mode() === 'perch') {
    menus.push({ id: 'model', full: `model ${model}`, short: model.split(/[/:]/).pop(), open: true, items: (rows, loading) => [
      ...(loading ? [{ label: 'loading models…', off: 'one moment' }] : []),
      ...(rows || []).map((m) => ({ label: m.key, tick: m.key === model, hint: m.note || m.name || '', off: busy, on: () => pickModel(m.key) })),
      ...(rows && rows.some((m) => m.key === model) ? [] : [{ label: model, tick: true, off: busy, on: () => {} }]),
      ...(rows || loading ? [{ sep: true }] : []),
      { label: 'other…', hint: rows ? 'type a model name' : `${modelsErr || 'no list'}; type a model name`, off: busy, on: () => { other.value = ''; setTimeout(() => p.focusOther && p.focusOther(), 0); } },
    ] });
    menus.push({ id: 'mode', full: `mode ${chat || 'gated'}`, short: chat || '⚑', items: () => MODE_ROWS.map(([name, hint]) =>
      ({ label: name, hint, tick: name === chat, off: busy, on: () => { if (name !== chat) pickMode(name); } })) });
  }
  // The model menu is filled from the perch's list: fetched when it is opened (a menu cannot wait on a promise).
  const menuAt = (m) => async (e) => {
    const at = under(e.currentTarget), opts = { title: m.full, owner: `cm-${m.id}-${p.id}` };
    const fresh = !!(modelRows && Date.now() - modelsAt < 60000);
    openMenu(m.items(m.open ? modelRows : undefined, m.open && !fresh), at, opts);
    if (m.open && !fresh) {
      const rows = await loadModels();
      const open = menu.peek();
      if (open && open.owner === opts.owner) { closeMenu(); openMenu(m.items(rows), at, opts); }
    }
  };
  const cmenus = fit >= 3
    ? html`<${K} cls="cm cm-all" title=${menus.map((m) => m.id).join(', ')} on=${(e) => openMenu(menus.map((m) => ({ label: m.full, sub: m.items(m.open ? modelRows : undefined) })),
        under(e.currentTarget), { title: 'composer', owner: `cm-all-${p.id}` })}>⋯ ▾</${K}>`
    : menus.map((m, i) => html`${i > 0 && sep}<${K} cls=${'cm cm-' + m.id} title=${m.full} on=${menuAt(m)}>${fit >= 2 ? m.short : m.full} ▾</${K}>`);
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
    ${other.value !== null && html`<div class="cother"><span class="faint">model </span><input ref=${(el) => { p.focusOther = () => el && el.focus(); }} value=${other.value} spellcheck="false" aria-label="model name"
      placeholder="provider:model" onInput=${(e) => { other.value = e.currentTarget.value; }} onKeyDown=${(e) => {
        if (e.key === 'Enter') { e.preventDefault(); const v = other.value.trim(); other.value = null; if (v) pickModel(v); ta.current.focus(); }
        else if (e.key === 'Escape') { e.preventDefault(); other.value = null; ta.current.focus(); }
        e.stopPropagation();
      }} /><span class="faint"> ⏎ switch · esc cancel</span></div>`}
    <div class=${'fbar fit' + fit} ref=${bar}><span class="cmenus">${cmenus}</span><span class="grow"></span>
      <span class="verbs">${ed ? html`<${K} cls="verb" on=${() => endEdit(true)}><span class="kh">⏎ </span>save</${K}>${sep}<${K} cls="verb" on=${() => endEdit(false)}><span class="kh">esc </span>cancel</${K}>`
        : html`${s.queued.value > 0 && html`<span class="hi">${s.queued.value} queued</span>${sep}`}${verbs.map(([how, kh, narrow], i) => html`<span class=${narrow ? '' : 'wide'}>${i > 0 && sep}<${K} off=${bye} on=${() => go(how)} cls="verb">${kh && html`<span class="kh">${kh} </span>`}${how}</${K}></span>`)}`}</span>
    </div>
  </div>`;
}
