// The mesh tab of the left panel: the nodes this machine knows. Today only the table head and the sentence about what it needs; the rows come
// from the perch's /hub/mesh in a later slice.
import { html } from '../core/ui.js';
import { Soon } from '../core/bits.js';

export function MeshTab() {
  return html`<div class="note">node        paired  grants  seen</div>
      <div class="note dim">──────────  ──────  ──────  ────</div>
      <${Soon} what="the mesh" door="mesh reads Aoide through the hub (M3)" />`;
}
