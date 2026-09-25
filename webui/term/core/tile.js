// Dwindle tiling at the golden ratio (web-ui.md 4.1) as a real split tree.
//
//   leaf  = { leaf: paneId }
//   split = { dir: 'row' | 'col', ratio, a, b }   a gets `ratio` of the box
//
// A new pane splits the FOCUSED leaf along that leaf's longer side: the old
// pane keeps 0.618 (`a`), the new one gets the rest (`b`). Every split keeps
// its own ratio, which dragging its divider changes. Closing a leaf promotes
// its sibling into the parent's place. The tree is plain data: replace it,
// never mutate it, so a signal holding it notices.
export const PHI = 0.618;
const MIN = 0.12;

export const leaf = (id) => ({ leaf: id });

// Split leaf `at` (or the root when the tree is empty) to add `id`.
// `box` is the pixel size of the tile area, used to pick the longer side.
export function add(tree, at, id, box) {
  if (!tree) return leaf(id);
  const rects = layout(tree, box);
  const r = rects.panes.find((p) => p.id === at) || rects.panes[rects.panes.length - 1];
  const target = r.id;
  const dir = r.w >= r.h ? 'row' : 'col';
  const swap = (n) => (n.leaf === target ? { dir, ratio: PHI, a: n, b: leaf(id) }
    : n.leaf != null ? n : { ...n, a: swap(n.a), b: swap(n.b) });
  return swap(tree);
}

// Remove leaf `id`; its sibling takes the parent's place.
export function remove(tree, id) {
  if (!tree || tree.leaf === id) return null;
  if (tree.leaf != null) return tree;
  if (tree.a.leaf === id) return tree.b;
  if (tree.b.leaf === id) return tree.a;
  return { ...tree, a: remove(tree.a, id), b: remove(tree.b, id) };
}

// Set the ratio of the split at `path` (a string of 'a'/'b' steps from the root).
export function setRatio(tree, path, ratio) {
  const r = Math.min(1 - MIN, Math.max(MIN, ratio));
  const go = (n, i) => (i === path.length ? { ...n, ratio: r } : { ...n, [path[i]]: go(n[path[i]], i + 1) });
  return go(tree, 0);
}

export const leaves = (t) => (!t ? [] : t.leaf != null ? [t.leaf] : [...leaves(t.a), ...leaves(t.b)]);

// Pixel rectangles for every pane and every divider, snapped to cells.
// Neighbouring panes overlap by 1px so they share one border line.
export function layout(tree, { w, h, cw = 1, ch = 1 }) {
  const panes = [], dividers = [];
  const walk = (n, x, y, W, H, path) => {
    if (!n) return;
    if (n.leaf != null) { panes.push({ id: n.leaf, x, y, w: W, h: H }); return; }
    if (n.dir === 'row') {
      const a = Math.round((W * n.ratio) / cw) * cw;
      walk(n.a, x, y, a, H, path + 'a');
      walk(n.b, x + a, y, W - a, H, path + 'b');
      dividers.push({ path, dir: 'row', x: x + a, y, w: 0, h: H, origin: x, size: W });
    } else {
      const a = Math.round((H * n.ratio) / ch) * ch;
      walk(n.a, x, y, W, a, path + 'a');
      walk(n.b, x, y + a, W, H - a, path + 'b');
      dividers.push({ path, dir: 'col', x, y: y + a, w: W, h: 0, origin: y, size: H });
    }
  };
  walk(tree, 0, 0, w, h, '');
  return { panes, dividers };
}
