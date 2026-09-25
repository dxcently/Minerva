// The screen: sidebars, tiles, the hint line, the status line, keys, the `:`
// line. Sessions live in core/state.js and panes draw them (panes/session.js);
// this file arranges panes and routes every action. Every action is reachable
// by mouse; keys and the `:` line are shortcuts to the same functions.
import { html, render, K, signal, computed, batch, useRef, useLayoutEffect } from './core/ui.js';
import { takeToken, DOOR } from './core/api.js';
import { Session, tildify } from './core/state.js';
import * as tile from './core/tile.js';
import { Pane, focusAsk, askEl } from './panes/session.js';

const LEFT_TABS = ['sessions', 'projects', 'mesh'];
const RIGHT_TABS = ['rec', 'tree', 'diff', 'graph', 'run']; // components: INSPECTOR below

// ---------------------------------------------------------------- screen state
const ui = {
  panes: signal([]),            // [{ id, sid, mirror, s: Session }]; panes on one sid share s
  tree: signal(null),           // core/tile.js split tree of pane ids
  focus: signal(null),          // pane id
  left: signal('sessions'),
  right: signal('rec'),
  lrail: signal(innerWidth < 900),
  rrail: signal(innerWidth < 900),
  hint: signal(null),           // { mode: 'toast' | 'which' | 'menu', text?, err? }
  cmd: signal(null),            // the `:` line's text while open, else null
  menu: signal(null),           // pane id whose title menu is open
  box: signal({ w: 0, h: 0, cw: 8, ch: 19 }),
  token: false,
};
const byId = (id) => ui.panes.value.find((p) => p.id === id);
const focused = computed(() => byId(ui.focus.value) || null);
let paneSeq = 0, toastTimer = 0;

// ---------------------------------------------------------------- actions
// One Session, so one /api/events stream, per session id. Panes are views:
// a mirror reuses the Session its id already has, and closing a pane closes
// the Session only when no pane still shows it. (One stream per pane hit
// HTTP/1.1's six-connections-per-host limit at six mirrors and hung every
// request after it.)
const sessionFor = (sid) => {
  const p = ui.panes.value.find((x) => x.sid === sid);
  return p ? p.s : new Session(sid, { toast });
};

function openPane(sid = DOOR, mirror = false) {
  const id = 'p' + ++paneSeq;
  const p = { id, sid, mirror, s: sessionFor(sid) };
  batch(() => {
    ui.panes.value = [...ui.panes.value, p];
    ui.tree.value = tile.add(ui.tree.value, ui.focus.value, id, ui.box.value);
    ui.focus.value = id;
  });
  requestAnimationFrame(() => p.focusComposer && p.focusComposer());
  return p;
}

function mirrorPane(id = ui.focus.value) {
  const p = byId(id);
  if (!p) { openPane(); return; }
  ui.focus.value = p.id;
  openPane(p.sid, true);
}

function closePane(id = ui.focus.value) {
  const p = byId(id);
  if (!p) return;
  if (!ui.panes.value.some((x) => x !== p && x.s === p.s)) p.s.close();
  batch(() => {
    ui.panes.value = ui.panes.value.filter((x) => x !== p);
    ui.tree.value = tile.remove(ui.tree.value, id);
    const rest = tile.leaves(ui.tree.value);
    if (ui.focus.value === id) ui.focus.value = rest[rest.length - 1] || null;
    if (ui.menu.value === id) ui.menu.value = null;
  });
  toast('pane closed; the session keeps running. :open brings it back');
}

const focusPane = (id) => { if (ui.focus.value !== id) ui.focus.value = id; };
const withPane = (fn) => () => { const p = focused.value; if (p) fn(p); else toast('no pane focused'); };
const cancel = withPane((p) => p.s.cancel());
const toComposer = withPane((p) => p.focusComposer && p.focusComposer());

// Focus the next (or previous) ask in the focused pane: this is what lets
// keys reach an ask (web-ui.md 7.1).
function cycleAsk(dir = 1) {
  const p = focused.value;
  if (!p) return false;
  const asks = p.s.asks.value;
  if (!asks.length) return false;
  const at = asks.findIndex((a) => askEl(p, a.id) === document.activeElement);
  const next = asks[(at < 0 ? (dir > 0 ? 0 : asks.length - 1) : at + dir + asks.length) % asks.length];
  return focusAsk(p, next);
}

const toggle = (rail) => { ui[rail].value = !ui[rail].value; };
function cycleRight(d) {
  const i = RIGHT_TABS.indexOf(ui.right.value);
  ui.right.value = RIGHT_TABS[(i + d + RIGHT_TABS.length) % RIGHT_TABS.length];
}

// ---------------------------------------------------------------- hint line
function toast(text, err) {
  if (ui.hint.value && ui.hint.value.mode === 'menu') return;
  ui.hint.value = { mode: 'toast', text, err };
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { if (ui.hint.value && ui.hint.value.mode === 'toast') ui.hint.value = null; }, err ? 7000 : 3500);
}

const KEYS = [
  ['⏎', 'send / steer', toComposer],
  ['⌥⏎', 'queue', toComposer],
  ['esc', 'leave composer', () => document.activeElement && document.activeElement.blur()],
  ['tab', 'focus next ask', () => cycleAsk(1)],
  ['y ⏎ / n / 1-9', 'answer the focused ask', () => cycleAsk(1)],
  ['^c', 'stop turn', cancel],
  ['^b', 'sidebar', () => toggle('lrail')],
  ['^i', 'inspector', () => toggle('rrail')],
  ['[ ]', 'inspector tab', () => cycleRight(1)],
  ['⌥w  :q', 'close pane', () => closePane()],
  [':vsplit', 'mirror pane', () => mirrorPane()],
  [':', 'commands', () => openCmd()],
];

function whichKey() {
  ui.hint.value = ui.hint.value && ui.hint.value.mode === 'which' ? null : { mode: 'which' };
}

// ---------------------------------------------------------------- the : line
const COMMANDS = {
  q: ['close the pane', () => closePane()],
  close: ['close the pane', () => closePane()],
  open: ['open a pane on this door', () => openPane()],
  vsplit: ['mirror the focused pane', () => mirrorPane()],
  split: ['mirror the focused pane', () => mirrorPane()],
  stop: ['cancel the running turn', cancel],
  queue: ['queue text (or the composer) behind the turn', (arg) => withPane((p) => p.s.submit('queue', arg || ''))()],
  reconnect: ['drop the stream and replay', withPane((p) => p.s.reconnect())],
  sidebar: ['toggle the left sidebar', () => toggle('lrail')],
  inspector: ['toggle the inspector', () => toggle('rrail')],
  keys: ['show keys', () => whichKey()],
};

function openCmd() {
  batch(() => { ui.cmd.value = ''; ui.hint.value = { mode: 'menu' }; ui.menu.value = null; });
}
function closeCmd() {
  batch(() => { ui.cmd.value = null; if (ui.hint.value && ui.hint.value.mode === 'menu') ui.hint.value = null; });
}
const matches = (text) => {
  const w = (text || '').trim().split(/\s+/)[0] || '';
  return Object.keys(COMMANDS).filter((c) => c.startsWith(w));
};
function runCmd(text = ui.cmd.value || '') {
  const [w, ...rest] = text.trim().split(/\s+/);
  closeCmd();
  if (!w) return;
  const c = COMMANDS[w];
  if (c) c[1](rest.join(' '));
  else toast(`:${w}: session commands arrive with the registry (M4)`, true);
}

// ---------------------------------------------------------------- views
const act = {
  focus: focusPane,
  menu: (id) => { ui.menu.value = ui.menu.value === id ? null : id; },
  mirror: mirrorPane,
  close: closePane,
};

// Frame-title tabs, plus the arrow that folds the sidebar to its rail
// (pointing at the edge it folds to).
function Tabs({ names, cur, pick, collapse, title, side }) {
  const fold = html`<${K} cls="tab fold" title=${title} on=${collapse}>${side === 'left' ? '«' : '»'}</${K}>`;
  return html`<div class="ftitle">
    ${side === 'left' && fold}
    ${names.map((n) => html`<${K} cls=${'tab' + (n === cur ? ' on' : '')} on=${() => pick(n)}>${n === cur ? `[${n}]` : n}</${K}>`)}
    ${side !== 'left' && fold}
  </div>`;
}

const Needs = ({ children }) => html`<div class="needs">${children}</div>`;
const Kv = ({ k, v, cls = '' }) => html`<div class="kv"><span class="faint">${k}</span><span class=${'grow ' + cls}>${String(v)}</span></div>`;

function Owl() {
  return html`<div class="owl">${' ,___,  '}<span class="name">minerva</span>${'\n ['}<span class="eyes">O.o</span>${']  '}<span class="faint">Make it,</span>${'\n /)_)   '}<span class="faint">Break it,</span>${'\n  ""    '}<span class="faint">Hack it.</span></div>`;
}

function SessionItem({ p }) {
  const s = p.s, hl = s.hello.value, conn = s.conn.value, run = s.running.value;
  const name = hl ? hl.session.split('/').pop().replace(/\.[^.]+$/, '') || 'session' : 'connecting…';
  const state = s.bye ? 'kept' : conn !== 'live' ? conn : run === true ? 'running' : run === false ? 'idle' : 'unknown';
  return html`<div class="it">
    <span class=${s.bye ? 'faint' : 'g'}>${s.bye ? '○' : '●'}</span>
    <${K} cls=${'grow' + (ui.focus.value === p.id ? ' on' : '')} title=${hl ? hl.session : ''} on=${() => { focusPane(p.id); toComposer(); }}>${name}${p.mirror ? ' (mirror)' : ''}</${K}>
    <span class=${run === true ? 'y' : 'faint'}>${state}</span>
  </div>`;
}

function Left() {
  const tab = ui.left.value, rail = ui.lrail.value, panes = ui.panes.value;
  const p = focused.value, hl = p && p.s.hello.value;
  let body;
  if (tab === 'sessions') {
    body = html`<${Owl} />
      ${panes.length ? html`<div class="list">${panes.map((x) => html`<${SessionItem} key=${x.id} p=${x} />`)}</div>`
        : html`<div class="faint">no pane. <${K} on=${() => openPane()}>open one</${K}></div>`}
      <div class="hdr">peers</div><${Needs}>peer list needs the hub</${Needs}>
      <div class="hdr">kept</div><${Needs}>other sessions need the hub (M2)</${Needs}>`;
  } else if (tab === 'projects') {
    body = html`<div class="hdr">this door</div>
      ${hl ? html`<div class="list"><div class="it"><span class="c">▸</span><span class="grow" title=${hl.cwd}>${tildify(hl.cwd)}</span></div></div>` : html`<div class="faint">—</div>`}
      <div class="hdr">others</div><${Needs}>project list needs the hub</${Needs}>`;
  } else {
    body = html`<div class="note">node        paired  grants  seen</div>
      <div class="note dim">──────────  ──────  ──────  ────</div>
      <${Needs}>mesh reads Aoide through the hub (M3)</${Needs}>`;
  }
  return html`<aside id="left" class="col side frame">
    ${!rail && html`<${Tabs} names=${LEFT_TABS} cur=${tab} pick=${(n) => { ui.left.value = n; }} side="left" collapse=${() => toggle('lrail')} title="collapse to a rail (ctrl+b)" />`}
    ${!rail && html`<div class="inner">${body}</div>`}
    ${rail && html`<div class="rail k" role="button" tabindex="0" title="open the sidebar (ctrl+b)" onClick=${() => toggle('lrail')}>${tab} ▾</div>`}
  </aside>`;
}

function Rec({ s }) {
  const vs = s.verdicts.value.slice(-10), asks = s.asks.value.length, calls = s.calls.value, errs = s.fails.value;
  const ctx = s.ctx.value, budget = s.budget.value;
  return html`
    <${Kv} k="turns  " v=${s.turns.value} />
    <${Kv} k="calls  " v=${calls + (errs ? ` (${errs} failed)` : '')} cls=${errs ? 'r' : ''} />
    <${Kv} k="asks   " v=${asks} cls=${asks ? 'y' : ''} />
    <${Kv} k="context" v=${ctx != null ? ctx.toLocaleString() + ' tok' : '—'} />
    <${Kv} k="budget " v=${budget != null ? budget + ' calls left' : '—'} cls=${budget != null && budget < 3 ? 'r' : ''} />
    <div class="hdr">verdicts</div>
    ${vs.length ? vs.map((v) => html`<div class="kv"><span class="m">${v.tool}</span><span class="grow faint" title=${v.reason}>${v.reason}</span><span>${v.outcome}</span></div>`)
      : html`<div class="faint">none yet</div>`}`;
}

function RunTab({ s }) {
  const open = s.open.value, run = s.running.value;
  return html`<div class="hdr">open calls</div>
    ${open.length ? open.map((c) => html`<div class="kv"><span class="m">${c.name}</span><span class="grow lc">${c.st.value}</span></div>`)
      : html`<div class="faint">${run === true ? 'thinking…' : 'nothing running'}</div>`}
    <div class="hdr">parked</div><${Needs}>live park samples need the park-tick frame (gap 7)</${Needs}>`;
}

function DiffTab({ s }) {
  const files = [...s.touched.value];
  return html`<div class="hdr">changed</div>
    ${files.length ? files.map(([f, n]) => html`<div class="kv"><span class="grow lc" title=${f}>${f}</span><span class="faint">${n} call${n > 1 ? 's' : ''}</span></div>`)
      : html`<div class="faint">no edit or write calls yet</div>`}`;
}

function TreeTab({ s }) {
  const hl = s.hello.value;
  return html`<div class="faint">${hl ? tildify(hl.cwd) : ''}</div><${Needs}>file tree needs the hub route GET /s/<id>/tree (M3)</${Needs}>`;
}

// The graph tab's slot. M3 replaces this with a Preact graph component
// (see web-ui.md section 10); it gets the same { s, sid } props as every tab.
const GraphTab = () => html`<${Needs}>turn graph needs the hub (M3)</${Needs}>`;

// Inspector tab -> component. Every tab is a Preact component taking
// { s: Session, sid }, so a new tab (or a real graph) is one entry here.
export const INSPECTOR = { rec: Rec, tree: TreeTab, diff: DiffTab, graph: GraphTab, run: RunTab };

function Right() {
  const tab = ui.right.value, rail = ui.rrail.value, p = focused.value;
  const Tab = INSPECTOR[tab];
  const body = p && !rail ? html`<${Tab} key=${tab} s=${p.s} sid=${p.sid} />` : html`<div class="faint">no pane focused</div>`;
  return html`<aside id="right" class="col side frame">
    ${!rail && html`<${Tabs} names=${RIGHT_TABS} cur=${tab} pick=${(n) => { ui.right.value = n; }} collapse=${() => toggle('rrail')} title="collapse to a rail (ctrl+i)" />`}
    ${!rail && html`<div class="inner">${body}</div>`}
    ${rail && html`<div class="rail k" role="button" tabindex="0" title="open the inspector (ctrl+i)" onClick=${() => toggle('rrail')}>${tab} ▾</div>`}
  </aside>`;
}

function Divider({ d }) {
  const drag = (e) => {
    e.preventDefault();
    const el = e.currentTarget, box = el.parentElement.getBoundingClientRect();
    el.setPointerCapture(e.pointerId);
    const move = (m) => {
      const at = d.dir === 'row' ? m.clientX - box.left : m.clientY - box.top;
      ui.tree.value = tile.setRatio(ui.tree.value, d.path, (at - d.origin) / d.size);
    };
    // A drag ends on pointerup, and also when the browser takes the pointer
    // away (pointercancel) or capture is lost; otherwise the divider stays stuck.
    const ends = ['pointerup', 'pointercancel', 'lostpointercapture'];
    const up = () => {
      el.removeEventListener('pointermove', move);
      for (const t of ends) el.removeEventListener(t, up);
      el.classList.remove('drag');
    };
    el.classList.add('drag');
    el.addEventListener('pointermove', move);
    for (const t of ends) el.addEventListener(t, up);
  };
  const style = d.dir === 'row' ? `left:${d.x}px;top:${d.y}px;height:${d.h}px` : `left:${d.x}px;top:${d.y}px;width:${d.w}px`;
  return html`<div class=${'divider ' + d.dir} style=${style} title="drag to resize" onPointerDown=${drag}></div>`;
}

function Tiles() {
  const ref = useRef(null);
  useLayoutEffect(() => {
    const el = ref.current;
    const size = () => {
      const r = el.getBoundingClientRect();
      const b = ui.box.value;
      if (b.w !== r.width || b.h !== r.height) ui.box.value = { w: r.width, h: r.height, ...cells() };
    };
    size(); // now, not only when a ResizeObserver fires: it never does in a tab that is not drawing
    const ro = new ResizeObserver(size);
    ro.observe(el);
    addEventListener('resize', size);
    return () => { ro.disconnect(); removeEventListener('resize', size); };
  }, []);
  const panes = ui.panes.value, tree = ui.tree.value, box = ui.box.value, f = ui.focus.value, menu = ui.menu.value;
  const lay = tile.layout(tree, box);
  const rect = new Map(lay.panes.map((r) => [r.id, r]));
  let body;
  if (!ui.token) {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle r">no token</div>
      <div class="empty">${'this page has no token.\n\nopen it from the launcher, or from the URL `eidolon web` printed\nwith #token=<contents of the token file> on the end.\n\nthe token is never sent to a server in the URL: the fragment stays in the browser.'}</div></div>`;
  } else if (!panes.length) {
    body = html`<div class="pane frame tiles-empty" style="inset:0"><div class="ftitle faint">no pane</div>
      <div class="empty">no pane open.${'\n\n'}<${K} cls="y" on=${() => openPane()}>:open</${K}>  reconnect this session${'\n'}<span class="faint">ctrl+enter  new pane with a picker (arrives with the hub, M2)</span></div></div>`;
  } else {
    body = [
      ...panes.map((p) => rect.get(p.id) && html`<${Pane} key=${p.id} p=${p} rect=${rect.get(p.id)} focused=${p.id === f} menuOpen=${menu === p.id} act=${act} />`),
      ...lay.dividers.map((d) => html`<${Divider} key=${d.path} d=${d} />`),
    ];
  }
  return html`<div id="tiles" class="col" ref=${ref}>${body}</div>`;
}

function Hint() {
  const h = ui.hint.value;
  if (!h) return html`<div id="hint"></div>`;
  if (h.mode === 'toast') return html`<div id="hint" class=${'toast' + (h.err ? ' err' : '')} aria-live="polite">${h.text}</div>`;
  if (h.mode === 'which') {
    return html`<div id="hint">${KEYS.map(([k, label, fn]) => html`<${K} cls="pair" on=${() => { ui.hint.value = null; fn(); }}><span class="key">${k}</span> ${label}</${K}>`)}</div>`;
  }
  const text = ui.cmd.value || '';
  const m = matches(text);
  return html`<div id="hint" class="menu">${m.length ? m.map((c, i) => html`<${K} cls=${'pair' + (i === 0 && text ? ' sel' : '')} on=${() => runCmd(c)}>${c}</${K}>${i === 0 && text ? html`<span class="faint"> ${COMMANDS[c][0]}   </span>` : ' '}`)
    : html`<span class="faint">session commands arrive with the registry (M4)</span>`}
    <${K} cls="pair faint" on=${closeCmd}>esc close</${K}></div>`;
}

function CmdLine() {
  const ref = useRef(null);
  useLayoutEffect(() => { ref.current && ref.current.focus(); }, []);
  const key = (e) => {
    if (e.key === 'Enter') { e.preventDefault(); runCmd(); }
    else if (e.key === 'Escape') { e.preventDefault(); closeCmd(); }
    else if (e.key === 'Tab') {
      e.preventDefault();
      const m = matches(ui.cmd.value);
      if (m.length) ui.cmd.value = m[0] + ' ';
    }
  };
  const blur = () => setTimeout(() => { if (ui.cmd.value != null && document.activeElement !== ref.current) closeCmd(); }, 200);
  return html`<div id="cmd"><span class="colon">:</span><input ref=${ref} type="text" spellcheck="false" autocomplete="off"
    aria-label="command" value=${ui.cmd.value} onInput=${(e) => { ui.cmd.value = e.currentTarget.value; }} onKeyDown=${key} onBlur=${blur} /></div>`;
}

function Status() {
  if (ui.cmd.value != null) return html`<footer id="status" class="cmd"><${CmdLine} /></footer>`;
  const p = focused.value, s = p && p.s, hl = s && s.hello.value;
  const conn = s ? s.conn.value : null, run = s ? s.running.value : null;
  const off = !p || !['live', 'replaying'].includes(conn);
  const badge = !p ? 'NONE' : s.bye ? 'BYE' : off ? 'OFF' : run === true ? 'RUN' : run === false ? 'IDLE' : '?';
  const cls = { RUN: 'run', IDLE: 'idle', '?': 'unknown' }[badge] || 'off';
  const ctx = s && s.ctx.value, budget = s && s.budget.value, asks = s ? s.asks.value.length : 0;
  const hk = (k, label, fn, opt) => html`<${K} cls=${opt ? 'opt' : ''} on=${fn}><span class="key">${k}</span> ${label}</${K}>`;
  return html`<footer id="status"><span class="norm">
    <span class=${'badge ' + cls} title=${badge === '?' ? 'this page connected after the turn began or before any turn event' : ''}>${badge}</span>
    ${badge === '?' && html`<span class="faint unk">state unknown until next turn event</span>`}
    ${hl && html`<span class="c b">${hl.model || '?'}</span>`}
    ${hl && hl.yolo && html`<span class="yolo">yolo</span>`}
    ${ctx != null && html`<span class="faint">ctx ${ctx >= 1000 ? (ctx / 1000).toFixed(1) + 'k' : ctx} tok</span>`}
    ${budget != null && budget < 3 && html`<span class="warn">${budget} calls left</span>`}
    ${asks > 0 && html`<${K} cls="y" title="focus the first ask" on=${() => cycleAsk(1)}>${asks} ask${asks > 1 ? 's' : ''}</${K}>`}
    ${p && conn !== 'live' && html`<span class=${conn === 'replaying' ? 'faint' : 'warn'}>${conn}</span>`}
    <span class="grow"></span>
    <span class="hints">
      ${s && run !== false && !s.bye && hk('^c', 'stop', cancel)}
      ${hk(':', 'commands', openCmd)}
      ${hk('^b', 'sidebar', () => toggle('lrail'), true)}
      ${hk('^i', 'inspector', () => toggle('rrail'), true)}
      ${hk('?', 'keys', whichKey)}
    </span>
  </span></footer>`;
}

function App() {
  const cls = (ui.lrail.value ? 'lrail ' : '') + (ui.rrail.value ? 'rrail' : '');
  return html`<main id="main" class=${cls}><${Left} /><${Tiles} /><${Right} /></main><${Hint} /><${Status} />`;
}

// ---------------------------------------------------------------- cells
let cell = { cw: 8, ch: 19 };
function measure() {
  const probe = document.createElement('span');
  probe.textContent = 'M'.repeat(80);
  probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre;';
  document.body.append(probe);
  const r = probe.getBoundingClientRect();
  probe.remove();
  const fs = parseFloat(getComputedStyle(document.documentElement).fontSize);
  cell = { cw: r.width / 80, ch: Math.round(fs * 1.35) };
  document.documentElement.style.setProperty('--cw', cell.cw + 'px');
  document.documentElement.style.setProperty('--ch', cell.ch + 'px');
}
const cells = () => cell;

// ---------------------------------------------------------------- keys
document.addEventListener('keydown', (e) => {
  const k = e.key.toLowerCase(), t = e.target;
  const typing = t && (t.tagName === 'TEXTAREA' || t.tagName === 'INPUT');
  // global, even while typing
  if (e.ctrlKey && !e.altKey && k === 'b') { e.preventDefault(); toggle('lrail'); return; }
  if (e.ctrlKey && !e.altKey && k === 'i') { e.preventDefault(); toggle('rrail'); return; }
  // alt+w: by the character first (so AZERTY's w, on the KeyZ key, works),
  // by the physical key only when the character is not ASCII (macOS Option
  // turns alt+w into '∑'); and never while there is text in the composer,
  // where it would eat a keystroke.
  const isW = e.key === 'w' || e.key === 'W' || (e.code === 'KeyW' && /[^\x00-\x7f]/.test(e.key));
  if (e.altKey && !e.ctrlKey && !e.metaKey && isW) {
    if (t && t.tagName === 'INPUT') return;
    if (t && t.tagName === 'TEXTAREA' && t.value) return;
    e.preventDefault(); closePane(); return;
  }
  if (e.ctrlKey && e.key === 'Enter') { e.preventDefault(); toast('new pane with a picker arrives with the hub (M2); :vsplit mirrors this one'); return; }
  if (typing) return;
  if (e.key === ':') { e.preventDefault(); openCmd(); return; }
  if (e.key === '?') { e.preventDefault(); whichKey(); return; }
  if (e.key === 'Escape') { ui.hint.value = null; ui.menu.value = null; return; }
  const p = focused.value;
  if (!p) return;
  if (e.ctrlKey && k === 'c' && !String(getSelection())) { e.preventDefault(); p.s.cancel(); return; }
  if (e.key === 'Tab' && p.s.asks.value.length) { e.preventDefault(); cycleAsk(e.shiftKey ? -1 : 1); return; }
  if (e.ctrlKey || e.altKey || e.metaKey || e.repeat) return;
  if (t && t.closest && t.closest('.ask')) return; // an ask's own keys, handled by its block
  if (e.key === '[') { cycleRight(-1); return; }
  if (e.key === ']') { cycleRight(1); return; }
  if (e.key === 'i' || e.key === 'Enter') {
    if (t && t.getAttribute && t.getAttribute('role') === 'button') return;
    e.preventDefault(); toComposer();
  }
});

// A click anywhere outside an open pane menu closes it.
document.addEventListener('mousedown', (e) => {
  if (ui.menu.value && !e.target.closest('.pmenu, .ptitle')) ui.menu.value = null;
});

// ---------------------------------------------------------------- boot
measure();
ui.token = takeToken();
// A token pasted into this tab's URL later (a hash change, no reload) is
// taken and scrubbed the same way; it is never left sitting in the URL.
addEventListener('hashchange', () => { if (/token=/.test(location.hash)) takeToken(); });
const root = document.getElementById('root');
render(html`<${App} />`, root);
let wasNarrow = innerWidth < 760;
addEventListener('resize', () => {
  const narrow = innerWidth < 760;
  if (narrow && !wasNarrow) batch(() => { ui.lrail.value = true; ui.rrail.value = true; });
  wasNarrow = narrow;
});
if (ui.token) openPane(DOOR); // not in rAF: a tab opened in the background must still connect
