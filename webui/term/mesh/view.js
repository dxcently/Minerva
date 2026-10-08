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
//   eventLine(entry)        one event of an answer, {stream, event}, as a line labelled by its stream
//   foldFeed / resumeFeed / feedPath / feedView / emptySentence   the two streams held by the page: cursors, resets, a cap, and the sentences
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
// Two streams, held together: the audit log (`audit`) and aoided's feed (`daemon`). Each has its own cursor (`next`), its own file as the
// page is told it (`files`) and its own reset. Each held line keeps the label of the stream it came from (`cls`).
export const HELD = 200;          // events the page keeps, both streams together
export const SHOWN = 40;          // events drawn (newest first); the rest are counted
export const FEED_STALE_MS = 60000; // a tab reopened after this long starts the tail again instead of catching up through a backlog
export const STREAMS = ['audit', 'daemon'];
const LABEL = { audit: 'audit log', daemon: 'daemon feed' };
export const NOT_SAME = 'Nothing shown is not the same as nothing happening.';

// `HH:MM:SS` local, from unix seconds (milliseconds when the number is that big); dashes for anything else.
export function clock(ts) {
  const t = num(ts);
  if (t == null || t < 0) return '--:--:--';
  const d = new Date(t > 1e11 ? t : t * 1000);
  if (Number.isNaN(d.getTime())) return '--:--:--';
  const p = (n) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

// The payload fields the page draws, by class: the ones aoide's docs name for each (the perch forwards only these; the page draws only these
// too, so a field a later aoided adds is never drawn). A gate's fields are CONTRACTS.md's "Pairing events feed (P-P5)"; an audit's, the
// hand-edit watcher's `file` (docs/architecture/AOIDED.md).
const PAYLOAD_KEYS = { gate: ['id', 'name', 'originAddr', 'url', 'direction'], audit: ['file'] };

// A mail command's `message` is a letter's text, so it is never drawn (the perch does not send it either). A mail command is one whose first
// word, before a `.`, `/` or `-`, is `mail` in any letter case: the same rule the perch applies.
const isMail = (command) => typeof command === 'string' && /^mail([./-]|$)/i.test(command);

// A payload as `key=value` words: the first four of the class's fields whose value is text, a number or a boolean, each cut short.
function payloadText(cls, p) {
  if (!isObj(p)) return '';
  const words = [];
  for (const k of PAYLOAD_KEYS[cls] || []) {
    if (!Object.prototype.hasOwnProperty.call(p, k)) continue;
    const v = p[k];
    if (typeof v !== 'string' && num(v) == null && typeof v !== 'boolean') continue;
    words.push(`${clean(k, 16)}=${clean(String(v), 40)}`);
    if (words.length === 4) break;
  }
  return words.join(' ');
}

// One event of an answer: `{ stream, event }`, where `event` is the record as its stream keeps it. The line's label is the stream (`audit`
// or `daemon`), and its text starts with the record's own class (`audit` or `gate`), then what it is, who sent it, its status and its
// message or payload. Every string is cleaned and cut, as all of this page's strings are.
export function eventLine(entry) {
  const e = entry.event;
  const what = str(e.kind, 40) || str(e.command, 40);
  const who = str(e.source, 24) || str(e.door, 24);
  const cls = e.class === 'gate' ? 'gate' : 'audit';
  const msg = isMail(e.command) ? '' : str(e.message, 100) || payloadText(cls, e.payload);
  return {
    at: clock(e.ts),
    cls: entry.stream === 'daemon' ? 'daemon' : 'audit',
    text: [cls, what || '?', who ? `(${who})` : '', str(e.status, 24), msg].filter(Boolean).join(' '),
  };
}

export const emptyFeed = () => ({ lines: [], next: { audit: null, daemon: null }, files: { audit: '', daemon: '' }, err: { audit: '', daemon: '' }, why: '', at: 0 });

// Take one answer of /hub/mesh/events into what is held. A stream's `reset` clears what that stream held first; its cursor is the answer's
// `next` for it, and its file is the answer's `files` for it. Its `error`, when it has one, is what that stream's label says (feedView). An
// entry for a stream that is not one of the two is left out. A failed or unreadable answer changes nothing but the sentence for it: a hiccup
// keeps the lines and the cursors.
export function foldFeed(feed, r, now = Date.now()) {
  if (!r || r.status !== 200 || !isObj(r.data) || !Array.isArray(r.data.events)) {
    return { ...feed, why: r && r.status === 200 ? 'the events answer was not in a shape this page reads' : why(r), at: now };
  }
  const { events, next, reset, files, error } = r.data;
  const kept = feed.lines.filter((line) => !(isObj(reset) && reset[line.cls] === true));
  const fresh = events.filter((entry) => isObj(entry) && STREAMS.includes(entry.stream) && isObj(entry.event)).map(eventLine);
  const cursor = { ...feed.next };
  const shown = { ...feed.files };
  const err = { audit: '', daemon: '' };
  for (const s of STREAMS) {
    if (isObj(next) && num(next[s]) != null && next[s] >= 0) cursor[s] = next[s];
    if (isObj(files) && (typeof files[s] === 'string' || files[s] === null)) shown[s] = str(files[s], 200);
    if (isObj(error)) err[s] = str(error[s], 200);
  }
  return { lines: [...kept, ...fresh].slice(-HELD), next: cursor, files: shown, err, why: '', at: now };
}

// The feed a reopened tab continues: the one it held, or a fresh one when that is older than FEED_STALE_MS.
export const resumeFeed = (feed, now = Date.now()) => (feed.at && now - feed.at <= FEED_STALE_MS ? feed : emptyFeed());

// Where the next poll goes. With no cursor yet (the first poll) it has no query and gets the newest events of both streams. After that both
// cursors go; a stream with none yet (an answer that named none) is asked from 0, so none of it is skipped.
export function feedPath(feed) {
  if (STREAMS.every((s) => feed.next[s] == null)) return 'mesh/events';
  return `mesh/events?${STREAMS.map((s) => `${s}=${feed.next[s] == null ? 0 : feed.next[s]}`).join('&')}`;
}

// What the tail draws: the newest events first, how many older ones are held, and the sentence for when there is nothing (or a failure). A
// stream that could not be read is a line of its own, first, under that stream's label (`cls` is the label: `audit` or `daemon`), so the
// sentence is on the stream it is about and the other stream's lines are not blanked by it.
export function feedView(feed) {
  const shown = feed.lines.slice(-SHOWN).reverse();
  const older = Math.max(0, feed.lines.length - shown.length);
  const errors = feed.at ? STREAMS.filter((s) => feed.err && feed.err[s]).map((s) => ({ at: clock(feed.at), cls: s, text: `could not be read: ${feed.err[s]}` })) : [];
  const lines = [...errors, ...shown];
  if (feed.why) return { lines, older, sentence: `the events feed could not be read: ${feed.why}`, cls: 'o' };
  if (!feed.at) return { lines, older, sentence: 'reading the events feed…', cls: 'faint' };
  if (!lines.length) return { lines, older, sentence: emptySentence(feed), cls: 'faint' };
  return { lines, older, sentence: '', cls: '' };
}

// The sentence for a tail with nothing in it: each stream's file as the page was told it, and what was read of that file. Bytes read with
// nothing shown is said as that: the audit log keeps only some of its records and the feed only two classes, so it is not "nothing there".
export function emptySentence(feed) {
  const said = STREAMS.map((s) => {
    const path = feed.files[s];
    if (!path) return `${LABEL[s]}: no file to read`;
    const read = feed.next[s] > 0 ? `${feed.next[s]} bytes read, none of them shown` : 'nothing read yet (empty or missing)';
    return `${LABEL[s]} ${path}: ${read}`;
  });
  return `no events to show. ${said.join('; ')}. ${NOT_SAME}`;
}
