// Every URL the page calls is built here, from a session id.
//
// M1 talks to ONE `eidolon web` door directly. That door is the session whose
// id is DOOR, and its base is `/api/`. With the hub (docs/design/hub.md
// section 5) every other id is proxied at `/s/<id>/api/`. A pane holds a
// session id and never builds a URL itself.
export const DOOR = 'door';
export const apiBase = (sid) => (sid === DOOR ? '/api/' : `/s/${encodeURIComponent(sid)}/api/`);
export const apiUrl = (sid, route) => apiBase(sid) + route;

const KEY = 'minerva-token';
const TOKEN = /^[A-Za-z0-9_-]+$/;
let token = null;

// The launcher puts the token in the URL fragment (#token=...), which a
// browser never sends to a server. A `?token=` in the query is accepted too
// (it has already reached the server once, so it is scrubbed at once). Either
// way the token moves to sessionStorage and leaves the URL. It only ever goes
// out again in an Authorization header: never into the DOM, a log or an error.
export function takeToken() {
  const hash = new URLSearchParams(location.hash.replace(/^#/, ''));
  const query = new URLSearchParams(location.search);
  const found = hash.get('token') || query.get('token');
  if (hash.has('token') || query.has('token')) {
    hash.delete('token');
    query.delete('token');
    const q = query.toString(), f = hash.toString();
    history.replaceState(null, '', location.pathname + (q ? '?' + q : '') + (f ? '#' + f : ''));
  }
  if (found && TOKEN.test(found)) {
    token = found;
    try { sessionStorage.setItem(KEY, token); } catch { /* private mode: keep it in memory */ }
  } else {
    try { token = sessionStorage.getItem(KEY); } catch { token = null; }
  }
  return !!token;
}

export function auth(extra = {}) {
  return { Authorization: 'Bearer ' + token, ...extra };
}

// POST a JSON body (or none). Resolves to { status, data } and never throws on
// an HTTP status; a network failure comes back as status 0.
export async function post(sid, route, body) {
  const init = { method: 'POST', cache: 'no-store', headers: auth() };
  if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  try {
    const r = await fetch(apiUrl(sid, route), init);
    const t = await r.text();
    let data = null;
    if (t) { try { data = JSON.parse(t); } catch { data = { error: t.slice(0, 200) }; } }
    return { status: r.status, data };
  } catch (e) {
    return { status: 0, data: { error: 'network error' } };
  }
}

// ---------------------------------------------------------------- the hub
// `minerva perch` serves `/hub/sessions` (list, spawn, and per session
// resume/stop); a bare `eidolon web --ui-dir` door answers `/hub/...` with a
// static 404. That difference is how the page tells the two apart. Only
// `/hub/sessions...` and `/hub/tree?...` paths are built here.
let hubMode = 'unknown'; // 'unknown' | 'door' | 'perch'
let hubRows = [];
export const mode = () => hubMode;
export const rows = () => hubRows;

// `timeout` (ms) aborts a call the perch never answers: it comes back as status 0, like a
// network failure. The list poll and the boot probe set it; spawn, resume and stop do not
// (a door can take ~20 s to come up).
export const POLL_TIMEOUT_MS = 5000;
async function hubCall(method, path, body, timeout) {
  if (typeof path !== 'string' || !(path.startsWith('sessions') || path.startsWith('tree?'))) throw new Error('hub path must start with sessions or tree?');
  const init = { method, cache: 'no-store', headers: auth() };
  if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  const ac = timeout ? new AbortController() : null;
  const timer = ac ? setTimeout(() => ac.abort(), timeout) : 0;
  if (ac) init.signal = ac.signal;
  try {
    const r = await fetch(`/hub/${path}`, init);
    const t = await r.text();
    let data = null;
    if (t) { try { data = JSON.parse(t); } catch { data = { error: t.slice(0, 200) }; } }
    return { status: r.status, data };
  } catch (e) {
    return { status: 0, data: { error: 'network error' } };
  } finally {
    clearTimeout(timer);
  }
}
export const hubGet = (path, timeout) => hubCall('GET', path, undefined, timeout);
export const hubPost = (path, body) => hubCall('POST', path, body);
// GET /hub/tree?session=<id>&path=<rel> (hub.md section 7): the query is built
// from parts with encodeURIComponent, never by joining a raw path.
export const hubTree = (sid, rel) => hubGet(`tree?session=${encodeURIComponent(sid)}&path=${encodeURIComponent(rel || '')}`);

// What serves this page: 'perch' (rows kept for rows()), 'door', 'refused'
// (the token is wrong: the perch restarted) or 'offline' (nothing answered).
export async function detect() {
  const { status, data } = await hubGet('sessions', POLL_TIMEOUT_MS);
  if (status === 200 && Array.isArray(data)) { hubMode = 'perch'; hubRows = data; return 'perch'; }
  if (status === 404) { hubMode = 'door'; hubRows = []; return 'door'; }
  if (status === 401) return 'refused';
  return 'offline';
}
