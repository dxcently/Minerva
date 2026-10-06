// Images in the transcript: thumbnails under the row that has them, and one full-size viewer over the page.
//
// The bytes are not in the stream (the door's frames carry a count). They come from the perch:
// `GET /hub/sessions/<id>/image/<record>/<n>` (perch/src/images.rs), same origin with the cookie, so a plain
// <img src> loads them. A row says which it is: `{ record, images, names?, local? }`.
//   - `record` + `images`: the bytes are in the log, at (record, 0..images-1).
//   - `local`: an image this tab just sent, `{ name, media_type, data }` per image. It is drawn at once from the
//     data and swapped for the route as soon as the route answers, and the data is then dropped.
// Under a bare door (no perch) there is no route: a local preview still shows, anything else says what it was.
import { html, K, signal, useState, useEffect, useMemo } from '../core/ui.js';
import { mode, hubGet } from '../core/api.js';
import { resolveIn } from '../core/path.js';

export const imageUrl = (sid, record, n) => `/hub/sessions/${encodeURIComponent(sid)}/image/${Number(record)}/${Number(n)}`;

// `GET /hub/file` (perch/src/rawfile.rs): one image a tool wrote into the chat's folder, `rel` relative to that folder.
// Nothing is fetched until the viewer (or a visible <img>) asks.
export const fileUrl = (sid, rel) => `/hub/file?session=${encodeURIComponent(sid)}&path=${encodeURIComponent(rel)}`;
export const isImageName = (name) => /\.(png|jpe?g|gif|webp)$/i.test(name);
export const viewFile = (sid, rel) => { const u = fileUrl(sid, rel); view(u, rel, u); };

// Paths in a reply that name an image inside the chat's folder become clickable; everything else stays text.
// `cwd` is the session's folder as the door reported it. A path that is not under it, or a `~` path, stays text, and
// the perch checks it all again when asked (links, deny list), so a wrong guess here is only a click that says so.
const IMG_PATH = /(?<![\w@%+=~./-])(?:\.{0,2}\/)?[\w@%+=~-][\w@%+=~./-]*\.(?:png|jpe?g|gif|webp)(?![\w@%+=~-]|\.\w)/gi;
export function linkPaths(text, sid, cwd) {
  if (!sid || typeof text !== 'string' || mode() !== 'perch' || typeof cwd !== 'string') return text;
  const root = cwd.replace(/\/+$/, '');
  const parts = [];
  let last = 0;
  for (const m of text.matchAll(IMG_PATH)) {
    const r = resolveIn(cwd, m[0]);
    if (!r.path || !r.path.startsWith(root + '/')) continue;
    const rel = r.path.slice(root.length + 1);
    if (!rel || !isImageName(rel)) continue;
    if (m.index > last) parts.push(text.slice(last, m.index));
    parts.push(html`<${K} cls="imgtog" title=${'view ' + rel} on=${() => viewFile(sid, rel)}>${m[0]}</${K}>`);
    last = m.index + m[0].length;
  }
  if (!parts.length) return text;
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

const dataUrl = (l) => `data:${l.media_type};base64,${l.data}`;
const plural = (n) => `${n} image${n === 1 ? '' : 's'}`;

// ---------------------------------------------------------------- the viewer
// One overlay node, made on first use, outside the component tree so no pane owns it. Esc or a click closes it.
let overlay = null;
export function view(src, alt, raw) {
  closeView();
  const box = document.createElement('div');
  box.className = 'imgview';
  box.tabIndex = -1;
  const bar = document.createElement('div');
  bar.className = 'bar';
  const name = document.createElement('span');
  name.textContent = alt || 'image';
  const open = document.createElement('a');
  open.textContent = 'open in new tab';
  if (raw) { open.href = raw; open.target = '_blank'; open.rel = 'noopener'; } else open.hidden = true;
  open.addEventListener('click', (e) => e.stopPropagation());
  const esc = document.createElement('span');
  esc.className = 'faint';
  esc.textContent = 'esc closes';
  bar.append(name, open, esc);
  const img = document.createElement('img');
  img.src = src;
  img.alt = alt || 'image';
  img.addEventListener('error', () => { name.textContent = (alt || 'image') + ' (could not load)'; });
  box.append(bar, img);
  const back = document.activeElement;
  const done = () => { box.remove(); document.removeEventListener('keydown', key, true); overlay = null; if (back && back.focus) back.focus(); };
  const key = (e) => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); done(); } };
  box.addEventListener('click', done);
  document.addEventListener('keydown', key, true);
  document.body.append(box);
  box.focus();
  overlay = done;
}
export function closeView() { if (overlay) overlay(); }

// ---------------------------------------------------------------- names
// A live chat's frames carry a count and no names; the perch's list does (GET images). One call per session
// answers every thumbnail in it for a few seconds, and a failure just leaves the default alt.
const listed = new Map();
function nameOf(sid, record, n) {
  let e = listed.get(sid);
  if (!e || Date.now() - e.at > 8000) { e = { at: Date.now(), p: hubGet(`sessions/${encodeURIComponent(sid)}/images`, 15000) }; listed.set(sid, e); }
  return e.p.then((r) => {
    const hit = r.status === 200 && Array.isArray(r.data) && r.data.find((i) => i.record === record && i.n === n);
    return hit && typeof hit.alt === 'string' ? hit.alt : null;
  });
}

// ---------------------------------------------------------------- one thumbnail
const WAITS = [800, 2500, 6000, 15000]; // the route is ready when the record is journaled; a miss is retried, not cached

function Thumb({ sid, it }) {
  const route = sid && it.record != null && mode() === 'perch' ? imageUrl(sid, it.record, it.n) : null;
  const [src, setSrc] = useState(it.local ? dataUrl(it.local) : route);
  const [bad, setBad] = useState(false);
  const [named, setNamed] = useState(null);
  const alt = it.alt || it.local && it.local.name || named || `image ${it.n + 1}`;
  // Without a name in hand, ask the perch's list once the picture is there to name.
  useEffect(() => {
    if (it.alt || it.local || !route) return undefined;
    let dead = false;
    nameOf(sid, it.record, it.n).then((v) => { if (!dead && v) setNamed(v); });
    return () => { dead = true; };
  }, [route]);
  // A local preview is replaced by the route when it loads, then forgotten.
  useEffect(() => {
    if (!it.local || !route) return undefined;
    let dead = false, timer = 0, at = 0;
    const probe = () => {
      const p = new Image();
      p.onload = () => { if (!dead) { it.drop(); setSrc(route); } };
      p.onerror = () => { if (!dead && at < WAITS.length) timer = setTimeout(probe, WAITS[at++]); };
      p.src = route;
    };
    timer = setTimeout(probe, WAITS[0]);
    return () => { dead = true; clearTimeout(timer); };
  }, [route]);
  if (!src || bad) return html`<span class="faint imgmiss">[${alt}]</span>`;
  const open = () => view(src, alt, route);
  const key = (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); open(); } };
  return html`<img class="thumb" src=${src} alt=${alt} title=${alt} loading="lazy" tabindex="0" onClick=${open} onKeyDown=${key} onError=${() => setBad(true)} />`;
}

// ---------------------------------------------------------------- a row's images
// `row`: `{ record, images, names?, local? }`. Shown as they are, or (`fold`) behind one `▸ N images` toggle.
export function items(row) {
  const out = [];
  for (let n = 0; n < (row.images || 0); n++) out.push({ record: row.record, n, alt: row.names && row.names[n], local: row.local && row.local[n], drop: () => { if (row.local) row.local[n] = null; } });
  return out;
}

export function Strip({ sid, list, fold }) {
  const open = useMemo(() => signal(!fold), []);
  if (!list.length) return null;
  const shown = open.value;
  return html`<div class="imgs">
    ${fold && html`<${K} cls="imgtog" on=${() => { open.value = !open.value; }}>${(shown ? '▾ ' : '▸ ') + plural(list.length)}</${K}>`}
    ${shown && html`<div class="thumbs">${list.map((it) => html`<${Thumb} key=${it.record + ':' + it.n} sid=${sid} it=${it} />`)}</div>`}
  </div>`;
}
