// Which renderer draws which tool. M5 fills `named` from manifest.json
// ({ "renderers": { "bash": "renderers/bash.js" } }) with one import() per
// entry; until then every tool gets the generic one. Calls go through
// `render()`, which catches a throwing renderer and falls back to generic, so
// one broken extension never blanks a pane (web-ui.md section 10).
import { html, h, K, useState } from '../core/ui.js';
import { markdown } from '../core/markdown.js';
import generic from './generic.js';
import files from './files.js';
import jev from './jev.js';

// One module per family of tools, each `{ tool: renderer }`: a later slice adds its module to this list.
const named = new Map([files, jev].flatMap((mod) => Object.entries(mod)));
export const register = (tool, mod) => named.set(tool, mod);
export const ctx = Object.freeze({ html, h, K, markdown, useState });

const pick = (tool) => named.get(tool) || generic;

// `render` draws with the shared ctx; `renderCall` hands the renderer the
// call too, as ctx.tool and ctx.input (a read's output wants its path).
export const render = (tool, part, ...args) => draw(ctx, tool, part, args);
export const renderCall = (input, tool, part, ...args) => draw(Object.freeze({ ...ctx, tool, input }), tool, part, args);

// Whether a finished call's row is drawn open: a renderer that has `autoOpen(output, isError)` says so for a result it can draw well (jev's
// bars are the point of the call). A renderer that throws, or has none, is not open: a person's click still opens any row.
export function autoOpen(tool, output, isError) {
  const r = pick(tool);
  try {
    return typeof r.autoOpen === 'function' && r.autoOpen(output, !!isError) === true;
  } catch (e) {
    console.error('renderer failed:', tool, 'autoOpen', e && e.message);
    return false;
  }
}

function draw(c, tool, part, args) {
  const r = pick(tool);
  try {
    if (typeof r[part] === 'function') return r[part](...args, c);
  } catch (e) {
    console.error('renderer failed:', tool, part, e && e.message);
  }
  return generic[part](...args, c);
}
