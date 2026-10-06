// The inspector's files tab: two sections under one `/` filter. `folder` lists
// the session's working directory through the perch's
// GET /hub/tree?session=<id>&path=<rel> (hub.md section 7), a directory at a
// time, on first unfold; `touched` holds the files this session's edit and
// write calls touched. FileTree draws any list of paths.
import { html, K, signal, useMemo, useEffect } from '../core/ui.js';
import { tildify, kfmt } from '../core/state.js';
import { mode, hubTree } from '../core/api.js';

function build(paths) {
  const root = { kids: new Map() };
  for (const p of paths) {
    let n = root, at = '';
    const parts = p.split('/').filter(Boolean);
    parts.forEach((name, i) => {
      at += (at ? '/' : '') + name;
      if (!n.kids.has(name)) n.kids.set(name, { name, path: at, dir: i < parts.length - 1 || p.endsWith('/'), kids: new Map() });
      n = n.kids.get(name);
    });
  }
  return root;
}

const sorted = (n) => [...n.kids.values()].sort((a, b) => (a.dir !== b.dir ? (a.dir ? -1 : 1) : a.name.localeCompare(b.name)));

// Rows to draw, depth first. With a filter, a file shows when its path holds
// the text and a folder when anything under it does; folds are ignored.
function rows(root, q, shut) {
  const out = [];
  const walk = (n, depth) => {
    let hit = false;
    for (const k of sorted(n)) {
      const at = out.length;
      if (k.dir) {
        out.push({ k, depth });
        const inner = (q || !shut.has(k.path)) && walk(k, depth + 1);
        const self = !q || k.path.toLowerCase().includes(q);
        if (q && !inner && !self) out.length = at; else hit = true;
      } else if (!q || k.path.toLowerCase().includes(q)) {
        out.push({ k, depth });
        hit = true;
      }
    }
    return hit;
  };
  walk(root, 0);
  return out;
}

// The filter box. `q` is a signal owned by the tab so both sections share it.
function Filter({ q }) {
  const key = (e) => {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    q.value = '';
    e.currentTarget.blur();
  };
  return html`<div class="filter"><span class="hi">/</span><input type="text" spellcheck="false" autocomplete="off" aria-label="filter files"
    placeholder="filter" value=${q.value} onInput=${(e) => { q.value = e.currentTarget.value; }} onKeyDown=${key} /></div>`;
}

export function FileTree({ paths, q }) {
  const shut = useMemo(() => signal(new Set()), []);
  const root = useMemo(() => build(paths), [paths]);
  const text = q.value.trim().toLowerCase();
  const list = rows(root, text, shut.value);
  const fold = (path) => {
    const s = new Set(shut.value);
    if (!s.delete(path)) s.add(path);
    shut.value = s;
  };
  return html`<div class="files">
    ${list.length ? list.map(({ k, depth }) => html`<div class=${'fl' + (k.dir ? ' dir' : '')} style=${`padding-left:calc(${depth * 2} * var(--cw))`} title=${k.path}>
        ${k.dir ? html`<${K} on=${() => fold(k.path)}>${!text && shut.value.has(k.path) ? '▸' : '▾'} ${k.name}/</${K}>` : html`<span>${'  ' + k.name}</span>`}</div>`)
      : html`<div class="faint">${text ? 'no file matches' : 'empty'}</div>`}
  </div>`;
}

// ---------------------------------------------------------------- the folder
// Listings are cached per session id + path for the life of the page. An entry
// is { loading } | { ok: answer } | { err: line, retry }. `ver` bumps on every
// change so a tab that reads it redraws.
const cache = new Map();
const open = new Set();            // `${sid}\0${path}` of unfolded directories
const ver = signal(0);
const key = (sid, path) => sid + '\0' + path;
const bump = () => { ver.value++; };

// What a non-200 answer says. 404 is the same line for a missing and a denied
// name (the perch hides both alike). 429/503/anything else: busy, retry.
function fail(status, data) {
  if (status === 0) return { err: 'the server is not answering', retry: true };
  if (status === 401) return { err: 'the server refused the login for this page (it restarted?). Run `hoot` again.' };
  if (status === 404) return { err: 'not found' };
  if (status === 409) return { err: `the folder lists only while the session runs (${data && typeof data.state === 'string' ? data.state : 'not running'})` };
  return { err: 'busy, retry', retry: true };
}

async function load(sid, path) {
  const k = key(sid, path);
  const have = cache.get(k);
  if (have && !have.err) return;
  cache.set(k, { loading: true });
  bump();
  const { status, data } = await hubTree(sid, path);
  const ok = status === 200 && data && Array.isArray(data.entries);
  cache.set(k, ok ? { ok: data } : fail(status === 200 ? 502 : status, data));
  bump();
}

function drop(sid) {
  for (const k of [...cache.keys()]) if (k.startsWith(sid + '\0')) cache.delete(k);
  for (const k of [...open]) if (k.startsWith(sid + '\0')) open.delete(k);
  bump();
  load(sid, '');
}

// Rows for the loaded part of the tree. With a filter nothing is fetched: a
// file shows when its path holds the text, a directory when its own path does
// or any loaded entry under it does.
function folderRows(sid, text) {
  const out = [];
  const note = (depth, cls, text2, retry) => out.push({ note: true, depth, cls, text: text2, retry });
  const walk = (path, depth) => {
    const e = cache.get(key(sid, path));
    if (!e) return false;
    if (e.loading) { if (!text) note(depth, 'faint', '…'); return false; }
    if (e.err) { if (!text) note(depth, 'faint', depth ? `(${e.err})` : e.err, e.retry ? path : null); return false; }
    const { entries, skipped, truncated } = e.ok;
    let hit = false;
    for (const en of entries) {
      if (!en || typeof en.name !== 'string') continue;
      const p = path ? path + '/' + en.name : en.name;
      const at = out.length;
      const own = !text || p.toLowerCase().includes(text);
      if (en.kind === 'dir') {
        const op = open.has(key(sid, p));
        out.push({ en, p, depth, dir: true, op: text ? true : op });
        const inner = (text || op) && walk(p, depth + 1);
        if (text && !inner && !own) out.length = at; else hit = true;
      } else if (own) {
        out.push({ en, p, depth });
        hit = true;
      }
    }
    if (!text) {
      if (truncated) note(depth, 'faint', '(first 2000)');
      if (skipped) note(depth, 'faint', `(${skipped} names not shown)`);
    }
    return hit;
  };
  walk('', 0);
  return out;
}

function Folder({ sid, q }) {
  ver.value; // redraw when a listing lands
  const text = q.value.trim().toLowerCase();
  useEffect(() => { if (mode() === 'perch' && sid) load(sid, ''); }, [sid]);
  if (mode() !== 'perch') return html`<div class="faint">the folder list needs the Minerva server (type hoot)</div>`;
  if (!sid) return null;
  const toggle = (p) => {
    const k = key(sid, p);
    if (open.has(k)) open.delete(k); else { open.add(k); load(sid, p); }
    bump();
  };
  const list = folderRows(sid, text);
  const pad = (d) => `padding-left:calc(${d * 2} * var(--cw))`;
  return html`<div class="files">
    ${list.length ? list.map((r) => {
      if (r.note) return html`<div class=${'fl ' + r.cls} style=${pad(r.depth)}>${r.retry !== null && r.retry !== undefined
        ? html`<${K} on=${() => { cache.delete(key(sid, r.retry)); load(sid, r.retry); }}>${r.text}</${K}>` : r.text}</div>`;
      const { en, p, depth } = r;
      if (r.dir) return html`<div class="fl dir" style=${pad(depth)} title=${p}><${K} on=${() => toggle(p)}>${r.op ? '▾' : '▸'} ${en.name}/</${K}></div>`;
      if (en.kind === 'file') return html`<div class="fl" style=${pad(depth)} title=${p}><span>${'  ' + en.name}</span>${typeof en.size === 'number' ? html` <span class="faint">${kfmt(en.size)}B</span>` : null}</div>`;
      if (en.kind === 'link') return html`<div class="fl faint" style=${pad(depth)} title=${p}>${'  ' + en.name + ' ->'}</div>`;
      return html`<div class="fl faint" style=${pad(depth)} title=${p}>${'  ' + en.name}</div>`;
    }) : html`<div class="faint">${text ? 'no file matches' : 'empty'}</div>`}
  </div>`;
}

export function FilesTab({ s }) {
  const q = useMemo(() => signal(''), []);
  const hl = s.hello.value, cwd = hl && hl.cwd ? String(hl.cwd).replace(/\/$/, '') + '/' : null;
  const paths = useMemo(() => [...s.touched.value.keys()].map((p) => (cwd && p.startsWith(cwd) ? p.slice(cwd.length) : p)), [s.touched.value, cwd]);
  return html`<div class="faint" title=${hl ? hl.cwd : ''}>${hl ? tildify(hl.cwd) : ''}</div>
    <div class="files"><${Filter} q=${q} /></div>
    <div class="hdr">folder${mode() === 'perch' ? html` <${K} cls="faint" on=${() => drop(s.sid)}>refresh</${K}>` : null}</div>
    <${Folder} sid=${s.sid} q=${q} />
    <div class="hdr">touched</div>
    ${paths.length ? html`<${FileTree} paths=${paths} q=${q} />` : html`<div class="faint">no edit or write calls yet</div>`}`;
}
