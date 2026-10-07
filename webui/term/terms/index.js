// The terminals' tab in the right panel. Today only its slot: the stack of web terminals is drawn here by slice T4a. It registers like any
// tab (core/registry.js), so app.js lists nothing of it; global, so it is drawn with no chat in view.
import { html } from '../core/ui.js';
import { rightTabs } from '../core/registry.js';

export const TermsTab = () => html`<div class="faint">terminals: the ⌨ stack lands with slice T4a</div>`;

rightTabs.add({ name: 'terms', icon: '⌨︎', component: TermsTab, global: true });
