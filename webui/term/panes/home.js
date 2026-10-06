// The start screen: what the tiles show when no pane is open (perch mode), and
// what a first visit lands on. Bracketed text actions, visible and focusable
// in order (Tab), activated with Enter or a click: buttons in function, TUI in
// look (web-ui.md 2.3, the 2026-10-05 ruling). `[ new chat ]` has focus at boot,
// so Enter alone starts a chat. The recent chats come from the perch's list
// (core/hub.js); this file holds no session state and starts nothing itself:
// app.js passes the actions in.
import { html, K, signal, useRef, useLayoutEffect } from '../core/ui.js';
import { list, live, keptRows, age, label } from '../core/hub.js';

// What the actions last said, in plain words (a refused start); cleared by the next try.
export const home = { msg: signal(''), busy: signal(false) };
const asking = signal(false), path = signal('');

const RECENT = 8;
// Running and saved chats of this server, newest first (the ones held by another app are not ours to open).
export function recent(rs = list.value) {
  return [...live(rs), ...keptRows(rs)].sort((a, b) => (b.mtime || 0) - (a.mtime || 0)).slice(0, RECENT);
}

// `new` starts a chat, `folder(path)` one in a folder, `open(row, beside)` shows a recent one.
export function Home({ act }) {
  const ref = useRef(null), field = useRef(null);
  const rows = recent(), busy = home.busy.value, msg = home.msg.value;
  useLayoutEffect(() => {
    const at = document.activeElement;
    if (ref.current && (!at || at === document.body)) { const first = ref.current.querySelector('.k'); if (first) first.focus({ preventScroll: true }); }
  }, []);
  useLayoutEffect(() => { if (asking.value && field.current) field.current.focus(); }, [asking.value]);
  const go = () => {
    const v = path.peek().trim();
    if (!v) return;
    act.folder(v).then((ok) => { if (ok) { asking.value = false; path.value = ''; } });
  };
  const key = (e) => {
    if (e.key === 'Enter') { e.preventDefault(); go(); }
    else if (e.key === 'Escape') { e.preventDefault(); asking.value = false; home.msg.value = ''; }
  };
  return html`<div class="home" ref=${ref}>
    <div class="owl big">${'   ,___,\n   ['}<span class="eyes">O.o</span>${']    '}<span class="name">minerva</span>${'\n   /)__)    '}<span class="faint">Make it, Break it, Hack it.</span>${'\n  ---"-"---'}</div>
    <div class="acts">
      <${K} cls="act" off=${busy} title="start a chat (it works in a scratch folder)" on=${() => act.chat()}>[ new chat ]</${K}>
      <${K} cls="act" off=${busy} title="start a chat in a folder you name" on=${() => { asking.value = true; home.msg.value = ''; }}>[ open folder… ]</${K}>
    </div>
    ${asking.value && html`<div class="folder"><span class="faint">folder </span><input ref=${field} type="text" spellcheck="false" autocomplete="off"
        aria-label="folder" placeholder="/home/you/project or C:\\Users\\you\\project" value=${path.value}
        onInput=${(e) => { path.value = e.currentTarget.value; }} onKeyDown=${key} />
      <${K} cls="act" off=${busy} on=${go}>[ start ]</${K}> <${K} cls="act" on=${() => { asking.value = false; home.msg.value = ''; }}>[ cancel ]</${K}></div>`}
    <div class=${'msg' + (busy ? ' faint' : '')} aria-live="polite">${busy ? 'starting…' : msg}</div>
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
