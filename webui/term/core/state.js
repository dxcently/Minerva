// One session stream's state, as signals. No DOM here: panes/session.js draws
// it. There is ONE Session (one /api/events stream) per session id; every pane
// showing that id, mirrors included, is a view of it (app.js, web-ui.md 4.3). Frame -> state follows web-ui.md section 6;
// asks, reconnect, lagged and goodbye follow section 7. Every `hello` resets
// (P2). Views read only the signals they draw, so a frame updates only the
// nodes it changes; streamed text is appended to a live text node, never
// rebuilt (see Draft).
import { signal, batch } from './ui.js';
import { apiUrl, auth, post } from './api.js';
import { stream } from './sse.js';
import { World, TICK_MS } from './life.js';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const kfmt = (n) => (n >= 10000 ? (n / 1000).toFixed(0) + 'k' : n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(n));
export const tildify = (p) => String(p || '').replace(/^\/home\/[^/]+(?=\/|$)|^\/root(?=\/|$)/, '~');
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
    this.conn = signal('connecting');
    this.banner = signal(null);        // { text, err }
    this.life = new World();
    this.strip = signal(this.life.render());
    this.clock = signal(0);
    this.timer = 0;
    // The door turned down a note beside an answer (notes arrive with a
    // pending eidolon PR): later answers go without one.
    this.noNote = signal(false);
    this.arrived = new Map(); // ask_id -> arrival order, kept across reconnects
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
      for (const k of ['queued', 'turns', 'calls', 'fails', 'finished']) { this[k] = this[k] || signal(0); this[k].value = 0; }
      for (const k of ['chunks', 'asks', 'open', 'verdicts']) { this[k] = this[k] || signal([]); this[k].value = []; }
      this.touched = this.touched || signal(new Map());
      this.touched.value = new Map();
      this.draft = this.draft || signal(null);
      this.draft.value = null;
      this.stick = this.stick || signal(0); // bumped to ask the view to jump to the bottom
    });
    this.records = new Set();
    this.byId = new Map();   // call id -> call
    this.pending = [];       // rows waiting for the next flush
    this.lastWho = null;
    this.partial = null;     // a reply row an `error` cut short, until its assistant-message
    this.seqReplay = [];     // replayed frames, for the overlap window
    this.win = null;
  }

  // Rows are appended through one buffer, flushed once per burst of frames,
  // into the last chunk: only that chunk's version is bumped, and the chunk
  // list is copied only when a new chunk starts (every CHUNK rows).
  addRow(row) {
    row.key = row.key || 'r' + ++seq;
    // Anything that is not a speaker's own line ends that speaker's run, so
    // the next message names who says it again.
    if (!['you', 'bot', 'tool', 'ask', 'verdict'].includes(row.kind)) this.lastWho = null;
    this.pending.push(row);
    if (this.pending.length === 1) queueMicrotask(() => this.flush());
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

  who(kind) {
    if (this.lastWho === kind) return null;
    this.lastWho = kind;
    return kind;
  }

  // ---------------------------------------------------------------- connection
  async run() {
    let fails = 0;
    while (!this.closed && !this.bye) {
      this.conn.value = fails >= 3 ? 'door not answering' : fails ? 'reconnecting' : 'connecting';
      this.gotHello = false;
      this.lagged = false;
      this.abort = new AbortController();
      let res;
      try {
        res = await stream(apiUrl(this.sid, 'events'), auth(), (f) => batch(() => this.frame(f)), this.abort.signal);
      } catch {
        res = { status: 0 };
      }
      if (this.closed || this.bye) break;
      // The stream ended with no goodbye (the door died, or it will be
      // reopened): what runs is unknown until a turn-state says, the same `?`
      // as a page that connected mid-turn, never a stale RUN.
      this.setRunning(null);
      if (res.status === 401) {
        this.conn.value = 'refused';
        this.banner.value = { text: 'the door refused the token. Open Minerva again from the launcher.', err: true };
        return;
      }
      this.live = false;
      if (this.lagged) { fails = 0; continue; }
      fails = this.gotHello ? 1 : fails + 1;
      this.conn.value = fails >= 3 ? 'door not answering' : 'reconnecting';
      await sleep(Math.min(8000, 250 * 2 ** fails));
    }
  }

  reconnect() { if (this.abort) this.abort.abort(); }

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
        this.stick.value++;
        break;
      }
      case 'turn-state':
        this.setRunning(!!f.running);
        if (!f.running) this.partial = null; // a turn that ended owes no assistant-message
        break;
      case 'lagged':
        this.lagged = true;
        this.hooks.toast(`stream lagged (${f.dropped} dropped), rebuilding`);
        break;
      case 'goodbye':
        this.bye = true;
        this.conn.value = 'goodbye';
        clearInterval(this.timer);
        this.banner.value = { text: (f.text || 'the session said goodbye') + '\n(resume needs the hub)' };
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
      busy: signal(false), err: signal(''), done: signal(null),
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
    if (c) c.ask.value = a;
    else this.addRow({ kind: 'ask', ask: a });
  }

  askSettled(f) {
    const a = this.asks.value.find((x) => x.id === f.ask_id);
    if (a) this.dropAsk(a, f.how === 'cancelled' ? { how: 'cancelled' } : { how: 'answered', answer: f.answer });
  }

  // A settled ask leaves the queue but stays in the transcript, folded to
  // what was picked (panes/session.js Settled), so its choices can still be
  // read. The note is known only when this page sent it.
  // done: { how: 'answered' | 'cancelled' | 'elsewhere', answer?, note? }
  dropAsk(a, done) {
    this.asks.value = this.asks.value.filter((x) => x !== a);
    const sent = a.sent && a.sent.answer === done.answer ? a.sent : {};
    a.done.value = { ...done, note: sent.note };
    if (a.call) { a.call.ask.value = null; a.call.settled.value = a; }
  }

  // reply: { answer, note? }. A door that turns the note down (400/422, a
  // door without the pending eidolon PR) is remembered, so the next try goes
  // without it. Resolves to the status (0 = not sent).
  async answer(a, reply) {
    if (a.busy.value || !this.asks.value.includes(a)) return 0;
    a.busy.value = true;
    a.err.value = '';
    a.sent = reply; // set first: the ask-settled frame can beat the POST's own answer
    const r = await post(this.sid, 'answer', { ask_id: a.id, ...reply });
    if (r.status === 204) return 204; // ask-settled draws the rest
    a.sent = null;
    if (r.status === 409) {
      if (this.asks.value.includes(a)) this.dropAsk(a, { how: 'elsewhere' });
      this.hooks.toast('that ask was answered elsewhere');
      return 409;
    }
    a.busy.value = false;
    const why = `${r.status || 'network'}: ${(r.data && r.data.error) || 'not accepted'}`;
    if ((r.status === 400 || r.status === 422) && reply.note != null) {
      this.noNote.value = true;
      a.err.value = `the door did not take the note (${why}); choose again to send without it`;
    } else a.err.value = why;
    return r.status;
  }

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
    const bits = [String(f.stop_reason || 'settled').replace(/-/g, ' ')];
    if (u.input_tokens || u.output_tokens) bits.push(`${kfmt(u.input_tokens + (u.cache_read_input_tokens || 0))} in`, `${kfmt(u.output_tokens)} out`);
    if (f.timing) bits.push((f.timing.total_ms / 1000).toFixed(1) + 's');
    this.addRow({ kind: 'settle', text: bits.join(' · ') });
    this.lastWho = null;
  }

  // ---------------------------------------------------------------- verbs
  // how: 'send' | 'steer' | 'queue'. A steer is never sent to a session known
  // to be idle (it would sit unread, core agent.rs:430); `queue` is a send,
  // which the door queues behind a running turn.
  async submit(how, text) {
    if (!text.trim() || this.sending) return false;
    if (this.bye) { this.hooks.toast('this session said goodbye', true); return false; }
    const mode = how === 'steer' && this.running.value !== false ? 'steer' : 'send';
    this.sending = true;
    const r = await post(this.sid, 'say', { text, mode });
    this.sending = false;
    if (r.status === 202) {
      if (r.data && r.data.queued) this.hooks.toast('queued behind the running turn');
      else if (mode === 'steer') this.hooks.toast('steer sent; it lands at the next safe point');
      return true;
    }
    this.hooks.toast(`say failed: ${r.status || 'network'} ${(r.data && r.data.error) || ''}`, true);
    return false;
  }

  async cancel() {
    const r = await post(this.sid, 'cancel');
    if (r.status === 204) this.hooks.toast('cancelling the turn');
    else if (r.status === 409) this.hooks.toast('nothing is running');
    else this.hooks.toast(`cancel failed: ${r.status || 'network'}`, true);
  }
}
