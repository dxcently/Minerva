// A saved chat, drawn read-only. The perch runs `eidolon log --json` (GET /hub/sessions/<id>/transcript: the
// `log` route's lines with every image's bytes emptied, so pictures never count against the size cap; they load
// one by one from GET /hub/sessions/<id>/image/<record>/<n>, panes/imgs.js) and
// sends eidolon's own lines back: one record per line, `{id, parent, ts_ms, kind}`, where `kind` is a
// variant name (a bare string) or a one-key object (`{UserMessage: {role, content: [...]}}`). Nothing here
// decodes the binary log, and nothing starts a door: this only reads.
//
// The records are mapped to the frames a live door would have sent, and handed to the same Session
// handlers the live stream uses, so a saved chat is drawn by exactly the rows a running one is. A record
// kind this page does not know becomes one plain line instead of failing the chat.
import { batch } from './ui.js';
import { hubText } from './api.js';
import { tildify } from './state.js';

// Kinds that draw nothing in a live chat either (bookkeeping a person never reads).
const SILENT = new Set(['ContextSize', 'BackendSession', 'Excluded', 'Pinned', 'PolicyVerdict', 'TurnBudget', 'TurnPace', 'ModelSpend']);
// A cap on the lines drawn from one log: the rest is said in one line, not rendered.
export const MAX_RECORDS = 20000;

const blocks = (m) => (m && Array.isArray(m.content) ? m.content : []);
const textOf = (m) => blocks(m).filter((b) => b && b.type === 'text').map((b) => String(b.text || '')).join('\n');
const parsed = (v) => { if (typeof v !== 'string') return v; try { return JSON.parse(v); } catch { return v; } };

// One record -> the frames to draw (door frames, or `{type: 'x-line', text}` for a plain line).
export function framesOf(rec) {
  const ts = rec.ts_ms, record = rec.id;
  const k = rec.kind;
  const name = typeof k === 'string' ? k : k && typeof k === 'object' ? Object.keys(k)[0] : '';
  const v = typeof k === 'object' && k ? k[name] : null;
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
    case 'AskUser': return [f('x-line', { text: `asked: ${v && v.prompt}${v && v.answer != null ? ` → ${v.answer}` : ''}`, cls: 'faint' })];
    case 'UserToolCall': return [f('x-line', { text: `you ran ${v && v.name}`, cls: 'faint' })];
    default:
      if (SILENT.has(name)) return [];
      return [f('x-line', { text: `[record #${record}: ${name || 'unknown'}, not shown here]`, cls: 'faint' })];
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
  batch(() => {
    s.reset();
    for (const rec of shown) {
      for (const f of framesOf(rec)) {
        if (f.type === 'x-line') {
          s.real = Number.isFinite(f.ts); s.at = s.real ? f.ts : null;
          s.sys(f.text, null, f.cls || '');
        } else s.frame(f);
      }
    }
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
