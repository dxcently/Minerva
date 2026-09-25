// A small, safe markdown subset (web-ui.md Q1 default: our own), as Preact
// vnodes. Model text is only ever text children, never HTML.
//   ``` fences -> <pre>, # headings, `code`, **bold**, [label](https://...)
// Links are made only for http(s) URLs, and always open with
// rel="noopener noreferrer".
import { html } from './ui.js';

export function markdown(src) {
  const out = [];
  const lines = String(src || '').split('\n');
  let para = [];
  const flush = () => {
    if (para.length) out.push(html`<div>${inline(para.join('\n'))}</div>`);
    para = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    if (/^\s*```/.test(l)) {
      flush();
      const code = [];
      for (i++; i < lines.length && !/^\s*```/.test(lines[i]); i++) code.push(lines[i]);
      out.push(html`<pre>${code.join('\n')}</pre>`);
      continue;
    }
    const hd = /^(#{1,6})\s+(.*)$/.exec(l);
    if (hd) { flush(); out.push(html`<div class="md-h">${(hd[1].length <= 2 ? '' : hd[1] + ' ') + hd[2]}</div>`); continue; }
    para.push(l);
  }
  flush();
  return out;
}

const INLINE = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(\[[^\]\n]+\]\((https?:\/\/[^)\s]+)\))/g;

export function inline(text) {
  const parts = [];
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    if (m[1]) parts.push(html`<code>${m[1].slice(1, -1)}</code>`);
    else if (m[2]) parts.push(html`<b>${m[2].slice(2, -2)}</b>`);
    else if (m[3] && /^https?:\/\//i.test(m[4])) {
      const label = m[3].slice(1, m[3].indexOf(']('));
      parts.push(html`<a href=${m[4]} target="_blank" rel="noopener noreferrer">${label}</a>`);
    } else parts.push(m[0]);
    last = m.index + m[0].length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}
