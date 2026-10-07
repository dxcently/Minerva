// jev's tools drawn in a chat (renderers/jev.js): the bars (ProbBars) with their ordering, the pick and the margin, and the three renderers' summaries,
// over the real results in tests/fixtures/jev. Pure: no DOM. A vnode is read by walking its children (a component without hooks is called); the hooks
// component (Choice, the "show all" state) is checked by what it is given. Text from a page or a model must stay text: only <pre> and <span>, only class.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import * as J from '../renderers/jev.js';
import { render, renderCall, autoOpen } from '../renderers/index.js';

// As LF: a checkout with core.autocrlf has CRLF sources.
const src = (rel) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8').replace(/\r\n/g, '\n');
const journal = (c) => src(`./fixtures/jev/${c}/journal.ndjson`).trim().split('\n').map((l) => JSON.parse(l));
// The calls of a case: [{ tool, input, content, isError }] in order.
function callsOf(c) {
  const steps = new Map(), out = [];
  for (const r of journal(c)) {
    const k = Object.keys(r.kind)[0], v = r.kind[k];
    if (k === 'WorkflowStep') steps.set(v.call_id, { tool: v.tool, input: JSON.parse(v.input) });
    if (k === 'ToolResult' && steps.has(v.tool_use_id)) out.push({ ...steps.get(v.tool_use_id), content: v.content, isError: !!v.is_error });
  }
  return out;
}

// A vnode as the text it draws, and as the list of element types and attribute names it uses.
function walk(v, acc = { text: '', types: new Set(), attrs: new Set(), styles: [], comps: [] }) {
  if (v == null || v === false || v === true) return acc;
  if (typeof v === 'string' || typeof v === 'number') { acc.text += String(v); return acc; }
  if (Array.isArray(v)) { for (const x of v) walk(x, acc); return acc; }
  if (typeof v.type === 'function') {
    acc.comps.push(v.type.name);
    if (v.type.name === 'Choice') return acc; // hooks: the test holds what it is given
    let drawn;
    try { drawn = v.type({ ...v.props }); } catch { return acc; } // a component with hooks (generic.js's Folded) cannot be called outside a render
    return walk(drawn, acc);
  }
  acc.types.add(v.type);
  for (const a of Object.keys(v.props || {})) {
    if (a === 'children') continue;
    acc.attrs.add(a);
    if (a === 'style') acc.styles.push(v.props[a]);
  }
  return walk(v.props && v.props.children, acc);
}
const textOf = (v) => walk(v).text;
const ZWSP = String.fromCharCode(0x200b), RLO = String.fromCharCode(0x202e), BEL = String.fromCharCode(7);

// ---------------------------------------------------------------- ordering and the pick
test('rank sorts by p, highest first, and equal p keep the order they came in (the first of equals is the top)', () => {
  const r = J.rank([{ label: 'a', p: 0.2 }, { label: 'b', p: 0.4 }, { label: 'c', p: 0.4 }, { label: 'd', p: 0.4 }, { label: 'e', p: 0.1 }]);
  assert.deepEqual(r.map((x) => x.label), ['b', 'c', 'd', 'a', 'e']);
  // Many ties, so an unstable sort would show: 50 equal rows stay in order.
  const flat = Array.from({ length: 50 }, (_, i) => ({ label: 'o' + i, p: 0.02 }));
  assert.deepEqual(J.rank(flat).map((x) => x.label), flat.map((x) => x.label));
});

test('rank drops what is no row: a p that is not a finite number, a label that is not a string', () => {
  const r = J.rank([{ label: 'a', p: 0.5 }, { label: 'b', p: NaN }, { label: 'c', p: '0.9' }, { label: 7, p: 0.9 }, { label: 'd', p: Infinity }, null, 'x', { label: 'e', p: 0 }]);
  assert.deepEqual(r.map((x) => x.label), ['a', 'e']);
  assert.deepEqual(J.rank(null), []);
  assert.deepEqual(J.rank('nope'), []);
});

test('rowsOf puts the call\'s options first in their order, so a tie is broken by the options and not by the engine\'s key order', () => {
  // JS orders integer-like keys first: {"2020", "1999", "b"} comes out 1999, 2020, b whatever the JSON said.
  const probs = JSON.parse('{"2020": 0.4, "b": 0.2, "1999": 0.4}');
  assert.deepEqual(Object.keys(probs), ['1999', '2020', 'b']);
  const rows = J.rowsOf(probs, ['2020', 'b', '1999']);
  assert.deepEqual(rows.map((r) => r.label), ['2020', 'b', '1999']);
  assert.equal(J.rank(rows)[0].label, '2020', 'the option listed first wins the tie');
  assert.equal(J.rank(J.rowsOf(probs))[0].label, '1999', 'without the options it is the object\'s order');
  // A label not in the options follows them; a repeated option is one row; "__proto__" is a label like another.
  assert.deepEqual(J.rowsOf({ x: 0.1, y: 0.2 }, ['y', 'y']).map((r) => r.label), ['y', 'x']);
  assert.deepEqual(J.rowsOf(JSON.parse('{"__proto__": 0.7, "a": 0.3}')).map((r) => r.label), ['__proto__', 'a']);
  assert.deepEqual(J.rowsOf([0.5]), []);
  assert.deepEqual(J.rowsOf(null), []);
});

test('barsModel: the top is the argmax, the first of equals, and it carries the pick mark', () => {
  const m = J.barsModel({ rows: [{ label: 'a', p: 0.4 }, { label: 'b', p: 0.4 }, { label: 'c', p: 0.2 }] });
  assert.deepEqual(m.lines.map((l) => [l.label, l.picked, l.mark]), [['a', true, '>'], ['b', false, ' '], ['c', false, ' ']]);
  assert.equal(m.margin, 0, 'a tie has no margin');
  // A pick that is not the top is marked instead, a pick that is no row falls back to the top.
  assert.deepEqual(J.barsModel({ rows: [{ label: 'a', p: 0.6 }, { label: 'b', p: 0.4 }], pick: 'b' }).lines.map((l) => l.picked), [false, true]);
  assert.deepEqual(J.barsModel({ rows: [{ label: 'a', p: 0.6 }, { label: 'b', p: 0.4 }], pick: 'zzz' }).lines.map((l) => l.picked), [true, false]);
  assert.deepEqual(J.barsModel({ rows: [{ label: 'a', p: 0.6 }, { label: 'b', p: 0.4 }], pick: 7 }).lines.map((l) => l.picked), [true, false]);
  assert.deepEqual(J.barsModel({ rows: [] }).lines, []);
  assert.deepEqual(J.barsModel({}).lines, []);
  assert.deepEqual(J.barsModel().lines, []);
});

test('barsModel: the margin is the top minus the runner-up, and a single row has none', () => {
  assert.ok(Math.abs(J.barsModel({ rows: [{ label: 'a', p: 0.9 }, { label: 'b', p: 0.1 }] }).margin - 0.8) < 1e-12);
  assert.ok(Math.abs(J.barsModel({ rows: [{ label: 'b', p: 0.1 }, { label: 'a', p: 0.9 }, { label: 'c', p: 0.05 }] }).margin - 0.8) < 1e-12, 'rows in any order');
  const one = J.barsModel({ rows: [{ label: 'a', p: 1 }] });
  assert.equal(one.margin, null);
  assert.equal(one.foot, '', 'nothing to say about a margin of one');
});

test('barsModel: the cut keeps the top rows and the pick, and says how many it left out', () => {
  const rows = Array.from({ length: 64 }, (_, i) => ({ label: 'o' + i, p: 1 / 64 + (i === 40 ? 0.001 : 0) }));
  const m = J.barsModel({ rows });
  assert.equal(m.lines.length, 8);
  assert.equal(m.hidden, 56);
  assert.equal(m.lines[0].label, 'o40', 'the highest first');
  assert.deepEqual(m.lines.slice(1).map((l) => l.label), ['o0', 'o1', 'o2', 'o3', 'o4', 'o5', 'o6'], 'then the ties in their order');
  const picked = J.barsModel({ rows, pick: 'o63' });
  assert.equal(picked.lines.length, 9, 'a pick outside the cut is shown after all');
  assert.equal(picked.lines[8].label, 'o63');
  assert.equal(picked.lines[8].picked, true);
  assert.equal(picked.hidden, 55);
  assert.equal(J.barsModel({ rows, max: Infinity }).lines.length, 64);
  assert.equal(J.barsModel({ rows, max: Infinity }).hidden, 0);
  assert.equal(J.barsModel({ rows, max: 3 }).lines.length, 3);
  assert.equal(J.barsModel({ rows, max: 0 }).lines.length, 1, 'never none of many');
});

// ---------------------------------------------------------------- the bar
// The page's font (VT323) has no block characters, so a bar is boxes sized in cells and percents, not glyphs (see the comment in jev.js).
test('a bar is a box `width` cells long with a fill as wide as p; above 1 or below 0 is clamped, a sliver is still seen', () => {
  assert.equal(J.trackStyle(16), 'display:inline-block;position:relative;vertical-align:middle;width:calc(16 * var(--cw));height:calc(var(--ch) * 0.4);background:var(--line)');
  assert.match(J.fillStyle(0.9, 'var(--bright)'), /;width:90\.00%;min-width:1px;background:var\(--bright\)$/);
  assert.match(J.fillStyle(0.5, 'red'), /width:50\.00%/);
  assert.match(J.fillStyle(0, 'red'), /width:0\.00%;min-width:0;/, 'zero is empty');
  assert.match(J.fillStyle(0.004, 'red'), /width:0\.40%;min-width:1px/, 'a p of 0.004 is not drawn as nothing');
  assert.match(J.fillStyle(7, 'red'), /width:100\.00%/, 'above 1 is 1');
  assert.match(J.fillStyle(-3, 'red'), /width:0\.00%/, 'below 0 is 0');
  assert.match(J.tickStyle(0.35), /left:35\.00%;width:1px;background:var\(--orange\)$/);
  // Whatever p is, the model hands the view a fraction in [0, 1].
  for (const p of [0, 0.004, 0.0625, 0.1, 0.35, 0.5, 0.8, 0.999, 1, -3, 7]) {
    const f = J.barsModel({ rows: [{ label: 'a', p }] }).lines[0].frac;
    assert.ok(f >= 0 && f <= 1, `p ${p}`);
  }
  assert.equal(J.barsModel({ rows: [{ label: 'a', p: 7 }] }).lines[0].frac, 1);
  assert.equal(J.barsModel({ rows: [{ label: 'a', p: -3 }] }).lines[0].frac, 0);
  assert.equal(J.barsModel({ rows: [{ label: 'a', p: NaN }, { label: 'b', p: '0.5' }] }).lines.length, 0, 'a p that is no number is no bar');
});

test('the width is the caller\'s number of cells, within sense', () => {
  const rows = [{ label: 'a', p: 0.5 }];
  assert.equal(J.barsModel({ rows }).width, 16);
  assert.equal(J.barsModel({ rows, width: 24 }).width, 24);
  for (const w of [0, 3, 65, 1e9, NaN, -4, 8.5, '16', null]) assert.equal(J.barsModel({ rows, width: w }).width, 16, `${w}`);
  assert.match(walk(J.ProbBars({ rows, width: 24 })).styles[0], /width:calc\(24 \* var\(--cw\)\)/);
});

test('the floor tick is at the same place in every row, and a bar under the floor is orange', () => {
  const rows = [{ label: 'a', p: 0.9 }, { label: 'b', p: 0.1 }];
  const m = J.barsModel({ rows, floor: 0.35 });
  assert.equal(m.floor, 0.35);
  assert.deepEqual(m.lines.map((l) => l.color), ['var(--bright)', 'var(--orange)'], 'the pick is bright, the one under the floor orange');
  const w = walk(J.ProbBars({ rows, floor: 0.35 }));
  const ticks = w.styles.filter((s) => /^position:absolute;top:-30%/.test(s));
  assert.equal(ticks.length, 2, 'a tick in each row');
  assert.ok(ticks.every((s) => s === J.tickStyle(0.35)), 'in the same place');
  // No floor in a chat: no tick, and a bar is bright or faint.
  const chat = walk(J.ProbBars({ rows }));
  assert.equal(chat.styles.filter((s) => /top:-30%/.test(s)).length, 0);
  assert.deepEqual(J.barsModel({ rows }).lines.map((l) => l.color), ['var(--bright)', 'var(--faint)']);
  assert.equal(J.barsModel({ rows, floor: 7 }).floor, 1, 'a floor past 1 is 1');
});

test('the margin line compares with its bound and the top with the floor, and says low when either fails', () => {
  const rows = [{ label: 'a', p: 0.41 }, { label: 'b', p: 0.38 }];
  const low = J.barsModel({ rows, floor: 0.35, margin: 0.05 });
  assert.equal(low.foot, 'margin 0.030 < 0.050 · floor 0.350 (the tick)');
  assert.equal(low.low, true);
  const ok = J.barsModel({ rows: [{ label: 'a', p: 0.9 }, { label: 'b', p: 0.1 }], floor: 0.35, margin: 0.05 });
  assert.equal(ok.foot, 'margin 0.800 ≥ 0.050 · floor 0.350 (the tick)');
  assert.equal(ok.low, false);
  const under = J.barsModel({ rows: [{ label: 'a', p: 0.015 }, { label: 'b', p: 0.01 }], floor: 0.35, margin: 0.001 });
  assert.equal(under.foot, 'margin 0.005 ≥ 0.001 · top 0.015 < floor 0.350 (the tick)');
  assert.equal(under.low, true);
  assert.equal(J.barsModel({ rows }).foot, 'margin 0.030', 'in a chat there is no floor and no bound');
  assert.equal(J.barsModel({ rows }).low, false);
});

// ---------------------------------------------------------------- the component
test('ProbBars draws the pick bright, a row to a line, the cut and the margin line; text only', () => {
  const rows = J.rowsOf({ Egypt: 0.9, Cat: 0.1 }, ['Egypt', 'Cat']);
  const v = J.ProbBars({ rows, pick: 'Egypt' });
  const w = walk(v);
  // The bars are boxes (no text), so the text is the mark, the number and the label: `>` is the ASCII mark, one cell in any font.
  assert.equal(w.text, '>  0.900  Egypt\n   0.100  Cat\nmargin 0.800');
  assert.deepEqual([...w.types].sort(), ['pre', 'span']);
  assert.deepEqual([...w.attrs].sort(), ['class', 'style']);
  assert.deepEqual(w.styles, [J.trackStyle(16), J.fillStyle(0.9, 'var(--bright)'), J.trackStyle(16), J.fillStyle(0.1, 'var(--faint)')]);
  assert.equal(v.props.class, 'tout pbars');
  assert.equal(J.ProbBars({ rows, cls: 'mine' }).props.class, 'mine');
  assert.equal(textOf(J.ProbBars({ rows, head: 'This is Felidae.' })).split('\n')[0], 'This is Felidae.');
  // The cut is a line; with `more` it is a click, with none it is text.
  const many = Array.from({ length: 12 }, (_, i) => ({ label: 'o' + i, p: 0.08 }));
  assert.match(textOf(J.ProbBars({ rows: many })), /\n… 4 more\nmargin 0\.000$/);
  const withMore = walk(J.ProbBars({ rows: many, more: () => {} }));
  assert.match(withMore.text, /… 4 more/);
  assert.ok(withMore.comps.includes('K'), 'a click');
  assert.equal(walk(J.ProbBars({ rows: many, max: Infinity })).text.includes('more'), false);
  assert.equal(textOf(J.ProbBars()), '', 'no rows, nothing');
});

test('hostile text from a page or a model is only text: no element, no attribute, cut, with the lying characters gone', () => {
  const evil = '</pre><img src=x onerror=alert(1)><script>alert(1)</script>';
  const label = `Cat${RLO}${ZWSP}${BEL}\nforged  0.999  ${evil}`;
  const w = walk(J.ProbBars({ rows: [{ label, p: 0.9 }, { label: evil.repeat(30), p: 0.1 }], head: null }));
  assert.deepEqual([...w.types].sort(), ['pre', 'span'], 'no element a label could add');
  assert.deepEqual([...w.attrs].sort(), ['class', 'style'], 'a label is in no attribute');
  assert.ok(w.styles.every((s) => /^[a-z0-9:;.,%()*\- ]+$/.test(s)), 'a style is ours: numbers and CSS words, nothing a label could say');
  assert.ok(w.styles.every((s) => !s.includes('url(') && !s.includes('expression')));
  assert.ok(!w.text.includes(RLO) && !w.text.includes(ZWSP) && !w.text.includes(BEL));
  assert.ok(w.text.includes('<img src=x onerror=alert(1)>'), 'it is shown as what it says');
  const lines = w.text.split('\n');
  assert.equal(lines.length, 3 + 0, 'a newline inside a label does not add a row: one row per option, and the margin');
  assert.ok(lines[1].length < 120, 'cut');
});

test('clean: one line, no control, bidi or zero-width characters, cut at a code point', () => {
  assert.equal(J.clean(`a${BEL}b\n\tc`), 'a b c');
  assert.equal(J.clean(`x${ZWSP}y${RLO}z`), 'xyz');
  assert.equal(J.clean('  many   spaces  '), 'many spaces');
  assert.equal(J.clean(null), '');
  assert.equal(J.clean(undefined), '');
  assert.equal(J.clean(12), '12');
  assert.equal(J.clean('abcdef', 4), 'abc…');
  assert.equal(J.clean('abcd', 4), 'abcd');
  const emoji = String.fromCodePoint(0x1f600).repeat(60);
  const cut = J.clean(emoji, 10);
  assert.equal([...cut].length, 10);
  assert.equal([...cut].slice(0, 9).join(''), String.fromCodePoint(0x1f600).repeat(9), 'a pair is never split');
  assert.equal(J.clean('x', 1), 'x');
  assert.equal(J.clean('xy', 1), '…', 'n of 1 is only the cut mark');
});

// ---------------------------------------------------------------- what the tools send (the real journals)
test('jev_choose: the result of cases a and b parses to rows in the options\' order with the service\'s best', () => {
  const a = callsOf('a').find((c) => c.tool === 'jev_choose');
  const r = J.parseChoose(a.content, a.input);
  assert.deepEqual(r.rows, [{ label: 'Egypt', p: 0.9 }, { label: 'Cat', p: 0.1 }]);
  assert.equal(r.best, 'Egypt');
  const b = callsOf('b').find((c) => c.tool === 'jev_choose');
  const rb = J.parseChoose(b.content, b.input);
  assert.deepEqual(rb.rows.map((x) => x.p), [0.5, 0.5]);
  assert.equal(J.rank(rb.rows)[0].label, 'Egypt', 'a tie: the first option');
  const i = callsOf('i').find((c) => c.tool === 'jev_choose');
  assert.equal(J.parseChoose(i.content, i.input).rows.length, 64);
});

test('jev_choose: an error text, an older shape or junk is not rows, and the generic output draws it', () => {
  for (const out of ['stub: the model is not loaded', '{}', '{"probs": {}}', '{"probs": []}', '{"probs": {"a": "x"}}', 'null', '', '[1]', '{"probs": 5}']) {
    assert.equal(J.parseChoose(out), null, out);
  }
  assert.equal(J.parseChoose('{"probs": {"a": 0.5, "b": "x"}}').rows.length, 1);
  assert.equal(J.parseChoose('{"probs": {"a": 0.5}, "best": 7}').best, null);
  const c = callsOf('c').find((x) => x.tool === 'jev_choose');
  assert.equal(c.isError, true);
  assert.equal(J.parseChoose(c.content, c.input), null);
  const v = renderCall(c.input, 'jev_choose', 'output', c.content, true);
  assert.deepEqual(walk(v).comps, ['Folded'], 'an error is the generic folded text, not bars');
});

test('jev_choose output, through the registry, is the Choice with the rows and the best', () => {
  const a = callsOf('a').find((c) => c.tool === 'jev_choose');
  const v = renderCall(a.input, 'jev_choose', 'output', a.content, false);
  assert.equal(v.type.name, 'Choice');
  assert.deepEqual(v.props.rows, [{ label: 'Egypt', p: 0.9 }, { label: 'Cat', p: 0.1 }]);
  assert.equal(v.props.best, 'Egypt');
});

test('jev_choose input: the page it is on, where it wants to get, and the options', () => {
  const a = callsOf('a').find((c) => c.tool === 'jev_choose');
  assert.equal(render('jev_choose', 'input', a.input), 'Ancient Egypt → Felidae · 2 options: Egypt, Cat');
  assert.equal(render('jev_choose', 'input', { context: 'refund or sales?', options: ['refund', 'sales', 'technical support'] }), '3 options: refund, sales, technical support');
  assert.equal(render('jev_choose', 'input', { context: 'x', options: Array.from({ length: 9 }, (_, i) => 'o' + i) }), '9 options: o0, o1, o2, o3, o4, o5, …');
  assert.equal(render('jev_choose', 'input', { context: 'x', options: ['only'] }), '1 option: only');
  // No options array: whatever the generic summary says, and no throw.
  assert.equal(render('jev_choose', 'input', { context: 'just this' }), 'just this');
  assert.equal(render('jev_choose', 'input', null), '');
  assert.equal(render('jev_choose', 'input', 'plain'), 'plain');
});

test('jev_entail: the three scores per hypothesis, the service\'s label picked, the premise against the first hypothesis', () => {
  const e = callsOf('a').find((c) => c.tool === 'jev_entail');
  const rs = J.parseEntail(e.content);
  assert.equal(rs.length, 1);
  assert.equal(rs[0].hypothesis, 'This is the encyclopedia article about Felidae.');
  assert.equal(rs[0].label, 'neutral');
  assert.deepEqual(rs[0].rows, [{ label: 'entailment', p: 0.1 }, { label: 'neutral', p: 0.8 }, { label: 'contradiction', p: 0.1 }]);
  const w = walk(renderCall(e.input, 'jev_entail', 'output', e.content, false));
  assert.equal(w.text, 'This is the encyclopedia article about Felidae.\n>  0.800  neutral\n   0.100  entailment\n   0.100  contradiction\nmargin 0.700');
  assert.equal(render('jev_entail', 'input', e.input), 'Title: Ancient Egypt - Wikipedia. Heading: Anci… vs This is the encyclopedia article about Felidae.');
  assert.equal(render('jev_entail', 'input', { premise: 'p', hypotheses: ['a', 'b', 'c'] }), 'p vs a (+2 more)');
  assert.equal(render('jev_entail', 'input', { premise: 'p', hypotheses: [] }), 'p');
  for (const out of ['error text', '{}', '{"results": []}', '{"results": [{"scores": {}}]}', '{"results": 5}']) assert.equal(J.parseEntail(out), null, out);
  // Several hypotheses are several blocks, and past six the rest is a line.
  const many = JSON.stringify({ results: Array.from({ length: 8 }, (_, i) => ({ label: 'neutral', hypothesis: 'h' + i, scores: { neutral: 0.6, entailment: 0.3, contradiction: 0.1 } })) });
  const t = textOf(renderCall({ premise: 'p', hypotheses: [] }, 'jev_entail', 'output', many, false));
  assert.match(t, /h5\n/);
  assert.ok(!t.includes('h6'));
  assert.match(t, /… 2 more hypotheses$/);
});

test('jev_mark: where a run is, as the line a row shows and the sentence it opens to', () => {
  const marks = callsOf('b').filter((c) => c.tool === 'jev_mark');
  const lines = marks.map((m) => render('jev_mark', 'input', m.input));
  assert.deepEqual(lines, [
    'enter open · step 0 · 0 actions', 'enter reading · step 0 · 1 action', 'park reading · step 1 · 2 actions',
    'enter reading · step 1 · 3 actions', 'enter reading · step 2 · 5 actions', 'final arrived · step 2 · 6 actions',
  ]);
  const said = (m) => walk(renderCall(m.input, 'jev_mark', 'output', m.content, false));
  assert.equal(said(marks[2]).text, 'parked at reading (step 1): waiting for a person');
  assert.equal(said(marks[5]).text, 'finished at arrived (step 2)');
  assert.equal(said(marks[0]).text, 'at open (step 0)');
  const e = callsOf('e').filter((c) => c.tool === 'jev_mark');
  const failed = said(e[e.length - 1]);
  assert.equal(failed.text, 'finished at failed (step 0)');
  assert.match(textOf(renderCall({ graph: 'g', kind: 'end', state: 'x', outcome: 'exhausted', step: 30, actions: 70 }, 'jev_mark', 'output', 'marked g end x', false)), /ended: exhausted \(step 30\)/);
  assert.equal(render('jev_mark', 'input', { graph: 'g', kind: 'end', state: 'work.x', step: 30, actions: 70, outcome: 'exhausted' }), 'end exhausted · work.x · step 30 · 70 actions');
  // A mark without a kind and state is the generic summary, and its output the generic output.
  assert.equal(render('jev_mark', 'input', { graph: 'g' }), 'g');
  assert.equal(render('jev_mark', 'input', null), '');
});

test('the output of a mark says its color: a park is orange, a failed final red, an arrival bright, an end red', () => {
  const cls = (input) => renderCall(input, 'jev_mark', 'output', 'marked', false).props.children.props.class;
  assert.equal(cls({ graph: 'g', kind: 'park', state: 's' }), 'o');
  assert.equal(cls({ graph: 'g', kind: 'final', state: 'failed' }), 'r');
  assert.equal(cls({ graph: 'g', kind: 'final', state: 'work.failed' }), 'r');
  assert.equal(cls({ graph: 'g', kind: 'final', state: 'arrived' }), 'hi');
  assert.equal(cls({ graph: 'g', kind: 'end', state: 's', outcome: 'stopped' }), 'r');
  assert.equal(cls({ graph: 'g', kind: 'enter', state: 's' }), 'faint');
});

// ---------------------------------------------------------------- the registry
test('the registry knows the three tools, leaves the others generic, and says which results are drawn open', () => {
  const a = callsOf('a');
  const choose = a.find((c) => c.tool === 'jev_choose'), entail = a.find((c) => c.tool === 'jev_entail'), mark = a.find((c) => c.tool === 'jev_mark');
  assert.equal(autoOpen('jev_choose', choose.content, false), true);
  assert.equal(autoOpen('jev_entail', entail.content, false), true);
  assert.equal(autoOpen('jev_mark', mark.content, false), false, 'a mark is one line already');
  assert.equal(autoOpen('jev_choose', choose.content, true), false, 'an error opens by itself anyway');
  assert.equal(autoOpen('jev_choose', 'stub: the model is not loaded', false), false);
  assert.equal(autoOpen('jev_choose', undefined, false), false);
  assert.equal(autoOpen('bash', choose.content, false), false, 'only a tool whose renderer says so');
  assert.equal(autoOpen('read', 'x', false), false);
  assert.equal(autoOpen('nosuchtool', 'x', false), false);
  // The other tools of the run stay as they were.
  const snap = a.find((c) => c.tool === 'browser_snapshot'), click = a.find((c) => c.tool === 'browser_click');
  assert.equal(render('browser_snapshot', 'input', snap.input), render('nosuchtool', 'input', snap.input));
  assert.equal(render('browser_click', 'input', click.input), 'e3');
  // Registered ahead of the generic one: the generic summary of jev_choose would be the whole context.
  assert.notEqual(render('jev_choose', 'input', a.find((c) => c.tool === 'jev_choose').input), render('nosuchtool', 'input', a.find((c) => c.tool === 'jev_choose').input));
});
