// One session stream's state, as signals. No DOM here: panes/session.js draws
// it. There is ONE Session (one /api/events stream) per session id; every pane
// showing that id, mirrors included, is a view of it (app.js, web-ui.md 4.3). Frame -> state follows web-ui.md section 6;
// asks, reconnect, lagged and goodbye follow section 7. Every `hello` resets
// (P2). Views read only the signals they draw, so a frame updates only the
// nodes it changes; streamed text is appended to a live text node, never
// rebuilt (see Draft).
import { signal, batch } from './ui.js';
import { apiUrl, auth, post, hubPost, mode } from './api.js';
import { stream } from './sse.js';
import { World, TICK_MS } from './life.js';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// The wait before the nth consecutive reconnect (n = 0, 1, ...): 250 ms doubled
// each time up to `cap`, then jittered into its upper half so tabs and panes do
// not reconnect in step. A connection that held for HEALTHY_MS resets n.
const HEALTHY_MS = 5000;
export const backoffMs = (n, cap = 15000) => {
  const ms = Math.min(cap, 250 * 2 ** Math.min(n, 16));
  return Math.round(ms / 2 + Math.random() * (ms / 2));
};
export const kfmt = (n) => (n >= 10000 ? (n / 1000).toFixed(0) + 'k' : n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(n));
export const tildify = (p) => String(p || '').replace(/^\/home\/[^/]+(?=\/|$)|^\/root(?=\/|$)/, '~');
export const MODES = ['yolo', 'accept edits', 'manual', 'auto'];

// A connection state as a person reads it (the state names are the code's own).
export const connWord = (c) => ({ kept: 'saved' }[c] || c);
export const STATE_CLS = { RUN: 'run', IDLE: 'idle', ASK: 'ask', '?': 'unknown', OFF: 'off', BYE: 'bye', NONE: 'off' };
// Where a session stands, in one word: the status line's badge and the
// composer frame's colour both read it. ASK: an ask of its is not yet sent
// (answered in the panel but not submitted still waits on the operator).
export function stateOf(s) {
  if (!s) return 'NONE';
  const conn = s.conn.value;
  if (s.bye || conn === 'goodbye') return 'BYE';
  if (conn !== 'live' && conn !== 'replaying') return 'OFF';
  if (s.asks.value.some((a) => !a.busy.value)) return 'ASK';
  const run = s.running.value;
  return run === true ? 'RUN' : run === false ? 'IDLE' : '?';
}
const deadText = (exit) => (exit ? `the chat process exited (${exit})` : 'the chat process exited');
const pad = (n) => String(n).padStart(2, '0');
const DAYS = ['SUN', 'MON', 'TUE', 'WED', 'THU', 'FRI', 'SAT'];
const MONTHS = ['JAN', 'FEB', 'MAR', 'APR', 'MAY', 'JUN', 'JUL', 'AUG', 'SEP', 'OCT', 'NOV', 'DEC'];
export const dayLabel = (t) => { const d = new Date(t); return `${DAYS[d.getDay()]} ${pad(d.getDate())} ${MONTHS[d.getMonth()]}`; };
const dayOf = (t) => new Date(t).toDateString();
export const hhmm = (t) => { const d = new Date(t); return pad(d.getHours()) + ':' + pad(d.getMinutes()); };
// A record's own time: a `ts` (epoch ms or s, or an RFC 3339 string), else
// null. No door frame carries one yet, so no day rows are drawn until one
// does; a live row is stamped on arrival instead (Session.frame).
function stampOf(f) {
  const v = f.ts;
  const t = typeof v === 'number' ? (v < 1e12 ? v * 1000 : v) : typeof v === 'string' ? Date.parse(v) : NaN;
  return Number.isFinite(t) ? t : null;
}
const oneLine = (t) => String(t || '').replace(/`+/g, '').replace(/\s+/g, ' ').trim();
const VERDICT_CAP = 50;
// Read once and kept current by its change event, not queried every tick.
const motion = matchMedia('(prefers-reduced-motion: reduce)');
let reducedMotion = motion.matches;
motion.addEventListener('change', (e) => { reducedMotion = e.matches; });
const reduced = () => reducedMotion;
let seq = 0;

// Rows live in fixed-size chunks, each with its own version signal, so a
// flush touches only the chunk it appends to (O(new rows)), and a view
// re-renders only that chunk, however long the session grows.
const CHUNK = 64;

// Frames the replay/live overlap can double that carry no record id (see
// Session.dupInWindow), and how long after caught-up that window can stay open.
const KEYLESS = new Set(['turn-settled', 'policy-verdict', 'compacted', 'context-size', 'cancelled']);
const WINDOW_MS = 2000;
const keyOf = (f) => (f.record != null ? 'r' + f.record : f.type === 'tool-call-started' && f.id != null ? 'c' + f.id : null);

// The reply as it streams. Text is kept as chunks and handed to every sink
// (each showing pane's text node, via appendData), so a delta costs its own
// length per pane.
class Draft {
  constructor() {
    this.key = 'draft' + ++seq;
    this.at = Date.now(); // it is live by nature: its time is now
    this.text = []; this.think = [];
    this.sinks = new Set(); this.thinkSinks = new Set();
    this.thinkLines = signal(0);
    this.tools = signal([]);
  }
  add(t) { this.text.push(t); for (const f of this.sinks) f(t); }
  addThink(t) {
    if (!this.think.length) this.thinkLines.value = 1;
    this.think.push(t);
    const nl = t.split('\n').length - 1;
    if (nl) this.thinkLines.value += nl;
    for (const f of this.thinkSinks) f(t);
  }
  get body() { return this.text.join(''); }
}

export class Session {
  constructor(sid, hooks) {
    this.sid = sid;
    this.hooks = hooks; // { toast(text, isErr) }
    this.closed = false;
    this.bye = false;
    this.epoch = 0;        // bumped by resume(): a loop and its stream from an older epoch are superseded
    this.conn = signal('connecting');
    this.banner = signal(null);        // { text, err }
    this.life = new World();
    this.strip = signal(this.life.render());
    this.clock = signal(0);
    this.timer = 0;
    // A door from before eidolon #18 turned down the unknown `note` field
    // (a 400): later answers go without one. A door with the feature 422s
    // only a bad note, which does not set this.
    this.noNote = signal(false);
    this.arrived = new Map(); // ask_id -> arrival order, kept across reconnects
    this.picked = new Map();  // ask_id -> the answer recorded and not yet sent, kept across reconnects
    this.unread = signal(0);  // live replies, peer messages and asks while no focused pane shows this session
    this.at = null;           // the time of the frame being handled: its record's, else its arrival while live, else null
    this.real = false;        // whether that time is the record's own (only those draw day rows)
    this.reset();
    this.run();
  }

  // ---------------------------------------------------------------- state
  reset() {
    batch(() => {
      this.hello = this.hello || signal(null);
      this.hello.value = null;
      this.live = false;
      this.running = this.running || signal(null);   // true | false | null = unknown (gap 1)
      this.setRunning(null);
      for (const k of ['ctx', 'budget']) { this[k] = this[k] || signal(null); this[k].value = null; }
      for (const k of ['queued', 'turns', 'calls', 'fails', 'finished', 'usedIn', 'usedOut']) { this[k] = this[k] || signal(0); this[k].value = 0; }
      for (const k of ['chunks', 'asks', 'open', 'verdicts']) { this[k] = this[k] || signal([]); this[k].value = []; }
      this.last = this.last || signal(null); // { who, text }: the sidebar's one-line preview
      this.last.value = null;
      this.touched = this.touched || signal(new Map());
      this.touched.value = new Map();
      this.tools = this.tools || signal(new Map()); // tool name -> calls seen (the door lists no tools; these are the ones used)
      this.tools.value = new Map();
      this.draft = this.draft || signal(null);
      this.draft.value = null;
      this.jumpToEnd = this.jumpToEnd || signal(0); // a counter; each bump puts every view at the bottom
    });
    this.records = new Set();
    this.byId = new Map();   // call id -> call
    this.pending = [];       // rows waiting for the next flush
    this.lastWho = null;
    this.head = null;        // the first row of the agent's current run: its settles add up there
    this.day = null;         // the date of the last row with a known time: a new one draws a day row
    this.partial = null;     // a reply row an `error` cut short, until its assistant-message
    this.seqReplay = [];     // replayed frames, for the overlap window
    this.stale = false;      // a `context-invalidated` waits for a safe point
    this.win = null;
  }

  // Rows are appended through one buffer, flushed once per burst of frames,
  // into the last chunk: only that chunk's version is bumped, and the chunk
  // list is copied only when a new chunk starts (every CHUNK rows).
  addRow(row) {
    row.key = row.key || 'r' + ++seq;
    row.at = this.at;
    const first = !this.pending.length;
    // Anything that is not a speaker's own line ends that speaker's run, so
    // the next message names who says it again.
    if (!['you', 'bot', 'tool', 'ask', 'verdict'].includes(row.kind)) this.lastWho = null;
    if (this.real && dayOf(row.at) !== this.day) {
      this.day = dayOf(row.at);
      this.pending.push({ kind: 'day', key: 'r' + ++seq, text: dayLabel(row.at) });
      if ((row.kind === 'you' || row.kind === 'bot') && !row.who) row.who = this.lastWho = row.kind;
    }
    // The first row of a speaker's run carries the run's facts beside the
    // name: its time, and for the agent what its turns cost (settled below).
    if (row.who) {
      row.meta = signal(null);
      this.head = row.who === 'bot' ? row : null;
    }
    if (row.kind === 'you' || row.kind === 'bot' || row.kind === 'peer') {
      const text = oneLine(row.text);
      if (text) this.last.value = { who: row.kind === 'peer' ? '~ ' + row.from : row.kind, text };
    }
    if (row.kind === 'bot' || row.kind === 'peer' || row.kind === 'ask') this.bump();
    this.pending.push(row);
    if (first) queueMicrotask(() => this.flush());
    return row;
  }
  flush() {
    if (!this.pending.length) return;
    const add = this.pending;
    this.pending = [];
    const list = this.chunks.value;
    let last = list[list.length - 1], fresh = null;
    const touched = new Set();
    for (const row of add) {
      if (!last || last.rows.length >= CHUNK) {
        last = { key: 'c' + ++seq, rows: [], ver: signal(0) };
        (fresh = fresh || []).push(last);
      } else touched.add(last);
      last.rows.push(row);
    }
    batch(() => {
      for (const c of touched) c.ver.value++;
      if (fresh) this.chunks.value = list.concat(fresh);
    });
  }

  bump() {
    if (this.live && !(this.hooks.seen && this.hooks.seen(this))) this.unread.value++;
  }

  who(kind) {
    if (this.lastWho === kind) return null;
    this.lastWho = kind;
    return kind;
  }

  // ---------------------------------------------------------------- connection
  async run() {
    const ep = this.epoch;
    if (this.looping === ep) return;
    this.looping = ep;
    try { await this.loop(ep); } finally { if (this.looping === ep) this.looping = null; }
  }

  // Whether the session still holds (or is about to open) an event stream:
  // what the page's connection cap counts. A stopped, kept, dead, gone,
  // refused or goodbye'd session holds none.
  holdsStream() {
    return !this.closed && !['stopped', 'kept', 'dead', 'gone', 'refused', 'goodbye'].includes(this.conn.value);
  }

  // How a session that will not come back is shown: the connection word, and
  // a banner that offers `resume` under the perch (the perch never restarts a
  // door by itself; resuming can start a turn, hub.md section 4).
  finish(conn, text) {
    this.setRunning(null);
    this.live = false;
    this.conn.value = conn;
    this.banner.value = { text, resume: conn !== 'gone' && mode() === 'perch' };
  }

  async loop(ep) {
    let fails = 0, quick = 0;
    while (!this.closed && !this.bye && ep === this.epoch) {
      this.conn.value = fails >= 3 ? 'door not answering' : fails ? 'reconnecting' : 'connecting';
      this.gotHello = false;
      this.lagged = false;
      this.refresh = false;
      this.ended = null;
      this.stale = false; // a waiting context-invalidated is moot: this connection replays the log
      const t0 = performance.now();
      this.abort = new AbortController();
      let res;
      try {
        // Frames of a connection from a superseded epoch (resume() ran) are dropped.
        res = await stream(apiUrl(this.sid, 'events'), auth(), (f) => { if (ep === this.epoch) batch(() => this.frame(f)); }, this.abort.signal);
      } catch {
        res = { status: 0 };
      }
      if (this.closed || this.bye || ep !== this.epoch) break;
      // The stream ended with no goodbye (the door died, or it will be
      // reopened): what runs is unknown until a turn-state says, the same `?`
      // as a page that connected mid-turn, never a stale RUN.
      this.setRunning(null);
      if (res.status === 401) {
        this.conn.value = 'refused';
        this.banner.value = { text: mode() === 'perch' ? 'the server refused the login for this page (it restarted?). Run `hoot` again.' : 'the server refused the login for this page. Run `hoot` again.', err: true };
        return;
      }
      this.live = false;
      if (mode() === 'perch') {
        // How the perch says a stream ended (hub.md section 5), and what it
        // answers at connect when there is no stream to open.
        const e = this.ended, st = res.data && res.data.state;
        if (e && e.door === 'stopping') { this.finish('stopped', 'stopped'); return; }
        if (e && e.door === 'dead') { this.finish('dead', deadText(e.exit)); return; }
        if (res.status === 409 && st === 'kept') { this.finish('kept', 'saved, not running'); return; }
        if (res.status === 503 && st === 'dead') { this.finish('dead', deadText(null)); return; }
        if (res.status === 404) { this.finish('gone', `no chat ${this.sid} here`); return; }
        // `live`, 409 starting|stopping, 502 and anything else: try again
      }
      if (this.lagged || this.refresh || (mode() === 'perch' && this.ended && this.ended.door === 'live')) {
        // A stream that ended on purpose (lagged, a redraw, the door still there):
        // reconnect after a short jittered wait that doubles while the
        // connections keep ending quickly; a stream that held resets it.
        fails = 0;
        quick = this.gotHello && performance.now() - t0 >= HEALTHY_MS ? 0 : quick + 1;
        if (quick >= 3) this.conn.value = 'reconnecting';
        await sleep(backoffMs(Math.max(0, quick - 1)));
        continue;
      }
      quick = 0;
      fails = this.gotHello ? 1 : fails + 1;
      this.conn.value = fails >= 3 ? 'door not answering' : 'reconnecting';
      await sleep(backoffMs(fails, 8000));
    }
  }

  reconnect() { if (this.abort) this.abort.abort(); }

  // The page keeps no cache beyond its rows, so "shed it and redraw from the
  // log" is a reconnect: the stream replays the log and `hello` resets the
  // rows. Done only when no turn is running (the safe point), and without
  // counting as a failure or showing a banner.
  redraw() {
    this.stale = false;
    this.refresh = true;
    this.reconnect();
  }

  // Whether `resume` is on offer: the perch is here, the banner says so, and
  // no resume is already out.
  canResume() { const b = this.banner.value; return mode() === 'perch' && !!(b && b.resume) && !this.resuming; }

  // Resume a stopped, dead or kept session (perch only). It can take ~20 s
  // (a door has to come up) and can start a turn by itself; only ever called
  // from a click or a key, never automatically.
  async resume() {
    if (!this.canResume() || this.closed) return false;
    if (this.hooks.cap && this.hooks.cap(this.sid)) return false; // a resumed door opens a stream: the page's cap
    const prev = this.banner.value;
    this.resuming = true;
    this.banner.value = { text: 'resuming…' };
    const r = await hubPost(`sessions/${encodeURIComponent(this.sid)}/resume`);
    this.resuming = false;
    if (this.closed) return false;
    if (r.status === 200) {
      // The old loop may still be waiting on the stream of the session that
      // ended (the goodbye window): supersede it, so its stream-end cannot
      // paint `stopped` over the session just resumed.
      this.epoch++;
      if (this.abort) this.abort.abort();
      batch(() => {
        this.bye = false;
        this.ended = null;
        this.banner.value = null;
        this.conn.value = 'connecting';
      });
      this.run();
      return true;
    }
    this.banner.value = prev;
    const t = r.status === 409 ? 'this chat is already running (here or in another app)'
      : r.status === 404 ? 'no such kept session'
      : r.status === 503 ? `the chat did not start: ${(r.data && r.data.error) || 'no reason given'}`
      : r.status === 401 ? 'the server refused the login for this page (it restarted?). Run `hoot` again.'
      : r.status === 0 ? 'the server is not answering'
      : `resume failed: ${r.status}`;
    this.hooks.toast(t, true);
    return false;
  }

  close() {
    this.closed = true;
    this.setRunning(false);
    if (this.abort) this.abort.abort();
  }

  // ---------------------------------------------------------------- running + the pulse
  // Only `turn-state` sets running. Not `turn-settled`: core runs queued
  // follow-ups inside the same turn and publishes TurnSettled for each one
  // (core agent.rs:1327-1337, :1600) while the driver sends no new
  // turn-state, so a settle is not the end of the run. Not `cancelled` or
  // `error` either. null = unknown: a page that connected mid-turn cannot
  // tell (gap 1), so it says so instead of claiming idle.
  setRunning(v) {
    const was = this.running && this.running.value;
    if (this.running) this.running.value = v;
    if (v === true && was !== true) {
      this.t0 = this.live ? performance.now() : null;
      this.life.restart();
      this.strip.value = this.life.render();
      clearInterval(this.timer);
      this.timer = setInterval(() => this.tick(), TICK_MS);
    } else if (v !== true) {
      clearInterval(this.timer);
      this.timer = 0;
      this.t0 = null;
    }
  }

  tick() {
    if (document.hidden) return; // paused while the tab is hidden; the clock catches up on return
    if (!reduced()) {
      this.life.step();
      this.strip.value = this.life.render();
    }
    if (this.t0 != null) this.clock.value = performance.now() - this.t0;
  }

  ship(fromLeft) {
    if (this.running.value !== true || !this.live || reduced()) return;
    if (fromLeft) this.life.call(); else this.life.result();
    this.strip.value = this.life.render();
  }

  // ---------------------------------------------------------------- the overlap window
  // The door subscribes to the bus BEFORE it walks the branch (stream.rs:5-11),
  // so what lands in between arrives twice: once in the replay, once live just
  // after caught-up. Frames with a record id (or a tool-call id) are deduped by
  // that id elsewhere. The keyless ones (KEYLESS) are deduped here, by position:
  //   - the live duplicates are a suffix of the replay, in the same order;
  //   - a live keyed frame the replay also had puts a cursor just past it in
  //     the replay; a keyless live frame whose type matches an entry in the
  //     run of keyless entries at the cursor is that entry again (matched
  //     greedily, cursor moved past it), and is dropped. A new keyless fact
  //     cannot come first: each follows a new keyed frame (a user or
  //     assistant message, a tool call), which closes the window;
  //   - before any keyed duplicate, the cursor starts after the replay's last
  //     keyed frame (the window can open on keyless frames alone, e.g. a
  //     settle published just after the walk read its record);
  //   - the window closes at the first keyed frame the replay did not have
  //     (new facts from there on), or WINDOW_MS after caught-up.
  // Live-only frames (deltas, asks, error, queued, turn-state) pass untouched.
  // Not covered: a keyless duplicate that arrives before the first keyed one
  // when the replay has a keyed frame after it; that one can still double.
  openWindow() {
    const seqR = this.seqReplay, idx = new Map();
    let q = 0;
    seqR.forEach((e, i) => { if (e.key) { idx.set(e.key, i); q = i + 1; } });
    this.seqReplay = [];
    this.win = seqR.length ? { seq: seqR, idx, p: null, q, until: performance.now() + WINDOW_MS } : null;
  }

  dupInWindow(f) {
    const w = this.win;
    if (!w) return false;
    if (performance.now() > w.until) { this.win = null; return false; }
    const key = keyOf(f);
    if (key) {
      const i = w.idx.get(key);
      if (i == null) this.win = null; else w.p = i + 1;
      return false;
    }
    if (!KEYLESS.has(f.type)) return false;
    // Greedy forward match inside the keyless run at the cursor: the live
    // duplicates are a suffix of that run, so always a subsequence of it.
    let at = w.p != null ? w.p : w.q;
    while (at < w.seq.length && !w.seq[at].key && w.seq[at].type !== f.type) at++;
    const e = w.seq[at];
    if (!e || e.key) return false;
    if (w.p != null) w.p = at + 1; else w.q = at + 1;
    return true;
  }

  // ---------------------------------------------------------------- frames
  frame(f) {
    const ts = stampOf(f);
    this.real = ts != null;
    this.at = ts ?? (this.live ? Date.now() : null);
    if (this.live) {
      if (this.dupInWindow(f)) return;
    } else if (this.gotHello && f.type !== 'hello' && f.type !== 'caught-up') {
      const key = keyOf(f);
      if (key || KEYLESS.has(f.type)) this.seqReplay.push({ type: f.type, key });
    }
    switch (f.type) {
      case 'hello':
        this.reset();
        this.gotHello = true;
        this.hello.value = f;
        this.conn.value = 'replaying';
        if (f.protocol !== 1) this.hooks.toast(`door speaks protocol ${f.protocol}; this page knows 1`, true);
        break;
      case 'caught-up': {
        this.flush();
        this.live = true;
        this.openWindow();
        this.conn.value = 'live';
        const hl = this.hello.value;
        if (hl && this.asks.value.length !== hl.pending) {
          console.warn(`hello.pending=${hl.pending} but ${this.asks.value.length} asks arrived`);
        }
        // `running` stays unknown here: hello has no `running` (gap 1), and
        // only turn-state may set it.
        this.jumpToEnd.value++;
        break;
      }
      case 'turn-state':
        this.setRunning(!!f.running);
        if (!f.running) this.partial = null; // a turn that ended owes no assistant-message
        if (!f.running && this.stale) this.redraw();
        break;
      case 'lagged':
        this.lagged = true;
        this.hooks.toast(`stream lagged (${f.dropped} dropped), rebuilding`);
        break;
      case 'goodbye':
        this.bye = true;
        this.conn.value = 'goodbye';
        clearInterval(this.timer);
        this.banner.value = mode() === 'perch'
          ? { text: f.text || 'the session said goodbye', resume: true }
          : { text: (f.text || 'the session said goodbye') + '\n(resume needs the hub)' };
        break;
      case 'hub-stream-end': // the perch's last frame; draws nothing, run() reads it
        this.ended = f;
        break;
      case 'context-invalidated': // `{what, record?}`: drop what was cached and redraw from the log at a safe point
        // Live frames only: a replayed one is already what the redraw would show
        // (and acting on it would loop). Unknown `running` (null: nothing says
        // so right after connect) counts as idle; only a known running turn waits.
        if (!this.live) break;
        if (this.running.value !== true) this.redraw(); else this.stale = true;
        break;

      // review #4: a new message, an error and a stop all close the draft,
      // so nothing blinks forever and nothing appends onto stale text.
      case 'message-start': this.draft.value = new Draft(); break;
      case 'text-delta': this.openDraft().add(f.text || ''); break;
      case 'thinking-delta': this.openDraft().addThink(f.text || ''); break;
      case 'tool-use-start': {
        const d = this.openDraft();
        d.tools.value = [...d.tools.value, { id: f.id, name: f.name || '?', bytes: signal(0) }];
        break;
      }
      case 'tool-input-delta': { // spinner and a byte count only; never parse partial JSON
        const t = this.draft.value && this.draft.value.tools.value.find((x) => x.id === f.id);
        if (t) t.bytes.value += (f.partial_json || '').length;
        break;
      }
      case 'assistant-message': this.draft.value = null; this.assistant(f); break;
      case 'user-message': this.user(f); break;
      case 'tool-call-started': this.callStarted(f); break;
      case 'tool-call-finished': this.callFinished(f); break;
      case 'policy-verdict': this.verdict(f); break;
      case 'ask': this.ask(f); break;
      case 'ask-settled': this.askSettled(f); break;
      case 'context-size': this.ctx.value = f.tokens; break;
      case 'turn-budget':
        if (f.record != null && this.records.has(f.record)) break;
        this.budget.value = f.calls_left;
        if (f.calls_left < 3) this.sys(`${f.calls_left} tool calls left this turn`, f.record, 'y');
        else if (f.record != null) this.records.add(f.record);
        break;
      case 'turn-settled': this.settled(f); break;
      case 'cancelled':
        this.freezeDraft('stopped');
        this.partial = null;
        this.stopOpen();
        this.sys('stopped' + (f.calls ? ` after ${f.calls} call${f.calls > 1 ? 's' : ''}` : ''), null, 'y');
        break;
      case 'error':
        // The partial reply becomes a row with no caret; its assistant-message,
        // if one follows, fills that same row instead of adding a second.
        this.freezeDraft('cut off by an error', true);
        this.sys('error: ' + (f.text || ''), null, 'err');
        break;
      case 'compacted':
        this.addRow({ kind: 'fold', label: `earlier messages summarised (${f.replaced_messages})`, text: f.summary || '' });
        break;
      case 'peer-message':
        if (this.records.has(f.record)) break;
        this.records.add(f.record);
        this.lastWho = 'peer';
        this.addRow({ kind: 'peer', from: f.from + (f.channel ? ` #${f.channel}` : '') + (f.external ? ' (external)' : ''), text: f.text || '' });
        break;
      case 'command-results':
        if (this.records.has(f.record)) break;
        this.records.add(f.record);
        this.addRow({ kind: 'pre', text: (f.lines || []).join('\n') });
        break;
      case 'queued': this.queued.value = f.waiting || 0; break;
      case 'quiesced':
        this.sys(`settling to ${f.destination}`, f.record);
        this.hooks.toast(`session settling to ${f.destination}`);
        break;
      case 'trigger-fired': {
        const c = this.byId.get(f.call_id);
        if (c) c.fired.value = f.outcome || 'fired';
        this.sys(`◆ fired: ${f.condition}  (${f.outcome})`, f.record);
        break;
      }
      default: // ask-user (the `ask` frame is its drawable twin) and anything unknown (P6)
    }
  }

  // ---------------------------------------------------------------- rows
  sys(text, record, cls = '') {
    if (record != null) {
      if (this.records.has(record)) return;
      this.records.add(record);
    }
    this.addRow({ kind: 'sys', text, cls });
  }

  openDraft() {
    if (!this.draft.value) this.draft.value = new Draft();
    return this.draft.value;
  }

  freezeDraft(why, keep) {
    const d = this.draft.value;
    if (!d) return;
    this.draft.value = null;
    const text = d.body;
    if (!text.trim()) return;
    const row = this.addRow({ kind: 'bot', who: this.who('bot'), text, cut: why, full: keep ? signal(null) : null });
    if (keep && !this.partial) this.partial = row;
  }

  user(f) {
    if (this.records.has(f.record)) return;
    this.records.add(f.record);
    // A new user message means the cut-off reply's assistant-message is not
    // coming (core sends none after a failed stream, agent.rs:1611-1613).
    this.partial = null;
    this.queued.value = 0;
    this.addRow({ kind: 'you', who: this.who('you'), text: f.text || '', images: f.images || 0 });
  }

  assistant(f) {
    if (this.records.has(f.record)) return;
    this.records.add(f.record);
    if (!f.text && !f.thinking && !f.redacted) { this.who('bot'); return; }
    if (this.partial) {
      this.partial.full.value = { text: f.text || '', thinking: f.thinking || '', redacted: f.redacted || 0 };
      this.partial = null;
      this.lastWho = 'bot';
      return;
    }
    this.addRow({ kind: 'bot', who: this.who('bot'), text: f.text || '', thinking: f.thinking || '', redacted: f.redacted || 0 });
  }

  // ---------------------------------------------------------------- tools
  callStarted(f) {
    const known = this.byId.get(f.id);
    if (known) { if (known.input == null && f.input != null) known.input = f.input; return; }
    this.lastWho = 'bot';
    const c = {
      id: f.id, name: f.name || '?', input: f.input, origin: f.origin || 'model',
      t0: this.live ? performance.now() : null, done: false, isError: false,
      st: signal('run'), time: signal(''), out: signal(null),
      verdict: signal(null), ask: signal(null), settled: signal(null), fired: signal(null),
    };
    this.byId.set(f.id, c);
    this.calls.value++;
    if (f.name) this.tools.value = new Map(this.tools.value).set(f.name, (this.tools.value.get(f.name) || 0) + 1);
    this.open.value = [...this.open.value, c];
    if (/^(edit|write|multi_?edit|patch|apply_patch)$/i.test(c.name)) {
      const p = c.input && (c.input.path || c.input.file_path);
      if (p) {
        const m = new Map(this.touched.value);
        m.set(p, (m.get(p) || 0) + 1);
        this.touched.value = m;
      }
    }
    this.addRow({ kind: 'tool', call: c });
    if (c.origin !== 'script') this.ship(true);
  }

  callFinished(f) {
    let c = this.byId.get(f.id);
    if (!c) { this.callStarted({ id: f.id, name: f.name || '?', input: null, origin: 'model' }); c = this.byId.get(f.id); }
    if (c.done) return;
    if (f.record != null) this.records.add(f.record);
    c.done = true;
    c.isError = !!f.is_error;
    c.st.value = c.isError ? 'err' : 'ok';
    if (c.t0 != null) c.time.value = ((performance.now() - c.t0) / 1000).toFixed(1) + 's';
    c.out.value = { output: f.output, isError: c.isError };
    this.finished.value++;
    if (c.isError) this.fails.value++;
    if (this.open.value.includes(c)) this.open.value = this.open.value.filter((x) => x !== c);
    if (c.origin !== 'script') this.ship(false);
  }

  // review #6: the wire's policy-verdict has no call_id. Attach it only when
  // exactly one open call has that tool name; otherwise it is its own row.
  verdict(f) {
    const v = this.verdicts.value;
    this.verdicts.value = (v.length >= VERDICT_CAP ? v.slice(1 - VERDICT_CAP) : v).concat(f);
    const cands = this.open.value.filter((c) => c.name === f.tool && !c.verdict.value);
    if (cands.length === 1) cands[0].verdict.value = f;
    else this.addRow({ kind: 'verdict', f });
  }

  // ---------------------------------------------------------------- asks
  ask(f) {
    if (this.asks.value.some((a) => a.id === f.ask_id)) return;
    // Arrival order across every session (the page's one ask queue is oldest
    // first). An ask replayed after a reconnect keeps the place it had.
    if (!this.arrived.has(f.ask_id)) this.arrived.set(f.ask_id, ++seq);
    const a = {
      id: f.ask_id, f, fresh: this.live, n: this.arrived.get(f.ask_id),
      busy: signal(false), err: signal(''), done: signal(null), chosen: signal(this.picked.get(f.ask_id) || null),
    };
    if (f.kind === 'approval') {
      // `always` is offered only by a door that lists it (gap 13), and is
      // never what yes or no fall back to. No plain no offered: `no` is greyed.
      const offered = f.answers && f.answers.length >= 2 ? f.answers : ['yes', 'no'];
      const plain = offered.filter((x) => x !== 'always');
      a.yes = plain.includes('yes') ? 'yes' : plain[0] || null;
      a.no = plain.includes('no') ? 'no' : plain.length > 1 ? plain[plain.length - 1] : null;
      a.always = offered.includes('always') ? 'always' : null;
    } else {
      a.options = (f.options || []).map((o, i) => ({ ...o, key: i < 9 ? String(i + 1) : null }));
      // A question whose options are exactly yes and no (core confirm()
      // arrives this way, user.rs:380-403) also answers to `y` and `n`.
      const labels = a.options.map((o) => String(o.label).trim().toLowerCase());
      if (labels.length === 2 && labels.includes('yes') && labels.includes('no')) {
        a.yesno = true;
        a.yes = a.options[labels.indexOf('yes')].label;
        a.no = a.options[labels.indexOf('no')].label;
      }
    }
    const c = f.call_id != null ? this.byId.get(f.call_id) : null;
    a.call = c;
    this.asks.value = [...this.asks.value, a];
    if (c) { c.ask.value = a; this.bump(); }
    else this.addRow({ kind: 'ask', ask: a });
  }

  askSettled(f) {
    const a = this.asks.value.find((x) => x.id === f.ask_id);
    if (a) this.dropAsk(a, f.how === 'cancelled' ? { how: 'cancelled' } : { how: 'answered', answer: f.answer, note: f.note });
  }

  // A settled ask leaves the queue but stays in the transcript, folded to
  // what was picked (panes/session.js Settled), so its choices can still be
  // read. The note is the door's (ask-settled carries it, whoever sent it);
  // the one this page sent is only a fallback.
  // done: { how: 'answered' | 'cancelled' | 'elsewhere', answer?, note? }
  dropAsk(a, done) {
    this.asks.value = this.asks.value.filter((x) => x !== a);
    this.picked.delete(a.id);
    const sent = a.chosen.peek() && a.chosen.peek().answer === done.answer ? a.chosen.peek() : {};
    a.done.value = { ...done, note: done.note || sent.note };
    if (a.call) { a.call.ask.value = null; a.call.settled.value = a; }
  }

  // Answering is two steps: choose records the reply (the panel shows it as
  // answered, and it can be chosen again until it is sent; a reconnect keeps
  // it), send posts it. The door has taken notes since eidolon #18 and 422s
  // only a bad one (over 1024 bytes, or otherwise refused): that shows the
  // door's reason and keeps notes on. A 400 on an answer with a note is a
  // door from before #18 refusing the unknown field: remembered, so the next
  // try goes without it. Any refusal clears the choice so it can be made
  // again. A 409
  // (answered elsewhere) or 404 (the session is gone) drops the ask.
  // reply: { answer, note? }
  choose(a, reply) {
    if (a.busy.peek() || !this.asks.peek().includes(a)) return false;
    a.err.value = '';
    a.chosen.value = reply;
    this.picked.set(a.id, reply);
    return true;
  }

  // Resolves to { status, why } (status 0: not sent).
  async send(a) {
    const reply = a.chosen.peek();
    if (!reply || a.busy.peek() || !this.asks.peek().includes(a)) return { status: 0 };
    a.busy.value = true;
    const r = await post(this.sid, 'answer', { ask_id: a.id, ...reply });
    if (r.status === 204) { this.picked.delete(a.id); return { status: 204 }; } // ask-settled draws the rest
    a.busy.value = false;
    if (r.status === 409 || r.status === 404) {
      if (this.asks.peek().includes(a)) this.dropAsk(a, { how: 'elsewhere' });
      this.hooks.toast(r.status === 409 ? 'that ask was answered elsewhere' : 'that ask is gone (404): its session ended');
      return { status: r.status };
    }
    a.chosen.value = null;
    this.picked.delete(a.id);
    const why = `${r.status || 'network'}: ${(r.data && r.data.error) || 'not accepted'}`;
    if (r.status === 422 && reply.note != null) {
      a.err.value = `the door refused the note: ${(r.data && r.data.error) || 'not accepted'}`;
    } else if (r.status === 400 && reply.note != null) {
      this.noNote.value = true;
      a.err.value = `the door did not take the note (${why}); choose again to send without it`;
    } else a.err.value = why;
    return { status: r.status || -1, why };
  }

  answer(a, reply) { return this.choose(a, reply) ? this.send(a) : Promise.resolve({ status: 0 }); }

  // ---------------------------------------------------------------- turns
  // A turn that ended leaves no spinner behind: calls it never finished are
  // marked stopped (a late tool-call-finished still fills them in).
  stopOpen() {
    for (const c of this.open.value) c.st.value = 'stop';
    this.open.value = [];
  }

  // A settle closes one model loop, not the run: `running` is left to
  // turn-state (see setRunning).
  settled(f) {
    this.draft.value = null;
    this.partial = null;
    this.stopOpen();
    this.turns.value++;
    const u = f.usage || {};
    this.usedIn.value += (u.input_tokens || 0) + (u.cache_read_input_tokens || 0);
    this.usedOut.value += u.output_tokens || 0;
    const h = this.head;
    if (h) {
      const m = h.meta.peek() || { in: 0, out: 0, ms: 0 };
      h.meta.value = { in: m.in + (u.input_tokens || 0) + (u.cache_read_input_tokens || 0), out: m.out + (u.output_tokens || 0), ms: m.ms + ((f.timing && f.timing.total_ms) || 0) };
    }
    const bits = [String(f.stop_reason || 'settled').replace(/-/g, ' ')];
    if (u.input_tokens || u.output_tokens) bits.push(`${kfmt(u.input_tokens + (u.cache_read_input_tokens || 0))} in`, `${kfmt(u.output_tokens)} out`);
    if (f.timing) bits.push((f.timing.total_ms / 1000).toFixed(1) + 's');
    this.addRow({ kind: 'settle', text: bits.join(' · ') });
    this.lastWho = null;
  }

  // ---------------------------------------------------------------- verbs
  // how: 'send' | 'steer' | 'queue'. A steer is never sent to a session known
  // to be idle (it would sit unread, core agent.rs:430); `queue` is a send,
  // which the door queues behind a running turn. images: [{ name, media_type,
  // data }] as core/attach.js reads them (checked there against the door's limits).
  async submit(how, text, images = []) {
    if ((!text.trim() && !images.length) || this.sending) return false;
    if (this.bye) { this.hooks.toast('this session said goodbye', true); return false; }
    const mode = how === 'steer' && this.running.value !== false ? 'steer' : 'send';
    this.sending = true;
    const body = { text, mode };
    if (images.length) body.images = images.map(({ name, media_type, data }) => ({ name, media_type, data }));
    const r = await post(this.sid, 'say', body);
    this.sending = false;
    if (r.status === 202) {
      if (r.data && r.data.queued) this.hooks.toast('queued behind the running turn');
      else if (mode === 'steer') this.hooks.toast('steer sent; it lands at the next safe point');
      return true;
    }
    // A sentence, not a status code: the cases a person can act on.
    this.hooks.toast(r.status === 409 || r.status === 404 ? 'this chat is not running; use resume to continue it'
      : r.status === 0 ? 'could not reach the server; the message was not sent'
      : `the message was not sent (${r.status}${r.data && r.data.error ? ': ' + r.data.error : ''})`, true);
    return false;
  }

  // Why a window cannot be opened for this session now, or null. There is no
  // door to ask once it said goodbye, stopped, died or is only a kept log.
  launchOff() {
    if (this.bye || ['goodbye', 'stopped', 'dead', 'kept', 'gone'].includes(this.conn.value)) return 'not running';
    return null;
  }

  // Open a terminal, an editor or a file manager on the machine that runs the
  // door (POST launch, the operator's act, not a tool call). `path` omitted is
  // the session's folder; when given it came from a tool line's input and means
  // what the door's launcher says it means.
  async launch(window, path) {
    const off = this.launchOff();
    if (off) { this.hooks.toast(`cannot open a window: ${off}`, true); return false; }
    const body = { window };
    if (path != null) {
      if (typeof path !== 'string' || path.includes('\0') || new TextEncoder().encode(path).length > 4096) {
        this.hooks.toast('launch refused: a path of at most 4096 bytes and no NUL is what this route takes', true);
        return false;
      }
      body.path = path;
    }
    const r = await post(this.sid, 'launch', body);
    const why = (r.data && r.data.error) || '';
    if (r.status === 200) {
      const o = String((r.data && r.data.opened) || '');
      this.hooks.toast(`opened: ${o.length > 120 ? o.slice(0, 119) + '…' : o}`);
      return true;
    }
    if (r.status === 501) this.hooks.toast('this server has no launcher configured', true);
    else if (r.status === 422) this.hooks.toast(`launch refused: ${why}`, true);
    else if (r.status === 500) this.hooks.toast(`the launcher failed: ${why}`, true);
    else if (r.status === 401) this.hooks.toast('the server refused the login for this page (it restarted?). Run `hoot` again.', true);
    else if (r.status === 0) this.hooks.toast('launch failed: the server is not answering', true);
    else this.hooks.toast(`launch failed: ${r.status}${why ? ' ' + why : ''}`, true);
    return false;
  }

  async cancel() {
    const r = await post(this.sid, 'cancel');
    if (r.status === 204) this.hooks.toast('cancelling the turn');
    else if (r.status === 409) this.hooks.toast('nothing is running');
    else this.hooks.toast(`cancel failed: ${r.status || 'network'}`, true);
  }
}
