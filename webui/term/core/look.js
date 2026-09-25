// The look: a base16 colour scheme and a font, both switched live and kept in
// localStorage (the page works without it). A scheme is themes/<name>.yaml,
// listed in themes/manifest.json because the page cannot list a directory;
// its sixteen colours become --base00..--base0F on <html>, and theme.css
// derives every other colour from those. A font sets --font, --fs and --lh;
// the cell (--cw, --ch) is then measured from the font itself, so the grid,
// the tiling and the Life strip stay on whole cells.
import { signal } from './ui.js';

export const DEFAULT_THEME = 'phosphor';
export const DEFAULT_FONT = 'vt323';

// Sizes are per font: VT323 is tall and narrow, Departure Mono is drawn on
// an 11px grid and stays crisp only at whole multiples of it.
export const FONTS = {
  vt323: { label: 'VT323', family: '"VT323"', fs: 20, lh: 1.15 },
  'departure-mono': { label: 'Departure Mono', family: '"Departure Mono"', fs: 11, lh: 1.6 },
  system: { label: 'system mono', family: null, fs: 14, lh: 1.35 },
};

export const theme = signal(DEFAULT_THEME);
export const themes = signal([DEFAULT_THEME]);
export const font = signal(DEFAULT_FONT);
export const cell = signal({ cw: 8, ch: 23 });

const THEME_KEY = 'minerva.theme', FONT_KEY = 'minerva.font';
const SLOTS = [...'0123456789ABCDEF'].map((d) => 'base0' + d);
const NAME = /^[a-z0-9][a-z0-9._-]*$/i;
const root = document.documentElement;

const load = (k) => { try { return JSON.parse(localStorage.getItem(k)); } catch { return null; } };
const save = (k, v) => { try { localStorage.setItem(k, JSON.stringify(v)); } catch { /* no storage: this tab only */ } };

// The YAML a base16 scheme uses and nothing else: `key: value` lines, the
// value bare or quoted, an optional `# comment` after it, whole-line
// comments, and the tinted-theming layout (`system`, `name`, `variant`, the
// slots indented under `palette:`). Anything else is refused, not guessed.
const LINE = /^\s*([A-Za-z0-9_-]+):\s*(?:"([^"]*)"|'([^']*)'|(#[0-9A-Fa-f]{6}|[^#\s'"][^#]*?))?\s*(?:#.*)?$/;
const KNOWN = ['scheme', 'name', 'author', 'system', 'variant', 'slug', 'description', 'palette'];

export function parseScheme(text) {
  const colors = {};
  let name = '';
  text.split(/\r?\n/).forEach((raw, i) => {
    if (!raw.trim() || /^\s*#/.test(raw)) return;
    if (raw.length > 512) throw new Error(`line ${i + 1}: too long`);
    const m = LINE.exec(raw);
    if (!m) throw new Error(`line ${i + 1}: not a "key: value" line`);
    const key = m[1], val = m[2] ?? m[3] ?? m[4] ?? '';
    if (/^base0[0-9a-f]$/i.test(key)) {
      const slot = 'base0' + key[5].toUpperCase(), hex = /^#?([0-9a-fA-F]{6})$/.exec(val);
      if (!hex) throw new Error(`line ${i + 1}: ${key} is not a 6-digit hex colour`);
      if (colors[slot]) throw new Error(`line ${i + 1}: ${key} twice`);
      colors[slot] = '#' + hex[1].toLowerCase();
    } else if (key === 'scheme' || key === 'name') name = val;
    else if (!KNOWN.includes(key)) throw new Error(`line ${i + 1}: unknown key ${key}`);
  });
  const missing = SLOTS.filter((s) => !colors[s]);
  if (missing.length) throw new Error('missing ' + missing.join(' '));
  return { name, colors };
}

const paint = (colors) => { for (const s of SLOTS) root.style.setProperty('--' + s, colors[s]); };

// Resolves to an error message, or null once the scheme is on screen.
let themeAsk = 0;
export async function setTheme(name) {
  if (!NAME.test(name) || !themes.peek().includes(name)) return `no theme "${name}"; themes: ${themes.peek().join(' · ')}`;
  const ask = ++themeAsk;
  let scheme;
  try {
    const r = await fetch(`themes/${name}.yaml`, { cache: 'no-cache' });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    scheme = parseScheme(await r.text());
  } catch (e) {
    return `theme ${name}: ${e.message}`;
  }
  if (ask !== themeAsk) return null;
  paint(scheme.colors);
  theme.value = name;
  save(THEME_KEY, { name, colors: scheme.colors });
  return null;
}

// The cell is one glyph's advance by the line height the font asks for.
function measure() {
  const probe = document.createElement('span');
  probe.textContent = 'M'.repeat(80);
  probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre;';
  document.body.append(probe);
  const w = probe.getBoundingClientRect().width;
  probe.remove();
  const cs = getComputedStyle(root);
  const c = { cw: w / 80, ch: Math.round(parseFloat(cs.fontSize) * parseFloat(cs.getPropertyValue('--lh'))) };
  root.style.setProperty('--cw', c.cw + 'px');
  root.style.setProperty('--ch', c.ch + 'px');
  cell.value = c;
}

export async function setFont(key) {
  const f = Object.hasOwn(FONTS, key) && FONTS[key];
  if (!f) return `no font "${key}"; fonts: ${Object.keys(FONTS).join(' · ')}`;
  root.style.setProperty('--font', f.family ? `${f.family}, var(--system-mono)` : 'var(--system-mono)');
  root.style.setProperty('--fs', f.fs + 'px');
  root.style.setProperty('--lh', String(f.lh));
  font.value = key;
  save(FONT_KEY, key);
  measure();
  if (f.family) {
    try { await document.fonts.load(`${f.fs}px ${f.family}`); } catch { /* measured on the fallback */ }
    if (font.peek() === key) measure();
  }
  return null;
}

// A unique prefix is enough: `:font dep`, `:theme dusk` does not match.
export function resolve(names, arg) {
  if (names.includes(arg)) return arg;
  const m = names.filter((n) => n.startsWith(arg));
  return m.length === 1 ? m[0] : arg;
}

// Boot: the remembered font and colours at once (no flash of the default),
// then the manifest and the remembered scheme's file, which wins if it changed.
export function startLook() {
  const f = load(FONT_KEY), t = load(THEME_KEY);
  setFont(Object.hasOwn(FONTS, f) ? f : DEFAULT_FONT);
  if (t && t.colors && SLOTS.every((s) => /^#[0-9a-f]{6}$/.test(t.colors[s]))) { paint(t.colors); theme.value = t.name; }
  return fetch('themes/manifest.json', { cache: 'no-cache' })
    .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
    .then((list) => {
      const ok = Array.isArray(list) ? list.filter((n) => typeof n === 'string' && NAME.test(n)) : [];
      if (ok.length) themes.value = ok;
    })
    .catch(() => { /* themes stays [default]: theme.css already draws it */ })
    .then(() => setTheme(themes.peek().includes(theme.peek()) ? theme.peek() : DEFAULT_THEME));
}
