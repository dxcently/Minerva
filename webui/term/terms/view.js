// The terminals' screens. One xterm per terminal (vendor/xterm.js, loaded when the first screen is made), which is moved between hosts and
// never duplicated: a stack item's clipped view, a terminal pane (T4b), or the parking shelf (a folded terminal, or none). A screen is made
// only while the ⌨ tab is drawn or a host shows its terminal: makeScreens runs when the tab mounts, so the module is not loaded at page
// boot, and a terminal the page holds before that queues its bytes. Once made, a screen is written as the stream brings bytes; the stream is
// open while a view shows and for 10 s after the last one, so a screen keeps up for that long and then stops until the stream opens again.
//
// The perch answers the queries a shell may ask (terms/view.js QUERIES is the list; perch/src/term/README.md "The screen writes back"),
// so the page's xterm swallows exactly those, or the shell would read two answers. Links open only as http or https, on ctrl+click.
import { roster, live, setHooks, bell, resync, typed, BACKLOG_MAX, clipText, TITLE_MAX } from './model.js';
import { tabShown } from './activity.js';
import { host } from '../core/registry.js';

export const FONT = '"Cascadia Mono", Consolas, monospace';   // the terminals' own font (D11): the system mono, not the page's
export const LIVE_ROWS = 8;                                    // the stack shows the last this many rows, at 1:1 cells
const BASE = [...'0123456789ABCDEF'].map((d) => 'base0' + d);  // the sixteen --base0X the look writes on <html>

// ---------------------------------------------------------------- what the page answers, and what it does not
// The queries the perch answers (DA1, DA2, DSR 5 and 6, DECRQM ANSI and DEC, CSI 18 t, OSC 4/10/11/12 asking) and the two it never answers
// (DECRQSS, XTVERSION): all are swallowed here, so a program gets one answer or none, never two. `when` says which forms are the query.
// `kind` is how xterm's parser takes it: csi (an id with prefix/intermediates/final), dcs (the same) or osc (the number, the string given).
export const QUERIES = [
  { name: 'DA1', kind: 'csi', id: { final: 'c' }, when: (p) => p.length === 0 || p[0] === 0 },
  { name: 'DA2', kind: 'csi', id: { prefix: '>', final: 'c' }, when: (p) => p.length === 0 || p[0] === 0 },
  { name: 'DSR', kind: 'csi', id: { final: 'n' }, when: (p) => p.length === 1 && (p[0] === 5 || p[0] === 6) },
  { name: 'DECRQM', kind: 'csi', id: { intermediates: '$', final: 'p' }, when: () => true },
  { name: 'DECRQM DEC', kind: 'csi', id: { prefix: '?', intermediates: '$', final: 'p' }, when: () => true },
  { name: 'CSI 18 t', kind: 'csi', id: { final: 't' }, when: (p) => p.length === 1 && p[0] === 18 },
  { name: 'DECRQSS', kind: 'dcs', id: { intermediates: '$', final: 'q' }, when: () => true },
  { name: 'XTVERSION', kind: 'csi', id: { prefix: '>', final: 'q' }, when: (p) => p.length === 0 || p[0] === 0 },
  { name: 'OSC 4/10/11/12', kind: 'osc', ids: [4, 10, 11, 12], when: (data) => String(data).includes('?') },
];

// Registers QUERIES on one xterm parser. A handler that returns true takes the sequence: xterm writes no reply.
export function registerQueries(parser) {
  for (const q of QUERIES) {
    if (q.kind === 'csi') parser.registerCsiHandler(q.id, (p) => q.when(p));
    else if (q.kind === 'dcs') parser.registerDcsHandler(q.id, () => q.when());
    else for (const n of q.ids) parser.registerOscHandler(n, (data) => q.when(data));
  }
}

// ---------------------------------------------------------------- the theme, from the page's own colours
// xterm takes concrete colours, so they are read from the --base0X the look wrote, once per change. A value that is not #rrggbb is left
// to xterm's default rather than handed to it.
const hex = (x) => (typeof x === 'string' && /^#[0-9a-f]{6}$/i.test(x.trim()) ? x.trim() : undefined);
export function xtermTheme(v) {
  const c = (k) => hex(v[k]);
  return {
    background: c('base00'), foreground: c('base05'), cursor: c('base06'), cursorAccent: c('base00'),
    selectionBackground: c('base02'),
    black: c('base00'), red: c('base08'), green: c('base0B'), yellow: c('base0A'), blue: c('base0D'), magenta: c('base0E'), cyan: c('base0C'), white: c('base05'),
    brightBlack: c('base03'), brightRed: c('base08'), brightGreen: c('base0B'), brightYellow: c('base0A'), brightBlue: c('base0D'),
    brightMagenta: c('base0E'), brightCyan: c('base0C'), brightWhite: c('base07'),
  };
}
export function readBase() {
  const cs = getComputedStyle(document.documentElement), v = {};
  for (const k of BASE) v[k] = cs.getPropertyValue('--' + k);
  return v;
}

// ---------------------------------------------------------------- links
// Only http and https open, and only on ctrl+click (or cmd on a Mac). xterm's own default would ask and open any scheme, so the handler is
// set on every terminal; no window.open is ever called for another protocol.
export function linkUrl(text) {
  let u;
  try { u = new URL(String(text)); } catch { return null; }
  return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : null;
}
export const LINKS = {
  allowNonHttpProtocols: false,
  activate(ev, text) {
    if (!ev || !(ev.ctrlKey || ev.metaKey)) return;
    const u = linkUrl(text);
    if (!u) { if (typeof host.toast === 'function') host.toast('that link is not http or https: not opened', true); return; }
    window.open(u, '_blank', 'noopener');
  },
};

// ---------------------------------------------------------------- loading and the shelf
let xt = null, loading = null, park = null, themed = false;
const shells = new Map(); // id -> the xterm made for it (the view's; `live` holds the bookkeeping)

// The module loads on first need, after the terminal font is ready (a measure taken before it is wrong). No fit addon is loaded: a screen
// takes the perch's size (the roster row, then term-size), and the page never fits it to an element.
async function load() {
  if (xt) return xt;
  if (!loading) {
    loading = (async () => {
      try { await document.fonts.load(`14px ${FONT}`); } catch { /* measured on the fallback */ }
      const t = await import('../vendor/xterm.js');
      xt = { Terminal: t.Terminal };
      return xt;
    })();
  }
  return loading;
}

// A shelf out of sight, laid out (so an xterm on it measures), that holds every terminal no host shows.
function shelf() {
  if (!park) {
    park = document.createElement('div');
    park.className = 'tpark';
    park.setAttribute('aria-hidden', 'true');
    document.body.appendChild(park);
  }
  return park;
}

const reducedMotion = () => typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
const enc = new TextEncoder();
const latin1 = (s) => Uint8Array.from(s, (c) => c.charCodeAt(0) & 0xff);

// ---------------------------------------------------------------- one screen
export function create(e, x) {
  const row = roster.peek().find((r) => r.id === e.id);
  const t = new x.Terminal({
    cols: row ? row.cols : 120, rows: row ? row.rows : 32, scrollback: 2000,
    fontFamily: FONT, fontSize: 14, lineHeight: 1.2,
    cursorBlink: !reducedMotion(), cursorStyle: 'block',
    theme: xtermTheme(readBase()), linkHandler: LINKS,
  });
  registerQueries(t.parser);
  t.open(e.host || shelf());
  t.onData((s) => typed(e.id, enc.encode(s)));
  t.onBinary((s) => typed(e.id, latin1(s)));
  t.onBell(() => bell(e.id));
  e.term = t;
  shells.set(e.id, t);
  watchTheme();
  const waiting = e.queue.splice(0);
  e.queued = 0;
  for (const c of waiting) push(e, c.bytes, c.reset, c.cont);
}

// A screen is made only while the ⌨ tab is drawn or a host shows the terminal. Until one does, the terminal's bytes queue (push) and the
// module is not loaded.
function ensure(id) {
  const e = live.get(id);
  if (!e || e.term || e.loading) return;
  if (!e.host && !tabShown.peek()) return;
  e.loading = true;
  load().then((x) => {
    const cur = live.get(id);
    if (cur && !cur.term) create(cur, x);
  }).catch((err) => {
    console.error('minerva: the terminal view did not load:', err);
  }).finally(() => {
    const cur = live.get(id);
    if (cur) cur.loading = false;
  });
}

// Bytes to a screen. A reset is the RIS sequence written in order, so it lands after what is already waiting to be parsed. Past
// BACKLOG_MAX unparsed bytes the terminal is resynced (the stream reconnects and sends a reset), once until that reset arrives. The
// reset clears that mark only when its own bytes are parsed, so the backlog it waits behind cannot resync it again. A reset is a
// snapshot that replaces the screen, and so are the term-out chunks that follow a reset over 32 KiB (`cont`, marked by onFrame): their
// bytes are not counted either. The ring can be 8 MiB (RING_MAX), so a reset can be bigger than the bound; counting its bytes would
// resync on every reset. Before a screen exists the output queues under the same bound: past it the queue is dropped and the terminal
// resynced, and the output that comes before the reset is not kept.
export function push(e, bytes, reset, cont = false) {
  const n = reset || cont ? 0 : bytes.length;
  if (!e.term) {
    if (reset) { e.queue = []; e.queued = 0; e.resyncing = false; }
    else if (e.resyncing) return;
    e.queue.push({ bytes, reset, cont });
    e.queued += n;
    if (e.queued > BACKLOG_MAX) { e.queue = []; e.queued = 0; e.resyncing = true; resync(e.id); }
    return;
  }
  const t = e.term;
  if (reset) t.write('\x1bc');
  e.backlog += n;
  t.write(bytes, () => { e.backlog = Math.max(0, e.backlog - n); if (reset) e.resyncing = false; follow(e); });
  if (e.backlog > BACKLOG_MAX && !e.resyncing) { e.resyncing = true; resync(e.id); }
}

// The clipped view keeps the cursor in sight: its last rows, and its column, at the cells' own size. It never resizes the terminal.
function follow(e) {
  if (!e.host || !e.term || e.follow || typeof requestAnimationFrame !== 'function') return;
  e.follow = true;
  requestAnimationFrame(() => {
    e.follow = false;
    const h = e.host, t = e.term;
    if (!h || !t) return;
    const scr = t.element && t.element.querySelector('.xterm-screen');
    const ch = (scr && scr.getBoundingClientRect().height / t.rows) || 16;
    const cw = (scr && scr.getBoundingClientRect().width / t.cols) || 8;
    if (Math.abs(ch - (e.ch || 0)) > 0.5) { e.ch = ch; h.style.setProperty('--tch', ch + 'px'); }
    const buf = t.buffer.active;
    h.scrollTop = Math.max(0, buf.cursorY - (LIVE_ROWS - 1)) * ch;
    h.scrollLeft = Math.max(0, buf.cursorX - Math.floor(h.clientWidth / cw) + 4) * cw;
  });
}

function watchTheme() {
  if (themed || typeof MutationObserver !== 'function') return;
  themed = true;
  // The look writes the sixteen colours as style properties on <html>; any such change re-reads them.
  new MutationObserver(() => {
    const v = xtermTheme(readBase());
    for (const t of shells.values()) t.options.theme = v;
  }).observe(document.documentElement, { attributes: true, attributeFilter: ['style'] });
}

// ---------------------------------------------------------------- placing a screen (a stack item, a pane)
// `el` takes the terminal's element while it is mounted there; one element, never a copy. A second mount moves it again.
export function mountTerm(id, el) {
  const e = live.get(id);
  if (!e || !el) return;
  e.host = el;
  if (e.term) { el.appendChild(e.term.element); follow(e); }
  else ensure(id);
}
// Gives the screen back to the shelf, if `el` is still the host (a later mount elsewhere is left alone).
export function unmountTerm(id, el) {
  const e = live.get(id);
  if (!e || e.host !== el) return;
  e.host = null;
  if (e.term) shelf().appendChild(e.term.element);
}
// The xterm for a terminal the page has (null until its module is in), and whether its element is in a host right now.
export const screenOf = (id) => shells.get(id) || null;
export const mountedIn = (id) => { const e = live.get(id); return e && e.host ? e.host : null; };

// The ⌨ tab, on mount: a screen for each terminal the page holds. A folded terminal's screen goes to the shelf (laid out, hidden), which
// is where xterm measures it.
export function makeScreens() {
  for (const id of [...live.keys()]) ensure(id);
}

// ---------------------------------------------------------------- the stream's changes, onto the screens
setHooks({
  made: (id) => ensure(id),
  out: (id, bytes, cont) => { const e = live.get(id); if (e) push(e, bytes, false, cont); },
  reset: (id, bytes) => { const e = live.get(id); if (!e) return; push(e, bytes, true); },
  size: (id, cols, rows) => { const t = shells.get(id); if (t) t.resize(cols, rows); },
  gone: (id) => {
    const t = shells.get(id);
    shells.delete(id);
    if (t) { try { t.dispose(); } catch { /* a screen that will not go is left to the garbage */ } }
  },
});

// The title as the header shows it: plain text, cut to TITLE_MAX characters. (The page never takes a title from the program.)
export const titleText = (s) => clipText(s, TITLE_MAX);
