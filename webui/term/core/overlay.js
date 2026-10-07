// A panel drawn over the tiles, like the folder picker: one at a time, held as { component, props }. Tiles (app.js) has the slot, so a
// feature opens its own panel from its own folder without editing app.js:
//   openOverlay(KeysPanel, { onDone })      the panel is `<KeysPanel onDone=... />`
//   closeOverlay()
// The component draws its own frame (a `.pane.frame` with `style="inset:0"`, as panes/picker.js does), takes Esc itself and calls
// `closeOverlay()` when it is done; this file only holds what is open. Opening another replaces it.
import { html, signal } from './ui.js';

export const overlay = signal(null); // null, or { component, props }
export const openOverlay = (component, props = {}) => { overlay.value = { component, props }; };
export const closeOverlay = () => { overlay.value = null; };

export function Overlay() {
  const o = overlay.value;
  return o ? html`<${o.component} ...${o.props} />` : null;
}
