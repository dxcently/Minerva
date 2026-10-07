// The inspector's loaded tab: what the chat's door has loaded, as far as it says. app.js lists it in INSPECTOR; it gets { s, sid } like every tab.
import { html, K, signal, useMemo } from '../core/ui.js';
import { Needs, Soon, Kv } from '../core/bits.js';
import { modeOf } from '../core/hub.js';
import { tildify } from '../core/state.js';

// What this door has loaded, as far as it says: the hello frame names the
// model and the gate (yolo); the tools are the ones called so far (no door
// route lists them), MCP tools grouped by server when named `mcp__<server>__<tool>`.
// The rest is listed dim with what it needs, not hidden.
export function LoadedTab({ s }) {
  const hl = s.hello.value, open = useMemo(() => signal(false), []);
  if (!hl) return html`<div class="faint">waiting for the chat to start</div>`;
  const tools = [...s.tools.value].sort((a, b) => a[0].localeCompare(b[0]));
  const m = String(hl.model || '?'), at = m.search(/[/:]/);
  const mcp = new Map();
  for (const [t, n] of tools) {
    const x = /^mcp__(.+?)__(.+)$/.exec(t);
    if (x) { if (!mcp.has(x[1])) mcp.set(x[1], []); mcp.get(x[1]).push([x[2], n]); }
  }
  const plain = tools.filter(([t]) => !t.startsWith('mcp__'));
  const count = ([t, n]) => html`<div class="kv"><span class="hi">${t}</span><span class="grow"></span><span class="faint">${n}×</span></div>`;
  const per = hl.persona;
  return html`
    <div class="hdr">persona</div>
    ${per ? html`<div class="m b">${per.name || per}</div>${per.text && html`<${K} cls="faint" on=${() => { open.value = !open.value; }}>${open.value ? '▾ ' : '▸ '}excerpt</${K}>${open.value ? html`<div class="pre">${per.text}</div>` : html`<div class="faint">${String(per.text).slice(0, 120)}…</div>`}`}`
      : html`<${Soon} what="the persona" door="the persona needs the hub (the door's hello does not name it)" />`}
    <div class="hdr">model</div>
    <${Kv} k="model    " v=${at > 0 ? m.slice(at + 1) : m} cls="c" />
    ${at > 0 ? html`<${Kv} k="provider " v=${m.slice(0, at)} />` : html`<${Needs}>provider: not in the model key</${Needs}>`}
    <${Soon} what="effort" door="effort needs the hub" />
    <div class="hdr">gate</div>
    <${Kv} k="mode     " v=${({ yolo: 'yolo: the gate answers for you', manual: 'manual: it asks before anything that changes things', auto: 'auto: the stock rules, it asks for risky commands' })[modeOf(s.sid, hl)] || 'gated: it asks when policy says'} cls=${modeOf(s.sid, hl) === 'yolo' ? 'r' : modeOf(s.sid, hl) === 'manual' ? 'o' : ''} />
    <${Kv} k="verdicts " v=${s.verdicts.value.length} />
    <${Soon} what="the policy" door="the policy itself needs the hub" />
    <div class="hdr">tools used</div>
    ${plain.length ? plain.map(count) : html`<div class="faint">none called yet</div>`}
    <${Soon} what="the full tool list" door="the full tool list needs the hub (the door lists none)" />
    <div class="hdr">mcp servers</div>
    ${mcp.size ? [...mcp].map(([srv, ts]) => html`<div class="kv"><span class="m">${srv}</span></div>${ts.map(([t, n]) => html`<div class="kv"><span class="faint">  </span><span class="hi">${t}</span><span class="grow"></span><span class="faint">${n}×</span></div>`)}`)
      : html`<${Soon} what="MCP servers" door="MCP servers need the hub (none called yet)" />`}
    <div class="hdr">extensions</div><${Soon} what="extensions" door="extensions need the hub" />
    <div class="hdr">session</div>
    <${Kv} k="file     " v=${tildify(hl.session)} />
    <${Kv} k="cwd      " v=${tildify(hl.cwd)} />
    <${Kv} k="protocol " v=${hl.protocol ?? '?'} />`;
}
