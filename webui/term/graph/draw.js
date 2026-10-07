// Draws a graph as SVG in the page's TUI look: square boxes, monospace names, labels sitting on the lines. It takes the drawing project.js
// makes ({ states, edges, root_on?, initial? }, or the same shape from anything else, W3's turn graph) and the marks of a run, and returns
// a Preact tree. No innerHTML anywhere: every name is text.
//
//   layout(states, edges, { initial, rootOn, engine })   boxes, frames and routed lines, in SVG units; pure, so it runs under node
//   drawGraph(graph, marks, { engine })                  the SVG vnode, laid out by layout(); marks: { current, visited, final, failed }
//
// A state is { path, kind, label?, description?, initial? }. kind is leaf | compound | final | history (anything else is drawn as a leaf).
// A compound state is a frame around the states whose path is its path plus one segment, so a dotted id is the child of what precedes the
// dot when that is a compound state in the list, and only a name otherwise. `label` is what is written (default: the last segment).
// An edge is { from, to, kind, event?, guard?, reenter?, label? }:
//   - to null, or back to the same state, is a loop: a row written inside the box, not a line. kind 'initial' is not a line either: the
//     state it points at (and the graph's own `initial`) is marked with a triangle.
//   - a line says `label`, else the event of an `on`, the guard type of an `always`, 'done' for an `onDone`. Edges between the same two
//     states are one line with one label (identify -> network: "entails · count").
//   - from or to "*" (the root `on`: from any state) is a box written "*", with a dashed line.
//   - dagre cannot route to a frame, so an end on a compound state is laid out against its initial child. A `from` is cut where the line
//     leaves the frame; an onDone leaves from the frame's final state.
// If dagre throws, the graph is drawn as a column with its lines down one side (layout().fallback, .error), never as nothing.
//
// marks are Sets (or arrays, or one string) of state paths; a frame counts as reached when anything inside it is.
//   current  the state a run is in: lit            visited  states it has been in (once any mark is given, the others go faint)
//   final    the final state it ended in           failed   states it failed in
// Classes (graph.css): svg gdraw [live]; g.gc frame, g.gn box, g.ge line; k-<kind>, initial, any, current, visited, final, failed;
// frames also has-current, has-visited; a line is lit when both its ends are reached.
import { html } from '../core/ui.js';
import { layout as dagreLayout, graphlib } from '../vendor/dagre.js';

// Units are SVG user units, which the page scales to its container by viewBox. A character is CW wide: the label text is fitted to it
// with textLength, so the font in use (VT323, Departure Mono, the system mono) changes the look but never the fit.
export const CW = 8;
const BOX_H = 28; // a box with only its name
const ROW_H = 18; // each loop row in a box
const HDR_H = 20; // a frame's title row
const LBL_H = 16; // the label on a line
const MAX_CH = 22; // longest label in characters; more is cut with an ellipsis (the title keeps all of it)
const NODE_PAD = 2; // characters either side of a name
const ELBOW = 9; // how far from a box a line turns
const ARROW = 7;
const MARK = '▸ ';
const LOOP = '↻ ';
const ANY = '*';
const GHOST = '\u0001'; // prefix of the hidden title row of a frame

const isStr = (v) => typeof v === 'string';
const cut = (s, n) => (s.length > n ? s.slice(0, n - 1) + '…' : s);
const lastSeg = (p) => p.slice(p.lastIndexOf('.') + 1);
const kindClass = (k) => 'k-' + String(k).toLowerCase().replace(/[^a-z0-9]+/g, '-').slice(0, 24);
const asSet = (v) => (v instanceof Set ? v : Array.isArray(v) ? new Set(v) : isStr(v) && v ? new Set([v]) : new Set());

// What a line says. An `on` is its event; an `always` its guard (an unguarded one says nothing); an onDone says done.
function edgeText(e) {
  if (isStr(e.label)) return e.label;
  if (e.kind === 'on') return e.event || '';
  if (e.kind === 'always') return e.guard || '';
  if (e.kind === 'onDone') return 'done';
  return e.event || '';
}

// ---- the model: states and edges -> boxes, frames, loops and lines to route -------------------------------------------------------

function model(states, edges, opts) {
  const byId = new Map();
  for (const s of Array.isArray(states) ? states : []) {
    if (!s || !isStr(s.path) || s.path === '' || s.path === ANY || byId.has(s.path)) continue;
    byId.set(s.path, {
      id: s.path, path: s.path, kind: isStr(s.kind) ? s.kind : 'leaf', label: isStr(s.label) ? s.label : lastSeg(s.path),
      description: isStr(s.description) ? s.description : '', parent: null, children: [], loops: [], initial: false, any: false,
      initialChild: isStr(s.initial) ? s.initial : '',
    });
  }
  // nesting: the longest proper prefix that is a compound state in the list
  for (const n of byId.values()) {
    for (let i = n.path.lastIndexOf('.'); i > 0; i = n.path.lastIndexOf('.', i - 1)) {
      const p = byId.get(n.path.slice(0, i));
      if (p && p.kind === 'compound') { n.parent = p; p.children.push(n); break; }
    }
  }
  for (const n of byId.values()) n.cluster = n.kind === 'compound' && n.children.length > 0;

  // a state's stand-in where a line must end on a box: a frame is its initial child (descending), else its first
  const rep = (n) => {
    for (let guard = 0; n.cluster && guard < 32; guard++) {
      const init = n.children.find((c) => c.path === n.path + '.' + n.initialChild) || n.children[0];
      n = init;
    }
    return n;
  };
  for (const e of Array.isArray(edges) ? edges : []) if (e && e.kind === 'initial' && byId.has(e.from)) byId.get(e.from).initialChild = lastSeg(e.to || '');
  const finalOf = (n) => n.children.find((c) => c.kind === 'final') || null;

  const mark = (p) => { const n = byId.get(p); if (n) n.initial = true; };
  if (isStr(opts.initial) && opts.initial) mark(opts.initial);
  for (const e of Array.isArray(edges) ? edges : []) if (e && e.kind === 'initial' && isStr(e.to)) mark(e.to);

  const any = { id: ANY, path: ANY, kind: 'any', label: ANY, description: 'any state', parent: null, children: [], loops: [], initial: false, any: true, cluster: false };
  let anyUsed = false;
  const groups = new Map();
  let skipped = 0;
  const all = [...(Array.isArray(edges) ? edges : []), ...(Array.isArray(opts.rootOn) ? opts.rootOn : [])];
  for (const e of all) {
    if (!e || e.kind === 'initial') continue;
    const from = e.from === ANY ? any : byId.get(e.from);
    const to = e.to === null || e.to === undefined ? from : e.to === ANY ? any : byId.get(e.to);
    if (!from || !to) { skipped++; continue; }
    if (from === any) anyUsed = true;
    const text = edgeText(e);
    const a = from.cluster ? (e.kind === 'onDone' && finalOf(from)) || rep(from) : from;
    const b = to.cluster ? rep(to) : to;
    if (from === to || a === b) { // a loop: a row in the box
      (from.cluster ? from : a).loops.push({ text: text || e.kind, title: loopTitle(e) });
      continue;
    }
    const key = from.id + '\u0000' + to.id;
    let g = groups.get(key);
    if (!g) groups.set(key, (g = { from, to, a, b, items: [], parts: [], kind: e.kind }));
    g.items.push(e);
    if (text !== '' && !g.parts.includes(text)) g.parts.push(text);
    if (g.kind !== e.kind) g.kind = 'mixed';
  }
  const nodes = [...byId.values()];
  if (anyUsed) nodes.push(any);
  for (const n of nodes) size(n);
  return { nodes, byId, groups: [...groups.values()], skipped };
}

function loopTitle(e) {
  const bits = [e.kind === 'on' ? e.event : e.kind, e.to === null || e.to === undefined ? 'stays in the state' : 'enters the state again'];
  return bits.filter(Boolean).join(': ');
}

// a box is its name (and a triangle if it is where its graph starts) with a row for each loop; a frame is as wide as its title
function size(n) {
  const name = cut((n.initial ? MARK : '') + n.label, MAX_CH);
  n.text = name;
  const rows = n.loops.map((l) => cut(LOOP + l.text, MAX_CH));
  n.rows = rows;
  if (n.cluster) {
    n.title = name;
    n.w = (n.title.length + 2) * CW;
    n.h = HDR_H;
    return;
  }
  const chars = Math.max(name.length, ...rows.map((r) => r.length));
  n.w = (chars + 2 * NODE_PAD) * CW;
  n.h = BOX_H + (rows.length ? rows.length * ROW_H + 2 : 0);
}

// ---- placing: dagre -----------------------------------------------------------------------------------------------------------------

// the child of frame n that holds state x (x itself, or a frame around it), or null
function childHolding(n, x) {
  for (; x; x = x.parent) if (x.parent === n) return x;
  return null;
}

function place(m) {
  const g = new graphlib.Graph({ multigraph: true, compound: true });
  g.setGraph({ rankdir: 'TB', nodesep: 20, ranksep: 18, edgesep: 10, marginx: 10, marginy: 10 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const n of m.nodes) g.setNode(n.id, n.cluster ? {} : { width: n.w, height: n.h });
  for (const n of m.nodes) {
    if (!n.cluster) continue;
    // dagre has no title row for a frame, so the title is a hidden box inside it, held above the states nothing else in the frame leads to
    g.setNode(GHOST + n.id, { width: n.w, height: n.h });
    g.setParent(GHOST + n.id, n.id);
    const into = new Set();
    for (const gr of m.groups) {
      const from = childHolding(n, gr.a), to = childHolding(n, gr.b);
      if (from && to && from !== to) into.add(to);
    }
    const tops = n.children.filter((c) => !into.has(c));
    (tops.length ? tops : [n.children[0]]).forEach((c, i) => g.setEdge(GHOST + n.id, c.cluster ? GHOST + c.id : c.id, { minlen: 1 }, 'h' + i));
  }
  for (const n of m.nodes) if (n.parent && n.parent.cluster) g.setParent(n.id, n.parent.id);
  m.groups.forEach((gr, i) => {
    gr.text = cut(gr.parts.join(' · '), MAX_CH);
    gr.name = 'e' + i;
    g.setEdge(gr.a.id, gr.b.id, gr.text ? { width: gr.text.length * CW + 8, height: LBL_H, labelpos: 'c' } : {}, gr.name);
  });
  dagreLayout(g);

  const nodes = [];
  for (const n of m.nodes) {
    const d = g.node(n.id);
    const b = box(n, d.x, d.y, n.cluster ? d.width : n.w, n.cluster ? d.height : n.h);
    if (n.cluster) { // where dagre put the title row: the frame's title is written there
      const t = g.node(GHOST + n.id);
      b.tx = Math.round(t.x - t.width / 2);
      b.ty = Math.round(t.y);
    }
    nodes.push(b);
  }
  const edges = m.groups.map((gr) => {
    const d = g.edge(gr.a.id, gr.b.id, gr.name);
    let pts = ortho(d.points);
    if (gr.from.cluster) pts = leave(pts, nodes.find((n) => n.id === gr.from.id));
    return edgeOut(gr, pts, gr.text ? d.x : 0, gr.text ? d.y : 0);
  });
  // dagre can answer a line through a frame with no position for some of its points (a cycle through a frame's border): the fallback then
  for (const e of edges) if (!e.points.every((p) => Number.isFinite(p.x) && Number.isFinite(p.y))) throw new Error('dagre gave a line no position');
  for (const n of nodes) if (![n.x, n.y, n.w, n.h].every(Number.isFinite)) throw new Error('dagre gave a box no position');
  const gd = g.graph();
  return finish(nodes, edges, gd.width, gd.height);
}

// the dagre centre and size -> a box with its corner
function box(n, cx, cy, w, h) {
  return {
    id: n.id, path: n.path, label: n.text, kind: n.kind, cluster: n.cluster, initial: n.initial, any: n.any, parent: n.parent ? n.parent.id : null,
    description: n.description, title: n.title || '', rows: n.rows, loops: n.loops, x: Math.round(cx - w / 2), y: Math.round(cy - h / 2), w: Math.round(w), h: Math.round(h),
  };
}

function edgeOut(gr, points, lx, ly) {
  return {
    from: gr.from.id, to: gr.to.id, kind: gr.kind, label: gr.text,
    title: gr.items.map((e) => [e.kind, e.event, e.guard && '(' + e.guard + ')', e.reenter && 'reenter'].filter(Boolean).join(' ')).join('\n'),
    items: gr.items, points, lx: Math.round(lx), ly: Math.round(ly), lw: gr.text ? gr.text.length * CW + 8 : 0,
  };
}

function finish(nodes, edges, w, h) {
  // dagre's own size leaves out a line that bends out past the boxes (a cycle through a frame): slide everything in if one went negative
  let left = 0, top = 0;
  for (const n of nodes) { left = Math.min(left, n.x); top = Math.min(top, n.y); }
  for (const e of edges) {
    for (const p of e.points) { left = Math.min(left, p.x - 2); top = Math.min(top, p.y - 2); }
    if (e.label) { left = Math.min(left, e.lx - e.lw / 2); top = Math.min(top, e.ly - LBL_H / 2); }
  }
  if (left < 0 || top < 0) {
    const dx = left < 0 ? Math.ceil(-left) : 0, dy = top < 0 ? Math.ceil(-top) : 0;
    for (const n of nodes) { n.x += dx; n.y += dy; if (n.tx !== undefined) { n.tx += dx; n.ty += dy; } }
    for (const e of edges) { e.lx += dx; e.ly += dy; e.points = e.points.map((p) => ({ x: p.x + dx, y: p.y + dy })); }
    w = (w || 0) + dx; h = (h || 0) + dy;
  }
  let width = w || 0, height = h || 0;
  for (const n of nodes) { width = Math.max(width, n.x + n.w + 4); height = Math.max(height, n.y + n.h + 4); }
  for (const e of edges) for (const p of e.points) { width = Math.max(width, p.x + 4); height = Math.max(height, p.y + 4); }
  return { nodes, edges, width: Math.ceil(width), height: Math.ceil(height) };
}

// dagre routes a polyline with a slope; the page draws right angles. Each step of the route becomes down, across, down, with the turn
// just off the box a line leaves or enters (so lines from one box share a run) and halfway between the dummy points in between.
function ortho(pts) {
  const out = [{ x: Math.round(pts[0].x), y: Math.round(pts[0].y) }];
  for (let i = 1; i < pts.length; i++) {
    const a = pts[i - 1], b = pts[i];
    const x1 = Math.round(a.x), y1 = Math.round(a.y), x2 = Math.round(b.x), y2 = Math.round(b.y);
    if (x1 !== x2) {
      const dir = y2 >= y1 ? 1 : -1, span = Math.abs(y2 - y1);
      const turn = pts.length === 2 ? span / 2 : i === 1 ? Math.min(ELBOW, span / 2) : i === pts.length - 1 ? span - Math.min(ELBOW, span / 2) : span / 2;
      const ty = Math.round(y1 + dir * turn);
      out.push({ x: x1, y: ty }, { x: x2, y: ty });
    }
    out.push({ x: x2, y: y2 });
  }
  return out.filter((p, i) => i === 0 || p.x !== out[i - 1].x || p.y !== out[i - 1].y);
}

// an edge from a frame starts where it leaves the frame, not at the box inside it that dagre routed from
function leave(pts, fr) {
  if (!fr) return pts;
  const inside = (p) => p.x >= fr.x && p.x <= fr.x + fr.w && p.y >= fr.y && p.y <= fr.y + fr.h;
  let i = 0;
  while (i < pts.length && inside(pts[i])) i++;
  if (i === 0 || i === pts.length) return pts;
  const p = pts[i - 1], q = pts[i];
  const t = Math.min(
    q.x !== p.x ? ((q.x > p.x ? fr.x + fr.w : fr.x) - p.x) / (q.x - p.x) : Infinity,
    q.y !== p.y ? ((q.y > p.y ? fr.y + fr.h : fr.y) - p.y) / (q.y - p.y) : Infinity,
  );
  return [{ x: Math.round(p.x + (q.x - p.x) * t), y: Math.round(p.y + (q.y - p.y) * t) }, ...pts.slice(i)];
}

// ---- the safety net: dagre threw, so a column --------------------------------------------------------------------------------------

function stack(m) {
  let y = 10, width = 0;
  const nodes = m.nodes.map((n) => {
    const w = n.cluster ? (n.title.length + 2) * CW : n.w, h = n.cluster ? BOX_H : n.h;
    const b = box(n, 10 + w / 2, y + h / 2, w, h);
    b.cluster = false;
    b.label = n.cluster ? n.title : n.text;
    y += h + 24;
    width = Math.max(width, w + 20);
    return b;
  });
  const at = new Map(nodes.map((n) => [n.id, n]));
  const edges = m.groups.map((gr, i) => {
    const a = at.get(gr.a.id), b = at.get(gr.b.id), gx = width + 6 + i * 6;
    gr.text = ''; // no room for labels in the gutter; the line's title has them
    return edgeOut(gr, [{ x: a.x + a.w, y: a.y + a.h / 2 }, { x: gx, y: a.y + a.h / 2 }, { x: gx, y: b.y + b.h / 2 }, { x: b.x + b.w, y: b.y + b.h / 2 }], 0, 0);
  });
  const r = finish(nodes, edges, width + 12 + edges.length * 6, y);
  r.fallback = true;
  return r;
}

const memo = new WeakMap(); // states array -> { edges, key, result }: a poll that hands back the same arrays costs nothing

// layout(states, edges, { initial, rootOn, engine }) -> { nodes, edges, width, height, skipped, fallback?, error? }
//   nodes  every box and frame: { id, path, label, kind, cluster, initial, any, parent, description, rows, loops, x, y, w, h (the corner and the
//          size), tx, ty (a frame's title row), title }. The box "*" is there when a root `on` is.
//   edges  one per pair of states: { from, to, kind, label, title, items (the edges it stands for), points [{x,y}], lx, ly, lw (the label) }
//   skipped  edges dropped because an end is not a state in the list
// engine 'stack' asks for the column layout directly: it is what a graph gets when dagre throws (fallback: true, error: its message).
export function layout(states, edges, opts = {}) {
  const key = (opts.initial || '') + '\u0000' + (opts.rootOn ? opts.rootOn.length : 0) + '\u0000' + (opts.engine || '');
  const hit = Array.isArray(states) ? memo.get(states) : null;
  if (hit && hit.edges === edges && hit.key === key && hit.rootOn === opts.rootOn) return hit.result;
  const m = model(states, edges, opts);
  let result;
  try {
    if (opts.engine === 'stack') result = stack(m);
    else result = m.nodes.length ? place(m) : { nodes: [], edges: [], width: 0, height: 0 };
  } catch (err) {
    result = stack(model(states, edges, opts));
    result.error = String(err && err.message ? err.message : err);
  }
  result.skipped = m.skipped;
  if (Array.isArray(states)) memo.set(states, { edges, key, rootOn: opts.rootOn, result });
  return result;
}

// ---- the tree ----------------------------------------------------------------------------------------------------------------------

const cls = (...xs) => xs.filter(Boolean).join(' ');
// Text fitted to its own length, so a font wider or narrower than a cell still sits inside its box. The triangle and the loop arrow come
// from whatever font has them, which is not the page's, so each is fitted on its own and the name after it keeps its spacing.
const one = (x, y, s, c) => html`<text class=${c} x=${x} y=${y} textLength=${s.length * CW}>${s}</text>`;
const txt = (x, y, s, c) => (s.length > 2 && (s[0] === MARK[0] || s[0] === LOOP[0]) && s[1] === ' '
  ? [one(x, y, s[0], c + ' gg'), one(x + 2 * CW + 2, y, s.slice(2), c)] : one(x, y, s, c));

export function drawGraph(graph, marks = {}, opts = {}) {
  const g = graph || {};
  marks = marks || {};
  const lay = layout(g.states, g.edges, { initial: g.initial, rootOn: g.root_on, engine: opts.engine });
  if (!lay.nodes.length) return html`<div class="gempty">no states</div>`;
  const current = asSet(marks.current), visited = asSet(marks.visited), fin = asSet(marks.final), failed = asSet(marks.failed);
  const live = current.size > 0 || visited.size > 0 || fin.size > 0 || failed.size > 0;
  const reached = new Set([...current, ...visited, ...fin, ...failed]);
  // a frame has been reached when anything inside it has
  const within = (set, id) => set.has(id) || [...set].some((p) => p.startsWith(id + '.'));
  const isReached = (id) => within(reached, id);

  const frames = lay.nodes.filter((n) => n.cluster).sort((a, b) => a.w * a.h - b.w * b.h).reverse();
  const boxes = lay.nodes.filter((n) => !n.cluster);

  const frameVN = frames.map((n) => html`<g key=${n.id} class=${cls('gc', kindClass(n.kind), n.initial && 'initial', within(current, n.id) && 'has-current',
      isReached(n.id) && 'has-visited', failed.has(n.id) && 'failed', fin.has(n.id) && 'final')} data-id=${n.id}>
      <title>${n.path}${n.description ? '\n' + n.description : ''}${n.loops.map((l) => '\n' + LOOP + l.title).join('')}</title>
      <rect class="gb" x=${n.x} y=${n.y} width=${n.w} height=${n.h} />
      ${txt(n.tx + CW, n.ty, n.title, 'gt gtitle')}
    </g>`);

  const edgeVN = lay.edges.map((e, i) => {
    const pts = e.points, last = pts[pts.length - 1], prev = pts[pts.length - 2];
    const d = pts.map((p, j) => (j ? 'L' : 'M') + p.x + ' ' + p.y).join(' ');
    const lit = e.from !== ANY && e.from !== e.to && isReached(e.from) && isReached(e.to) && live;
    return html`<g key=${'e' + i} class=${cls('ge', 'e-' + e.kind, e.from === ANY && 'any', lit && 'lit')}>
      <title>${e.title}</title>
      <path class="gl" d=${d} />
      <path class="ga" d=${arrow(prev, last)} />
      ${e.label ? html`<rect class="gelb" x=${e.lx - e.lw / 2} y=${e.ly - LBL_H / 2} width=${e.lw} height=${LBL_H} />
        ${txt(e.lx - (e.label.length * CW) / 2, e.ly + 1, e.label, 'gt gel')}` : null}
    </g>`;
  });

  const boxVN = boxes.map((n) => {
    const id = n.id;
    const c = cls('gn', kindClass(n.kind), n.initial && 'initial', n.any && 'any', current.has(id) && 'current', (visited.has(id) || current.has(id)) && 'visited',
      fin.has(id) && 'final', failed.has(id) && 'failed');
    const inner = n.kind === 'final' ? html`<rect class="gb2" x=${n.x + 3} y=${n.y + 3} width=${n.w - 6} height=${n.h - 6} />` : null;
    return html`<g key=${id} class=${c} data-id=${id}>
      <title>${n.path === ANY ? 'any state' : n.path + (n.kind === 'leaf' ? '' : ' · ' + n.kind)}${n.description ? '\n' + n.description : ''}${n.loops.map((l) => '\n' + LOOP + l.title).join('')}</title>
      <rect class="gb" x=${n.x} y=${n.y} width=${n.w} height=${n.h} />
      ${inner}
      ${txt(n.x + (n.w - n.label.length * CW) / 2, n.y + BOX_H / 2 + 1, n.label, 'gt gname')}
      ${n.rows.map((r, i) => txt(n.x + NODE_PAD * CW, n.y + BOX_H + i * ROW_H + ROW_H / 2 + 1, r, 'gt gloop'))}
    </g>`;
  });

  const label = `graph${g.graph ? ' ' + g.graph : ''}: ${boxes.filter((n) => !n.any).length} states`;
  return html`<svg class=${cls('gdraw', live && 'live')} viewBox=${'0 0 ' + lay.width + ' ' + lay.height} width=${lay.width} height=${lay.height}
      role="img" aria-label=${label}>
    ${frameVN}${edgeVN}${boxVN}
  </svg>`;
}

// a triangle at q, pointing along p -> q
function arrow(p, q) {
  if (!p || !q) return '';
  const dx = q.x - p.x, dy = q.y - p.y, len = Math.hypot(dx, dy) || 1;
  const ux = dx / len, uy = dy / len;
  const bx = q.x - ux * ARROW, by = q.y - uy * ARROW, w = ARROW / 2;
  return `M${q.x} ${q.y} L${Math.round(bx - uy * w)} ${Math.round(by + ux * w)} L${Math.round(bx + uy * w)} ${Math.round(by - ux * w)} Z`;
}
