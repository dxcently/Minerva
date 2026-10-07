// One lane for every periodic GET to the perch. The browser allows six connections per host (shared by every tab of the profile); chat
// streams and the terminal stream hold most of them, so polls must not hold more than one. `hubPoll(path, timeout)` is `hubGet` that
// waits its turn: one poll in flight across the whole page, the rest first in, first out behind it.
//
// Only polls come here. What a person did (`hubGet`/`hubPost` for a click, `hubPost` for terminal input and ask answers) never enters
// the lane, so the connection the lane leaves free is there for typing and answers however busy the polls are.
//
// A path that is already waiting is not queued twice: its callers share the one request and its one answer (the same promise), so a
// poll slower than its interval never piles up. A path that is already in flight is another matter: a caller after it wants an answer
// no older than its own call, so it waits as one more request, queued once, behind it.
//
//   hubPoll(path, timeout)      { status, data }, as hubGet
//   hubPollText(path, timeout)  { status, data } with the body as text on a 200, as hubText (the run log is JSON lines)
//
// A poll that names no timeout still gets LANE_TIMEOUT_MS: a request the perch never answers must not hold the lane for ever. It then
// comes back as status 0, like any failed call, and the lane moves on.
import { hubGet, hubText } from './api.js';

export const LANE_TIMEOUT_MS = 10000;

const queue = [];          // the entries not yet started, oldest first
const waiting = new Map(); // key -> the entry in `queue` for it
let flying = false;        // one entry is in flight

function pump() {
  if (flying) return;
  const e = queue.shift();
  if (!e) return;
  waiting.delete(e.key);
  flying = true;
  let p;
  try { p = Promise.resolve(e.call()); } catch (err) { p = Promise.reject(err); }
  p.then(e.resolve, e.reject).finally(() => { flying = false; pump(); }); // a poll that failed or threw frees the lane too
}

function enqueue(key, call) {
  const queued = waiting.get(key);
  if (queued) return queued.promise;
  const e = { key, call };
  e.promise = new Promise((resolve, reject) => { e.resolve = resolve; e.reject = reject; });
  waiting.set(key, e);
  queue.push(e);
  pump();
  return e.promise;
}

export const hubPoll = (path, timeout) => enqueue('j ' + path, () => hubGet(path, timeout || LANE_TIMEOUT_MS));
export const hubPollText = (path, timeout) => enqueue('t ' + path, () => hubText(path, timeout || LANE_TIMEOUT_MS));
