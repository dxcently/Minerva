// The mesh tab of the left panel: what Aoide knows on this machine, read through the perch's /hub/mesh and drawn as text. app.js draws
// `MeshTab` when the tab is chosen; it takes no props.
//
//   header      aoide 0.0.26 · ~/.local/bin/aoide · aoided ● 3 ms   (or the sentence for why Aoide is not available)
//   nodes       node · verified · grants · meshes · added · roster cached Xm ago, and the line saying presence is not probed
//   drift       aoide mesh --json: the nodes that differ from their declaration, and the undeclared ones
//   stage graph counts and a list
//   outbox      what waits to be sent, and why
//   events      the audit and gate tail, polled every 5 s through the lane while the tab shows
//
// Network-free: nothing here asks a node anything. The only times are when a roster was last cached, so a node is never drawn as up or
// down. Everything the perch sends is Aoide's or another node's text, drawn as a text node (mesh/view.js makes the strings; no markup,
// no innerHTML). Mail lives in the notes tab, not here.
//
// The snapshot is asked at open and by `[ refresh ]` (a person's action: a plain hubGet), and every minute while the tab shows, in the
// lane (the perch caches mesh --json and the outbox for a minute, so asking faster costs nothing and learns nothing). The events are
// polled in the lane every 5 s, only while the page is visible; the answers are kept at module level, so a reopened tab draws at once.
import { html, K, signal, useEffect } from '../core/ui.js';
import { Needs, Kv } from '../core/bits.js';
import { mode, hubGet } from '../core/api.js';
import { hubPoll } from '../core/lane.js';
import * as V from './view.js';

export const EVENTS_MS = 5000;
export const SNAP_MS = 60000;
const OPEN_TIMEOUT = 15000; // the perch gives its three aoide runs 10 s between them; a snapshot at open waits for that
const LANE_TIMEOUT = 12000;
const EVENTS_TIMEOUT = 6000;

export const snap = signal(null);        // the last answer of GET /hub/mesh, good or not: { status, data, at }
export const good = signal(null);        // the last good one, kept while a refresh fails
export const busy = signal(false);       // a snapshot is on its way
export const feed = signal(V.emptyFeed()); // the events held (mesh/view.js foldFeed)
let snapBusy = false, feedBusy = false;

// Ask for the snapshot. `lane` is for the periodic read; the one at open and `[ refresh ]` are a person's and go straight out.
export async function loadSnap(lane) {
  if (snapBusy) return;
  snapBusy = true;
  busy.value = true;
  try {
    const r = lane ? await hubPoll('mesh', LANE_TIMEOUT) : await hubGet('mesh', OPEN_TIMEOUT);
    const got = { status: r.status, data: r.data, at: Date.now() };
    snap.value = got;
    if (V.stateOf(got).state !== 'failed') good.value = got;
  } finally { snapBusy = false; busy.value = false; }
}

// One poll of the events tail. Never two at once: a path waiting in the lane is shared by its callers, and each would fold the same answer.
export async function pollFeed() {
  if (feedBusy) return;
  feedBusy = true;
  try {
    const r = await hubPoll(V.feedPath(feed.peek()), EVENTS_TIMEOUT);
    feed.value = V.foldFeed(feed.peek(), r);
  } finally { feedBusy = false; }
}

const visible = () => document.visibilityState === 'visible';

// ---------------------------------------------------------------- the parts
const Line = ({ cls = '', children }) => html`<div class=${cls}>${children}</div>`;
const Hdr = ({ children }) => html`<div class="hdr">${children}</div>`;

function Header({ data, at }) {
  const parts = V.headerParts(data);
  return html`<div class="mhead">${parts.map((p, i) => html`${i ? html`<span class="faint"> · </span>` : null}<span class=${p.cls} title=${p.title || null}>${p.text}</span>`)}</div>
    <div class="faint">as of ${V.clock(at)}</div>`;
}

function Nodes({ rows }) {
  if (!rows.length) return html`<div class="faint">no nodes registered</div>`;
  return rows.map((n) => html`<div class="mnode" key=${n.name}>
    <div class="mname"><span class="hi b grow">${n.name}</span><span class=${n.listed ? (n.verified ? 'up' : 'o') : 'faint'}>${n.listed ? (n.verified ? '✓ verified' : 'not verified') : 'not in the registry'}</span></div>
    <${Kv} k="grants " v=${n.grants} />
    <${Kv} k="meshes " v=${n.meshes} />
    <${Kv} k="added  " v=${n.added} />
    <${Kv} k="roster " v=${n.cached} cls=${n.old ? 'o' : ''} />
  </div>`);
}

function Drift({ view }) {
  return html`${view.sentence && html`<div class="faint">${view.sentence}</div>`}
    ${view.sections.map((s, i) => html`<div class="msec" key=${i}>
      <div class="mname"><span class="hi grow">${s.name}</span></div>
      <div class="faint">${s.head}</div>
      ${s.rows.map((r, j) => html`<div class="mrow" key=${j}><span class="grow">${r.node}</span><span class=${r.colour}>${r.what}</span></div>
        ${r.detail && html`<div class="faint mdet">${r.detail}</div>`}`)}
    </div>`)}
    ${view.undeclared.length ? html`<div class="msec"><span class="faint">undeclared  </span>${view.undeclared.join(', ')}</div>` : null}`;
}

function Graph({ view }) {
  if (view.sentence) return html`<div class="faint">${view.sentence}</div>`;
  return html`<div>${view.head}</div>
    ${view.kinds && html`<div class="faint">${view.kinds}</div>`}
    ${view.rows.map((r, i) => html`<div class="mrow" key=${i}><span class="faint">${r.kind}</span><span class="grow">${r.label}${r.kids ? ` (+${r.kids})` : ''}</span><span class="faint">${[r.agent, r.state].filter(Boolean).join(' ')}</span></div>`)}
    ${view.more ? html`<div class="faint">… ${view.more} more</div>` : null}`;
}

function Outbox({ view }) {
  if (view.sentence) return html`<div class="faint">${view.sentence}</div>`;
  return html`<div>${view.head}</div>
    ${view.rows.map((r, i) => html`<div class="mrow" key=${i}><span class="grow">${r.to}</span><span class=${r.refused ? 'r' : 'o'}>${r.status}</span><span class="faint">×${r.tries}${r.id ? ' ' + r.id : ''}</span></div>
      ${r.detail && html`<div class="faint mdet">${r.detail}</div>`}`)}`;
}

function Events({ feedState }) {
  const v = V.feedView(feedState);
  return html`${v.sentence && html`<${Line} cls=${v.cls}>${v.sentence}</${Line}>`}
    ${v.lines.map((e, i) => html`<div class=${'mev ' + e.cls} key=${i}><span class="faint">${e.at}</span><span class="mcl">${e.cls}</span><span class="grow">${e.text}</span></div>`)}
    ${v.older ? html`<div class="faint">… ${v.older} older held</div>` : null}`;
}

// ---------------------------------------------------------------- the tab
export function MeshTab() {
  const perch = mode() === 'perch';
  useEffect(() => {
    if (!perch) return undefined;
    feed.value = V.resumeFeed(feed.peek());
    if (visible()) { loadSnap(false); pollFeed(); }
    const events = setInterval(() => { if (visible()) pollFeed(); }, EVENTS_MS);
    const whole = setInterval(() => { if (visible()) loadSnap(true); }, SNAP_MS);
    const back = () => {
      if (!visible()) return;
      pollFeed();
      if (!snap.peek() || Date.now() - snap.peek().at >= SNAP_MS) loadSnap(true);
    };
    document.addEventListener('visibilitychange', back);
    return () => { clearInterval(events); clearInterval(whole); document.removeEventListener('visibilitychange', back); };
  }, [perch]);

  if (mode() === 'door') return html`<${Needs}>the mesh needs the hub: it reads Aoide through the perch</${Needs}>`;
  if (!perch) return html`<div class="faint">…</div>`;

  const last = snap.value, kept = good.value, f = feed.value;
  const st = V.stateOf(kept || last);
  const now = Date.now();
  const failed = last && kept && last !== kept ? V.stateOf(last) : null; // a refresh that failed: the good answer stays, with the reason
  const data = kept && kept.data;
  return html`<div class="mesh">
    ${st.state === 'loading' ? html`<div class="faint">asking the perch…</div>` : null}
    ${st.state === 'failed' ? html`<${Line} cls="o">${st.sentence}</${Line}>` : null}
    ${st.state === 'unavailable' ? html`<${Line} cls="o">${st.sentence}</${Line}>` : null}
    ${st.state === 'ok' ? html`<${Header} data=${data} at=${kept.at} />` : null}
    ${failed && failed.state === 'failed' ? html`<${Line} cls="o">${failed.sentence}; showing the answer from ${V.clock(kept.at)}</${Line}>` : null}
    <div class="mact"><${K} on=${() => loadSnap(false)} off=${busy.value}>${busy.value ? '[ refreshing… ]' : '[ refresh ]'}</${K}></div>
    ${st.state === 'ok' ? html`
      <${Hdr}>nodes</${Hdr}>
      <${Nodes} rows=${V.nodeRows(data, (now - kept.at) / 1000)} />
      <${Needs}>${V.NOT_PROBED}</${Needs}>
      <${Needs}>${V.MESHES_NOTE}</${Needs}>
      <${Hdr}>mesh drift</${Hdr}>
      <${Drift} view=${V.driftView(data.mesh)} />
      <${Hdr}>stage graph</${Hdr}>
      <${Graph} view=${V.graphView(data.graph)} />
      <${Hdr}>outbox</${Hdr}>
      <${Outbox} view=${V.outboxView(data.outbox, now)} />
      ${V.noteLines(data).length ? html`<${Hdr}>notes</${Hdr}>${V.noteLines(data).map((s, i) => html`<${Needs} key=${i}>${s}</${Needs}>`)}` : null}` : null}
    <${Hdr}>audit and gate events</${Hdr}>
    <${Events} feedState=${f} />
  </div>`;
}
