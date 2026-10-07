// The mesh tab's words, pure: what /hub/mesh and /hub/mesh/events said, turned into rows and sentences. No DOM, no state, no fetch; the
// component (mesh/index.js) only lays these out. Every value returned is a string, a number, a boolean or a list or plain object of those:
// never markup, never a vnode. All of it is Aoide's, or another node's, so each string is run through `clean` (control characters, bidi
// overrides and zero-width marks out, cut short) and is drawn as a text node and nothing else.
//
// What this tab never says: that a node is up, down, online or present. The perch does not probe a node (it is network-free); the only
// time it has is when a roster was last cached, so that is the only thing shown, as an age.
//
//   stateOf(r)              'loading' | 'failed' | 'unavailable' | 'ok', with the sentence for the first three
//   headerParts(data)       [{ text, cls, title? }]: aoide 0.0.26 · ~/.local/bin/aoide · aoided ● 3 ms
//   nodeRows(data, since)   one row per registered node (and per roster cached for a node the registry lacks), ages moved on by `since` s
//   driftView(mesh)         `data` of `aoide mesh --json`: sections of rows that differ from their declaration, and the undeclared nodes
//   graphView(graph)        the stage graph: counts and a list
//   outboxView(outbox, now) `data` of `aoide mail outbox --json`: what waits, and why
//   noteLines(data)         the perch's own sentences about what it could not read
//   eventLine(e)            one audit or gate event as a line
//   foldFeed / resumeFeed / feedPath / feedView   the events tail held by the page: cursor, reset, a cap, and its honest sentences
import { clean } from '../renderers/jev.js';

export const NOT_PROBED = 'presence is not probed: ages are when each roster was last cached';
export const MESHES_NOTE = 'meshes: where the node has a grant, or a row in mesh --json, which lists only what differs from the declaration';
export const NO_AOIDE = 'Aoide is not available here';
export const OLD_SECS = 86400; // a roster cached more than a day ago is drawn as old (never as absent: nothing here knows who is there)
export const NODE_NAME = 64;

const isObj = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);
const list = (v) => (Array.isArray(v) ? v : []);
const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : null);
const str = (v, n) => (typeof v === 'string' ? clean(v, n) : '');
const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

// 45s, 3m, 5h, 2d: how long ago, from seconds.
export function ageText(secs) {
  const s = num(secs);
  if (s == null || s < 0) return '?';
  if (s < 60) return Math.floor(s) + 's';
  if (s < 3600) return Math.floor(s / 60) + 'm';
  if (s < 86400) return Math.floor(s / 3600) + 'h';
  return Math.floor(s / 86400) + 'd';
}

// The sentence for an answer that was not a 200: the perch's own (`{ error }`), else what happened. Status 0: nothing came back.
export function why(r) {
  if (!r || r.status === 0) return 'the perch did not answer';
  if (r.data && typeof r.data.error === 'string' && r.data.error) return clean(r.data.error, 200);
  if (r.status === 401) return "the perch refused this page's login (it restarted?)";
  if (r.status === 404) return 'this perch has no such route (an older perch?)';
  return `the perch answered ${r.status}`;
}

// ---------------------------------------------------------------- the snapshot
// What an answer of GET /hub/mesh is, before any part of it is read. `unavailable` is the perch saying it has no Aoide at all (no program,
// no state folder): its own sentence is the one shown.
export function stateOf(r) {
  if (!r) return { state: 'loading', sentence: '' };
  if (r.status !== 200 || !isObj(r.data)) return { state: 'failed', sentence: `the mesh view could not be read: ${why(r)}` };
  if (r.data.available === false) return { state: 'unavailable', sentence: str(list(r.data.errors)[0], 240) || NO_AOIDE };
  return { state: 'ok', sentence: '' };
}

// `aoide 0.0.26 · ~/.local/bin/aoide · aoided ● 3 ms`. The daemon is the one thing the perch does ping (its own, on a local socket), so
// `●` is an answer, `○` is none, and with no socket known it says that. A node's presence is never part of it.
export function headerParts(data) {
  const parts = [];
  const a = isObj(data && data.aoide) ? data.aoide : null;
  if (a) {
    const v = str(a.version, 32);
    parts.push({ text: v ? `aoide ${v}` : 'aoide (version not reported)', cls: '' });
    parts.push({ text: str(a.path, 120), cls: 'faint' });
  } else {
    parts.push({ text: 'no aoide program is configured', cls: 'o' });
  }
  const d = isObj(data && data.daemon) ? data.daemon : {};
  const ms = num(d.ping_ms), sock = str(d.socket, 160);
  if (ms != null) parts.push({ text: `aoided ● ${ms} ms`, cls: 'up', title: sock });
  else if (sock) parts.push({ text: 'aoided ○ no answer', cls: 'o', title: sock });
  else parts.push({ text: 'aoided: no socket known', cls: 'faint' });
  return parts.filter((p) => p.text);
}
export const headerText = (parts) => parts.map((p) => p.text).join(' · ');

// ---------------------------------------------------------------- the nodes
// mesh name -> the nodes `mesh --json` has a row for in it.
function driftMeshes(mesh) {
  const by = new Map();
  for (const s of list(isObj(mesh) && isObj(mesh.report) ? mesh.report.sections : null).filter(isObj)) {
    const name = str(s.name, NODE_NAME);
    if (!name) continue;
    for (const r of list(s.rows).filter(isObj)) {
      const node = str(r.node, NODE_NAME);
      if (!node) continue;
      if (!by.has(node)) by.set(node, new Set());
      by.get(node).add(name);
    }
  }
  return by;
}

// `message, read` for the flat words (aoide before P-CHARTER); `alpha: message · beta: message, spawn` for the words per mesh (after it).
function grantsText(n) {
  const flat = list(n.allows).map((w) => str(w, 32)).filter(Boolean);
  const per = isObj(n.grants) ? Object.entries(n.grants).map(([m, ws]) => [str(m, NODE_NAME), list(ws).map((w) => str(w, 32)).filter(Boolean)]) : [];
  const out = [];
  if (flat.length) out.push(flat.join(', '));
  for (const [m, ws] of per) if (m) out.push(`${m}: ${ws.join(', ') || 'none'}`);
  return out.join(' · ') || 'none listed';
}

// The day an aoide timestamp names (2026-09-01T00:00:00Z gives 2026-09-01); anything else as the short text it is; nothing as a dash.
const addedText = (v) => { const s = str(v, 40); return /^\d{4}-\d\d-\d\d/.test(s) ? s.slice(0, 10) : s || '—'; };

// One row per node the registry lists, in the file's order, then one per cached roster the registry has no node for (a roster outlives the
// node's registration; it is said so). `since` is how many seconds have passed since the answer was got: a roster's age moves on with it.
export function nodeRows(data, since = 0) {
  const moved = num(since) != null && since > 0 ? since : 0;
  const drift = driftMeshes(data && data.mesh);
  const rosters = new Map();
  for (const c of list(data && data.cache).filter(isObj)) if (typeof c.name === 'string') rosters.set(c.name, c);
  const rowOf = (name, n, c) => {
    const secs = c && num(c.age_secs) != null ? c.age_secs + moved : null;
    const meshes = [...new Set([...(n && isObj(n.grants) ? Object.keys(n.grants).map((m) => str(m, NODE_NAME)) : []), ...(drift.get(name) || [])])].filter(Boolean).sort();
    return {
      name, listed: !!n,
      verified: n ? n.verified === true : false,
      grants: n ? grantsText(n) : '—',
      meshes: meshes.join(' ') || '—',
      added: n ? addedText(n.addedAt) : '—',
      cached: c ? `cached ${ageText(secs)} ago${secs != null && secs >= OLD_SECS ? ' · old' : ''}${c.cut === true ? ' · cut at 64 KiB' : ''}` : 'no roster cached',
      cachedSecs: secs,
      old: secs != null && secs >= OLD_SECS,
    };
  };
  const rows = [];
  const named = new Set();
  for (const n of list(data && data.registry).filter(isObj)) {
    const name = str(n.name, NODE_NAME);
    if (!name || named.has(name)) continue;
    named.add(name);
    rows.push(rowOf(name, n, rosters.get(n.name)));
  }
  for (const [raw, c] of rosters) {
    const name = str(raw, NODE_NAME);
    if (name && !named.has(name)) { named.add(name); rows.push(rowOf(name, null, c)); }
  }
  return rows;
}

// ---------------------------------------------------------------- mesh --json
// The class of a row is aoide's word (`unverified`, `via-mismatch`); the colour comes from a table here, never from the word.
const DRIFT_COLOUR = new Map([['unverified', 'o']]);
const detailOf = (r) => ['declared', 'recorded'].filter((k) => k in r).map((k) => `${k} ${r[k] == null ? 'none' : str(String(r[k]), 80) || '?'}`).join(' · ');

export function driftView(mesh) {
  if (!isObj(mesh)) return { sentence: 'no mesh report: aoide mesh --json gave none (the notes below say why)', sections: [], undeclared: [] };
  const report = isObj(mesh.report) ? mesh.report : null;
  if (!report || !Array.isArray(report.sections)) {
    return { sentence: 'aoide mesh --json answered in a shape this page does not read', sections: [], undeclared: [] };
  }
  const sections = report.sections.filter(isObj).map((s) => {
    const rows = list(s.rows).filter(isObj).map((r) => ({ node: str(r.node, NODE_NAME) || '?', what: str(r.class, 32) || '?', colour: DRIFT_COLOUR.get(r.class) || '', detail: detailOf(r) }));
    const grant = list(s.grant).map((w) => str(w, 32)).filter(Boolean).join(', ');
    const declared = num(s.declared);
    const head = [`grant ${grant || 'none listed'}`, declared != null ? `${declared} declared` : '', rows.length ? `${rows.length} ${rows.length === 1 ? 'differs' : 'differ'}` : 'none differ'];
    return { name: str(s.name, NODE_NAME) || '?', head: head.filter(Boolean).join(' · '), rows };
  });
  const undeclared = list(report.undeclared).map((n) => str(n, NODE_NAME)).filter(Boolean);
  return { sentence: sections.length || undeclared.length ? '' : 'mesh --json lists no mesh', sections, undeclared };
}

// ---------------------------------------------------------------- the stage graph
export const GRAPH_SHOWN = 30;

export function graphView(graph) {
  if (!isObj(graph) || !Array.isArray(graph.nodes)) return { sentence: 'no stage graph here', head: '', kinds: '', rows: [], more: 0 };
  const total = num(graph.total) != null ? graph.total : graph.nodes.length, edges = num(graph.edges) ?? 0;
  const kinds = isObj(graph.kinds) ? Object.entries(graph.kinds).filter(([, n]) => num(n) != null).sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)) : [];
  const rows = graph.nodes.filter(isObj).slice(0, GRAPH_SHOWN).map((n) => ({
    kind: str(n.kind, 24) || '?',
    label: str(n.petname, 48) || str(n.title, 48) || str(n.name, 48) || str(n.id, 48) || '?',
    state: str(n.state, 24),
    agent: str(n.agent, 24),
    kids: num(n.children) ?? 0,
  }));
  return {
    sentence: '',
    head: `${plural(total, 'node', 'nodes')} · ${plural(edges, 'edge', 'edges')}`,
    kinds: kinds.map(([k, n]) => `${str(k, 24) || '?'} ${n}`).join(' · '),
    rows,
    more: Math.max(0, total - rows.length),
  };
}

// ---------------------------------------------------------------- the outbox
// An ISO time relative to `now` (ms): `in 27m`, or `was due 3d ago`. A time that is not one is passed on as the short text it is.
function whenText(iso, now, past, future) {
  if (typeof iso !== 'string' || !iso) return '';
  const at = Date.parse(iso);
  if (!Number.isFinite(at)) return `${future} ${str(iso, 32)}`;
  return at >= now ? `${future} in ${ageText((at - now) / 1000)}` : `${past} ${ageText((now - at) / 1000)} ago`;
}

export function outboxView(outbox, now = Date.now()) {
  if (!isObj(outbox)) return { sentence: 'no outbox report: aoide mail outbox --json gave none (the notes below say why)', head: '', rows: [] };
  if (!Array.isArray(outbox.entries)) return { sentence: 'aoide mail outbox --json answered in a shape this page does not read', head: '', rows: [] };
  const entries = outbox.entries.filter(isObj);
  const rows = entries.map((e) => {
    const d = isObj(e.delivery) ? e.delivery : {};
    const reason = str(d.reason, 100), scope = str(d.reasonScope, 16);
    const tries = num(e.tries);
    return {
      to: str(e.to, 80) || str(e.node, NODE_NAME) || '?',
      id: str(e.msgid, 64).slice(0, 8),
      status: str(d.status, 24) || str(e.lastOutcome, 24) || '?',
      refused: e.refused === true,
      tries: tries != null ? tries : 0,
      detail: [reason ? `${reason}${scope ? ` (${scope})` : ''}` : '', whenText(e.lastTryAt, now, 'last try', 'last try'),
        d.nextAttemptAt ? whenText(d.nextAttemptAt, now, 'next try was due', 'next try') : ''].filter(Boolean).join(' · '),
    };
  });
  const refused = rows.filter((r) => r.refused).length;
  return {
    sentence: rows.length ? '' : 'nothing is waiting in the outbox',
    head: rows.length ? [`${rows.length} waiting`, refused ? `${refused} refused` : ''].filter(Boolean).join(' · ') : '',
    rows,
  };
}

// The perch's sentences about what it could not read or left out (an unread registry shape, a failed run): at most 8, each cut short.
export const noteLines = (data) => list(data && data.errors).map((s) => str(s, 240)).filter(Boolean).slice(0, 8);

// ---------------------------------------------------------------- the events tail
export const HELD = 200;          // events the page keeps
export const SHOWN = 40;          // events drawn (newest first); the rest are counted
export const FEED_STALE_MS = 60000; // a tab reopened after this long starts the tail again instead of catching up through a backlog

export const EVENTS_EMPTY = 'no events to show: the perch tails only the file named by its --aoide-events flag, and that file is empty, missing or not set. '
  + 'Nothing shown is not the same as nothing happening.';

// `HH:MM:SS` local, from unix seconds (milliseconds when the number is that big); dashes for anything else.
export function clock(ts) {
  const t = num(ts);
  if (t == null || t < 0) return '--:--:--';
  const d = new Date(t > 1e11 ? t : t * 1000);
  if (Number.isNaN(d.getTime())) return '--:--:--';
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

// A payload as `key=value` words: the first four keys whose value is text, a number or a boolean, each cut short; nested values are left out.
function payloadText(p) {
  if (typeof p === 'string') return clean(p, 100);
  if (!isObj(p)) return '';
  const words = [];
  for (const [k, v] of Object.entries(p)) {
    if (typeof v !== 'string' && num(v) == null && typeof v !== 'boolean') continue;
    words.push(`${clean(k, 16)}=${clean(String(v), 40)}`);
    if (words.length === 4) break;
  }
  return words.join(' ');
}

// One event. Two shapes are in the wild, and both are read: the feed aoided keeps (`kind`, `source`, `payload`) and the audit log
// (`command`, `door`, `status`, `message`). The class is one of two words the perch let through; anything else draws as audit.
export function eventLine(e) {
  const what = str(e.kind, 40) || str(e.command, 40);
  const who = str(e.source, 24) || str(e.door, 24);
  const msg = str(e.message, 100) || payloadText(e.payload);
  return {
    at: clock(e.ts),
    cls: e.class === 'gate' ? 'gate' : 'audit',
    text: [what || '?', who ? `(${who})` : '', str(e.status, 24), msg].filter(Boolean).join(' '),
  };
}

export const emptyFeed = () => ({ lines: [], next: null, why: '', at: 0 });

// Take one answer of /hub/mesh/events into what is held. `reset` (the file shrank or went) clears what is held first; the cursor is the
// answer's `next`. A failed or unreadable answer changes nothing but the sentence for it: a hiccup keeps the lines and the cursor.
export function foldFeed(feed, r, now = Date.now()) {
  if (!r || r.status !== 200 || !isObj(r.data) || !Array.isArray(r.data.events)) {
    return { ...feed, why: r && r.status === 200 ? 'the events answer was not in a shape this page reads' : why(r), at: now };
  }
  const base = r.data.reset === true ? [] : feed.lines;
  const next = num(r.data.next) != null && r.data.next >= 0 ? r.data.next : feed.next;
  return { lines: [...base, ...r.data.events.filter(isObj).map(eventLine)].slice(-HELD), next, why: '', at: now };
}

// The feed a reopened tab continues: the one it held, or a fresh one when that is older than FEED_STALE_MS.
export const resumeFeed = (feed, now = Date.now()) => (feed.at && now - feed.at <= FEED_STALE_MS ? feed : emptyFeed());
// Where the next poll goes: the first poll has no cursor and gets the newest events; the rest pass the last answer's `next`.
export const feedPath = (feed) => (feed.next == null ? 'mesh/events' : `mesh/events?after=${feed.next}`);

// What the tail draws: the newest events first, how many older ones are held, and the sentence for when there is nothing (or a failure).
export function feedView(feed) {
  const lines = feed.lines.slice(-SHOWN).reverse();
  const older = Math.max(0, feed.lines.length - lines.length);
  if (feed.why) return { lines, older, sentence: `the events feed could not be read: ${feed.why}`, cls: 'o' };
  if (!feed.at) return { lines, older, sentence: 'reading the events feed…', cls: 'faint' };
  if (!lines.length) {
    return { lines, older, sentence: feed.next > 0 ? `the feed has ${feed.next} bytes and none of it is an audit or gate event` : EVENTS_EMPTY, cls: 'faint' };
  }
  return { lines, older, sentence: '', cls: '' };
}
