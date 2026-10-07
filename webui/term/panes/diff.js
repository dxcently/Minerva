// The inspector's diff tab: the git branch of this chat's folder, and the files this chat's edit and write calls touched. app.js lists it in
// INSPECTOR; it gets { s, sid } like every tab.
//
// The branch comes from the perch's GET /hub/meta/git?session=<id>, which reads the folder's .git/HEAD and runs no git. The +/− counts and the
// pull request are not read at all: git run on a folder can run what the folder's own config names (filters, hooks), and Minerva never does that.
import { html, useEffect } from '../core/ui.js';
import { Needs } from '../core/bits.js';
import { mode } from '../core/api.js';
import { answer, pull, said, QUICK_MS } from './loaded.js';

export const GIT_NOTE = '+/− and PR state are not read: Minerva never runs git on a folder (its filters could run code)';
export const GIT_MS = 15000; // how often the branch is asked again while the tab is showing (the agent may switch it)

export const gitPath = (sid) => `meta/git?session=${encodeURIComponent(sid)}`;

// The line for an answer of /hub/meta/git: { text, title?, cls }. A 200 with `branch: null` and no `sha` is a folder with no repository, which
// is not an error; any other status is the perch's sentence.
export function branchLine(r) {
  if (!r) return { text: '…', cls: 'faint' };
  if (r.status === 200 && r.data && typeof r.data === 'object') {
    const { branch, sha } = r.data;
    if (typeof branch === 'string' && branch) return { text: branch, cls: 'c' };
    if (typeof sha === 'string' && sha) return { text: `detached at ${sha.slice(0, 12)}`, title: sha, cls: 'o' };
    return { text: 'no git repository in this folder', cls: 'faint' };
  }
  // The refusals of a chat that has no folder to read come without a sentence (404: not a chat of this perch; 409: not a live door or a saved
  // chat with a folder, `state` says what it is; 503: the perch's slots are full).
  const own = r.data && typeof r.data.error === 'string' && r.data.error;
  if (!own && r.status === 404) return { text: 'the perch has no folder for this chat', cls: 'faint' };
  if (!own && r.status === 409 && r.data && typeof r.data.state === 'string') return { text: `this chat is ${r.data.state}: no folder to read`, cls: 'faint' };
  if (!own && r.status === 503) return { text: 'the perch is busy; asked again soon', cls: 'faint' };
  return { text: said(r), cls: 'faint' };
}

// Ask for the branch of chat `id`. `lane` is for the periodic read only: it waits its turn among the page's polls (core/lane.js). The read at open
// and after an edit is not a poll, so it goes straight out (a plain hubGet with a timeout) and never waits behind a slow poll.
export const refreshGit = (id, lane = false) => pull(gitPath(id), 0, lane ? undefined : QUICK_MS, lane);

export function DiffTab({ s, sid }) {
  const id = sid || s.sid, perch = mode() === 'perch';
  const files = [...s.touched.value], calls = files.reduce((n, [, c]) => n + c, 0);
  // At open and when the chat shown changes, again after each edit or write call (the one time the folder is known to have moved), and
  // slowly while the tab is showing, in the lane.
  useEffect(() => {
    if (!perch) return undefined;
    const go = () => { if (document.visibilityState === 'visible') refreshGit(id, true); };
    const t = setInterval(go, GIT_MS);
    return () => clearInterval(t);
  }, [perch, id]);
  useEffect(() => { if (perch) refreshGit(id); }, [perch, id, calls]);
  const line = branchLine(perch ? answer(gitPath(id)).value : null);
  return html`<div class="hdr">git</div>
    ${perch ? html`<div class="kv"><span class="faint">branch   </span><span class=${'grow ' + line.cls} title=${line.title}>${line.text}</span></div>`
      : html`<${Needs}>the branch needs the hub</${Needs}>`}
    <${Needs}>${GIT_NOTE}</${Needs}>
    <div class="hdr">changed</div>
    ${files.length ? files.map(([f, n]) => html`<div class="kv"><span class="grow c" title=${f}>${f}</span><span class="faint">${n} call${n > 1 ? 's' : ''}</span></div>`)
      : html`<div class="faint">no edit or write calls yet</div>`}`;
}
