// The chat's turn graph: the graph tab when no run is shown (graph/index.js mounts it with { s, sid }, like every tab). It draws the session it
// is handed and nothing else: the session's rows, and the calls, asks and verdicts in them. It never reads the focused pane, the inspected chat
// or any other global, so a pinned graph pane can host it unchanged, and it reads the same under a door and under the perch (no hub call).
//
// A turn is what the model did between two closing rows: a settle (turn-settled, the transcript's `settle` row) or a stop (the `stopped` row
// a cancelled frame writes, core/state.js). The rows from one closing row up to the next that hold a message, a call, an ask or a verdict make
// the turn. The last turn stays open while the rows end without a closing row. It is running only while the session says `running` is true,
// and a stopped turn is never running: its calls read stopped (core/state.js stopOpen).
//
// The drawing is one chain for draw.js: a turn's calls run down from the turn box, one after another; an ask a call waits on sits in that chain
// just under its call; the asks on no call, then the verdicts on no call, follow the calls; and the next turn hangs off the last box of the
// turn. So a long chat is a tall column and not a fan, and a three-turn chat fits the inspector's width. The newest TURN_CAP turns are drawn,
// the newest CALL_CAP calls of each and the newest CALL_CAP asks and verdicts of each; the rest is one box each.
//
// The verdict rule (upstream gap 14): a policy verdict does not name its call. core/state.js puts it on the call when exactly one open call
// has its tool name and that call holds no verdict yet, at the moment it arrives, since only then is the open set known (a row carries no
// finish order, so it cannot be decided later). A verdict on a call is written on that call's box ("bash · refused"); any other stays a
// `verdict` row and is drawn as a box of its own.
import { html } from '../core/ui.js';
import { drawGraph } from './draw.js';
import { clean } from '../renderers/jev.js';

export const TURN_CAP = 20; // the newest turns drawn; earlier ones are one box
export const CALL_CAP = 10; // the newest calls drawn in a turn, and the newest asks and verdicts of a turn; the rest is one box

const BAD = ['refused', 'declined']; // verdicts that stopped a call: failed, not visited
const CONTENT = new Set(['you', 'tool', 'verdict', 'ask', 'bot']); // the rows that make a turn: a settle or a sys line alone makes none
const CALL_WORD = { run: 'running', ok: 'done', err: 'failed', stop: 'stopped' };
const ASK_WORD = { open: 'open', answered: 'answered', cancelled: 'cancelled', elsewhere: 'answered elsewhere' };

// Every string a model, a tool or a page sent goes through clean (controls, bidi and zero-width marks out) and is drawn as a text node.
const oneLine = (t) => clean(t, 2000);
const cut = (t, n) => (t.length > n ? t.slice(0, n - 1) + '…' : t);
const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

// ---------------------------------------------------------------- reading the session
// The session's rows as plain items, in the order they arrived. Reading a signal here subscribes the render to it, so a new row, a call
// finishing, a verdict or an ask settling redraws the graph; nothing else of the session is read.
export function itemsOf(s) {
  const out = [];
  for (const chunk of s.chunks.value) {
    void chunk.ver.value; // a chunk's rows change in place: its version is read so a flush redraws
    for (const r of chunk.rows) out.push(itemOf(r));
  }
  return out;
}

function itemOf(r) {
  switch (r.kind) {
    case 'you': return { kind: 'you', text: r.text };
    case 'bot': return { kind: 'bot', text: r.text }; // the model's words: they make the turn, and are not drawn
    case 'settle': return { kind: 'settle', text: r.text };
    case 'sys': return /^stopped\b/.test(r.text || '') ? { kind: 'settle', text: r.text } : { kind: 'sys' }; // a stop closes its turn, as a settle does
    case 'tool': return { kind: 'tool', call: callOf(r.call) };
    case 'verdict': return { kind: 'verdict', f: verdictOf(r.f) };
    case 'ask': return { kind: 'ask', ask: askOf(r.ask) };
    default: return { kind: r.kind };
  }
}

// A call's attached verdict and ask. An ask is `ask` while it waits and `settled` once it has been answered, cancelled or taken elsewhere.
function callOf(c) {
  const v = c.verdict.value, a = c.ask.value || c.settled.value;
  return { id: c.id, name: clean(c.name || '?', 60), st: c.st.value, verdict: v ? verdictOf(v) : null, ask: a ? askOf(a) : null };
}

const verdictOf = (f) => ({ tool: clean(f.tool || '?', 60), outcome: clean(f.outcome || '?', 24), reason: oneLine(f.reason) });

function askOf(a) {
  const d = a.done.value;
  return { id: a.id, prompt: oneLine((a.f && (a.f.prompt || a.f.tool)) || 'ask'), how: d ? d.how : 'open' };
}

// ---------------------------------------------------------------- the turns (pure)
// Each turn: { n, label (its first message), calls [call], verdicts [verdict], asks [ask], settle (the words of its settle or its stop, or null), open, current }.
// A call is { id, name, st (run | ok | err | stop), verdict, ask }; a verdict { tool, outcome, reason }; an ask { id, prompt, how }.
export function turnsOf(items, running = null) {
  const turns = [];
  let cur = null;
  for (const it of items || []) {
    if (!it) continue;
    if (it.kind === 'settle') {
      if (cur) { cur.settle = oneLine(it.text) || 'settled'; turns.push(cur); cur = null; }
      continue;
    }
    if (it.kind === 'bot' && !/\S/.test(String(it.text || ''))) continue; // a message with no words is no message
    if (!CONTENT.has(it.kind)) continue;
    cur = cur || { label: '', settle: null, calls: [], verdicts: [], asks: [] };
    if (it.kind === 'you') { if (!cur.label) cur.label = oneLine(it.text); }
    else if (it.kind === 'tool') cur.calls.push(it.call);
    else if (it.kind === 'verdict') cur.verdicts.push(it.f);
    else if (it.kind === 'ask') cur.asks.push(it.ask);
  }
  if (cur) turns.push(cur);
  turns.forEach((t, i) => {
    t.n = i + 1;
    t.open = t.settle === null;
    t.current = t.open && i === turns.length - 1 && running === true;
  });
  return turns;
}

// The words above the drawing: what there is, and whether the newest turn is still running.
export function captionOf(turns) {
  const calls = turns.reduce((n, t) => n + t.calls.length, 0);
  const asks = turns.reduce((n, t) => n + t.asks.length + t.calls.filter((c) => c.ask).length, 0);
  return [plural(turns.length, 'turn'), plural(calls, 'call'), asks ? plural(asks, 'ask') : '', turns.some((t) => t.current) ? 'running' : '',
    turns.length > TURN_CAP ? `the newest ${TURN_CAP} are drawn` : ''].filter(Boolean).join(' · ');
}

// ---------------------------------------------------------------- the drawing (pure)
// The states and edges draw.js lays out, and the marks: current (running), visited (done) and failed. The ids are
//   earlier turns · turn N · turn N call K · turn N call K ask · turn N ask K · turn N verdict K
//   · turn N earlier calls · turn N earlier asks · turn N earlier verdicts
// and never hold a dot (draw.js reads a dot as nesting). Every edge is unlabelled: the box says what the line is.
export function graphOf(turns, opts = {}) {
  const tcap = opts.turns || TURN_CAP, ccap = opts.calls || CALL_CAP;
  const states = [], edges = [], current = [], visited = [], failed = [];
  const put = (path, label, kind, description, mark = 'visited') => {
    states.push({ path, label, kind, description });
    (mark === 'current' ? current : mark === 'failed' ? failed : visited).push(path);
    return path;
  };
  const link = (from, to, kind) => edges.push({ from, to, kind, label: '' });
  // A list of a turn's loose asks or verdicts as one chain from `from`: the newest ccap boxes, and one box for the rest. It returns the chain's
  // last box, which the next list, or the next turn, hangs off (`from` itself when the list is empty).
  const chain = (from, tp, list, word, kind, box) => {
    const hidden = Math.max(0, list.length - ccap);
    let at = from;
    if (hidden) {
      const more = put(`${tp} earlier ${word}`, `+${hidden} earlier ${word}`, 'more', `Not drawn: the newest ${ccap} of ${list.length} ${word} are drawn.`);
      link(at, more, kind);
      at = more;
    }
    list.forEach((x, j) => {
      if (j < hidden) return;
      const p = box(x, j);
      link(at, p, kind);
      at = p;
    });
    return at;
  };
  const shown = turns.slice(-tcap);
  const earlier = turns.length - shown.length;
  let tail = null; // the box the next turn hangs off: the last box of the turn before (its turn box when it drew no call)
  if (earlier > 0) tail = put('earlier turns', `${earlier} earlier turns`, 'earlier', `Not drawn: the newest ${tcap} of ${turns.length} turns are drawn.`);

  for (const t of shown) {
    const tp = `turn ${t.n}`;
    const asks = t.asks.length + t.calls.filter((c) => c.ask).length;
    const counts = [plural(t.calls.length, 'call'), asks ? plural(asks, 'ask') : '', t.settle || (t.current ? 'running' : 'not settled')];
    const turn = put(tp, `#${t.n} ${t.label || 'turn'}`, 'turn', `${cut(t.label || 'turn', 160)}\n${counts.filter(Boolean).join(' · ')}`, t.current ? 'current' : 'visited');
    if (tail) link(tail, turn, 'then');

    // The calls run down as one chain from the turn box; the next turn hangs off the last box of the chain.
    let head = turn;
    const hidden = Math.max(0, t.calls.length - ccap);
    if (hidden) {
      const more = put(`${tp} earlier calls`, `+${hidden} earlier calls`, 'more', `Not drawn: the newest ${ccap} of ${t.calls.length} calls are drawn.`);
      link(head, more, 'call');
      head = more;
    }
    t.calls.forEach((c, i) => {
      if (i < hidden) return;
      const cp = `${tp} call ${i + 1}`;
      const refused = !!c.verdict && BAD.includes(c.verdict.outcome); // a policy that stopped the call: failed whatever the call's own end
      const why = refused ? c.verdict.outcome : c.st === 'run' && !t.current ? 'not finished' : CALL_WORD[c.st] || 'not finished';
      const mark = c.st === 'err' || refused ? 'failed' : c.st === 'run' && t.current ? 'current' : 'visited';
      const desc = c.verdict ? `${c.name} · ${why}\npolicy ${c.verdict.outcome}: ${c.verdict.reason}` : `${c.name} · ${why}`;
      link(head, put(cp, c.verdict ? `${c.name} · ${c.verdict.outcome}` : c.name, 'call', desc, mark), 'call');
      head = cp;
      if (c.ask) {
        head = put(`${cp} ask`, `ask: ${c.ask.prompt}`, 'ask', `${c.ask.prompt}\n${ASK_WORD[c.ask.how] || c.ask.how}`, c.ask.how === 'open' && t.current ? 'current' : 'visited');
        link(cp, head, 'ask');
      }
    });

    // The asks on no call run on from the turn's last box as one chain, and the verdicts on no call from the asks' end: the newest CALL_CAP of
    // each, and one box for the rest. A chain keeps the order its rows came in. The rows carry no order between the calls and the loose asks and
    // verdicts, so the order here (calls, then asks, then verdicts) is layout, not a claim about when each came.
    head = chain(head, tp, t.asks, 'asks', 'ask', (a, j) => put(`${tp} ask ${j + 1}`, `ask: ${a.prompt}`, 'ask', `${a.prompt}\n${ASK_WORD[a.how] || a.how}`, a.how === 'open' && t.current ? 'current' : 'visited'));
    tail = chain(head, tp, t.verdicts, 'verdicts', 'verdict', (v, j) => put(`${tp} verdict ${j + 1}`, `${v.outcome}: ${v.tool}`, 'verdict', `${v.tool} · ${v.outcome}\n${v.reason}`, BAD.includes(v.outcome) ? 'failed' : 'visited'));
  }
  return { graph: { graph: 'turns', states, edges }, marks: { current, visited, failed } };
}

// ---------------------------------------------------------------- the tab
// The chat's turns, drawn from the session it is handed. `sid` is passed like every tab's and is not read: the session is the chat.
export function TurnGraph({ s, sid }) {
  if (!s) return html`<div class="grun"><div class="faint">no chat to draw</div></div>`;
  const turns = turnsOf(itemsOf(s), s.running.value);
  if (!turns.length) return html`<div class="grun"><div class="faint">no turns yet: each turn of this chat is drawn here as it runs</div></div>`;
  const g = graphOf(turns);
  return html`<div class="grun">
    <div class="gline faint">${captionOf(turns)}</div>
    ${drawGraph(g.graph, g.marks)}
  </div>`;
}
