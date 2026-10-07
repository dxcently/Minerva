// The graph tab. Today only its slot: the jev runs and the chat's turn graph are drawn here by later slices. It gets { s, sid } like every tab.
import { html } from '../core/ui.js';
import { Soon } from '../core/bits.js';

// The graph tab's slot. M3 replaces this with a Preact graph component
// (see web-ui.md section 10); it gets the same { s, sid } props as every tab.
export const GraphTab = () => html`<${Soon} what="the turn graph" door="turn graph needs the hub (M3)" />`;
