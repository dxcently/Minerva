# The web door: what was tested for security

**Written 2026-09-24, brought up to date against `HEAD`. Scope: the commit range `upstream/master be9fdc3 .. HEAD` on branch `web-door` — `HEAD` is `e090fa9`, on top of: `a343b86`, `5e20174`, `000e295`, `c399955`, `feb2cf7` (the revision this document was first written against), `18b4f73` (unit 4), `a8058b7` (unit 5), `953baa5` (unit 5b), and `e090fa9` (the whole `/api` namespace behind the token). Not upstream.** The subject is `crates/web` in `eidolon` plus the `Web` arm of `crates/cli/src/main.rs`. This document records what was measured, what rests on an argument, and what was not looked at. It changes no code and no other file.

**How to read this.** Every claim carries one of four labels, the four `triage.md` uses. **observed** — I read the code or the test at `HEAD` and it says so; file and symbol, or test name, is given. **reported** — evidence someone else measured during this work and I quote without reproducing it: every workspace test total, every `clippy` run, every manual `curl`/binary check and every mutation check is reported, and the person who measured it is named with the number. **reasoned** — follows from observed facts by an argument written out here. **hypothesis** — believed, not measured; the note says what would measure it. The reported numbers were taken in WSL (Ubuntu, cargo 1.98.1, `CARGO_TARGET_DIR` on ext4). I ran no cargo for this document and read no working tree.

Every code citation is against `HEAD` (`e090fa9`) as committed, never the working tree. The one revision named otherwise is `feb2cf7`, where the first version of this document was written.

---

## 0. The answer in one paragraph

The door is four `/api/` routes on loopback — plus, when `--ui-dir` names a directory, the static files of a browser UI on every other path — behind a per-run 256-bit token, minted at start, written `0600`, removed at shutdown. The token is what closes attacker **A** — a cross-site page in the operator's own browser — because such a page cannot read a file off the disk, while the `Host` check closes **B**, a rebinding page, and the `0600` mode plus the loopback bind close **C**, another process or user. The gate order is the design: a request that cannot present the token never reaches policy, the driver, a body read, or the allocator. The strongest evidence is the door suite driving the real `Dispatcher` — a `yes` runs the tool and a `no` does not — and the mutation check that turns the gate-order test red when the two gates are swapped. What is *not* proven: the constancy of the token compare (it rests on `subtle`), the byte-level behaviour of the 8 MiB cap (it rests on `Limited`), the read-failure and `JoinError` arms of the static path (no test reaches either), and the token hand-off itself — no launcher exists yet, so the URL-fragment delivery is designed and pinned only to the negative, by a test that the token appears nowhere in a served file. The two claims the convention review found bare — the lag close and the fail-closed token-file refusal — are tested (unit 4, `18b4f73`).

---

## 1. Threat model

The door serves **one** session to **one** person: the operator, on the machine the session runs on.

| # | Attacker | Can do | Cannot do |
|---|---|---|---|
| **A** | Any web page open in the operator's browser | Fire cross-origin requests at `127.0.0.1:<port>` (forms, `fetch` with `no-cors`, `EventSource`, `img`); read timing and status only where CORS allows | Read a file off the local disk; read a response body without CORS headers; set arbitrary headers on a simple-request cross-site POST |
| **B** | A DNS-rebinding page | Make its own hostname resolve to `127.0.0.1`, so its `fetch` is same-origin to the browser and it sees the response | Change the `Host` header, which carries the *target* authority of the request |
| **C** | Another local process or OS user on the same box | Connect to a loopback socket, attempt to read files the door writes | Read a `0600` file owned by someone else |

**Out of scope, and why.** An attacker **running as the operator's own user** is out of scope: such an attacker reads the token file, and the door has nothing left to withhold — the session journal, the vault, and the provider keys are all in that user's reach already (**reasoned**, from `src/lib.rs::write_token` writing `0600`, which constrains *other* users only). **Network attackers** are out of scope: the bind is loopback-only, and no flag widens it (**observed**, `src/lib.rs::loopback_only`). **The model itself** is out of scope: a tool call the model proposes is adjudicated by `eidolon`'s policy, which is the concern of `permissions.md`, not of the door — the door's job is that a browser cannot *originate* a turn the operator did not type and cannot *answer* a policy question the operator was never shown.

---

## 2. Attack surface

Four `/api/` routes (**observed**, `src/serve.rs::route`, `src/lib.rs` module doc).

| Route | Body | Can do | Cannot do |
|---|---|---|---|
| `GET /api/events` | none | read: `hello`, the walked branch replayed, every pending ask, `caught-up`, then the bus live | write |
| `POST /api/say` | `{"text":…,"mode":"send"\|"steer"}` | start a turn, or queue a follow-up / steer (`202`, `queued`) | run a tool; choose a policy verdict |
| `POST /api/cancel` | none | cancel the running turn (`204`) | cancel an idle session (`409`) |
| `POST /api/answer` | `{"ask_id":N,"answer":"…"}` | settle the one pending ask that id names (`204`) | settle anything else (`409`), or an unoffered label (`422`); run a tool by itself |

Anything else is `404` (**observed**, the `_ => status(NOT_FOUND)` arm — reached when `--ui-dir` was not given; with the flag a non-`/api/` path is a static read instead; `tests/door.rs::the_verbs_answer_statuses_and_nothing_else_is_served`).

**The second surface: `--ui-dir` (unit 5, `a8058b7`).** `eidolon web --ui-dir PATH` adds a static route for every path outside the API namespace (the bare `/api` and everything under `/api/`), so a browser UI is served from the door's own origin and needs no CORS (**observed**, `src/serve.rs::statics`, `src/lib.rs` module doc, `crates/cli/src/main.rs` `Cmd::Web`'s `ui_dir` arg).

| Property of the static route | Behaviour | Enforced in |
|---|---|---|
| Token | none — a static path is served without one, because the page has to load before a launcher can hand it a token | `src/serve.rs::route` — the static arm is filtered by `!in_api(&path)` |
| Methods | `GET` and `HEAD` only; anything else is `405` with `Allow: GET, HEAD` | `src/serve.rs::statics`, `ALLOW` |
| `Host` | checked on every static `GET`/`HEAD`: same comparison, same `403` sentence as the POSTs | `src/serve.rs::statics` → `host_ok` |
| `/` | `index.html` out of the directory | `src/files.rs::Static::resolve` |
| A directory | never listed — `404` | `src/files.rs::Static::resolve` (`!meta.is_file()`) |
| Path rules | one percent-decode; a NUL, a backslash, a non-absolute path, a `.`/`..` segment or an empty inner segment is `404`; then canonicalized and required to stay under the canonical root, which is also what a symlink out dies on | `src/files.rs::relative`, `src/files.rs::decode`, `src/files.rs::Static::resolve` |
| Size | over 32 MiB is `413`, and the read is capped at the same number | `src/files.rs::MAX_FILE`, `src/files.rs::read_capped` |
| Disk IO | `resolve`'s `canonicalize`/`metadata` and the capped read run under `spawn_blocking` | `src/serve.rs::statics` |
| Headers | every static response, refusals included: `nosniff`, the CSP below, `Referrer-Policy: no-referrer`, `Cache-Control: no-cache`; an `/api` response gets `nosniff` and `no-referrer` | `src/serve.rs::static_headers`, `src/serve.rs::CSP`, `src/serve.rs::handle` |
| Types | the extension table only, nothing sniffed; every text type carries `; charset=utf-8`, everything unknown is `application/octet-stream` | `src/files.rs::content_type` |
| Token in a file | the door never injects the token into a served file; the launcher builds the URL fragment out of the `0600` file, and a fragment is never sent to the server | `src/lib.rs` module doc, `src/serve.rs::Ctx::ui` |
| Without the flag | every non-`/api` path is `404` — the door is what it was | `src/serve.rs::route` |

Every status the door can emit, and which gate produces it (**observed**, `src/serve.rs`):

| Status | Where | Body |
|---|---|---|
| `401` | token gate, the whole API namespace (`/api` and `/api/*`) | empty — no `error`, no hint which of missing and wrong, never the token |
| `403` | `Host` gate: the POSTs and every static `GET`/`HEAD` | `{"error":"Host does not match the bound address"}` |
| `413` | the body cap (8 MiB) on the POSTs, and a static file over `MAX_FILE` (32 MiB) | a sentence on the body cap; empty on a file |
| `400` | a body that will not read, or will not parse | `{"error":"bad request body"}` / `{"error":"unknown mode …"}` |
| `404` | any path no route claims, a static directory, and every path rule the static route refuses | empty |
| `405` | a static path, any method but `GET`/`HEAD` | empty, with `allow: GET, HEAD` |
| `409` | cancel with no turn; answer with nothing pending under that id | `{"error": …}` |
| `422` | answer not one the pending ask offers — the ask stays pending | `{"error":"that ask does not offer that answer"}` |
| `202` | `/api/say` | `{"queued":bool}` |
| `204` | `/api/cancel`, `/api/answer` | empty |
| `200` | `/api/events` | `text/event-stream`, `cache-control: no-store` |
| `200` | a static file | the file byte for byte, typed from its extension |

The route table leaks nothing to an unauthenticated caller: `/api/nope` and a real route answer `401` alike without a token (**observed**, `tests/door.rs::every_api_path_refuses_a_request_that_presents_no_token` includes `/api/nope` in its loop).

**The gate pipeline. Nothing below the line is reached by a request that fails above it** (**observed**, `src/serve.rs::route`, in this order):

```text
  request
    |
    v
  path starts with "/api/" ?  ---- no ----> the static route (§2), or 404 when --ui-dir was not given
    | yes
    v
  token:  Authorization: Bearer <t>                        [all four routes]
          ?token=<t>                                        [GET /api/events only]
          constant-time compare, 401 with an EMPTY body on miss
    |
    v
  method == POST ?  ---- no ----> route table
    | yes
    v
  Host == bound address : port ?   403 on miss
    |
    v
  body read through Limited at 8 MiB   413 on over-cap
    |
    v
  handler  ->  driver::Handle (Agent::run_turn/steer/follow_up)
            ->  WebUser (the Dispatcher's UserIo)
```

And the static path, which shares only the `Host` check (**observed**, `src/serve.rs::statics`):

```text
  a path that is NOT /api/*, with --ui-dir given
    |
    v
  Host == bound address : port ?   ---- no ----> 403   [the POSTs' own check]
    | yes
    v
  GET or HEAD ?                    ---- no ----> 405 + Allow: GET, HEAD
    | yes
    v
  decode once; refuse NUL, backslash, non-absolute, . / .. / empty segment   404
    |
    v
  canonicalize, require under the canonical root, regular file               404
    |
    v
  size <= 32 MiB                                                            413 over it
    |
    v
  spawn_blocking: resolve + capped read  ->  200  + the static header set
```

**Loopback-only bind.** `run` calls `loopback_only(opts.bind)` before a listener exists; the CLI's `--bind` defaults to `127.0.0.1:0` (**observed**, `src/lib.rs::run`, `crates/cli/src/main.rs` `Cmd::Web`'s `bind` arg).

**Token file.** `<runtime dir>/web-<port>.token`, named for the port *actually bound*, so two doors on one box cannot read each other's token. Runtime dir is resolved once, in the CLI (`eidolon_swarm::Presence::root()`), where the swarm already registers (**observed**, `src/lib.rs::token_path`, `src/lib.rs::tests::the_token_file_is_named_for_the_port_in_the_runtime_dir`, `Cmd::Web` arm). Written `0600`, atomically; **lifetime** is mint-to-clean-shutdown, and shutdown is SIGINT/SIGTERM through `cancel_on_signals`, which the `Cmd::Web` arm ties to the door's cancellation token (**observed**).

---

## 3. Security properties

Legend for **defends**: A = cross-site page, B = DNS-rebinding page, C = another local process or user. Status is one of *tested*, *manual only*, *by construction*, *untested*, or a stated combination.

| # | Property | Defends | Enforced in | Proof | Label | Status |
|---|---|---|---|---|---|---|
| 1 | Token required on the whole API namespace: bare `/api` and every `/api/*` | A | `src/serve.rs::route` — `in_api(&path) && !presented(...).is_some_and(token_ok)`; `src/serve.rs::in_api` is `path == "/api" \|\| path.starts_with("/api/")` | `tests/door.rs::every_api_path_refuses_a_request_that_presents_no_token`; `tests/door.rs::the_api_namespace_is_whole_and_a_neighbouring_file_is_not` (bare `/api` is 401 with and without `--ui-dir`; `/api.js` stays static). Red with the old `starts_with("/api/")` rule (**reported**, implementing agent) | observed | tested (`e090fa9`) |
| 2 | A wrong token is refused exactly like a missing one | A | `src/serve.rs::token_ok` | `tests/door.rs::a_token_that_is_not_this_run_s_is_refused_like_no_token_at_all` | observed | tested |
| 3 | Query token accepted on `GET /api/events` only | A | `src/serve.rs::presented` — the `?token=` fallback is guarded by `method == GET && path == "/api/events"` | `tests/door.rs::the_query_token_opens_the_stream_and_no_post`, `src/serve.rs::tests::the_query_token_parameter_is_read_and_nothing_else_is` | observed | tested |
| 4 | Empty `401` body: no route oracle, no detail, never the token | A, C | `src/serve.rs::status` builds a zero-length body; the 401 arm returns before the route is matched | `tests/door.rs::every_api_path_refuses_a_request_that_presents_no_token` asserts no `error` and no token in the body; the same test covers `/api/nope` | observed | tested |
| 5 | Constant-time token compare | A, C | `src/serve.rs::token_ok` — `subtle::ConstantTimeEq::ct_eq` over both byte slices; a length mismatch is answered without reading either | `src/serve.rs::tests::the_token_comparison_answers_yes_and_no` proves the yes/no answers | observed (call site, crate) / reasoned (constancy follows only from `subtle`'s contract) | tested behaviourally; **timing constancy not measured** |
| 6 | Gate order: token, then Host, then cap | A, B | `src/serve.rs::route`, sequencing as drawn in §2 | `tests/door.rs::the_token_gate_runs_before_the_host_check`; mutation check — swapping the two gates turns that test red (**reported**, the implementing agent, unit 3b) | observed (code, test) / reported (the mutation) | tested |
| 7 | `Host` checked against the bound address on POST | B | `src/serve.rs::host_ok` — compares host and port *pieces*, bracketed IPv6 literal handled | `tests/door.rs::every_post_refuses_a_host_header_that_is_not_the_bound_address`, `src/serve.rs::tests::host_header_pieces_match`, `src/serve.rs::tests::host_header_mismatch_is_caught_by_the_same_comparison` | observed | tested |
| 8 | DNS rebinding closed | B | as #7, plus `Host` on every static `GET`/`HEAD` (row 26) | as #7, `tests/door.rs::a_rebound_host_does_not_read_a_static_file` | observed | tested on the POSTs and on static reads; **`GET /api/events` is not Host-checked** (finding 1) |
| 9 | Loopback-only bind, no widening flag | A, B, C | `src/lib.rs::loopback_only`, called by `run` before the listener | `src/lib.rs::tests::only_loopback_addresses_are_served` (accepts `127.0.0.1`, `127.5.5.5`, `[::1]`; refuses `0.0.0.0`, `[::]`, `192.168.1.5`, `10.0.0.1`), `tests/door.rs::run_refuses_a_bind_that_is_not_loopback` | observed | tested |
| 10 | IPv4-mapped IPv6 refused | B, C | `src/lib.rs::loopback_only` uses `SocketAddr::is_loopback`, which is true only for `127.0.0.0/8` and `::1` | the two IPv6 forms are in `src/lib.rs::tests::only_loopback_addresses_are_served`; `::ffff:0.0.0.0` and `::ffff:127.0.0.1` were probed by hand in the security review and refused | observed (code, test) / reported (the probe) | tested for `::1`/`::`; **manual only** for the `::ffff:` forms — no test names them, and the probe is reported (the security reviewer, §4) |
| 11 | Body cap 8 MiB, refused before the bytes past it are buffered | A (memory/DoS), C | `src/serve.rs::MAX_BODY` read through `http_body_util::Limited` in `src/serve.rs::json_body` | `tests/door.rs::an_oversize_body_is_refused_at_the_cap_and_starts_no_turn` sends `MAX_BODY + 1` real bytes, asserts `413`, zero user messages on the branch, and that the door still works after | observed | tested behaviourally (413, no turn); **byte-level buffering not measured** — relies on `Limited`'s contract |
| 12 | Token is 256 bits from the OS CSPRNG | C | `src/lib.rs::run` — `harnox::crypto::random_token()` | no test asserts the width | observed (call site) | relies on `harnox::crypto` — not measured here |
| 13 | Token file `0600`, and fail-closed if it cannot be made private | C | `src/lib.rs::write_token` — `harnox::fs::write_atomic_0600`, then `mode & 0o077 == 0` checked **after** the write, `bail` on failure | `src/lib.rs::tests::a_written_token_reads_back_at_0600` (happy path and mode), `src/lib.rs::tests::a_token_file_that_is_not_owner_only_fails_the_check` (a `0644` file fails the check), `src/lib.rs::tests::write_token_fails_when_the_file_cannot_be_created` (a read-only parent: `Err`, and no half-written file) | observed (code, tests) / reported (the suite's pass) | tested (unit 4, `18b4f73`) |
| 14 | Token file removed on clean shutdown | C | `src/lib.rs::run` removes it after `serve::run` returns; `Cmd::Web` cancels the door from SIGINT/SIGTERM via `cancel_on_signals` | `tests/door.rs::a_cancelled_run_removes_its_token_file`; manual: SIGINT and SIGTERM each exit 0 with no token file left | observed (code, test) / reported (the SIGINT/SIGTERM checks) | tested + manual |
| 15 | No CORS headers, so a cross-origin page cannot read a response | A | no response is built with an `Access-Control-*` header on any route; a grep of `crates/web` at `HEAD` for `access-control` and `cors` finds nothing (the `nosniff` and `content-security` hits at `HEAD` are the static route's own headers, §2) | absence in the source | observed (absence) / reasoned (no `Access-Control-Allow-Origin` means the browser withholds the body from the page) | by construction |
| 16 | One chokepoint: the door never calls `Dispatcher::dispatch`/`execute`/`adjudicate`; approvals only via `WebUser` as the `Dispatcher`'s `UserIo` | A | `src/serve.rs` forwards to `crate::driver::Handle` (which calls `Agent::run_turn`/`steer`/`follow_up`) and to `WebUser`; the non-test sources contain no call to `dispatch`/`execute`/`adjudicate` | `tests/door.rs::an_approval_ask_carries_the_call_and_a_yes_runs_the_tool` and `tests/door.rs::a_no_declines_the_call_without_running_it` drive the real `Dispatcher` and assert the tool ran or did not | observed | tested |
| 17 | Answer validation: `409` nothing pending, `422` label not offered, and a rejected answer leaves the ask pending | A | `src/serve.rs::post_answer` maps `Answer::{Accepted,Unknown,Rejected}`; the verdict is `src/user.rs::WebUser::answer` + `Ask::accepts` | `tests/door.rs::two_pending_asks_are_answered_independently_over_http`, `tests/door.rs::an_approval_refuses_an_answer_that_is_neither_yes_nor_no`, `src/user.rs::tests::an_ask_refuses_an_answer_it_does_not_offer_and_stays_pending`, `src/user.rs::tests::an_approval_refuses_an_answer_that_is_neither_yes_nor_no` | observed | tested |
| 18 | Two concurrent asks cannot resolve each other | A | `src/user.rs::answer` — a map keyed by `ask_id`; the entry for that id alone is removed | `src/user.rs::tests::two_pending_asks_each_resolve_only_by_their_own_id_oldest_first`, `src/user.rs::tests::two_pending_asks_each_resolve_only_by_their_own_id_newest_first`, `src/user.rs::tests::two_asks_are_pending_together_and_pending_lists_them_oldest_first`, `tests/door.rs::two_pending_asks_are_answered_independently_over_http` | observed | tested |
| 19 | Ask ids are never reused | A | `src/user.rs::next_ask_id` — `fetch_add(1, Relaxed)` on a `u64` that only ever rises | `tests/door.rs::two_pending_asks_are_answered_independently_over_http` asserts the two ids differ; the tests at #18 assert the second answer does not resolve the first | observed | distinctness tested; **never-reused by construction** (no wrap test) |
| 20 | Answer-versus-cancel race settles the ask exactly once | A | `src/user.rs::ask` — whichever of the two paths removes the map entry sends the settle; an entry still present means the other one won | the two paths are each tested (`src/user.rs::tests::cancel_settles_the_ask_as_cancelled`, `src/user.rs::tests::answer_resolves_pending_ask`) | observed (the arbiter is the `remove` return) | **by construction** — no test races answer and cancel concurrently |
| 21 | A stream opened late still gets every pending ask, before `caught-up` — no approval the operator never sees | A | `src/stream.rs::body` subscribes first, then reads `WebUser::pending()`; `hello.pending` is the same read's length | `tests/door.rs::a_stream_opened_after_the_ask_still_receives_it_before_caught_up`, and the two-pending case in `tests/door.rs::two_pending_asks_are_answered_independently_over_http` | observed | tested |
| 22 | A lagging subscriber closes the stream rather than skip a frame | A | `src/stream.rs::body` — a `RecvError::Lagged` on the bus or on the asks yields a `lagged` frame and `return`s, so `EventSource` reconnects into a fresh `hello`+replay | `src/stream.rs::tests::a_lagging_bus_subscriber_ends_the_stream_after_a_lagged_frame`, `src/stream.rs::tests::a_lagging_ask_subscriber_ends_the_stream_after_a_lagged_frame` | observed (the arms and both tests) / reported (the suite's pass) | tested (unit 4, `18b4f73`) |
| 23 | Legacy routes removed: `/`, `/index.html`, `/v1/models`, `/ext`, `/auth` are `404` without `--ui-dir` — with the flag those are static reads like any other non-`/api` path, and still `404` when no such file is there | A | the route table's `_` arm | `tests/door.rs::the_verbs_answer_statuses_and_nothing_else_is_served` | observed | tested |
| 24 | `hello` carries `protocol: 1`, so a client can ignore frame types it does not know | — (wire compatibility, not an attack) | `src/lib.rs::PROTOCOL`, emitted by `src/stream.rs::Local::Hello` | `tests/door.rs::the_stream_says_hello_then_replays_the_branch_then_caught_up` asserts the field and the value; the manual check agrees (**reported**, the implementing agent, unit 3) | observed (code, test) / reported (the manual check) | tested |
| 25 | A static path needs no token, and the `/api/*` paths still do | A | `src/serve.rs::route` — the static arm is filtered by `!in_api(&path)`, downstream of the token gate | `tests/door.rs::static_files_need_no_token_and_the_api_still_does` (a tokenless `GET /` is `200`; five tokenless `/api` requests are `401`) | observed | tested (unit 5, `a8058b7`) |
| 26 | `Host` checked on every static `GET`/`HEAD` | B | `src/serve.rs::statics` calls `host_ok` first, the same comparison and refusal the POSTs use | `tests/door.rs::a_rebound_host_does_not_read_a_static_file` (`403` on `/`, `/index.html`, `/assets/app.js` and on a `HEAD`; the bound authority on the same path is `200`) | observed | tested (unit 5, `a8058b7`) |
| 27 | Path traversal refused: one decode, no NUL or backslash, absolute only, no `.`/`..`/empty segment | C | `src/files.rs::decode` — once, with a malformed escape or non-UTF-8 a refusal — and `src/files.rs::relative` | `tests/door.rs::every_escape_lands_outside_the_directory_and_is_refused` (ten paths, each `404` and carrying neither file); `src/files.rs::tests::every_segment_that_could_leave_the_directory_is_refused`, `src/files.rs::tests::decoding_happens_once_and_malformed_escapes_are_refused` (`%2541` is `%41`, not `A`), `src/files.rs::tests::the_paths_that_are_inside_stay_inside` | observed | tested (unit 5, `a8058b7`) |
| 28 | Symlink escape refused | C | `src/files.rs::Static::resolve` — the named path is canonicalized and required to start with the canonical root | `src/files.rs::tests::a_symlink_out_of_the_directory_resolves_out_of_it` (a link out is `404`, a link that stays inside is served); `/escape.txt` is one of the ten in `tests/door.rs::every_escape_lands_outside_the_directory_and_is_refused` | observed | tested (unit 5, `a8058b7`) |
| 29 | `/` is `index.html`, and a directory is never listed | C | `src/files.rs::Static::resolve` — an empty relative path becomes `index.html`; anything not a regular file is `404` | `src/files.rs::tests::resolve_serves_the_index_for_the_root_and_nothing_for_a_directory`; `tests/door.rs::a_directory_is_never_listed_and_a_missing_file_is_the_same_404` (a directory is not a listing, and a UI directory with no `index.html` is `404` at `/`); `tests/door.rs::the_root_serves_index_and_a_nested_file_with_its_type` | observed | tested (unit 5, `a8058b7`) |
| 30 | `GET` and `HEAD` only; every other method is `405` with `Allow: GET, HEAD` | A | `src/serve.rs::statics`, `src/serve.rs::ALLOW` | `tests/door.rs::only_get_and_head_are_served_from_the_ui_directory` (POST, PUT, DELETE, PATCH × four paths), `tests/door.rs::a_head_carries_the_headers_and_no_body` | observed | tested (unit 5, `a8058b7`) |
| 31 | Every static response carries `nosniff`, the CSP, `no-referrer` and `no-cache`, refusals included | A | `src/serve.rs::static_headers`, `src/serve.rs::CSP` — one function, so the 404s and the 405 cannot drift from the 200s | every static test asserts the four through `tests/door.rs::assert_static_headers`, and the refusals through `tests/door.rs::assert_refused_static`, which also pins the empty body | observed | tested (unit 5, `a8058b7`) |
| 32 | An `/api` response carries `nosniff` and `no-referrer` | A | `src/serve.rs::handle` — the two are added to every response whose path starts with `/api/`, in one place | `tests/door.rs::static_files_need_no_token_and_the_api_still_does` asserts `nosniff` on the `/api` `401`s | observed | tested (unit 5, `a8058b7`) |
| 33 | Text types are served with `charset=utf-8`; an unknown extension is `octet-stream` | A | `src/files.rs::content_type` — the whole table, nothing sniffed | `src/files.rs::tests::the_cap_bounds_what_is_read_and_the_type_table_is_whole` (23 names), `tests/door.rs::the_root_serves_index_and_a_nested_file_with_its_type` (`text/html; charset=utf-8`, `text/javascript; charset=utf-8`, octet-stream for a file the table does not know) | observed | tested (unit 5b, `HEAD`) |
| 34 | This run's token appears in no served file | A, C | `src/lib.rs` module doc — the launcher builds the URL fragment out of the `0600` file, and a fragment is never sent to the server; `src/serve.rs::Ctx::ui` — the token has two homes, that field and the file | `tests/door.rs::a_served_file_carries_no_token_of_the_run` — the real `lib::run` with `--ui-dir`, the token read back from the `0600` file it wrote (43 characters), and absent from both the `/` and the `/assets/app.js` response | observed | tested (unit 5b, `HEAD`) |
| 35 | The static path's disk IO runs off the reactor | — (availability, not an attack) | `src/serve.rs::statics` — `resolve`'s `canonicalize`/`metadata` and `read_capped` inside `spawn_blocking`, with the two checks that touch no file left on the reactor | no test names the arm: the read-failure and `JoinError` arms are untested (§6) | observed (call site) | **by construction**, untested |
| 36 | A static file over 32 MiB is refused, and the read is capped at the same number | A (memory/DoS) | `src/files.rs::MAX_FILE`, `src/files.rs::read_capped` — `take(MAX_FILE)` | `tests/door.rs::a_file_over_the_cap_is_refused_and_one_at_the_cap_is_not` (`413` over, `200` at, both sparse files), `src/files.rs::tests::a_file_over_the_cap_is_refused_without_being_read`, `src/files.rs::tests::the_cap_bounds_what_is_read_and_the_type_table_is_whole` | observed | tested (unit 5, `a8058b7`) |
| 37 | Without `--ui-dir`, every non-`/api` path is `404` | A | `src/serve.rs::route` — the `None => status(NOT_FOUND)` arm | `tests/door.rs::the_verbs_answer_statuses_and_nothing_else_is_served` (row 23) | observed | tested |
| 38 | `run` refuses a `--ui-dir` that is not an existing directory, before the bind and before the token | C | `src/lib.rs::run` — `files::Static::new` is called first; `src/files.rs::Static::new` canonicalizes and requires `is_dir()` | `tests/door.rs::run_refuses_a_ui_dir_that_is_not_a_directory` (a missing path and a regular file: `Err` naming the path, and no token file left), `src/files.rs::tests::a_ui_dir_that_is_not_a_directory_is_refused_at_start` | observed | tested (unit 5, `a8058b7`) |

**Mutation checks on the static path — reported, the implementing agent, unit 5.** With the containment check *and* the segment rules removed, `/../secret.txt` came back `200` carrying the file outside the directory, and `tests/door.rs::every_escape_lands_outside_the_directory_and_is_refused` went red. With the containment check alone removed, the symlink escape returned the outside file. With the segment rules alone removed, `src/files.rs::tests::every_segment_that_could_leave_the_directory_is_refused` went red. And negating the token-absence assertion in `tests/door.rs::a_served_file_carries_no_token_of_the_run` made that test fail against the real response — so the assertion is a claim about the response and not about an empty needle.

**Cross-cutting, all of it reported.** The full workspace suite went 1053 passed / 0 failed at the `be9fdc3` baseline to 1099 (`5e20174`), 1111 (`000e295`), 1121 (`c399955`), 1123 (`feb2cf7`), 1127 (`18b4f73`), 1144 (`a8058b7`), 1145 (`953baa5`), and the latest total 1146 passed / 0 failed at `HEAD` (`e090fa9`) — measured by the orchestrator (**reported**), none of it re-run here. `clippy --workspace --all-targets` was clean apart from the pre-existing `conch-parser` future-incompat notice, and `cargo doc -D warnings` clean (**reported**). The door suite was run three times after units 2 and 3 and five times by a reviewer: no flakes (**reported**).

---

## 4. Review findings and disposition

Two independent reviews, run at `feb2cf7` and quoted here as **reported** — I did not reproduce either.

**Security review (Sonnet 5), against the threat model of §1: no exploitable defect found.** Its findings:

| # | Finding | Disposition |
|---|---|---|
| 1 | The module doc overstates the `Host` check — `Host` is checked on POST only, while `GET /api/events` relies on the token alone | **Fixed in `18b4f73`:** `lib.rs`'s and `serve.rs`'s module docs state the `Host` check is POST-only — and, since unit 5, on every static `GET`/`HEAD` as well — and that `GET /api/events` relies on the token alone. Rows 7–8 carry the same split |
| 2 | `?token=` on `/api/events` can land in browser history | **Accepted risk**, see §5 |
| 3 | Unguarded `std::os::unix` import (`write_token`) | **Dismissed.** `eidolon_core`'s `src/ipc.rs` does the same, and eidolon is unix-only by policy |
| 4 | `::ffff:0.0.0.0` and `::ffff:127.0.0.1` refused by `loopback_only` | **Verified as correct behaviour** — see row 10. Not a defect: refusal is the fail-closed outcome |
| 5 | `text/plain` cross-origin POST is parsed as JSON regardless of `Content-Type`, but still needs the token, so no CSRF | **Accepted.** `json_body` sniffs nothing: the body is JSON-parsed whatever the header says, and the token gate is upstream of it |

**Conventions/claims review (Sonnet 5).** Two claims in the doc comments had no test behind them — "a lagging subscriber closes the stream" and "refuses to start if the token file cannot be made private". Both are tested at `HEAD` (unit 4, `18b4f73`): rows 22 and 13. What still stands: the constant-time compare rests on the `subtle` crate rather than on a measurement (row 5), and the 8 MiB cap rests on `http_body_util::Limited`, tested behaviourally at `413` with no turn started but not measured at the byte level (row 11).

**The two gaps the first write listed, now closed — reported, unit 4, `18b4f73`.**

| Gap | Closed by | Test |
|---|---|---|
| A lagging subscriber closing the stream (row 22) | the stream now has a test that starves a subscriber and watches it end | `src/stream.rs::tests::a_lagging_bus_subscriber_ends_the_stream_after_a_lagged_frame`, `src/stream.rs::tests::a_lagging_ask_subscriber_ends_the_stream_after_a_lagged_frame` |
| The fail-closed token-file refusal (row 13) | the refusal is reached directly, on a file put there by hand, and on a directory that cannot be written | `src/lib.rs::tests::a_token_file_that_is_not_owner_only_fails_the_check`, `src/lib.rs::tests::write_token_fails_when_the_file_cannot_be_created` |

---

## 5. Accepted risks

| Risk | Why it is accepted |
|---|---|
| The token rides in the URL on `GET /api/events`, so it can be written to browser history or a shell log | A browser's `EventSource` cannot set a request header, and the stream is the whole point of the door. The alternative — a cookie — is worse for a loopback door, and the token is per-run and removed on shutdown |
| A process running as the operator's own user can read the token file and drive the door | That process can read the session journal directly. The `0600` mode is there to keep *other* users out; within the user's own account there is no boundary to defend |
| Plain HTTP, no TLS | Traffic never leaves loopback. TLS would add a certificate the operator must accept, for a hop that does not exist |
| The door trusts the browser to run the operator's own clicks | The door authenticates the *run* (token), not the *person*. A page that has the token is, by construction, a page that could read the file |
| A `lagged` frame closes the stream | Closing and letting `EventSource` reconnect is a correct catch-up (fresh `hello` + replay) where a partial buffer is not. Cost: a reconnect the operator may notice; no security loss |
| A hardlink planted inside the UI directory, pointing at a file outside it, is served | The refusal is path-based — canonicalize and stay under the root — and a hardlink is not a path out of the directory: it is the same inode under an extra name, so nothing about the path differs from a file the directory's own author put there. Creating one needs write access to the UI directory as the operator's own user, which is the access §1 puts out of scope. Stated in `src/files.rs`'s module doc (**observed**) |
| The URL fragment that carries the token lands in browser history, as the `?token=` query does | The same exposure as the query form, with the same answer: per-run, removed on shutdown. A fragment is never sent to the server and never rides a `Referer`, so it is strictly narrower than the query — but it is still a token in a browser's history file (**reasoned**) |

---

## 6. Not tested, and what would measure it

| Gap | What would measure it |
|---|---|
| Timing constancy of the token compare | A timing harness over `token_ok` with wrong-first-byte and wrong-last-byte inputs, asserting indistinguishable distributions. Today it rests on `subtle` (row 5) |
| Byte-level buffering behaviour of `Limited` | A body of `MAX_BODY + 1` bytes with a counted reader, proving the bytes past the cap are never read into memory. Today the proof is the `413` and the untouched branch (row 11) |
| The static path's read-failure arm and its `JoinError` arm | Break `read_capped` for a file `resolve` had already accepted — a mode that refuses the open, a file removed between the two calls — and make the blocking task itself fail: assert the `404` and the `500`. Today both arms are read, not run (row 35) |
| The hand-written percent-decoder | `src/files.rs::decode` and `src/files.rs::hex` are unit-tested against named inputs and nothing else. A fuzz target asserting that the path the door names is the path the filesystem opens would bound it |
| The launcher's fragment hand-off | No launcher exists yet, so the token delivery is designed and pinned only to the negative: that no served file carries the token (row 34). What would measure it: a launcher that builds the fragment out of the `0600` file, and a browser — or a script reading the fragment — that gets the stream with it |
| Answer-vs-cancel raced concurrently | A test that fires `answer` and the cancel token in the same instant and counts settle frames — assert exactly one (row 20) |
| Fuzzing of the request parser and the body parsing | No fuzz target exists for `crates/web`; the parsers are `hyper` and `serde_json`, trusted as crates |
| Many concurrent SSE connections | No load or DoS test: number of open connections, and memory per open stream, unmeasured. Relevant because each connection holds a broadcast receiver |
| Windows | `std::os::unix` in `write_token` means no Windows build. Unmeasured, and by policy irrelevant |
| A `nix` build of the pinned `harnox` tag | Not exercised; the token mint and the atomic `0600` write are taken on the crate's word |

---

## 7. What units 4, 5 and 5b added, and what each owed

One row per unit; every line of it is **reported** (the commits' own verified lines and the orchestrator's totals), because the unit-5 work was measured while this document was being revised.

| Unit | Commit | What it added, as the commit records it |
|---|---|---|
| 4 | `18b4f73` | The two tests the convention review asked for (rows 13 and 22), the `Host` doc correction (finding 1), and the module-doc trim — `lib.rs`'s module doc 101 lines to 45, `user.rs`'s 60 to 25. 1127 passed, 0 failed; the new tests red when their code is broken; `cargo doc -D warnings` clean |
| 5 | `a8058b7` | `--ui-dir`: the static surface of §2 and rows 25–38. 1144 passed, 0 failed; the escape tests red when containment or the segment rules are removed; the binary by hand |
| 5b | `953baa5` | `charset=utf-8` on the text types, `spawn_blocking` for the static path's filesystem work, and the token-never-in-a-served-file test (row 34). 1145 passed, 0 failed; that test red when its assertion is negated |
| 5d | `e090fa9` | The whole API namespace behind the token: `in_api` is true for the bare `/api` as well as `/api/*`, so `/api` no longer falls through to static serving under `--ui-dir` (found by the second fact-check). 1146 passed, 0 failed; the new test red with the old `starts_with("/api/")` rule; `cargo doc -D warnings` clean after a private intra-doc link was made plain code |

**What unit 5 owed, as designed at `feb2cf7`, is all delivered** — path traversal, symlink escape, the `Host` check on static reads, `nosniff`, a CSP, `no-referrer`, a file size cap, and token delivery by URL fragment rather than by injection into `index.html`. Two places the delivered behaviour differs from the sketch, both stated where they belong: the CSP is not `default-src 'self'` alone but that plus `connect-src 'self'`, `img-src 'self' data:`, `style-src 'self' 'unsafe-inline'` and `frame-ancestors 'none'` (§2), and `no-referrer` is on the static and `/api` responses, not on the bare `404` a door without `--ui-dir` answers (**observed**, `src/serve.rs::handle`, `src/serve.rs::static_headers`).

---

## 8. How to reproduce

All of these were run in WSL (Ubuntu, cargo 1.98.1, `CARGO_TARGET_DIR` on ext4) — the first set at `feb2cf7`, the unit-4/5/5b set at `HEAD` — and every result below is **reported**: the implementing agents' and the orchestrator's, not mine.

```sh
# the frozen revision this document describes
git -C /path/to/eidolon-door/eidolon checkout e090fa9   # HEAD

# the whole suite, the number the verified results quote
cargo test --workspace --locked

# the door alone, and the door suite three times over for flakes
cargo test -p eidolon-web
for i in 1 2 3; do cargo test -p eidolon-web --test door; done

# lints: silent but for the standing conch-parser future-incompat notice
cargo clippy --workspace --all-targets
```

The mutation check for row 6 — swap the token gate and the `Host` gate in `src/serve.rs::route`, so `Host` is decided first — makes `tests/door.rs::the_token_gate_runs_before_the_host_check` red (**reported**, the implementing agent, unit 3b). The unit-5 mutations are listed under §3: remove the containment check and the segment rules in `src/files.rs::Static::resolve` and the escape tests go red; negate the token-absence assertion in `tests/door.rs::a_served_file_carries_no_token_of_the_run` and it fails against the real response (**reported**, units 5 and 5b).

The manual binary checks (**reported**, run by the implementing agents) were performed with the door started by `eidolon web` and driven with a raw HTTP client: `401` without a token, `401` with a wrong token, `401` for a query token on a POST, `403` for a bad `Host` with a good token, `413` for a 9 MB body, `-rw-------` on the token file, `"protocol":1` in `hello`, and exit `0` with the token file gone after SIGINT and after SIGTERM. The reader reproducing them needs the token from the path `run` prints, and must set `Host` explicitly when probing with a tool that would otherwise let the client derive it.

Unit 5 adds one more, recorded in its commit without detail: the binary was driven by hand with `--ui-dir` (**reported**, the implementing agent). Which paths were probed is not in the record, so a reader wanting the static surface's manual evidence has only the tests in §3.
