// The update keys (W7): the keys `minerva update` trusts, and how one is added to or removed from that list from this page. It opens over
// the tiles from `[ update keys ]` on the start screen (panes/home.js), through core/overlay.js, and closes with `[ close ]` or Esc.
//
// The perch does the trusting; this page only shows and asks. Every answer is shown as the sentence the perch sent. A key change is a
// person's action, so each one is a hubGet or a hubPost, never the request lane. The page cannot arm a change: an add or a remove the perch
// has not been armed for answers 403 with the terminal command (`hoot keys allow-web <FP>`), and only that command, typed in a terminal,
// lets it run. The fingerprint typed back is checked here as well, but a script in this page's origin could show another key, so the
// fingerprint is also to be compared with the terminal's print and the maintainer's published one.
import { html, K, useState, useEffect, useRef } from '../core/ui.js';
import { hubGet, hubPost } from '../core/api.js';
import { closeOverlay } from '../core/overlay.js';
import { keyOwners } from '../core/registry.js';

const LIST_MS = 10000; // the perch answers a key read in 5 s at most (a 504 after that): wait a little longer than that

// ---------------------------------------------------------------- the fingerprint
// Sixteen hex digits, printed in four groups of four: "0F7A 3A5C 447E C7EF". What a person types back may have any case and any spacing,
// and nothing else is a fingerprint: a text that is not one matches nothing. Only ASCII letters are raised, so no other character can
// stand for a hex digit.
const HEX16 = /^[0-9A-F]{16}$/;
export function normFp(text) {
  if (typeof text !== 'string') return null;
  const s = text.replace(/\s+/g, '').replace(/[a-f]/g, (c) => c.toUpperCase());
  return HEX16.test(s) ? s : null;
}
// The fingerprint as four groups of four, or null when it is not one (nothing that is not hex is ever shown as one).
export function fpGroups(text) {
  const s = normFp(text);
  return s ? [s.slice(0, 4), s.slice(4, 8), s.slice(8, 12), s.slice(12)] : null;
}
// Whether what was typed back is the fingerprint: both must be fingerprints, and they must be the same one. Nothing matches a blank.
export function sameFp(typed, fp) {
  const a = normFp(typed), b = normFp(fp);
  return a !== null && b !== null && a === b;
}
// A key id: sixteen upper-case hex digits, as the perch sends them. Anything else is not an id.
const normId = (s) => (typeof s === 'string' && /^[0-9A-F]{16}$/.test(s) ? s : null);

// ---------------------------------------------------------------- what may be pressed
// `[ add key ]`: a key inspected, the fingerprint typed back for exactly that key, and no write running. A wrong or empty one never enables it.
export const addReady = (insp, typed, busy) => !busy && !!insp && sameFp(typed, insp.fingerprint);
// `[ remove ]` is offered only on a key added by hand (ruling 6): the compiled-in key, a key that came with a signed rotation and any
// other source are never offered it, whatever the perch would answer.
export const canRemove = (row) => !!row && row.source === 'added by hand';
export const removeReady = (row, typed, busy) => !busy && canRemove(row) && sameFp(typed, row.fingerprint);
// The step-up line for a change: the terminal command in the perch's own words, so it reads as the 403 does (`verb` is add or remove).
export function stepUp(fp, verb) {
  const g = fpGroups(fp);
  return g ? `in a terminal: hoot keys allow-web ${g.join(' ')}, then ${verb} within 2 minutes` : '';
}

// ---------------------------------------------------------------- answers
// The perch's sentence, as sent: a `why` is shown whole, never reworded or cut here.
const whyOf = (data) => (data && typeof data.why === 'string' ? data.why : '');

// What a key write's answer says, as { ok, id, text, relist, unsure, refused }. A 5xx, or no answer at all, is read by its status alone:
// the change may or may not have been made, so the page re-reads the list and says what it shows (`unsure`); `kind` is never read there.
// A 409 is a refusal made before the write began, so nothing changed. A 403 is the arm that is missing: its sentence is the terminal command,
// shown whole. `word` is what a good answer names: 'added' or 'removed'.
export function writeOutcome(word, status, data) {
  if (status === 0 || status >= 500) {
    return { unsure: true, relist: true, text: whyOf(data) || (status === 0 ? 'the server is not answering' : 'the server did not answer in time') };
  }
  if (status === 200) {
    const id = normId(data && data[word]);
    return id ? { ok: true, id, text: '', relist: true } : { unsure: true, relist: true, text: 'the answer was not one this page reads' };
  }
  if (status === 403) return { text: whyOf(data) || 'that change is not allowed from this page' };
  if (status === 409) return { refused: true, relist: true, text: whyOf(data) || 'nothing changed' };
  return { text: whyOf(data) || `the key could not be ${word} (${status})` };
}

// After an unsure answer: what the list, read again, says of the key. The list is the only witness the page has, so the words say that.
export function unsureNote(word, id, view) {
  if (!view.rows) return `the list could not be read either: ${view.why}`;
  const has = view.rows.some((row) => row.id === id);
  if (word === 'added') {
    return has ? 'the list has the key now, so it was added' : 'the list does not have the key yet: a change can still be running, so list again in a moment';
  }
  return has ? 'the list still has the key, so it was not removed yet: list again in a moment' : 'the list no longer has the key, so it was removed';
}

// `GET /hub/install/keys` as rows, or the sentence to show instead. A bare array is the list; anything else is not one.
export function keysView(res) {
  const status = res ? res.status : 0;
  if (status === 200 && res && Array.isArray(res.data)) return { rows: res.data, why: '' };
  const why = whyOf(res && res.data);
  return { rows: null, why: why || (status === 0 ? 'the server is not answering' : `the keys could not be read (${status})`) };
}

// The panel takes its own keys: what is typed in it (ctrl+enter, `:`, `?`, Tab over the asks) never reaches the page's keys, which
// app.js runs from its document handler (core/registry.js keyOwners). Only this section has the class, so only its own elements claim.
keyOwners.add((el) => !!el && !!el.closest && !!el.closest('.pane.keys'));

// ---------------------------------------------------------------- the panel
// Drawn over the tiles: absolute, as panes/picker.js is. The look is the page's own: frames, bracketed text actions, faint notes.
const OVERLAY = 'position: absolute; inset: 0; z-index: 30; background: var(--bg); overflow: hidden;';
const FIELD = 'background: transparent; color: var(--fg); border: 0; border-bottom: 1px solid var(--line); font: inherit; padding: 0; border-radius: 0; outline: none; width: 100%;';
const BIG = 'font-size: calc(var(--fs) * 2); line-height: calc(var(--ch) * 2); white-space: pre;';
const MSG = 'min-height: var(--ch); color: var(--yellow); white-space: pre-wrap;';
// The comment is the key file's own words, so it is its own element: its own direction, a fixed line height, a bounded height. It is never
// joined with the fingerprint, so bidi runs or stacked marks in it cannot change how the fingerprint reads.
const COMMENT = 'unicode-bidi: isolate; overflow: hidden; overflow-wrap: anywhere; white-space: pre-wrap; line-height: var(--ch); max-height: calc(var(--ch) * 3);';
const ROW = 'margin-bottom: var(--ch);';

export function KeysPanel() {
  const [list, setList] = useState(null);      // null until the first read; then keysView's { rows, why }
  const [pub, setPub] = useState('');          // the pasted key text
  const [insp, setInsp] = useState(null);      // the key inspected: { pubkey, id, fingerprint, comment }
  const [typed, setTyped] = useState('');      // the fingerprint typed back for the inspected key
  const [rm, setRm] = useState(null);          // the row whose removal is open, or null
  const [rmTyped, setRmTyped] = useState('');  // the fingerprint typed back for that row
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState('');
  const ref = useRef(null);
  const live = useRef(true);

  // The list is read by a person opening the panel and after every change: a person's action, so straight out, not in the lane.
  const relist = async () => {
    const view = keysView(await hubGet('install/keys', LIST_MS));
    if (live.current) setList(view);
    return view;
  };

  useEffect(() => {
    live.current = true;
    relist();
    const first = ref.current && ref.current.querySelector('textarea');
    if (first) first.focus({ preventScroll: true });
    return () => { live.current = false; };
  }, []);

  const inspect = async () => {
    const text = pub;
    if (busy || !text.trim()) return;
    setBusy(true); setMsg('');
    const res = await hubPost('install/keys/inspect', { pubkey: text });
    if (!live.current) return;
    setBusy(false);
    const d = res.data;
    if (res.status === 200 && d && normId(d.id) && normFp(d.fingerprint) && typeof d.comment === 'string') {
      setInsp({ pubkey: text, id: d.id, fingerprint: normFp(d.fingerprint), comment: d.comment });
      setTyped('');
    } else {
      setMsg(whyOf(d) || (res.status === 0 ? 'the server is not answering' : `the key could not be read (${res.status})`));
    }
  };

  // One key change. Whatever the answer, a change that may have been made is followed by a read of the list, and the page says what the list says.
  const write = async (word, id, path, body) => {
    setBusy(true); setMsg('');
    const res = await hubPost(path, body);
    const out = writeOutcome(word, res.status, res.data);
    const view = out.relist ? await relist() : null;
    if (!live.current) return;
    setBusy(false);
    if (out.ok) {
      setMsg(`the key was ${word}: ${out.id}`);
      if (word === 'added') { setInsp(null); setTyped(''); setPub(''); } else { setRm(null); setRmTyped(''); }
    } else if (out.unsure) {
      setMsg(`${out.text}. ${unsureNote(word, id, view)}`);
    } else {
      setMsg(out.text);
    }
  };

  const add = () => { if (addReady(insp, typed, busy)) write('added', insp.id, 'install/keys/add', { pubkey: insp.pubkey, confirm: normFp(typed) }); };
  const openRemove = (row) => { setRm(row); setRmTyped(''); setMsg(''); };
  const remove = () => { if (removeReady(rm, rmTyped, busy)) write('removed', rm.id, 'install/keys/remove', { id: rm.id, confirm: normFp(rmTyped) }); };
  const esc = (e) => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeOverlay(); } };
  const enter = (fn) => (e) => { if (e.key === 'Enter') { e.preventDefault(); fn(); } };

  const rows = list && list.rows;
  const grp = insp ? fpGroups(insp.fingerprint) : null;
  const rmGrp = rm ? fpGroups(rm.fingerprint) : null;
  return html`<section class="pane frame keys" style=${OVERLAY} ref=${ref} onKeyDown=${esc}>
    <div class="ftitle">update keys</div>
    <div class="scroll"><div class="kb">
      <div class="faint" style=${ROW}>the keys minerva trusts when it updates. Each row says what state the key is in and whether it expires.</div>
      <div class="faint" style=${ROW}>the terminal arm guards against this page alone, not against someone who holds this page's cookie once web terminals exist.</div>
      <div class="faint" style=${ROW}>agents run as you: an agent can add a key through the CLI without this page; Minerva's guard rows only slow that down.</div>

      <div class="hdr">keys</div>
      ${list === null && html`<div class="faint" style=${ROW}>reading the keys…</div>`}
      ${rows === null && list && html`<div style=${ROW}>${list.why}</div>`}
      ${Array.isArray(rows) && rows.map((r) => html`<div class="krow" key=${r.id} style=${ROW}>
        <div>${r.id} <span class="faint">${r.state}, ${r.source}, ${r.expires ? 'expires ' + r.expires : 'never expires'}</span></div>
        <div>${r.fingerprint}</div>
        ${canRemove(r) && html`<div><${K} off=${busy} title="remove this key (added by hand)" on=${() => openRemove(r)}>[ remove ]</${K}></div>`}
      </div>`)}
      ${Array.isArray(rows) && !rows.length && html`<div class="faint" style=${ROW}>no keys in the list</div>`}

      ${rm && html`<div class="kpanel" style=${ROW}>
        <div>remove ${rm.id}</div>
        <div style=${BIG}>${rmGrp ? rmGrp.join(' ') : ''}</div>
        <div class="faint">compare it with what the terminal printed and with the maintainer's published fingerprint, not only with this page; then type it back:</div>
        <div><input type="text" spellcheck="false" autocomplete="off" aria-label="type the fingerprint back to remove" style=${FIELD} value=${rmTyped}
          onInput=${(e) => setRmTyped(e.currentTarget.value)} onKeyDown=${enter(remove)} /></div>
        <div class="faint">${stepUp(rm.fingerprint, 'remove')}</div>
        <div><${K} off=${!removeReady(rm, rmTyped, busy)} on=${remove}>[ remove key ]</${K}> <${K} on=${() => setRm(null)}>[ cancel ]</${K}></div>
      </div>`}

      <div class="hdr">add a key</div>
      <div class="faint" style=${ROW}>paste the .pub file's text, then inspect it</div>
      <div style=${ROW}><textarea rows="4" spellcheck="false" aria-label="public key text" style=${FIELD + ' resize: none;'} value=${pub}
        onInput=${(e) => { setPub(e.currentTarget.value); setInsp(null); setTyped(''); setMsg(''); }}></textarea></div>
      <div style=${ROW}><${K} off=${busy || !pub.trim()} on=${inspect}>[ inspect ]</${K}></div>

      ${insp && html`<div class="kins" style=${ROW}>
        <div>id ${insp.id}</div>
        <div style=${BIG}>${grp ? grp.join(' ') : ''}</div>
        <div class="faint">comment, as the key file says it (not checked):</div>
        <div dir="auto" style=${COMMENT}>${insp.comment}</div>
        <div class="faint" style=${ROW}>compare the fingerprint with what the terminal printed and with the maintainer's published fingerprint, not only with this page.</div>
        <div class="faint">${stepUp(insp.fingerprint, 'add')}</div>
        <div style=${ROW}>type the fingerprint back: <input type="text" spellcheck="false" autocomplete="off" aria-label="type the fingerprint back to add" style=${FIELD + ' display: inline-block; width: auto; min-width: 22ch;'}
          value=${typed} onInput=${(e) => setTyped(e.currentTarget.value)} onKeyDown=${enter(add)} /></div>
        <div><${K} off=${!addReady(insp, typed, busy)} on=${add}>[ add key ]</${K}></div>
      </div>`}

      <div class=${'msg'} style=${MSG} aria-live="polite">${busy ? 'working…' : msg}</div>
      <div style=${ROW}><${K} on=${closeOverlay}>[ close ]</${K}></div>
    </div></div>
  </section>`;
}
