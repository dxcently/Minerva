// The start screen: what the tiles show when no pane is open (perch mode), and
// what a first visit lands on. Bracketed text actions, visible and focusable
// in order (Tab), activated with Enter or a click: buttons in function, TUI in
// look (web-ui.md 2.3, the 2026-10-05 ruling). `[ new chat ]` has focus at boot,
// so Enter alone starts a chat. The recent chats and the projects come from the
// perch (core/hub.js, core/projects.js); this file holds no session state.
// app.js passes the actions in, except in the install block (InstallBlock):
// that reads GET /hub/install through core/lane.js on its own, and opens the
// update keys overlay.
import { html, K } from '../core/ui.js';
import { useRef, useLayoutEffect, useState, useEffect, signal } from '../core/ui.js';
import { list, live, keptRows, age, label } from '../core/hub.js';
import { projects, projectFor, sorted } from '../core/projects.js';
import { hubPoll } from '../core/lane.js';
import { openOverlay } from '../core/overlay.js';
import { KeysPanel } from '../install/keys.js';

// What the actions last said, in plain words (a refused start); cleared by the next try.
export const home = { msg: signal(''), busy: signal(false) };

const RECENT = 8;
// Running and saved chats of this server, pinned ones first, then newest first (`other`, the trash and the ones held by
// another app are not ours to list).
export function recent(rs = list.value) {
  return [...live(rs), ...keptRows(rs)].sort((a, b) => (!!b.pinned - !!a.pinned) || (b.mtime || 0) - (a.mtime || 0)).slice(0, RECENT);
}

// The newest chat of a project, if it has one.
const latestOf = (p, rs) => [...live(rs), ...keptRows(rs)].filter((r) => projectFor(r.cwd, [p])).sort((a, b) => (b.mtime || 0) - (a.mtime || 0))[0] || null;

// ---------------------------------------------------------------- the install block (W7)
// What `GET /hub/install` says about this Minerva: its version, the notice to update (the text is minerva's, shown whole: it names
// `hoot update`, which a person runs in a terminal; nothing in this page updates), the plugins it installed and the jev. It is read when
// the start screen draws, through the request lane (a GET the page makes on its own). `[ update keys ]` is offered only with an installed
// Minerva: the keys routes are a 404 without one.
const str = (x) => (typeof x === 'string' ? x : '');

// The jev's state, in the words the start screen uses.
export function jevWords(j) {
  if (!j || typeof j !== 'object') return 'not known';
  if (j.running) return Number.isInteger(j.pid) ? `running (pid ${j.pid})` : 'running';
  return j.set_up ? 'set up, not running' : 'not set up';
}

// The lines of the block for an answer of `GET /hub/install`: { text, warn?, faint? }. Every text is shown as text, never as markup.
export function installLines(res) {
  const status = res ? res.status : 0;
  const d = res ? res.data : null;
  if (status === 0) return [{ text: 'the server is not answering', faint: true }];
  if (status !== 200 || !d || typeof d !== 'object') return [{ text: str(d && d.why) || 'the install could not be read', faint: true }];
  if (d.installed === true) {
    const out = [{ text: `minerva ${str(d.version)}` }];
    if (d.notice && typeof d.notice === 'object') {
      if (str(d.notice.line)) out.push({ text: d.notice.line, warn: true });
      if (str(d.notice.stale)) out.push({ text: d.notice.stale, faint: true });
    }
    const plugins = (Array.isArray(d.plugins) ? d.plugins : []).filter((p) => p && typeof p === 'object');
    const names = plugins.map((p) => `${str(p.name)} ${str(p.version)} (${str(p.how)})`).join(', ') || 'none';
    out.push({ text: `installed by Minerva: ${names}; trust and service state are not recorded here`, faint: true });
    out.push({ text: `jev: ${jevWords(d.jev)}; the openjev weights are not tracked`, faint: true });
    return out;
  }
  if (d.managed === 'nix') return [{ text: str(d.why) || 'managed by Nix: update through nix', faint: true }];
  if (d.kind === 'updated') return [{ text: str(d.why) || 'the server was updated; start it again', faint: true }];
  return [{ text: str(d.why) || 'no installed Minerva here (development server)', faint: true }];
}

// `[ update keys ]` is offered for an installed Minerva only.
export const keysOffered = (res) => !!res && res.status === 200 && !!res.data && res.data.installed === true;

export function InstallBlock() {
  const [res, setRes] = useState(null);
  useEffect(() => {
    let on = true;
    hubPoll('install').then((r) => { if (on) setRes(r); });
    return () => { on = false; };
  }, []);
  if (!res) return html`<div class="install"><div class="hdr">install</div><div class="faint">checking the install…</div></div>`;
  return html`<div class="install"><div class="hdr">install</div>
    ${installLines(res).map((l) => html`<div class=${l.warn ? 'y' : l.faint ? 'faint' : ''}>${l.text}</div>`)}
    ${keysOffered(res) && html`<div><${K} cls="act" title="the keys minerva trusts for updates: add or remove one here, after a step in a terminal" on=${() => openOverlay(KeysPanel)}>[ update keys ]</${K}></div>`}
  </div>`;
}

// `chat` starts a chat in the inbox (a folder of its own), `newProject` opens the folder picker, `project(p)` starts a chat in
// a project, `open(row, beside)` shows a chat, `forget(p)` asks to drop a project from the list.
export function Home({ act }) {
  const ref = useRef(null);
  const rows = recent(), busy = home.busy.value, msg = home.msg.value, ps = sorted(projects.value), rs = list.value;
  useLayoutEffect(() => {
    const at = document.activeElement;
    if (ref.current && (!at || at === document.body)) { const first = ref.current.querySelector('.k'); if (first) first.focus({ preventScroll: true }); }
  }, []);
  return html`<div class="home" ref=${ref}>
    <div class="owl big">${'   ,___,\n   ['}<span class="eyes">O.o</span>${']    '}<span class="name">minerva</span>${'\n   /)__)    '}<span class="faint">Make it, Break it, Hack it.</span>${'\n  ---"-"---'}</div>
    <div class="acts">
      <${K} cls="act" off=${busy} title="start a chat (it goes in the inbox; its folder is named after its title)" on=${() => act.chat()}>[ new chat ]</${K}>
      <${K} cls="act" off=${busy} title="pick a folder and make it a project" on=${() => act.newProject()}>[ new project… ]</${K}>
    </div>
    <div class=${'msg' + (busy ? ' faint' : '')} aria-live="polite">${busy ? 'starting…' : msg}</div>
    <${InstallBlock} />
    <div class="hdr">projects</div>
    ${ps.length ? html`<div class="recent">${ps.map((p) => {
      const last = latestOf(p, rs);
      return html`<div class="rrow" key=${p.path} onContextMenu=${(e) => act.forget(e, p)}>
        <${K} cls="rn" title=${'new chat in ' + p.path + '\nright-click: remove from the list'} on=${() => act.project(p)}>+ ${p.name}</${K}>
        ${last && html`<${K} cls="faint ra" title=${'open its latest chat: ' + label(last)} on=${(e) => act.open(last, !!(e.ctrlKey || e.metaKey))}>latest ${age(last.mtime)}</${K}>`}</div>`;
    })}</div>` : html`<div class="faint">none yet: [ new project… ] picks a folder</div>`}
    <div class="hdr">recent chats</div>
    ${rows.length ? html`<div class="recent">${rows.map((r) => html`<div class="rrow" key=${r.id}
        onMouseDown=${(e) => e.button === 1 && e.preventDefault()}
        onAuxClick=${(e) => { if (e.button === 1) { e.preventDefault(); act.open(r, true); } }}>
      <${K} cls="rn" title=${r.id + '\n' + (r.state === 'live' ? 'running' : 'saved') + '\nctrl-click or middle-click: open beside'}
        on=${(e) => act.open(r, !!(e.ctrlKey || e.metaKey))}><span class=${r.state === 'live' ? 'hi' : 'faint'}>${r.state === 'live' ? '●' : '○'}</span> ${label(r)}</${K}>
      <span class="faint ra">${r.pinned ? 'pinned · ' : ''}${age(r.mtime)}</span></div>`)}</div>`
      : html`<div class="faint">none yet</div>`}
    <div class="faint hintrow">ctrl+enter new-pane picker · ? keys · : commands</div>
  </div>`;
}
