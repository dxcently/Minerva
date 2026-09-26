// edit, write and read, drawn as the TUI draws them (render.rs:2877-3000):
// an edit or a write shows the change it makes under its tool line, from its
// own arguments, while it runs (its output only says it was done); a read's
// output is the file in its language, with the numbers eidolon's read puts
// on each line (fs.rs), its `[showing lines …]` footer a dim note (a unified
// diff, as any tool's, is drawn as one: generic.js).
import generic from './generic.js';
import { Code, Diff, editDiff, extOf, isUnified, numbered, parseUnified } from '../core/syntax.js';

// Lines of a change shown under its tool line until asked for more (render.rs TOOL_LINES).
const PREVIEW = 6;
const pathOf = (input) => (input && (input.path || input.file_path)) || '';

const change = {
  ...generic,
  preview(input, ctx) {
    const d = editDiff(ctx.tool, input);
    return d && ctx.html`<${Diff} lines=${d} name=${extOf(pathOf(input))} cap=${PREVIEW} />`;
  },
  inputFull(input, ctx) {
    const d = editDiff(ctx.tool, input);
    return d ? ctx.html`<${Diff} lines=${d} name=${extOf(pathOf(input))} />` : generic.inputFull(input, ctx);
  },
};

const read = {
  ...generic,
  output(output, isError, ctx) {
    const input = ctx.input;
    const t = String(output ?? '');
    if (isError || !t) return generic.output(output, isError, ctx);
    if (isUnified(t)) return ctx.html`<${Diff} lines=${parseUnified(t)} />`;
    const name = extOf(pathOf(input)), got = numbered(t);
    if (got) {
      return ctx.html`${got.text && ctx.html`<${Code} text=${got.text} nums=${got.nums} name=${name} cap=${40} />`}${got.note && ctx.html`<div class="faint cnote">${got.note}</div>`}`;
    }
    const off = Number(input && input.offset); // the schema takes "10" as well as 10
    const start = Number.isInteger(off) && off > 0 ? off : 1;
    return ctx.html`<${Code} text=${t} name=${name} start=${start} cap=${40} />`;
  },
};

export default { edit: change, write: change, read };
