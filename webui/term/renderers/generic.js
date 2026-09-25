// The generic tool renderer: what every tool gets until an extension names it
// (web-ui.md section 10; the manifest loader is M5). A renderer is
//   input(input, ctx)             -> one-line summary string for the tool line
//   inputFull(input, ctx)         -> Node | vnode, the whole input (approvals)
//   output(output, isError, ctx)  -> Node | vnode, folded past FOLD lines
// `ctx` carries { html, h, markdown, K } so an extension needs no imports.
const FOLD = 20;
const PREFERRED = ['command', 'cmd', 'path', 'file_path', 'pattern', 'query', 'url', 'condition', 'to', 'text', 'prompt'];

const oneLine = (s) => s.replace(/\s*\n\s*/g, ' ⏎ ');

export default {
  input(input) {
    if (input == null) return '';
    if (typeof input !== 'object') return String(input);
    for (const k of PREFERRED) {
      if (typeof input[k] === 'string' && input[k]) return oneLine(input[k]);
    }
    const first = Object.values(input).find((v) => typeof v === 'string' && v);
    if (first) return oneLine(first);
    const s = JSON.stringify(input);
    return s === '{}' ? '' : s;
  },

  inputFull(input, ctx) {
    if (input == null) return null;
    const txt = typeof input === 'string' ? input : JSON.stringify(input, null, 2);
    return ctx.html`<${Folded} text=${txt} ctx=${ctx} />`;
  },

  output(output, isError, ctx) {
    return ctx.html`<${Folded} text=${String(output ?? '')} err=${isError} ctx=${ctx} />`;
  },
};

// Folding keeps its own local state, so it is a tiny component.
function Folded({ text, err, ctx }) {
  const { html, K, useState } = ctx;
  const [open, setOpen] = useState(false);
  const lines = text.replace(/\n+$/, '').split('\n');
  const cls = 'tout' + (err ? ' err' : '');
  if (lines.length <= FOLD || open) return html`<pre class=${cls}>${lines.join('\n') || '(empty)'}</pre>`;
  return html`<pre class=${cls}>${lines.slice(0, FOLD).join('\n') + '\n'}<${K} cls="more" on=${() => setOpen(true)}>… ${lines.length - FOLD} more lines</${K}></pre>`;
}
