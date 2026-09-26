// The inspector's files tab: a tree with a `/` filter. The whole working
// directory is the hub's route (GET /s/<id>/tree, web-ui.md section 8); until
// it exists the tree holds the files this session's edit and write calls
// touched. FileTree draws any list of paths, so the route only has to hand it one.
import { html, K, signal, useMemo } from '../core/ui.js';
import { tildify } from '../core/state.js';

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

export function FileTree({ paths }) {
  const q = useMemo(() => signal(''), []);
  const shut = useMemo(() => signal(new Set()), []);
  const root = useMemo(() => build(paths), [paths]);
  const text = q.value.trim().toLowerCase();
  const list = rows(root, text, shut.value);
  const fold = (path) => {
    const s = new Set(shut.value);
    if (!s.delete(path)) s.add(path);
    shut.value = s;
  };
  const key = (e) => {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    q.value = '';
    e.currentTarget.blur();
  };
  return html`<div class="files">
    <div class="filter"><span class="hi">/</span><input type="text" spellcheck="false" autocomplete="off" aria-label="filter files"
      placeholder="filter" value=${q.value} onInput=${(e) => { q.value = e.currentTarget.value; }} onKeyDown=${key} /></div>
    ${list.length ? list.map(({ k, depth }) => html`<div class=${'fl' + (k.dir ? ' dir' : '')} style=${`padding-left:calc(${depth * 2} * var(--cw))`} title=${k.path}>
        ${k.dir ? html`<${K} on=${() => fold(k.path)}>${!text && shut.value.has(k.path) ? '▸' : '▾'} ${k.name}/</${K}>` : html`<span>${'  ' + k.name}</span>`}</div>`)
      : html`<div class="faint">${text ? 'no file matches' : 'empty'}</div>`}
  </div>`;
}

export function FilesTab({ s }) {
  const hl = s.hello.value, cwd = hl && hl.cwd ? String(hl.cwd).replace(/\/$/, '') + '/' : null;
  const paths = useMemo(() => [...s.touched.value.keys()].map((p) => (cwd && p.startsWith(cwd) ? p.slice(cwd.length) : p)), [s.touched.value, cwd]);
  return html`<div class="faint" title=${hl ? hl.cwd : ''}>${hl ? tildify(hl.cwd) : ''}</div>
    <div class="hdr">touched</div>
    ${paths.length ? html`<${FileTree} paths=${paths} />` : html`<div class="faint">no edit or write calls yet</div>`}
    <div class="needs">${'the whole tree needs the hub (its GET /s/<id>/tree route)'}</div>`;
}
