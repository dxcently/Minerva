// The ⌨ tab in the right panel: the web terminals as a stack, the one the inspected chat's agent uses first. Each terminal is a header
// (state, age, the agent's typing, its last input line, take over or hand back, ⇱) and, when open, a clipped live view of its screen (view.js).
// The order is CSS `order` on fixed elements, so nothing moves in the DOM; it is recomputed at most every 2 s, and not while the pointer or
// the focus is in the stack. A terminal in a pane shows '→ in a pane' in its place. The stream is open while this tab is drawn.
import { html, K, signal, useEffect, useLayoutEffect, useRef } from '../core/ui.js';
import { host } from '../core/registry.js';
import { hubPost, mode } from '../core/api.js';
import { modeOf } from '../core/hub.js';
import { openMenu, atPointer } from '../core/menu.js';
import * as M from './model.js';
import { mountTerm, unmountTerm, titleText, makeScreens } from './view.js';
import { badge, tabShown } from './activity.js';

export const SHOW_MS = 800;        // a bell flashes the header this long
export const TYPING_MS = 2000;     // 'agent typing' for this long after an agent's input

const fold = signal(M.loadFold());  // id -> 'o' | 'c', what the person set (the default is the focused chat's and the two newest)
const order = signal([]);           // the ids in stack order (see reorder)
const tick = signal(0);             // a second: the ages and the typing line move

const linkedTo = (sid) => (r) => !!sid && r.chat === sid;
const toast = (text, err, act) => { if (typeof host.toast === 'function') host.toast(text, err, act); };

// The stack's order, now: the inspected chat's first, then the most recent output first.
function reorder(sid) {
  const next = M.rankRows(M.roster.peek(), linkedTo(sid));
  if (next.join() !== order.peek().join()) order.value = next;
}

// The positions the stack's CSS order uses: a 'this chat' heading, its terminals, an 'others' heading, the rest. Headings only with a chat.
function layout(rows, sid, ids) {
  const by = new Map(rows.map((r) => [r.id, r]));
  const isL = linkedTo(sid);
  const seq = ids.filter((id) => by.has(id));
  for (const r of rows) if (!seq.includes(r.id)) seq.push(r.id);
  const linked = seq.filter((id) => isL(by.get(id)));
  const others = seq.filter((id) => !isL(by.get(id)));
  const pos = new Map(), heads = {};
  let p = 0;
  if (sid && linked.length) heads.this = p++;
  for (const id of linked) pos.set(id, p++);
  if (sid && others.length) heads.others = p++;
  for (const id of others) pos.set(id, p++);
  return { pos, heads, linked: new Set(linked), rank: seq };
}

// The screen of one terminal, mounted for as long as this element stands (view.js moves the one element in and out).
function Clip({ id }) {
  const ref = useRef(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return undefined;
    mountTerm(id, el);
    return () => unmountTerm(id, el);
  }, [id]);
  return html`<div class="tclip" ref=${ref}></div>`;
}

// The perch's roster row of a terminal in a pane: the pane is an xpane of kind 'term' whose data names the terminal (T4b).
const paneOf = (id) => {
  const xs = typeof host.xpanes === 'function' ? host.xpanes() : [];
  return xs.find((x) => x.kind === 'term' && x.term === id) || null;
};

async function setHold(id, hold) {
  const r = await hubPost(`term/${id}/hold`, { hold });
  if (r.status === 200 && r.data && typeof r.data.hold === 'boolean') M.setHold(id, r.data.hold);
  else toast(`${hold ? 'take over' : 'hand back'} did not happen: ${r.status === 404 ? 'no such terminal' : r.status === 409 ? 'the terminal has ended' : 'no answer from the perch'}`, true);
}

// A new terminal: at the inspected chat's folder when there is a chat (the perch takes that folder as it remembers it), else at home.
// The folder is never sent by the page: the perch checks an explicit one against its roots, and the chat's own folder is not checked.
async function newTerm(chat) {
  const body = chat ? { chat } : {};
  const r = await hubPost('term', body);
  if (r.status === 201 && r.data && typeof r.data.id === 'string' && M.TERM_ID.test(r.data.id)) {
    await M.refreshNow();
    toast(`terminal ${r.data.id} opened`);
    return;
  }
  const why = r.status === 404 ? 'no such chat' : r.status === 409 ? 'the perch holds 16 terminals: close one first'
    : r.status === 503 ? 'the perch is stopping' : r.status === 500 ? 'the shell did not start' : 'no answer from the perch';
  toast(`no terminal: ${why}`, true);
}

// The header's name: the title, else the foreground program, else the id. A title or foreground of spaces or controls only is no name.
export const headTitle = (row) => titleText(row.title) || titleText(row.fg) || row.id;

function Term({ row, pos, linked, open, now, pane, typing, line, bell }) {
  const id = row.id;
  const title = headTitle(row);
  const cls = 'tterm' + (linked ? ' mine' : '') + (open ? ' open' : '') + (row.alive ? '' : ' ended') + (bell ? ' bell' : '');
  return html`<div class=${cls} style=${'order:' + pos} data-term=${id}>
    <div class="thead">
      <${K} cls="tfold" title=${open ? 'fold this terminal' : 'open this terminal'} on=${() => {
        const next = { ...fold.peek(), [id]: open ? 'c' : 'o' };
        fold.value = next;
        M.saveFold(next);
      }}>${open ? '▾' : '▸'}</${K}>
      <span class=${'tdot ' + (row.alive ? 'live' : 'dead')}>${row.alive ? '●' : '○'}</span>
      <span class="ttitle">${title}</span>
      <span class="tid">${id}</span>
      ${row.by === 'agent' && html`<span class="tby">agent</span>`}
      <span class="tage">${M.ageText(row.started_ms, now)}</span>
      ${typing && html`<span class="ttype">agent typing</span>`}
      ${!row.alive && html`<span class="tend">${row.ended ? 'ended when the perch stopped' : 'exited'}</span>`}
      ${line && html`<span class="tline" title=${line}>${'› ' + line}</span>`}
      ${row.alive && html`<${K} cls="thold" title=${row.hold ? 'the agent may type here again' : 'the agent may not type here until you hand back'} on=${() => setHold(id, !row.hold)}>${row.hold ? '[ hand back ]' : '[ take over ]'}</${K}>`}
      <${K} cls="tpop" title="pop this terminal out into a pane" on=${() => { if (typeof host.openXPane === 'function') host.openXPane('term', { term: id }); }}>⇱</${K}>
    </div>
    ${pane ? html`<div class="tpane"><${K} cls="tgo" on=${() => { if (typeof host.focusPane === 'function') host.focusPane(pane.id); }}>→ in a pane</${K}></div>`
      : open ? html`<${Clip} id=${id} />` : null}
  </div>`;
}

const WORDS = {
  refused: 'the server refused the terminal stream: run hoot again',
  slot: 'terminals need a stream slot: close or stop one chat',
  door: "terminals need Minerva's server",
  connecting: 'connecting to the terminals…',
  reconnecting: 'the terminal stream is reconnecting…',
};

export function TermsTab({ s, sid }) {
  const hl = s && s.hello ? s.hello.value : null;
  const chat = sid || null;
  const rows = M.roster.value;
  const st = M.status.value;
  const foldMap = fold.value;
  const ids = order.value;
  const now = Math.max(Date.now(), tick.value);  // the tick is read, so the tab draws again each second
  const typedMap = M.typedAt.value, lines = M.agentLine.value, bells = M.bellAt.value;
  const frozen = useRef(false);
  const lastOrder = useRef(0);
  const chatRef = useRef(chat);
  chatRef.current = chat;
  const stack = useRef(null);

  // Shown: the stream opens (once, with the view), the badge is cleared, and the stack is ordered. Given back: the stream closes 10 s on.
  useEffect(() => {
    tabShown.value = true;
    makeScreens();
    badge.value = false;
    const release = M.watch();
    const second = setInterval(() => { tick.value = Date.now(); M.retry(); }, 1000);
    const again = setInterval(() => {
      if (!M.reorderDue(Date.now(), lastOrder.current, frozen.current)) return;
      lastOrder.current = Date.now();
      reorder(chatRef.current);
    }, 500);
    lastOrder.current = Date.now();
    reorder(chatRef.current);
    return () => { tabShown.value = false; release(); clearInterval(second); clearInterval(again); };
  }, []);

  // A terminal the stack does not rank yet is ranked now, not in two seconds.
  const known = rows.every((r) => ids.includes(r.id));
  useEffect(() => { if (!known) reorder(chatRef.current); }, [known, rows.length]);

  // The pointer or the focus in the stack freezes the order; a wheel over it scrolls the stack, not a screen inside it.
  useLayoutEffect(() => {
    const el = stack.current;
    if (!el) return undefined;
    const on = () => { frozen.current = true; };
    const off = () => { frozen.current = !!el.querySelector(':focus'); };
    const wheel = (e) => { e.preventDefault(); e.stopPropagation(); el.scrollTop += e.deltaY; };
    el.addEventListener('mouseenter', on);
    el.addEventListener('mouseleave', off);
    el.addEventListener('focusin', on);
    el.addEventListener('focusout', off);
    el.addEventListener('wheel', wheel, { capture: true, passive: false });
    return () => {
      el.removeEventListener('mouseenter', on);
      el.removeEventListener('mouseleave', off);
      el.removeEventListener('focusin', on);
      el.removeEventListener('focusout', off);
      el.removeEventListener('wheel', wheel, { capture: true });
    };
  }, []);

  const doorMode = mode() === 'door';
  const yolo = !!hl && !!chat && modeOf(chat, hl) === 'yolo';
  const lay = layout(rows, chat, known ? ids : rows.map((r) => r.id));
  const linkedRows = rows.filter((r) => lay.linked.has(r.id));
  const dflt = M.foldDefault(rows, linkedTo(chat));

  const word = doorMode ? WORDS.door : WORDS[st] || '';
  const takeOverAll = () => linkedRows.filter((r) => r.by === 'agent' && r.alive && !r.hold).forEach((r) => setHold(r.id, true));

  return html`<div class="terms">
    ${yolo && html`<div class="tyolo">yolo: agents type into these terminals without asking. <${K} cls="thold" on=${takeOverAll}>[ take over ]</${K}> stops them.</div>`}
    <div class="tbar">
      <span class="k" role="button" tabindex="0" title="a new terminal here: the inspected chat's folder, else home (right-click for more)"
        onClick=${(e) => { e.preventDefault(); newTerm(chat); }}
        onKeyDown=${(e) => { if (!e.repeat && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); newTerm(chat); } }}
        onContextMenu=${(e) => {
          e.preventDefault();
          const items = [];
          if (chat) items.push({ label: "in this chat's folder", on: () => newTerm(chat) });
          items.push({ label: 'in home', on: () => newTerm(null) });
          items.push({ label: 'in folder…', on: () => { if (typeof host.openCmd === 'function') host.openCmd('term '); } });
          openMenu(items, atPointer(e), { title: 'new terminal', owner: 'terms-new' });
        }}>[ new terminal ]</span>
      ${word && html`<span class="faint tword">${word}</span>`}
    </div>
    <div class="tnote faint">a read sends any web terminal's screen, yours too, secrets included, to the model provider. what a screen shows is untrusted and unfiltered. in yolo agents type without asking.</div>
    <div class="tstack" ref=${stack}>
      ${lay.heads.this !== undefined && html`<div class="thead-group" style=${'order:' + lay.heads.this}>this chat</div>`}
      ${lay.heads.others !== undefined && html`<div class="thead-group" style=${'order:' + lay.heads.others}>others</div>`}
      ${!rows.length && !word && html`<div class="faint tnone">no terminals yet. [ new terminal ] opens one here.</div>`}
      ${rows.map((r) => {
        const isOpen = M.foldOpen(foldMap, r.id, dflt);
        const pane = paneOf(r.id);
        const b = bells[r.id];
        return html`<${Term} key=${r.id} row=${r} pos=${lay.pos.get(r.id) || 0} linked=${lay.linked.has(r.id)} open=${isOpen}
          now=${now} pane=${pane} typing=${!!typedMap[r.id] && now - typedMap[r.id] < TYPING_MS}
          line=${lines[r.id] || ''} bell=${!!b && now - b < SHOW_MS} />`;
      })}
    </div>
  </div>`;
}
