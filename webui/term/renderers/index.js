// Which renderer draws which tool. M5 fills `named` from manifest.json
// ({ "renderers": { "bash": "renderers/bash.js" } }) with one import() per
// entry; until then every tool gets the generic one. Calls go through
// `render()`, which catches a throwing renderer and falls back to generic, so
// one broken extension never blanks a pane (web-ui.md section 10).
import { html, h, K, useState } from '../core/ui.js';
import { markdown } from '../core/markdown.js';
import generic from './generic.js';

const named = new Map();
export const register = (tool, mod) => named.set(tool, mod);
export const ctx = Object.freeze({ html, h, K, markdown, useState });

const pick = (tool) => named.get(tool) || generic;

export function render(tool, part, ...args) {
  const r = pick(tool);
  try {
    if (typeof r[part] === 'function') return r[part](...args, ctx);
  } catch (e) {
    console.error('renderer failed:', tool, part, e && e.message);
  }
  return generic[part](...args, ctx);
}
