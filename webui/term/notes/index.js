// The notes tab in the left panel. Today only its slot: the notepad is drawn here by slice N2a. It registers like any tab
// (core/registry.js), so app.js lists nothing of it.
import { html } from '../core/ui.js';
import { leftTabs } from '../core/registry.js';

export const NotesTab = () => html`<div class="faint">notes: the ✎ notepad lands with slice N2a</div>`;

leftTabs.add({ name: 'notes', icon: '✎', component: NotesTab });
