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
