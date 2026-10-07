// What vendor/ must stay true to, offline: node --test webui/term/tests/vendor.test.mjs
// (the tarball sha512 check needs the network: sh webui/term/vendor/update.sh --check)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { join } from 'node:path';

const vendor = fileURLToPath(new URL('../vendor/', import.meta.url));
const read = (name) => readFileSync(join(vendor, name), 'utf8');
const load = (name) => import(pathToFileURL(join(vendor, name)).href);

// package -> the files update.sh takes from it
const FILES = {
  'preact': ['preact.js', 'hooks.js', 'LICENSE-preact'],
  '@preact/signals-core': ['signals-core.js', 'LICENSE-signals-core'],
  '@preact/signals': ['signals.js', 'LICENSE-signals'],
  'htm': ['htm.js', 'LICENSE-htm'],
  '@dagrejs/graphlib': ['graphlib.js', 'LICENSE-graphlib'],
  '@dagrejs/dagre': ['dagre.js', 'LICENSE-dagre'],
  '@xterm/xterm': ['xterm.js', 'xterm.css', 'LICENSE-xterm'],
  '@xterm/addon-fit': ['addon-fit.js', 'LICENSE-xterm-addon-fit'],
};
const jsFiles = () => readdirSync(vendor).filter((f) => f.endsWith('.js'));

// the same patterns update.sh refuses (bare static, bare dynamic, eval-like)
const Q = '["\'`]';
const BARE_STATIC = new RegExp(`(?:from|import) *${Q}[^./"'\`][^"'\`]*${Q}`);
const BARE_DYNAMIC = new RegExp(`import *\\( *${Q}[^./"'\`][^"'\`]*${Q}`);
const EVAL_LIKE = /(?:^|[^A-Za-z0-9_$])(?:eval|Function) *\(|new +Function(?:[^A-Za-z0-9_$]|$)/;

test('VERSIONS pins exactly the packages update.sh vendors, each with a sha512', () => {
  const pins = read('VERSIONS')
    .split('\n')
    .filter((l) => l.trim() && !l.startsWith('#'))
    .map((l) => l.trim().split(/\s+/));
  assert.deepEqual(pins.map((p) => p[0]).sort(), Object.keys(FILES).sort());
  for (const [name, version, integrity] of pins) {
    assert.match(version, /^\d+\.\d+\.\d+$/, `${name} version`);
    assert.match(integrity, /^sha512-[A-Za-z0-9+/]{86}==$/, `${name} integrity (base64 of 64 bytes)`);
  }
});

test('every pinned package has its files and a licence', () => {
  for (const [pkg, files] of Object.entries(FILES)) {
    for (const f of files) {
      assert.ok(existsSync(join(vendor, f)), `${pkg}: vendor/${f} is missing`);
      assert.ok(read(f).length > 500 || f.startsWith('LICENSE'), `${pkg}: vendor/${f} is nearly empty`);
    }
    const lic = read(files.find((f) => f.startsWith('LICENSE')));
    assert.match(lic, /copyright|license/i, `${pkg}: LICENSE text`);
  }
});

test('vendor/ holds no file update.sh does not produce (no stray addon)', () => {
  const known = new Set(['VERSIONS', 'update.sh', 'README.md', ...Object.values(FILES).flat()]);
  for (const f of readdirSync(vendor)) assert.ok(known.has(f), `unexpected vendor/${f}`);
  // never vendored: webgl, canvas (5.x only), clipboard (OSC 52), web-links
  for (const f of readdirSync(vendor)) assert.doesNotMatch(f, /webgl|canvas|clipboard|web-?links/i, f);
});

test('no vendored file has a CR: the pins are checked on LF bytes (.gitattributes: vendor/** eol=lf)', () => {
  for (const f of readdirSync(vendor)) {
    assert.ok(!readFileSync(join(vendor, f)).includes(13), `vendor/${f} has CR bytes; rewrite it with sh vendor/update.sh`);
  }
});

test('every js file: relative imports only, all resolving; no eval, Function, sourceMappingURL', () => {
  for (const f of jsFiles()) {
    const src = read(f);
    assert.doesNotMatch(src, BARE_STATIC, `${f}: bare import specifier`);
    assert.doesNotMatch(src, BARE_DYNAMIC, `${f}: bare dynamic import()`);
    assert.doesNotMatch(src, EVAL_LIKE, `${f}: eval( or Function( or new Function (the CSP has no unsafe-eval)`);
    assert.doesNotMatch(src, /^\/\/# sourceMappingURL=/m, `${f}: sourceMappingURL (the .map files are not vendored)`);
    for (const m of src.matchAll(/(?:from|import) *["'](\.{1,2}\/[^"']+)["']/g)) {
      assert.ok(existsSync(join(vendor, m[1])), `${f}: imports ${m[1]}, which is not in vendor/`);
    }
  }
});

test('xterm.js: exports Terminal, nothing bare, no eval, no new Function', async () => {
  const src = read('xterm.js');
  const exported = [...src.matchAll(/export\s*\{([^}]*)\}/g)].flatMap((m) =>
    m[1].split(',').map((s) => s.trim().split(/\s+as\s+/).pop()),
  );
  assert.ok(exported.includes('Terminal'), `exports: ${exported}`);
  assert.doesNotMatch(src, BARE_STATIC);
  assert.doesNotMatch(src, BARE_DYNAMIC);
  assert.doesNotMatch(src, EVAL_LIKE);
  assert.doesNotMatch(src, /new +Function/);
  // and it loads under node (no DOM needed to import it)
  assert.equal(typeof (await load('xterm.js')).Terminal, 'function');
});

test('addon-fit.js exports FitAddon; xterm.css is plain css with no @import', async () => {
  assert.equal(typeof (await load('addon-fit.js')).FitAddon, 'function');
  const css = read('xterm.css');
  assert.match(css, /\.xterm\b/);
  assert.doesNotMatch(css, /@import|url\(\s*["']?https?:/i);
});

// 3 nodes: a -> b, a -> c, top to bottom
function layout3(dagre, Graph) {
  const g = new Graph({ directed: true });
  g.setGraph({ rankdir: 'TB', nodesep: 20, ranksep: 30 });
  g.setDefaultEdgeLabel(() => ({}));
  g.setNode('a', { width: 100, height: 40 });
  g.setNode('b', { width: 80, height: 40 });
  g.setNode('c', { width: 60, height: 40 });
  g.setEdge('a', 'b');
  g.setEdge('a', 'c');
  dagre.layout(g);
  return g;
}

for (const which of ['dagre.graphlib.Graph', 'graphlib.js Graph']) {
  test(`dagre.js lays out a 3-node graph (built with ${which})`, async () => {
    const dagre = await load('dagre.js');
    assert.equal(typeof dagre.layout, 'function');
    const Graph = which === 'graphlib.js Graph' ? (await load('graphlib.js')).Graph : dagre.graphlib.Graph;
    const g = layout3(dagre, Graph);
    const [a, b, c] = ['a', 'b', 'c'].map((id) => g.node(id));
    for (const n of [a, b, c]) {
      assert.ok(Number.isFinite(n.x) && Number.isFinite(n.y), 'every node got a position');
    }
    assert.ok(a.y < b.y, 'parent is above its children');
    assert.equal(b.y, c.y, 'siblings share a rank');
    assert.ok(Math.abs(b.x - c.x) >= (b.width + c.width) / 2 + 20, 'siblings keep nodesep apart');
    assert.ok(a.x > Math.min(b.x, c.x) && a.x < Math.max(b.x, c.x), 'parent sits between its children');
    const { width, height } = g.graph();
    assert.ok(width >= 160 && height >= 40 * 2 + 30, `graph box ${width}x${height}`);
    const pts = g.edge('a', 'b').points;
    assert.ok(pts.length >= 2, 'the edge got a route');
    assert.ok(Math.abs(pts[0].y - (a.y + a.height / 2)) < 1, 'route starts at the parent bottom');
    assert.ok(Math.abs(pts.at(-1).y - (b.y - b.height / 2)) < 1, 'route ends at the child top');
  });
}
