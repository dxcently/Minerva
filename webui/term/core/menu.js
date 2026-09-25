// Menus: a square frame of lines, opened by a right-click (at the pointer) or
// a click on a title or glyph (under it). One menu is open at a time; a
// submenu opens beside its line. Kept on screen: a menu that would run off
// the right or bottom edge opens on the other side of its anchor.
//
//   item = { label, on?, off?: 'why it is greyed', sub?: [item], tick?, hint? }
//        | { sep: true }
//
// Keys: up/down move, Enter or right opens a submenu, Enter runs, left or Esc
// backs out, Tab closes. The mouse selects on hover and runs on click.
import { html, signal, useRef, useState, useLayoutEffect } from './ui.js';

export const menu = signal(null); // { n, items, at, title, owner, back }
let seq = 0;

// Anchors. `y2` is where the menu ends instead when it does not fit below `y`;
// `x2` where it ends instead when it does not fit right of `x`.
export const atPointer = (e) => ({ x: e.clientX, y: e.clientY, x2: e.clientX, y2: e.clientY });
export function under(el) {
  const r = el.getBoundingClientRect();
  return { x: r.left, y: r.bottom, x2: r.right, y2: r.top };
}

// Opening the menu `owner` already shows closes it (a second click on a title).
export function openMenu(items, at, { title = '', owner = null } = {}) {
  const m = menu.peek();
  if (m && owner && m.owner === owner) { closeMenu(); return; }
  menu.value = { n: ++seq, items, at, title, owner, back: m ? m.back : document.activeElement };
}

export function closeMenu() {
  const m = menu.peek();
  if (!m) return;
  menu.value = null;
  if (m.back && m.back.isConnected && m.back !== document.body) m.back.focus({ preventScroll: true });
}

// A mousedown outside every menu (and outside the thing that opened it) closes it.
export function outside(e) {
  const m = menu.peek();
  if (!m || e.target.closest('.pop')) return;
  if (m.owner && e.target.closest(`[data-owner="${m.owner}"]`)) return;
  closeMenu();
}

export function Menus() {
  const m = menu.value;
  return m && html`<${Menu} key=${m.n} items=${m.items} at=${m.at} title=${m.title} />`;
}

const usable = (it) => it && !it.sep && !it.off;
const run = (it) => { closeMenu(); it.on(); };

function Menu({ items, at, title, leave }) {
  const ref = useRef(null), els = useRef([]);
  const [sel, setSel] = useState(() => Math.max(0, items.findIndex(usable)));
  const [sub, setSub] = useState(null); // { i, at }
  useLayoutEffect(() => {
    const el = ref.current, w = el.offsetWidth, h = el.offsetHeight;
    const x = at.x + w <= innerWidth ? at.x : at.x2 - w;
    const y = at.y + h <= innerHeight ? at.y : at.y2 - h;
    el.style.left = Math.max(0, Math.min(x, innerWidth - w)) + 'px';
    el.style.top = Math.max(0, Math.min(y, innerHeight - h)) + 'px';
    el.focus({ preventScroll: true });
  }, []);
  const openSub = (i) => {
    const r = els.current[i].getBoundingClientRect(), box = ref.current.getBoundingClientRect();
    setSub({ i, at: { x: box.right - 1, y: r.top - 1, x2: box.left + 1, y2: r.bottom + 1 } });
  };
  const pick = (i) => {
    const it = items[i];
    if (it.sub) openSub(i);
    else if (usable(it)) run(it);
  };
  const move = (d) => {
    for (let j = 1; j <= items.length; j++) {
      const i = (sel + d * j + items.length * j) % items.length;
      if (!items[i].sep) { setSel(i); return; }
    }
  };
  const onKey = (e) => {
    e.stopPropagation();
    const k = e.key;
    if (k === 'Tab') { closeMenu(); return; }
    e.preventDefault();
    if (e.repeat && (k === 'Enter' || k === ' ')) return;
    if (k === 'ArrowDown') move(1);
    else if (k === 'ArrowUp') move(-1);
    else if (k === 'Enter' || k === ' ' || (k === 'ArrowRight' && items[sel].sub)) pick(sel);
    else if (k === 'Escape' || k === 'ArrowLeft') { if (leave) leave(); else if (k === 'Escape') closeMenu(); }
  };
  const hover = (i) => {
    setSel(i);
    if (items[i].sub) { if (!sub || sub.i !== i) openSub(i); }
    else if (sub) { setSub(null); ref.current.focus({ preventScroll: true }); }
  };
  const back = () => { setSub(null); ref.current.focus({ preventScroll: true }); };
  return html`<div class="pop frame" role="menu" tabindex="-1" ref=${ref} onKeyDown=${onKey}
      onContextMenu=${(e) => e.preventDefault()}>
    ${title && html`<div class="ftitle">${title}</div>`}
    ${items.map((it, i) => (it.sep ? html`<div class="msep" role="separator"></div>`
      : html`<div class=${'mi' + (i === sel ? ' sel' : '') + (it.off ? ' dis' : '')} role="menuitem"
          aria-disabled=${it.off ? 'true' : null} title=${it.off || it.hint || ''}
          ref=${(el) => { els.current[i] = el; }}
          onMouseEnter=${() => hover(i)} onClick=${() => pick(i)}>
        <span class="tick">${it.tick ? '✓' : ' '}</span><span class="ml">${it.label}</span>
        <span class="mh">${it.off || it.hint || ''}</span><span class="arr">${it.sub ? '▸' : ' '}</span>
      </div>`))}
    ${sub && html`<${Menu} key=${sub.i} items=${items[sub.i].sub} at=${sub.at} leave=${back} />`}
  </div>`;
}
