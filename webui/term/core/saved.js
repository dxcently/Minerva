// A saved chat, drawn read-only. The perch runs `eidolon log --json` (GET /hub/sessions/<id>/transcript: the
// `log` route's lines with every image's bytes emptied, so pictures never count against the size cap; they load
// one by one from GET /hub/sessions/<id>/image/<record>/<n>, panes/imgs.js) and
// sends eidolon's own lines back: one record per line, `{id, parent, ts_ms, kind}`, where `kind` is a
// variant name (a bare string) or a one-key object (`{UserMessage: {role, content: [...]}}`). Nothing here
// decodes the binary log, and nothing starts a door: this only reads.
//
// The records are mapped to the frames a live door would have sent, and handed to the same Session
// handlers the live stream uses, so a saved chat is drawn by exactly the rows a running one is. A record
// kind this page does not know becomes one plain line instead of failing the chat; a record with no kind
// to name (no `kind`, or an empty one) draws nothing.
//
// A workflow run (jev's wiki-hop is one) is in the log as WorkflowStart, a WorkflowStep per tool the program
// called (its `input` is a JSON string, so it is parsed once more) with its ToolResult (`tool_use_id` is the
// step's `call_id`), a park as a jev_mark and an AskUser whose answer is a second AskUser with the same
// `call_id`, and WorkflowEnd. They draw as a start line, tool rows (origin `script`, drawn by renderers/jev.js
// for jev's own tools), an ask row that holds its answer, and an end line.
import { batch } from './ui.js';
import { hubText } from './api.js';
import { tildify } from './state.js';
import { clean } from '../renderers/jev.js';

// Kinds that draw nothing in a live chat either (bookkeeping a person never reads). A step's PolicyVerdict and ApiRequest (the service it
// called) belong to the step's tool row; WorkflowBudget is the run's counter.
const SILENT = new Set(['ContextSize', 'BackendSession', 'Excluded', 'Pinned', 'PolicyVerdict', 'TurnBudget', 'TurnPace', 'ModelSpend', 'ApiRequest', 'WorkflowBudget']);
// A cap on the lines drawn from one log: the rest is said in one line, not rendered.
export const MAX_RECORDS = 20000;

const blocks = (m) => (m && Array.isArray(m.content) ? m.content : []);
const textOf = (m) => blocks(m).filter((b) => b && b.type === 'text').map((b) => String(b.text || '')).join('\n');
const parsed = (v) => { if (typeof v !== 'string') return v; try { return JSON.parse(v); } catch { return v; } };

// ---------------------------------------------------------------- workflow runs
// What one drawing keeps between records: the answers of parks (an answer is a later AskUser than the question), the asks already drawn, and
// per run its name and the last mark jev_mark made. `framesOf(rec)` alone (no state) draws a record on its own terms.
export const newState = () => ({ answers: new Map(), hosted: new Set(), runs: new Map(), lastAsk: null });

// The state for a list of records: every park's answer, so the question's row (the first AskUser of its call_id) can hold it. The last answer wins.
export function prepare(recs) {
  const st = newState();
  for (const rec of recs) {
    const a = rec && rec.kind && typeof rec.kind === 'object' ? rec.kind.AskUser : null;
    if (a && a.call_id != null && a.answer != null) st.answers.set(String(a.call_id), String(a.answer));
  }
  return st;
}

// A park's prompt (interp.rn:406-449): lines of why and what is asked, one `  <index>  <p>  <label>` line per option (the indexes dense from 0),
// then the instruction. The head is what is above the menu, the options what is in it; a prompt with no menu has no options.
const OPTION = /^ {2}(\d+) {2}(-?\d+(?:\.\d+)?) {2}(.*)$/;
export function parkOf(prompt) {
  const head = [], options = [];
  for (const line of String(prompt ?? '').split('\n')) {
    const m = OPTION.exec(line);
    if (m && Number(m[1]) === options.length) options.push({ index: m[1], p: m[2], label: m[3] });
    else if (!options.length && line.trim()) head.push(line.trim());
  }
  return { head, options };
}

const optionLabel = (o) => `${o.index}  ${clean(o.label, 56)}`;

// The ask and its settling, drawn as the door draws a question a person answered. The answer is the option it names (by index or exact label), or
// what was typed: `stop`, or something that is no option (the interpreter parks again on it), or nothing yet (the log ends parked).
function askFrames(f, rec, v, st) {
  const id = v.call_id != null ? String(v.call_id) : null;
  if (id != null) {
    if (st.hosted.has(id)) return []; // the answer's own record: the row of the question holds it
    st.hosted.add(id);
  }
  const { head, options } = parkOf(v.prompt);
  const given = id != null && st.answers.has(id) ? st.answers.get(id) : v.answer;
  const typed = given == null ? null : String(given).trim();
  let said;
  if (typed == null) said = '(no answer in this log)';
  else {
    const at = options.findIndex((o) => o.index === typed || o.label === typed);
    said = at >= 0 ? optionLabel(options[at]) : options.length && !/^stop\b/.test(typed) ? `${clean(typed, 40)} (not an option)` : clean(typed, 200) || typed;
  }
  const ask_id = id != null ? id : `record-${rec.id}`;
  st.lastAsk = { answered: typed != null }; // the end line of the run that parked on it says whether it went on
  return [
    f('ask', { ask_id, kind: 'question', prompt: options.length ? head.map((l) => clean(l, 120)).join(' · ') : String(v.prompt ?? ''), call_id: id,
      options: options.map((o) => ({ label: optionLabel(o), description: o.p })) }),
    f('ask-settled', { ask_id, how: 'answered', answer: said }),
  ];
}

// "arrived", "failed", "exhausted"...: how the run's program ended, from its last mark (a `final` names the state, an `end` the outcome).
function endedAs(run) {
  const m = run && run.mark;
  if (!m) return '';
  return m.kind === 'end' ? `ended${m.outcome ? ': ' + clean(m.outcome, 24) : ''}` : m.kind === 'final' ? clean(m.state, 40) : '';
}

// `answered`: the park this ends was answered later in the log (its ask row above says how), so the run went on.
function endLine(run, status, answered) {
  const who = run ? run.name : 'workflow', how = endedAs(run);
  const failedFinal = run && run.mark && run.mark.kind === 'final' && String(run.mark.state).split('.').pop() === 'failed';
  const st = status.toLowerCase();
  if (st === 'completed' && failedFinal) return { text: `■ ${who} · failed: it reached its failed state (the run itself completed)`, cls: 'err' };
  if (st === 'completed') return { text: `■ ${who} · completed${how ? ' · ' + how : ''}`, cls: 'faint' };
  if (st === 'parked') return { text: `■ ${who} · parked, ${answered ? 'then resumed with the answer' : 'waiting for an answer'}`, cls: 'faint' };
  if (st === 'failed') return { text: `■ ${who} · failed${how ? ' · ' + how : ''}`, cls: 'err' };
  return { text: `■ ${who} · ${clean(status, 24) || 'ended'}`, cls: 'faint' };
}

// One record -> the frames to draw (door frames, or `{type: 'x-line', text}` for a plain line). `st` is the drawing's state (prepare()).
export function framesOf(rec, st = newState()) {
  const ts = rec.ts_ms, record = rec.id;
  const k = rec.kind;
  const name = typeof k === 'string' ? k : k && typeof k === 'object' ? Object.keys(k)[0] || '' : '';
  const v = (typeof k === 'object' && k ? k[name] : null) ?? {};
  const f = (type, o) => ({ type, ts, record, ...o });
  switch (name) {
    case 'SessionStart': return [f('x-line', { text: `started · ${v && v.model || '?'} · ${tildify(v && v.cwd)}`, cls: 'faint' })];
    case 'UserMessage': {
      const text = textOf(v), pics = blocks(v).filter((b) => b && b.type === 'image'), images = pics.length;
      const names = pics.map((b) => (typeof b.alt === 'string' ? b.alt : null));
      // A user message that answers tool calls and carries images is where a tool's images ride: they belong
      // under that call's row (the same rule perch/src/images.rs lists them by).
      const answered = blocks(v).filter((b) => b && b.type === 'tool_result').map((b) => b.tool_use_id).pop();
      if (images && answered) return [f('tool-images', { id: answered, images, names }), ...(text ? [f('user-message', { text, images: 0 })] : [])];
      return text || images ? [f('user-message', { text, images, names })] : [];
    }
    case 'AssistantMessage': {
      const out = [f('assistant-message', {
        text: textOf(v),
        thinking: blocks(v).filter((b) => b && b.type === 'thinking').map((b) => String(b.thinking || '')).join('\n'),
        redacted: blocks(v).filter((b) => b && b.type === 'redacted_thinking').length,
      })];
      for (const b of blocks(v)) if (b && b.type === 'tool_use') out.push(f('tool-call-started', { record: undefined, id: b.id, name: b.name, input: parsed(b.input), origin: 'model' }));
      return out;
    }
    case 'ToolResult': return [f('tool-call-finished', { id: v.tool_use_id, name: '', output: v.content, is_error: !!v.is_error })];
    case 'TurnSettled': return [f('turn-settled', { stop_reason: String(v.stop_reason || 'settled').replace(/_/g, '-'), usage: v.usage || {} })];
    case 'Compacted': return [f('compacted', { summary: v.summary, replaced_messages: v.replaced_messages })];
    case 'PeerMessage': return [f('peer-message', { from: v.from, from_cwd: v.from_cwd, channel: v.channel, text: v.text, external: false })];
    case 'CommandResults': return [f('command-results', { lines: v.lines || [] })];
    case 'Cancelled': return [f('cancelled', { calls: 0 })];
    case 'ModelChanged': return [f('x-line', { text: `model: ${v && v.model}`, cls: 'faint' })];
    case 'Note': case 'SessionNote': return [f('x-line', { text: `note: ${v && v.text}`, cls: 'faint' })];
    case 'AskUser': return askFrames(f, rec, v, st);
    case 'UserToolCall': return [f('x-line', { text: `you ran ${v && v.name}`, cls: 'faint' })];
    case 'WorkflowStart': {
      const args = parsed(v.args);
      const given = args && typeof args === 'object' && !Array.isArray(args) ? Object.entries(args).map(([a, b]) => `${clean(a, 16)}=${clean(typeof b === 'string' ? b : JSON.stringify(b), 40)}`) : args != null ? [clean(String(args), 60)] : [];
      const who = `${clean(v.plugin, 24)} ${clean(v.workflow, 32)}`.trim() || 'workflow';
      st.runs.set(record, { name: who, mark: null });
      return [f('x-line', { text: `▶ ${who}${given.length ? ' · ' + given.join(' ') : ''}`, cls: 'faint' })];
    }
    case 'WorkflowStep': {
      const input = parsed(v.input);
      const run = st.runs.get(v.run);
      if (run && v.tool === 'jev_mark' && input && typeof input === 'object') run.mark = { kind: input.kind, state: input.state, outcome: input.outcome };
      return [f('tool-call-started', { id: v.call_id, name: v.tool, input, origin: 'script' })];
    }
    case 'WorkflowEnd': {
      const status = typeof v.status === 'string' ? v.status : v.status && typeof v.status === 'object' ? Object.keys(v.status)[0] || '' : '';
      return [f('x-line', endLine(st.runs.get(v.run), status, !!st.lastAsk && st.lastAsk.answered))];
    }
    default:
      if (SILENT.has(name) || !name) return [];
      return [f('x-line', { text: `[record${record != null ? ' #' + record : ''}: ${clean(name, 40)}, not shown here]`, cls: 'faint' })];
  }
}

// The log text -> records (a line that is not JSON is skipped; there is nothing else to do with it).
export function recordsOf(text) {
  const out = [];
  for (const line of String(text).split('\n')) {
    if (!line.trim()) continue;
    try { const r = JSON.parse(line); if (r && typeof r === 'object') out.push(r); } catch { /* not a record */ }
  }
  return out;
}

// Draw the text into `s` (a state.js Session) as its transcript. Returns how many records were drawn.
export function draw(s, text) {
  const recs = recordsOf(text);
  const shown = recs.slice(0, MAX_RECORDS);
  const st = prepare(shown);
  batch(() => {
    s.reset();
    for (const rec of shown) {
      // One record this page cannot read is a record left out, not a chat that does not open.
      try {
        for (const f of framesOf(rec, st)) {
          if (f.type === 'x-line') {
            s.real = Number.isFinite(f.ts); s.at = s.real ? f.ts : null;
            s.sys(f.text, null, f.cls || '');
          } else s.frame(f);
        }
      } catch (e) { console.error('saved chat: record', rec.id, 'skipped:', e && e.message); }
    }
    // Nothing runs in a saved chat: a call with no result (a run cut off, or cancelled) is stopped, not a spinner that never ends.
    s.stopOpen();
    if (recs.length > shown.length) { s.real = false; s.at = null; s.sys(`… ${recs.length - shown.length} more records not shown`, null, 'y'); }
    s.flush();
    s.jumpToEnd.value++;
  });
  return shown.length;
}

// Fetch and draw. Resolves to '' when drawn, null when it was overtaken, else the reason in a sentence. Never resumes anything.
export async function load(s) {
  const r = await hubText(`sessions/${encodeURIComponent(s.sid)}/transcript`, 30000);
  // A resume (or a close) began while this loaded: draw nothing over it.
  if (s.closed || s.conn.peek() !== 'kept') return null;
  if (r.status === 200) {
    const n = draw(s, r.data);
    return n ? '' : 'there is nothing in this chat yet';
  }
  if (r.status === 413) return 'this chat is too large to show here';
  if (r.status === 404) return 'this chat is gone';
  if (r.status === 504 || r.status === 0) return 'the log did not load in time';
  return `the log could not be read (${r.status}${r.data && r.data.error ? ': ' + r.data.error : ''})`;
}
