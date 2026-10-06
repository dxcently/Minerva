// The start screen: what the tiles show when no pane is open (perch mode), and
// what a first visit lands on. Bracketed text actions, visible and focusable
// in order (Tab), activated with Enter or a click: buttons in function, TUI in
// look (web-ui.md 2.3, the 2026-10-05 ruling). `[ new chat ]` has focus at boot,
// so Enter alone starts a chat. The recent chats and the projects come from the
// perch (core/hub.js, core/projects.js); this file holds no session state and
// starts nothing itself: app.js passes the actions in.
import { html, K } from '../core/ui.js';
import { useRef, useLayoutEffect, signal } from '../core/ui.js';
import { list, live, keptRows, age, label } from '../core/hub.js';
import { projects, projectFor, sorted } from '../core/projects.js';

// What the actions last said, in plain words (a refused start); cleared by the next try.
export const home = { msg: signal(''), busy: signal(false) };

const RECENT = 8;
// Running and saved chats of this server, newest first (the ones held by another app are not ours to open).
export function recent(rs = list.value) {
  return [...live(rs), ...keptRows(rs)].sort((a, b) => (b.mtime || 0) - (a.mtime || 0)).slice(0, RECENT);
}

// The newest chat of a project, if it has one.
const latestOf = (p, rs) => [...live(rs), ...keptRows(rs)].filter((r) => projectFor(r.cwd, [p])).sort((a, b) => (b.mtime || 0) - (a.mtime || 0))[0] || null;

// `chat` starts a chat in a fresh scratch folder, `newProject` opens the folder picker, `project(p)` starts a chat in
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
      <${K} cls="act" off=${busy} title="start a chat (it works in a fresh scratch folder)" on=${() => act.chat()}>[ new chat ]</${K}>
      <${K} cls="act" off=${busy} title="pick a folder and make it a project" on=${() => act.newProject()}>[ new project… ]</${K}>
    </div>
    <div class=${'msg' + (busy ? ' faint' : '')} aria-live="polite">${busy ? 'starting…' : msg}</div>
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
      <span class="faint ra">${age(r.mtime)}</span></div>`)}</div>`
      : html`<div class="faint">none yet</div>`}
    <div class="faint hintrow">ctrl+enter new-pane picker · ? keys · : commands</div>
  </div>`;
}
