// The update keys overlay (install/keys.js) and the start screen's install block (panes/home.js), W7. The fingerprint rules, what may be
// pressed, which answers are shown and how, the notice states, and source scans for the wiring. Pure: no DOM, no network. The answers are
// built here the way the perch sends them (perch/src/install/README.md), so a change in their words shows up as a failing test.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { normFp, fpGroups, sameFp, addReady, canRemove, removeReady, stepUp, writeOutcome, unsureNote, keysView } from '../install/keys.js';
import { installLines, jevWords, keysOffered } from '../panes/home.js';

const FP = '0F7A3A5C447EC7EF';                  // as the page holds it
const GROUPED = '0F7A 3A5C 447E C7EF';          // as the perch prints it
const ID = '1122334455667788';
const ONE_OFF = '0F7A3A5C447EC7EE';             // the same key with one digit wrong
const SENT_403 = 'in a terminal: hoot keys allow-web 0F7A 3A5C 447E C7EF, then add within 2 minutes';
const NOT_HERE = 'no installed Minerva here (development server)';
const NIX = 'managed by Nix: update through nix';
const UPDATED = 'the server was updated; start it again';
const ok = (data) => ({ status: 200, data });
const row = (over = {}) => ({ id: ID, fingerprint: GROUPED, state: 'trusted', source: 'added by hand', expires: null, ...over });
const insp = { pubkey: 'untrusted comment: a\nFAKE 1122334455667788 0F7A3A5C447EC7EF\n', id: ID, fingerprint: FP, comment: 'a' };
const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8').replace(/\r\n/g, '\n');

test('a fingerprint is sixteen hex digits: case and spacing are ignored, nothing else is', () => {
  for (const typed of [GROUPED, FP, '0f7a3a5c447ec7ef', ' 0F7A  3A5C\t447E\nC7EF ', '0f7a 3a5c 447e c7ef']) {
    assert.equal(normFp(typed), FP, JSON.stringify(typed));
  }
  assert.deepEqual(fpGroups(GROUPED), ['0F7A', '3A5C', '447E', 'C7EF'], 'four groups of four');
  for (const bad of ['', '   ', '0F7A-3A5C-447E-C7EF', '0F7A3A5C447EC7E', '0F7A3A5C447EC7EFF', '0F7A3A5C447EC7EG', 'ﬀ' + FP.slice(2), null, undefined, 7]) {
    assert.equal(normFp(bad), null, JSON.stringify(bad));
    assert.equal(fpGroups(bad), null, JSON.stringify(bad));
  }
});

test('the comparison: the typed fingerprint must be the key\'s; a blank, a wrong or a non-fingerprint never matches', () => {
  assert.equal(sameFp(GROUPED, FP), true);
  assert.equal(sameFp('0f7a3a5c447ec7ef', GROUPED), true, 'case and spacing both ignored, on either side');
  for (const typed of ['', ' ', 'x', ONE_OFF, ONE_OFF.slice(0, 15), '1122334455667788', GROUPED.slice(0, -1), undefined, null]) {
    assert.equal(sameFp(typed, FP), false, JSON.stringify(typed));
  }
  assert.equal(sameFp('', ''), false, 'two blanks are not a match');
  assert.equal(sameFp(FP, ''), false, 'a key with no fingerprint matches nothing');
  assert.equal(sameFp(FP, 'not a fingerprint'), false);
});

test('a wrong fingerprint never enables [ add key ], and nothing runs while a write does or with no key inspected', () => {
  assert.equal(addReady(insp, GROUPED, false), true);
  for (const typed of ['', 'nope', ONE_OFF, ID, GROUPED.slice(0, 14), '0F7A 3A5C 447E C7EE']) {
    assert.equal(addReady(insp, typed, false), false, JSON.stringify(typed));
  }
  assert.equal(addReady(insp, GROUPED, true), false, 'a key write already running');
  assert.equal(addReady(null, GROUPED, false), false, 'no key inspected yet');
});

test('remove is shown only for a key added by hand, and never for the other sources', () => {
  assert.equal(canRemove(row()), true);
  for (const source of ['compiled in', 'added by a signed rotation', 'other', '', 'added by hand ', 'Added by hand', undefined]) {
    assert.equal(canRemove(row({ source })), false, JSON.stringify(source));
  }
  assert.equal(canRemove(null), false);
  assert.equal(removeReady(row({ source: 'compiled in' }), GROUPED, false), false, 'the release key is never removed from here');
  assert.equal(removeReady(row(), GROUPED, false), true);
  assert.equal(removeReady(row(), ONE_OFF, false), false, 'a wrong fingerprint does not remove either');
  assert.equal(removeReady(row(), GROUPED, true), false, 'nor while a write runs');
});

test('the 403 sentence is shown verbatim, and it is the terminal command the page shows before the add', () => {
  const out = writeOutcome('added', 403, { why: SENT_403 });
  assert.equal(out.text, SENT_403, 'verbatim, not reworded');
  assert.equal(out.relist, undefined, 'a refusal before the write changed nothing: no re-read is needed');
  assert.equal(stepUp(FP, 'add'), SENT_403, 'the step-up line is the perch\'s own words for this key');
  assert.equal(stepUp(FP, 'remove'), SENT_403.replace('then add', 'then remove'));
  assert.equal(stepUp('bad', 'add'), '', 'nothing is shown that is not a fingerprint');
});

test('a 5xx is read by its status, never by its kind: the change may have been made, so the page re-reads the list', () => {
  const why = 'minerva did not give an answer; list the keys to see whether the change was made';
  for (const status of [500, 502, 504]) {
    for (const kind of ['mismatch', 'listed', 'unknown', 'updated', 'nix', 'rotation', undefined]) {
      const out = writeOutcome('added', status, { why, kind });
      assert.equal(out.unsure, true, `${status} ${kind}`);
      assert.equal(out.relist, true, `${status} ${kind}: the list is read again`);
      assert.equal(out.refused, undefined, `${status} ${kind}: not a refusal`);
      assert.equal(out.text, why, `${status} ${kind}: shown as sent`);
    }
  }
  assert.doesNotMatch(writeOutcome('added', 504, { why: 'x', kind: 'listed' }).text, /already in the list/, 'a 504 is never the listed sentence');
  const none = writeOutcome('removed', 0, { error: 'network error' });
  assert.deepEqual([none.unsure, none.relist, none.text], [true, true, 'the server is not answering']);
});

test('after an unsure answer, the list says what happened to the key, and a failed re-read says so', () => {
  const has = keysView(ok([row()]));
  const gone = keysView(ok([]));
  assert.match(unsureNote('added', ID, has), /the list has the key now, so it was added/);
  assert.match(unsureNote('added', ID, gone), /does not have the key yet: a change can still be running/);
  assert.match(unsureNote('removed', ID, has), /still has the key, so it was not removed yet/);
  assert.match(unsureNote('removed', ID, gone), /no longer has the key, so it was removed/);
  assert.equal(unsureNote('added', ID, keysView({ status: 504, data: { why: 'minerva did not answer in time' } })),
    'the list could not be read either: minerva did not answer in time');
});

test('a 200 names the key, a 409 and a 429 say their own sentences, and an answer that is not read is unsure', () => {
  assert.deepEqual(writeOutcome('added', 200, { added: ID }), { ok: true, id: ID, text: '', relist: true });
  assert.deepEqual(writeOutcome('removed', 200, { removed: ID }).id, ID);
  assert.equal(writeOutcome('added', 200, { added: 'zz' }).unsure, true, 'an id that is not hex is not a success');
  assert.equal(writeOutcome('added', 200, { removed: ID }).unsure, true, 'the wrong word is not a success');
  const listed = writeOutcome('added', 409, { why: 'that key is already in the list', kind: 'listed' });
  assert.deepEqual([listed.refused, listed.relist, listed.text], [true, true, 'that key is already in the list']);
  const busy = writeOutcome('added', 429, { why: 'another key change is running; try again in a moment' });
  assert.equal(busy.text, 'another key change is running; try again in a moment');
  assert.equal(busy.relist, undefined);
});

test('the key list: a bare array is the list, and anything else is the sentence to show instead', () => {
  assert.deepEqual(keysView(ok([row()])).rows, [row()]);
  assert.deepEqual(keysView({ status: 404, data: { why: NOT_HERE } }), { rows: null, why: NOT_HERE });
  assert.deepEqual(keysView({ status: 404, data: { why: NIX, kind: 'nix' } }), { rows: null, why: NIX });
  assert.deepEqual(keysView({ status: 404, data: { why: UPDATED, kind: 'updated' } }), { rows: null, why: UPDATED });
  assert.equal(keysView({ status: 503, data: { why: 'minerva is being asked already; try again in a moment' } }).why, 'minerva is being asked already; try again in a moment');
  assert.equal(keysView({ status: 0, data: { error: 'network error' } }).why, 'the server is not answering');
  assert.equal(keysView(ok({ not: 'a list' })).rows, null);
});

test('the notice, installed: the version, the notice text as sent, the plugins and the jev, with their tails', () => {
  const lines = installLines(ok({
    installed: true, version: '0.1.0-gabc1234',
    notice: { line: 'Minerva 0.2.0 available -- run: hoot update', latest: '0.2.0', stale: null, checked: 1791269513 },
    plugins: [{ name: 'browser', version: '0.3.0', how: 'stock' }], jev: { set_up: true, running: true, pid: 4242 },
  }));
  const text = lines.map((l) => l.text);
  assert.ok(text.includes('minerva 0.1.0-gabc1234'));
  assert.deepEqual(lines.find((l) => l.text.startsWith('Minerva 0.2.0')), { text: 'Minerva 0.2.0 available -- run: hoot update', warn: true });
  assert.ok(text.includes('installed by Minerva: browser 0.3.0 (stock); trust and service state are not recorded here'));
  assert.ok(text.includes('jev: running (pid 4242); the openjev weights are not tracked'));
});

test('the notice, installed with no notice and with a stale note; the plugins and jev in their other states', () => {
  const none = installLines(ok({ installed: true, version: '1', notice: null, plugins: [], jev: { set_up: false, running: false } })).map((l) => l.text);
  assert.ok(none.includes('installed by Minerva: none; trust and service state are not recorded here'));
  assert.ok(none.includes('jev: not set up; the openjev weights are not tracked'));
  assert.ok(!installLines(ok({ installed: true, version: '1', notice: null, plugins: [], jev: { set_up: false, running: false } })).some((l) => l.warn));
  const stale = installLines(ok({ installed: true, version: '1', notice: { line: null, latest: null, stale: 'the last check is old', checked: 1 }, plugins: [], jev: { set_up: true, running: false } }));
  assert.ok(stale.some((l) => l.text === 'the last check is old' && l.faint));
  assert.ok(stale.some((l) => l.text === 'jev: set up, not running; the openjev weights are not tracked'));
  assert.equal(jevWords({ set_up: true, running: true }), 'running', 'no pid, no pid shown');
  assert.equal(jevWords(undefined), 'not known');
});

test('the notice, not installed: the why as sent for development, nix and an updated server', () => {
  assert.deepEqual(installLines(ok({ installed: false, why: NOT_HERE })), [{ text: NOT_HERE, faint: true }]);
  assert.deepEqual(installLines(ok({ installed: false, managed: 'nix', why: NIX })), [{ text: NIX, faint: true }]);
  assert.deepEqual(installLines(ok({ installed: false, kind: 'updated', why: UPDATED })), [{ text: UPDATED, faint: true }]);
  assert.deepEqual(installLines({ status: 0, data: { error: 'network error' } }), [{ text: 'the server is not answering', faint: true }]);
  assert.equal(installLines({ status: 500, data: null })[0].text, 'the install could not be read');
  // The same two states with no `why`: the page's own sentence must come out, not the development-server fallback.
  assert.deepEqual(installLines(ok({ installed: false, managed: 'nix' })), [{ text: NIX, faint: true }]);
  assert.deepEqual(installLines(ok({ installed: false, kind: 'updated' })), [{ text: UPDATED, faint: true }]);
});

test('[ update keys ] is offered for an installed Minerva only, and nothing on the page updates', () => {
  assert.equal(keysOffered(ok({ installed: true, version: '1', notice: null, plugins: [], jev: { set_up: false, running: false } })), true);
  for (const d of [{ installed: false, why: NOT_HERE }, { installed: false, managed: 'nix', why: NIX }, { installed: false, kind: 'updated', why: UPDATED }]) {
    assert.equal(keysOffered(ok(d)), false, JSON.stringify(d));
  }
  assert.equal(keysOffered({ status: 0, data: null }), false);
});

test('source scans: the arm is never asked for by the page, the comment stands apart, removal is gated, and no path updates', () => {
  const keys = read('../install/keys.js');
  const home = read('../panes/home.js');
  assert.ok(!/install\/keys\/arm/.test(keys), 'the page never arms: the arm route is the terminal\'s');
  assert.ok(!/innerHTML|dangerouslySetInnerHTML|\beval\(/.test(keys + home), 'no markup from text, no eval');
  assert.ok(/dir="auto" style=\$\{COMMENT\}/.test(keys), 'the comment is its own element, with dir=auto');
  assert.ok(/unicode-bidi: isolate/.test(keys) && /overflow: hidden/.test(keys) && /line-height: var\(--ch\)/.test(keys), 'isolated, clipped, fixed line height');
  assert.ok(/canRemove\(r\) &&/.test(keys), 'the remove button is rendered only where canRemove holds');
  assert.ok(/install\/keys\/add'/.test(keys) && /install\/keys\/remove'/.test(keys) && /install\/keys\/inspect'/.test(keys));
  assert.ok(!/hubPoll/.test(keys), 'a key change is a person\'s action: never in the request lane');
  assert.ok(/hubPoll\('install'\)/.test(home), 'the notice is a GET through the lane');
  assert.ok(/openOverlay\(KeysPanel\)/.test(home), '[ update keys ] opens the overlay');
  assert.ok(!/\[ update \]|\[ update minerva \]|\[ install \]/.test(home + keys), 'no update action anywhere');
  assert.ok(/stepUp\(insp\.fingerprint, 'add'\)/.test(keys), 'the step-up line for an add is built from the inspected key\'s fingerprint');
  assert.ok(keys.includes('off=${!addReady(insp, typed, busy)} on=${add}'), '[ add key ] is off unless the typed fingerprint is the key\'s: the button wiring, not only addReady');
  assert.ok(keys.includes('off=${!removeReady(rm, rmTyped, busy)} on=${remove}'), '[ remove key ] is off unless the typed fingerprint is the row\'s: the button wiring, not only removeReady');
  assert.ok(keys.includes("if (addReady(insp, typed, busy)) write('added'") && keys.includes("if (removeReady(rm, rmTyped, busy)) write('removed'"), 'add and remove re-check the same guard before any write is sent');
  assert.ok(/keyOwners\.add\(/.test(keys), 'the overlay claims its own keys');
  assert.ok(/closest\('\.pane\.keys'\)/.test(keys) && /class="pane frame keys"/.test(keys), 'the claim is the overlay section, found by its class');
});
