// The inspector's diff tab: the files this chat's edit and write calls touched. app.js lists it in INSPECTOR; it gets { s, sid } like every tab.
import { html } from '../core/ui.js';
import { Soon } from '../core/bits.js';

// Git state (branch, +/−, PR) belongs at the top of this tab once a source exists.
export function DiffTab({ s }) {
  const files = [...s.touched.value];
  return html`<div class="hdr">git</div><${Soon} what="branch, +/− and PR" door="branch, +/− and PR need the hub" />
    <div class="hdr">changed</div>
    ${files.length ? files.map(([f, n]) => html`<div class="kv"><span class="grow c" title=${f}>${f}</span><span class="faint">${n} call${n > 1 ? 's' : ''}</span></div>`)
      : html`<div class="faint">no edit or write calls yet</div>`}`;
}
