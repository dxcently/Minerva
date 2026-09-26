// Colour for code, as the TUI does it (crates/tui/src/syntax.rs): not a
// parser, a line-local scanner that finds a comment, a quoted string, a
// number and a word from a fixed list, and leaves the rest alone. Wrong at
// the edges (a string over two lines, a nested comment) and cheap enough for
// a 2,000-line read. A language the table does not know gets nothing: an
// apostrophe in prose is not a string.
// Code blocks, file reads and diffs are drawn here too, with the TUI's line
// numbers (render.rs:2946-3000): a diff is numbered by the side that
// survives (a removed line takes a blank), each line tinted by its sign.
import { html, K, signal, useMemo } from './ui.js';

const RUST = 'as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while';
const PYTHON = 'and as assert async await break class continue def del elif else except finally for from global if import in is lambda None not or pass raise return self True False try while with yield';
const JS = 'async await break case catch class const continue default delete else export extends false finally for from function if implements import in instanceof interface let new null return switch this throw true try type typeof undefined var while yield';
const GO = 'break case chan const continue default defer else fallthrough for func go goto if import interface map nil package range return select struct switch true false type var';
const SHELL = 'case do done elif else esac export fi for function if in local read return set then until while';
const C = 'break case char class const continue default do double else enum extern false float for if include int long namespace new nullptr private public return short sizeof static struct switch template true typedef union unsigned using void while';
const NIX = 'assert else if in inherit let or rec then with true false null import';
const DATA = 'true false null yes no on off';
const SQL = 'and as asc by create delete desc drop from group having insert into join left limit not null on or order select set table update values where';
const NGINX = 'server location listen server_name root index return proxy_pass include if set rewrite upstream http events';

const words = (s) => new Set(s.split(' '));
const LANGS = [
  [['rust', 'rs'], ['//'], '"', RUST],
  [['python', 'py'], ['#'], '"\'', PYTHON],
  [['js', 'javascript', 'mjs', 'ts', 'typescript', 'jsx', 'tsx'], ['//'], '"\'`', JS],
  [['go'], ['//'], '"`', GO],
  [['sh', 'bash', 'zsh', 'shell', 'console'], ['#'], '"\'', SHELL],
  [['c', 'cpp', 'c++', 'cc', 'h', 'hpp', 'java', 'cs'], ['//'], '"\'', C],
  [['nix'], ['#'], '"', NIX],
  [['toml', 'yaml', 'yml', 'ini', 'conf', 'cfg'], ['#'], '"\'', DATA],
  [['nginx'], ['#'], '"\'', NGINX],
  [['json'], [], '"', DATA],
  [['sql'], ['--'], '"\'', SQL],
];
const TABLE = new Map();
for (const [names, comment, quotes, list] of LANGS) {
  const l = { comment, quotes, words: words(list) };
  for (const n of names) TABLE.set(n, l);
}
const TEXT = { comment: [], quotes: '', words: new Set() };

// What a fence's language or a file's extension means here; unknown is TEXT.
export const lang = (name) => TABLE.get(String(name || '').trim().toLowerCase()) || TEXT;
export const extOf = (path) => { const f = String(path || '').split('/').pop(); const i = f.lastIndexOf('.'); return i > 0 ? f.slice(i + 1) : ''; };

const WORD = /[\p{L}\p{N}_]/u;
const ROLE = { c: 'sx-c', s: 'sx-s', n: 'sx-n', k: 'sx-k' };

// [text, role|null] runs of one line, in order (syntax.rs scan).
export function scan(line, l) {
  const out = [];
  let i = 0, plain = 0;
  const push = (end, role) => {
    if (i > plain) out.push([line.slice(plain, i), null]);
    out.push([line.slice(i, end), role]);
    i = plain = end;
  };
  while (i < line.length) {
    const c = line[i];
    if (l.comment.some((m) => line.startsWith(m, i))) { push(line.length, 'c'); break; }
    if (l.quotes.includes(c)) {
      let end = line.length;
      for (let j = i + 1, esc = false; j < line.length; j++) {
        if (esc) esc = false;
        else if (line[j] === '\\') esc = true;
        else if (line[j] === c) { end = j + 1; break; }
      }
      push(end, 's');
      continue;
    }
    if (c >= '0' && c <= '9' && !(i > 0 && WORD.test(line[i - 1]))) {
      let j = i;
      while (j < line.length && /[A-Za-z0-9._]/.test(line[j])) j++;
      push(j, 'n');
      continue;
    }
    if (WORD.test(c) && !(c >= '0' && c <= '9')) {
      let j = i;
      while (j < line.length && WORD.test(line[j])) j++;
      if (l.words.has(line.slice(i, j))) push(j, 'k');
      else i = j;
      continue;
    }
    i++;
  }
  if (plain < line.length) out.push([line.slice(plain), null]);
  return out;
}

// One line of code as spans; TEXT comes back as the plain string.
export function codeLine(line, l) {
  if (l === TEXT) return line;
  return scan(line, l).map(([t, r]) => (r ? html`<span class=${ROLE[r]}>${t}</span>` : t));
}

// Lines drawn at once; more come a page at a time, so a 2,000-line read
// costs what is on screen, not the file.
export const PAGE = 400;

function Lines({ rows, cap }) {
  const shown = useMemo(() => signal(cap), [rows, cap]);
  const n = Math.min(shown.value, rows.length), rest = rows.length - n;
  return html`${rows.slice(0, n)}${rest > 0 && html`<${K} cls="more cmore" on=${() => { shown.value = n + PAGE; }}>… ${rest} more line${rest === 1 ? '' : 's'}</${K}>`}`;
}

// A code block or a file: a number gutter, then the code. `start` numbers the
// first line; `nums` gives each line its own number instead (a read that
// arrives numbered); `cap` lines are drawn until asked.
export function Code({ text, name, start = 1, nums = null, cap = PAGE }) {
  const rows = useMemo(() => {
    const l = lang(name), lines = (nums ? String(text) : String(text).replace(/\n$/, '')).split('\n');
    const num = (i) => (nums ? nums[i] : start + i);
    const w = String(num(lines.length - 1)).length;
    return lines.map((t, i) => html`<div class="cr"><span class="ln">${String(num(i)).padStart(w)}</span><span class="cx">${codeLine(t.replace(/\t/g, '    '), l) || ' '}</span></div>`);
  }, [text, name, start, nums]);
  return html`<div class="code"><${Lines} rows=${rows} cap=${cap} /></div>`;
}

// A file as eidolon's read returns it (crates/tools/src/fs.rs read): each
// line `{n:>6}\t{line}`, then maybe `[showing lines a-b of N]` or
// `[empty file]`. { nums, text, note }, or null when it is not that shape.
export function numbered(out) {
  const lines = String(out).replace(/\n$/, '').split('\n');
  let note = '';
  if (/^\[(showing lines \d+-\d+ of \d+|empty file)\]$/.test(lines[lines.length - 1])) note = lines.pop().slice(1, -1);
  if (!lines.length) return note ? { nums: [], text: '', note } : null;
  const nums = [], body = [];
  for (const t of lines) {
    const m = /^\s*(\d+)\t/.exec(t);
    if (!m) return null;
    nums.push(+m[1]);
    body.push(t.slice(m[0].length));
  }
  return { nums, text: body.join('\n'), note };
}

// [sign, text] lines of a unified diff, with each line's old and new numbers
// read off its hunk headers (from 1 when it has none); a line outside a hunk
// (`---`, `+++`, `diff`) is a header. Inside a hunk its counts say how many
// lines are left, so a removed `-- x` or an added `++ x` is not a header.
export function parseUnified(text) {
  const out = [];
  let o = 1, n = 1, inHunk = !/^@@ /m.test(text), left = inHunk ? Infinity : 0;
  for (const t of String(text).replace(/\n$/, '').split('\n')) {
    const h = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(t);
    if (h) {
      o = +h[1]; n = +h[3]; inHunk = true;
      left = (h[2] != null ? +h[2] : 1) + (h[4] != null ? +h[4] : 1);
      out.push(['@', t]);
      continue;
    }
    if (!inHunk || (left <= 0 && /^(diff |index |--- |\+\+\+ )/.test(t))) { out.push(['h', t]); continue; }
    left -= t[0] === '+' || t[0] === '-' ? 1 : 2; // a shared line counts on both sides
    const sign = t[0] === '+' || t[0] === '-' ? t[0] : ' ';
    const body = t[0] === '+' || t[0] === '-' || t[0] === ' ' ? t.slice(1) : t;
    out.push([sign, body, sign === '+' ? null : o++, sign === '-' ? null : n++]);
  }
  return out;
}

export const isUnified = (t) => /^@@ .* @@/m.test(t) || (/^--- /.test(t) && /\n\+\+\+ /.test(t));

// The change an edit or write call makes, from its own arguments
// (render.rs:2877-2913): a write is every line added; an edit is its old
// and new text with the lines both share trimmed to CONTEXT either side.
// Numbered as the TUI does: the surviving lines, from 1 (for an edit, the
// fragment's lines; the call does not say where in the file it is).
const CONTEXT = 3;
export function editDiff(name, input) {
  if (!input || typeof input !== 'object') return null;
  if (name === 'write') {
    if (typeof input.content !== 'string' || !input.content) return null;
    return input.content.replace(/\n$/, '').split('\n').map((t, i) => ['+', t, null, i + 1]);
  }
  if (name !== 'edit') return null;
  const oldT = input.old_str ?? input.old_string, newT = input.new_str ?? input.new_string;
  if (typeof oldT !== 'string' || typeof newT !== 'string') return null;
  const o = oldT.split('\n'), n = newT.split('\n');
  let head = 0;
  while (head < o.length && head < n.length && o[head] === n[head]) head++;
  let tail = 0;
  while (tail < o.length - head && tail < n.length - head && o[o.length - 1 - tail] === n[n.length - 1 - tail]) tail++;
  const out = [];
  for (let i = Math.max(0, head - CONTEXT); i < head; i++) out.push([' ', o[i]]);
  for (let i = head; i < o.length - tail; i++) out.push(['-', o[i]]);
  for (let i = head; i < n.length - tail; i++) out.push(['+', n[i]]);
  for (let i = o.length - tail; i < Math.min(o.length, o.length - tail + CONTEXT); i++) out.push([' ', o[i]]);
  let k = 0;
  for (const x of out) x.push(null, x[0] === '-' ? null : ++k);
  return out.some(([s]) => s !== ' ') ? out : null;
}

// A diff: the new side's number (blank on a removed line), the sign, the
// line in its language on a ground of its sign's colour; headers dim.
export function Diff({ lines, name, cap = PAGE }) {
  const rows = useMemo(() => {
    const l = lang(name);
    const w = Math.max(1, ...lines.map(([, , , b]) => String(b || 0).length));
    const num = (x) => (x == null ? ''.padStart(w) : String(x).padStart(w));
    return lines.map(([sign, t, , b]) => (sign === '@' || sign === 'h'
      ? html`<div class="cr dh"><span class="ln">${''.padStart(w)}</span><span class="cx">${t}</span></div>`
      : html`<div class=${'cr' + (sign === '+' ? ' add' : sign === '-' ? ' del' : '')}><span class="ln">${num(b)}</span><span class="sg">${sign}</span><span class="cx">${codeLine(t.replace(/\t/g, '    '), l) || ' '}</span></div>`));
  }, [lines, name]);
  return html`<div class="code diff"><${Lines} rows=${rows} cap=${cap} /></div>`;
}
