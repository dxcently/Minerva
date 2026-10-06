// The folder picker: a file-manager panel drawn over the tiles (perch mode; web-ui.md 2.3, the 2026-10-06 project
// model). It lists folders only, from `GET /hub/dirs`, starting at the roots the server may open (the Windows profile
// is `C:\Users\<you>`). A breadcrumb, a path bar that takes a pasted `C:\...` or WSL path, clickable folder rows,
// `[ new folder ]` (one inline name field, `POST /hub/dirs`) and `[ use this folder ]`. With `naming`, using a folder
// goes on to one more line: the project's name, prefilled with the folder's.
// With `parent` too (`make it a project…`), the folder picked is where a NEW folder goes, not the project itself: the name line is that
// folder's name (prefilled with `prefill`), it says where the folder will end up, and `doneLabel` words the action.
//
// This file holds no policy: the server decides what may be listed or made, and every refusal is said in a sentence.
// `openPicker({ title, naming, parent?, prefill?, doneLabel?, onDone })`: `onDone({ path, name })` resolves to '' (done: the panel closes) or to a
// sentence (shown; the panel stays).
import { html, K, signal, useRef, useLayoutEffect } from '../core/ui.js';
import { hubGet, hubPost } from '../core/api.js';
import { toWslPath } from '../core/path.js';

export const picker = signal(null); // null, or { title, naming, onDone }
export const openPicker = (o) => { picker.value = o; };
export const closePicker = () => { picker.value = null; };

const cur = signal(null);      // the listing shown: { path, label, parent, crumbs, dirs, truncated }
const roots = signal([]);      // the roots listing's dirs, for `~`
const text = signal('');       // the path bar
const msg = signal('');
const busy = signal(false);
const making = signal(false);  // the new-folder field is open
const folder = signal('');     // its text
const stage = signal('pick');  // 'pick' | 'name'
const chosen = signal('');     // the folder picked, while naming
const name = signal('');

async function load(path) {
  busy.value = true; msg.value = '';
  const r = await hubGet('dirs' + (path ? '?path=' + encodeURIComponent(path) : ''), 8000);
  busy.value = false;
  if (r.status === 200 && r.data && Array.isArray(r.data.dirs)) {
    cur.value = r.data;
    if (!path) roots.value = r.data.dirs;
    text.value = r.data.label || '';
    making.value = false; folder.value = '';
    return true;
  }
  msg.value = r.status === 404 ? 'that is not a folder inside the folders this server may open'
    : r.status === 503 ? 'the server is busy listing folders: try again'
    : r.status === 0 ? 'the server is not answering' : `could not list that folder (${r.status})`;
  return false;
}

// What was typed in the path bar, as an absolute WSL path: `C:\x` and `\\wsl.localhost\...` converted, `~` expanded
// from the home root the server listed.
function typed(raw) {
  const p = toWslPath(raw);
  if (p === '~' || p.startsWith('~/')) {
    const home = roots.value.find((d) => d.name === '~');
    return home ? home.path + p.slice(1) : p;
  }
  return p;
}

export function Picker() {
  const p = picker.value;
  const ref = useRef(null);
  useLayoutEffect(() => {
    if (!p) return;
    stage.value = 'pick'; chosen.value = ''; name.value = ''; msg.value = ''; making.value = false; cur.value = null;
    load('');
    const first = ref.current && ref.current.querySelector('input');
    if (first) first.focus({ preventScroll: true });
  }, [p]);
  useLayoutEffect(() => { if (making.value && ref.current) { const f = ref.current.querySelector('.newf input'); if (f) f.focus(); } }, [making.value]);
  useLayoutEffect(() => { if (stage.value === 'name' && ref.current) { const f = ref.current.querySelector('.namef input'); if (f) { f.focus(); f.select(); } } }, [stage.value]);
  if (!p) return null;
  const c = cur.value, b = busy.value;
  const atRoots = !!c && c.parent === null;
  const esc = (e) => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closePicker(); } };
  const go = () => { const v = text.peek().trim(); if (v) load(typed(v)); else load(''); };
  const create = async () => {
    const n = folder.peek().trim();
    if (!n || !c) return;
    busy.value = true; msg.value = '';
    const r = await hubPost('dirs', { parent: c.path, name: n });
    busy.value = false;
    if (r.status === 201 && r.data && r.data.path) { await load(r.data.path); return; }
    msg.value = r.status === 409 ? 'a folder with that name is already there'
      : r.status === 0 ? 'the server is not answering'
      : (r.data && r.data.error) || `could not make the folder (${r.status})`;
  };
  const finish = async (path, label) => {
    busy.value = true; msg.value = '';
    const why = await p.onDone({ path, name: label });
    busy.value = false;
    if (why) msg.value = why; else closePicker();
  };
  const use = () => {
    if (!c || atRoots) return;
    if (p.naming) {
      chosen.value = c.path;
      name.value = p.parent ? p.prefill || '' : c.crumbs.length ? c.crumbs[c.crumbs.length - 1].name.replace(/^.*[\\/]/, '') || c.label : c.label;
      stage.value = 'name';
    } else finish(c.path, '');
  };
  const enter = (fn) => (e) => { if (e.key === 'Enter') { e.preventDefault(); fn(); } };

  if (stage.value === 'name') {
    return html`<div class="pane frame picker" style="inset:0" ref=${ref} onKeyDown=${esc}>
      <div class="ftitle">${p.title || 'new project'}</div>
      <div class="scroll"><div class="pk">
        <div class="faint">${p.parent ? 'its folder goes in' : 'folder'}</div>
        <div>${chosen.value}</div>
        <div class="row namef"><span class="faint">${p.parent ? 'folder name ' : 'name '}</span><input type="text" spellcheck="false" autocomplete="off" aria-label=${p.parent ? 'folder and project name' : 'project name'} value=${name.value}
          onInput=${(e) => { name.value = e.currentTarget.value; }} onKeyDown=${enter(() => finish(chosen.value, name.peek().trim()))} /></div>
        ${p.parent && html`<div class="faint">it will be ${chosen.value.replace(/\/$/, '')}/${name.value.trim() || '…'}, and a project of that name</div>`}
        <div class="acts">
          <${K} cls="act" off=${b || !name.value.trim()} on=${() => finish(chosen.value, name.peek().trim())}>${p.doneLabel || '[ create project ]'}</${K}>
          <${K} cls="act" on=${() => { stage.value = 'pick'; msg.value = ''; }}>[ back ]</${K}>
          <${K} cls="act" on=${closePicker}>[ cancel ]</${K}>
        </div>
        <div class=${'msg' + (b ? ' faint' : '')} aria-live="polite">${b ? 'working…' : msg.value}</div>
      </div></div></div>`;
  }
  return html`<div class="pane frame picker" style="inset:0" ref=${ref} onKeyDown=${esc}>
    <div class="ftitle">${p.title || 'choose a folder'}</div>
    <div class="scroll"><div class="pk">
      <div class="row"><span class="faint">path </span><input type="text" spellcheck="false" autocomplete="off" aria-label="folder path"
        placeholder="C:\\Users\\you\\work or /home/you/work" value=${text.value}
        onInput=${(e) => { text.value = e.currentTarget.value; }} onKeyDown=${enter(go)} />
        <${K} cls="act" off=${b} on=${go}>[ go ]</${K}></div>
      <div class="crumbs">${c && !atRoots ? html`<${K} cls="hi" title="the folders this server may open" on=${() => load('')}>roots</${K}>${c.crumbs.map((x) => html` <span class="faint">›</span> <${K} cls="hi" title=${x.path} on=${() => load(x.path)}>${x.name}</${K}>`)}` : html`<span class="faint">start from one of these</span>`}</div>
      <div class="dirs">
        ${c && !atRoots && html`<div><${K} cls="drow" on=${() => load(c.parent)}>↑ up</${K}></div>`}
        ${c && c.dirs.map((d) => html`<div key=${d.path}><${K} cls="drow" title=${d.path} on=${() => load(d.path)}><span class="faint">▸</span> ${d.name}</${K}></div>`)}
        ${c && !c.dirs.length && !atRoots && html`<div class="faint">no folders in here</div>`}
        ${c && c.truncated && html`<div class="faint">…more folders than are listed: type the path instead</div>`}
        ${!c && !msg.value && html`<div class="faint">loading…</div>`}
      </div>
      ${making.value && html`<div class="row newf"><span class="faint">new folder </span><input type="text" spellcheck="false" autocomplete="off" aria-label="new folder name" value=${folder.value}
        onInput=${(e) => { folder.value = e.currentTarget.value; }} onKeyDown=${(e) => { if (e.key === 'Enter') { e.preventDefault(); create(); } else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); making.value = false; } }} />
        <${K} cls="act" off=${b || !folder.value.trim()} on=${create}>[ create ]</${K}> <${K} cls="act" on=${() => { making.value = false; msg.value = ''; }}>[ cancel ]</${K}></div>`}
      <div class="acts">
        <${K} cls="act" off=${b || !c || atRoots} title=${atRoots ? 'open a folder first' : p.parent ? 'put the project inside this folder' : 'work in this folder'} on=${use}>[ use this folder ]</${K}>
        <${K} cls="act" off=${b || !c || atRoots} on=${() => { making.value = true; msg.value = ''; }}>[ new folder ]</${K}>
        <${K} cls="act" on=${closePicker}>[ cancel ]</${K}>
      </div>
      <div class=${'msg' + (b ? ' faint' : '')} aria-live="polite">${b ? 'working…' : msg.value}</div>
    </div></div></div>`;
}
