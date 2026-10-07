// Small pieces the sidebars and the inspector tabs share: a line saying what a part needs (`Needs`), a feature that is not built yet
// (`Soon`) and a `key  value` row (`Kv`). Text only, no state of their own.
import { html } from './ui.js';
import { mode } from './api.js';

export const Needs = ({ children }) => html`<div class="needs">${children}</div>`;
// A feature that is not there yet: on the perch the hub exists, so say only that.
export const Soon = ({ what, door }) => html`<${Needs}>${mode() === 'perch' ? what + ': not available yet' : door || what + ' needs the hub'}</${Needs}>`;
export const Kv = ({ k, v, cls = '' }) => html`<div class="kv"><span class="faint">${k}</span><span class=${'grow ' + cls}>${String(v)}</span></div>`;
