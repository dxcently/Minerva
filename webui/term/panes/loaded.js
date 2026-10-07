// The inspector's loaded tab: what the chat's door has loaded, as far as anything says. app.js lists it in INSPECTOR, wrapped by its `Loaded`
// (which draws the registered loadedSections under it: this file does not); it gets { s, sid } like every tab.
//
// Two sources: the hello frame and the calls seen so far (the model, the gate, the verdicts, the tools used), and the perch's /hub/meta
// (the full tool list, the policy rows, the guard's status, the plugins, the connectors, the default mode). In door mode there is no perch:
// the parts that need one say `needs the hub`. Everything /hub/meta sends is drawn as text.
import { html, K, signal, useMemo, useEffect } from '../core/ui.js';
import { Needs, Kv } from '../core/bits.js';
import { mode, hubGet } from '../core/api.js';
import { hubPoll } from '../core/lane.js';
import { modeOf } from '../core/hub.js';
import { tildify } from '../core/state.js';

// ---------------------------------------------------------------- the words
export const PERSONA_NOTE = 'the web door does not name a persona';
export const GATE_NOTE = "eidolon's gate is on in Minerva auto (your config turns it off elsewhere)";

// The guard's line from /hub/meta/policy's `guard` ({ on, file, why? }): on names the file, off says why. S0P-a's stub is the off case
// (`why: "not built yet"`); an off with no reason still says so.
export function guardLine(g) {
  if (!g || typeof g !== 'object') return '';
  if (g.on) return g.file ? `guard rows on (${g.file})` : 'guard rows on';
  return `guard rows are off: ${g.why || 'no reason given'}`;
}

// What the guard says besides its status: a member whose config turns eidolon's gate off (`[policy] enabled = false`) gets it back in
// Minerva's generated config, and `why` is the note then (on, with a reason). Any other note on a guard that is on is shown as it is.
export function gateNote(g) {
  if (!g || !g.on || typeof g.why !== 'string' || !g.why) return '';
  return /enabled\s*=\s*false/.test(g.why) ? GATE_NOTE : g.why;
}

// The mode a new chat starts in, and where that came from (/hub/meta/home): the file names it, or nothing does and it is yolo.
export function homeLine(h) {
  if (!h || typeof h.default_mode !== 'string' || !h.default_mode) return '';
  const from = h.source === 'unset' ? `unset → ${h.default_mode}` : h.source || 'unset';
  return `new chats: ${h.default_mode} (${from})`;
}

// eidolon has no MCP client list; the two connectors Minerva's doors may have are the ones /hub/meta/connectors reports.
export function connectorsLine(c) {
  const mark = (on) => (on ? '✓' : '✗');
  return `eidolon has no MCP client list; its connectors: mneme ${mark(c && c.mneme)} melete ${mark(c && c.melete)}`;
}

// eidolon's tool list ([{ name, approval?, plugin?, desc? }], its own order) with the calls this chat has made (a Map or pairs, name -> count).
// The tools called come first, in eidolon's order; then the ones called that the list does not have (a name the door reports alone), by
// name; then every tool not called yet, in eidolon's order. Each row: { name, approval, plugin, desc, n }.
export function mergeTools(list, used) {
  const calls = new Map();
  for (const [name, n] of used instanceof Map ? used : used || []) if (n > 0) calls.set(name, n);
  const rows = (Array.isArray(list) ? list : []).filter((t) => t && typeof t.name === 'string')
    .map((t) => ({ name: t.name, approval: t.approval || '', plugin: t.plugin || '', desc: t.desc || '', n: calls.get(t.name) || 0 }));
  const listed = new Set(rows.map((t) => t.name));
  const alone = [...calls].filter(([name]) => !listed.has(name)).sort((a, b) => a[0].localeCompare(b[0]))
    .map(([name, n]) => ({ name, approval: '', plugin: '', desc: '', n }));
  return [...rows.filter((t) => t.n), ...alone, ...rows.filter((t) => !t.n)];
}

// The sentence for an answer that was not a 200: the perch's own (`{ error }`), else what happened. Status 0: nothing came back.
export function said(r) {
  if (r.status === 0) return 'the perch did not answer';
  if (r.data && typeof r.data.error === 'string' && r.data.error) return r.data.error;
  if (r.status === 401) return "the perch refused this page's login (it restarted?)";
  return `the perch answered ${r.status}`;
}

// ---------------------------------------------------------------- what /hub/meta said
// The last answer by path, kept when the tab closes so a reopened tab draws at once and refreshes; { status, data, at }. A refresh that
// fails keeps the good answer it had: a hiccup must not blank the tab. The diff tab reads its branch through the same store.
const answers = new Map();
const flying = new Set();
export function answer(path) {
  let a = answers.get(path);
  if (!a) answers.set(path, a = signal(null));
  return a;
}

export const FRESH_MS = 30000; // the perch caches tools, plugins and policy rows for 10 minutes; the page asks again after this
export const SLOW_MS = 22000;  // eidolon's own commands: the perch gives up at 20 s and says so, which is worth waiting for
export const QUICK_MS = 10000; // a config read or a file read: a call the perch never answers must not stay "in flight" for ever

// Ask for `path`, unless it is answered and younger than `fresh` ms. Perch mode only. A path already asked for is not asked twice at once.
//
// By default this is a person's action (opening a tab, unfolding a section), which never enters the request lane (core/lane.js): the lane is
// one request in flight across the page, so a cold or stuck eidolon behind meta/tools would freeze the sidebar's polls for as long as it took.
// It is a plain hubGet with a timeout. `lane` true is for the periodic read of a tab that stays open (the diff tab's branch): that one
// waits its turn like every other poll.
export async function pull(path, fresh = 0, timeout, lane = false) {
  if (mode() !== 'perch' || flying.has(path)) return;
  const a = answer(path), had = a.peek();
  if (had && had.status === 200 && Date.now() - had.at < fresh) return;
  flying.add(path);
  try {
    const r = await (lane ? hubPoll(path, timeout) : hubGet(path, timeout || QUICK_MS));
    if (!(had && had.status === 200 && r.status !== 200)) a.value = { status: r.status, data: r.data, at: Date.now() };
  } finally { flying.delete(path); }
}

// What opening the loaded tab asks for: the connectors and the default mode (small config reads, every open), then eidolon's tool list and the
// policy of the chat's mode (cached by the perch, so asked again here only after FRESH_MS). One after the other, so the tab holds a single
// connection at a time beside the lane's: the browser allows six per host and the chats' streams take most of them.
export async function fetchOpen(policy) {
  await pull('meta/connectors');
  await pull('meta/home');
  await pull('meta/tools', FRESH_MS, SLOW_MS);
  await pull(policy, FRESH_MS, SLOW_MS);
}

// `[ show plugins ]`: the plugin listing, asked for when it is first unfolded.
export const fetchPlugins = () => pull('meta/plugins', FRESH_MS, SLOW_MS);

// Draw what `path` answered: a dot while nothing has, the perch's sentence when it failed, else `ok(data)`.
function view(path, what, ok) {
  const r = answer(path).value;
  if (!r) return html`<div class="faint">…</div>`;
  if (r.status !== 200 || r.data == null) return html`<${Needs}>${what}: ${said(r)}</${Needs}>`;
  return ok(r.data);
}

// The guard's lines (they ride on the policy answer): nothing while that is on its way or failed, since the policy part below says so.
function guardRows(r) {
  const g = r && r.status === 200 && r.data ? r.data.guard : null;
  if (!g) return null;
  const note = gateNote(g);
  return html`<div class=${g.on ? 'c' : 'o'}>${guardLine(g)}</div>${note && html`<div class="faint">${note}</div>`}`;
}

// ---------------------------------------------------------------- the tab
const MODE_WORDS = { yolo: 'yolo: the gate answers for you', manual: 'manual: it asks before anything that changes things', auto: 'auto: the stock rules, it asks for risky commands' };

export function LoadedTab({ s }) {
  const hl = s.hello.value, perch = mode() === 'perch';
  const open = useMemo(() => signal(false), []), rules = useMemo(() => signal(false), []), plugs = useMemo(() => signal(false), []);
  const gate = hl ? modeOf(s.sid, hl) : '', policy = 'meta/policy' + (gate ? `?mode=${gate}` : '');
  // At every open (and when the chat's mode becomes known, which changes the policy asked for); see fetchOpen.
  useEffect(() => { if (perch) fetchOpen(policy); }, [perch, policy]);
  if (!hl) return html`<div class="faint">waiting for the chat to start</div>`;
  const m = String(hl.model || '?'), at = m.search(/[/:]/);
  const per = hl.persona;
  const listed = perch ? answer('meta/tools').value : null;
  const full = listed && listed.status === 200 && Array.isArray(listed.data) ? listed.data : null;
  const tools = mergeTools(full, s.tools.value);
  const row = (t) => html`<div class="kv" title=${[t.plugin, t.approval, t.desc].filter(Boolean).join(' · ')}>
    <span class=${'grow ' + (t.n ? 'hi' : 'faint')}>${t.name}</span><span class="faint">${t.n ? t.n + '×' : t.approval}</span></div>`;
  const unfold = (sig, then) => () => { if (!sig.value && then) then(); sig.value = !sig.value; };
  return html`
    <div class="hdr">persona</div>
    ${per ? html`<div class="m b">${per.name || per}</div>${per.text && html`<${K} cls="faint" on=${() => { open.value = !open.value; }}>${open.value ? '▾ ' : '▸ '}excerpt</${K}>${open.value ? html`<div class="pre">${per.text}</div>` : html`<div class="faint">${String(per.text).slice(0, 120)}…</div>`}`}`
      : html`<div class="faint">${PERSONA_NOTE}</div>`}
    <div class="hdr">model</div>
    <${Kv} k="model    " v=${at > 0 ? m.slice(at + 1) : m} cls="c" />
    ${at > 0 ? html`<${Kv} k="provider " v=${m.slice(0, at)} />` : html`<${Needs}>provider: not in the model key</${Needs}>`}
    <div class="hdr">gate</div>
    <${Kv} k="mode     " v=${MODE_WORDS[gate] || 'gated: it asks when policy says'} cls=${gate === 'yolo' ? 'r' : gate === 'manual' ? 'o' : ''} />
    ${perch ? view('meta/home', 'new chats', (h) => html`<div class="faint">${homeLine(h)}</div>`) : html`<${Needs}>the default mode for new chats needs the hub</${Needs}>`}
    ${perch ? guardRows(answer(policy).value) : html`<${Needs}>the guard rows need the hub</${Needs}>`}
    <${Kv} k="verdicts " v=${s.verdicts.value.length} />
    <div class="hdr">policy</div>
    ${perch ? view(policy, 'the policy', (p) => html`<${Kv} k="file     " v=${p.file || 'eidolon names no policy file'} />
      <${K} cls="c" on=${unfold(rules)}>${rules.value ? '[ hide rules ]' : '[ show rules ]'}</${K}>
      ${rules.value && html`<div class="pre">${typeof p.rows === 'string' && p.rows.trim() ? p.rows : 'eidolon printed no rows'}</div>`}`)
      : html`<${Needs}>the policy itself needs the hub</${Needs}>`}
    <div class="hdr">tools</div>
    ${full && html`<div class="faint">${full.length} listed, ${tools.filter((t) => t.n).length} called by this chat</div>`}
    ${tools.length ? tools.map(row) : html`<div class="faint">none called yet</div>`}
    ${!perch ? html`<${Needs}>the full tool list needs the hub (the door lists none)</${Needs}>`
      : !full && listed ? html`<${Needs}>the tool list: ${said(listed)}</${Needs}>` : !listed ? html`<div class="faint">…</div>` : null}
    <div class="hdr">mcp</div>
    ${perch ? view('meta/connectors', 'connectors', (c) => html`<div class="faint">${connectorsLine(c)}</div>`) : html`<${Needs}>the connectors need the hub</${Needs}>`}
    <div class="hdr">plugins</div>
    ${perch ? html`<${K} cls="c" on=${unfold(plugs, fetchPlugins)}>${plugs.value ? '[ hide plugins ]' : '[ show plugins ]'}</${K}>
      ${plugs.value && view('meta/plugins', 'plugins', (p) => html`<div class="pre">${typeof p.text === 'string' && p.text.trim() ? p.text : 'eidolon lists no plugins'}</div>`)}`
      : html`<${Needs}>plugins need the hub</${Needs}>`}
    <div class="hdr">session</div>
    <${Kv} k="file     " v=${tildify(hl.session)} />
    <${Kv} k="cwd      " v=${tildify(hl.cwd)} />
    <${Kv} k="protocol " v=${hl.protocol ?? '?'} />`;
}
