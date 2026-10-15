# T1b security inventory (claims for a reviewer to check)

Commits on branch worktree-wf_2902c6f9-c37-1, base d8c5353 (master): 2c8262f (the slice) and 8e6233f (the fixes after the verifier's review).
Worktree: C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1 (WSL /mnt/c/Users/dxcen/Projects/Minerva/.claude/worktrees/wf_2902c6f9-c37-1).

## Files

- perch/src/term/routes.rs: the whole operator surface (new code). Nothing else in src/ changed.
- perch/tests/term.rs: 18 tests over a real /bin/sh (replaces the T1b stub pin).
- perch/src/term/README.md (T1b section: routes and refusals table; the ids and existence bullets) and perch/tests/README.md (term.rs row).
- Not touched, on purpose: serve.rs, lib.rs, main.rs, tests/common/mod.rs, tests/http.rs, Cargo.*, term/mod.rs, pty.rs, screen.rs, stream.rs, snap.rs, agent.rs.
- Outside the repo: the slice spec T1b.json (scratchpad slices folder) now says a cross-site cookie gets 401, not 403.

## Gate (existing, unchanged; routes sit behind it)

- Gate 1 (serve.rs route): no credential -> 401 empty, before any route. Master bearer or a same-origin login cookie authenticate.
  A cookie that comes with Sec-Fetch-Site cross-site does NOT authenticate: 401 (T1b.json said 403; corrected, since the existing gate answers 401).
  The bearer with Sec-Fetch-Site cross-site -> 403 (gate 3), tested.
- Gate 2: Host must be the bound authority (DNS rebinding), unchanged.
- Gate 4: DELETE, PUT and all other methods -> 405 at the gate, before any route (tested: DELETE /hub/term, DELETE /hub/term/t1/input).
- Gate 5: serve answers a declared length over MAX_BODY (8 MiB) with 413 before the seam; the route's own cap is lower (below).
- Agent gate: /hub/term/a/<verb> is decided by agent::authenticate (agent/token.rs) before the master check. A master credential (the bearer,
  or a same-origin cookie) gets the empty 404 there; no credential or a wrong token gets 401. Never a route of this file.
  pick() sends `a` and `a/...` (non-verbs) to nobody (404).

## Routes, inputs, answers and refusals

- GET /hub/term: 200 JSON list of Info (no env, no token, no argv). Extra field started_ms beyond the brief's list.
- POST /hub/term {chat?, cwd?, cols?, rows?, title?} (empty body = all defaults). deny_unknown_fields; 16 KiB body cap (body_of).
  201 {id}; 400 bad shape; 404 unknown chat (problem JSON); 422 cwd refused (anyhow sentence, echoes the caller's own path);
  409 table full (MAX_TERMS); 422 folder not a directory (OpenError::Cwd); 503 perch stopping; 500 shell did not start (detail logged,
  not returned); 400 NoChat (unreachable for the operator). The chat lookup (remembered folder, then the session list) runs on the
  blocking pool (spawn_blocking, since 8e6233f).
- POST /hub/term/<t>/input: id checked (valid_id), then terminal exists (404 empty), then Content-Type (415 unless exactly one header
  whose media type is application/octet-stream, parameters allowed, case of the media type not judged), then declared length
  (413 before any byte if > 64 KiB), then body_of with cap 64 KiB (413 at the first frame past the cap for chunked bodies), then
  Terms::input (204; 404 NotFound; 409 Ended; 413 TooBig; 503 Full; 409 Held is unreachable from the operator lane).
- POST /hub/term/<t>/resize {cols: u32, rows: u32} strict (deny_unknown_fields; anything else 400). Clamp is T1a's (20-400, 5-200).
  200 {cols, rows}. Runs on spawn_blocking via Terms::get -> Arc<Terminal>. 409 for an ended terminal read back from terms.json.
- POST /hub/term/<t>/hold {hold: bool} strict. 200 {hold}. Terms::set_hold (never blocks on the screen lock). 409 as resize.
- POST /hub/term/<t>/close: 202, empty body, no wait (Hangup dropped). Second close is 404.
- GET /hub/term/<t>/text?lines=N: digits only, given once (400 otherwise; empty or repeated 400). Absent 100 lines; a number over 400
  reads 400 (overflow saturates). max_chars fixed at 7000 (screen::MAX_TEXT_CHARS). 200 {text, cursor, alt_screen}. Live screen read on
  spawn_blocking; an ended-from-file terminal read with no lock.
- Ids: valid_id (^t[0-9]{1,6}$) checked first; invalid id, missing terminal and unknown verb/path -> empty 404. Wrong method on a
  real route -> 405 with Allow, checked before existence. `events` is the stream's (T1c); `events/<x>` is an invalid id -> 404.

## Decisions applied (from the brief and the operator's rules)

- Input goes through Terms::input only (the operator's lane). Never agent_input. The operator's hold never refuses it.
- Owner is the operator; chat recorded only when named and known.
- cwd: an explicit cwd must pass sessions::Registry::cwd_under_a_root (canonical, under a --root, not the home's own). The default is
  the chat's remembered folder (known_cwd) or $HOME; neither is root-checked (documented in the README).
- Known chat: remembered folder (known_cwd) first; otherwise a row in Registry::list() (the same question tree::base asks). Unknown is 404.
- Existence before body and before Content-Type: a request to a missing terminal is 404 whatever body or Content-Type it sends. The
  method is checked before the terminal is looked up, so a wrong method on a real route is a 405 first.
- No answer carries: the environment (Info has no env field), a token (none is ever held in a route; tests check the body does not
  contain the perch token), the perch's argv (tests check for --bind, --root, --ui-dir). Refusals are JSON {"error": sentence} or
  empty; the one sentence that echoes input is the cwd refusal (the caller's own path).
- Blocking: no route waits on a screen lock on the runtime; text and resize use spawn_blocking on an Arc<Terminal>.

## What is never exposed

- The master token, the login secrets, the agent tokens, the environment of the perch and of its shells, the perch's argv, the
  terms.json path, the sessions dir, the roots list. (No route here reads or returns any of them.)
- Shell output is returned as the screen text the shell wrote; that is the operator's own terminal and may hold anything it typed
  (T1a's README already says so).

## Risks I see (for the reviewer to weigh)

1. Input is code execution. The gate is the existing one (master bearer or same-origin HttpOnly SameSite=Strict cookie); a cookie
   holder can type into any shell the perch runs, as a bearer holder can. No new credential path.
2. The cookie + cross-site case is 401 by the existing gate; the spec's 403 expectation for it was wrong and is corrected in T1b.json.
   The bearer with cross-site stays 403.
3. A chat's remembered folder and $HOME are not root-checked (by decision). A chat's folder can be anywhere the chat was started.
4. Known-chat lookup falls back to Registry::list() (reads the sessions dir and eidolon's roster files) for a chat with no remembered
   folder. Operator-triggered and bounded; tree::base already does the same. It runs on the blocking pool since 8e6233f.
5. Synchronous file I/O on the runtime: Terms::list (prune may rewrite terms.json), Terms::close of an ended terminal, and the create
   path's reserve (T1a's design). Small writes, same as T1a. The chat lookup is no longer among them (8e6233f).
6. A program that floods the PTY can hold the screen lock for a few ms per read; text and resize wait for it on the blocking pool
   (tokio's default bound), not on the runtime.
7. Content-Type media type is matched case-insensitively and parameters are allowed; "exactly" in the brief could mean the bytes. A
   reviewer may want exact matching.
8. JSON bodies are strict (deny_unknown_fields), which the brief did not say; the page must send exactly the named keys.
9. resize and hold on a shell that has ended but is still listed (kept 30 min, in the table) answer as a live one (T1a's Terms
   semantics); only input answers 409. An ended terminal read back from terms.json answers 409 to all three.
10. set_hold on a live terminal flushes the PTY's unread input, which can include the operator's own unread bytes (T1a's documented
    residual, reached through the operator's own hold).
11. Covered by tests/term.rs since 8e6233f: a chat with a remembered folder (a --home chat-cwds.json, no door), a listed chat with no
    folder (starts in HOME), a 503 Full input (a foreground sleep that reads nothing), and an ended terminal read back from terms.json
    (a restart with --home: 409 input, resize, hold; text and close still answer). Not covered: the 500 shell-start path.
12. serve.rs pins red until merge (serve.rs is not edited here; the orchestrator applies these at merge):
    - serve::tests::the_master_credential_is_a_404_and_no_credential_a_401_on_an_agent_path, serve.rs:1522: left 200, right 404 for
      GET /hub/term with the master bearer. Fix: expect 200 at serve.rs:1522. Line 1523 (the same path with the same-origin cookie) is also
      200 now; expect 200 there too. The test stops at 1522 in this run, so 1523 and the agent-path loop after it (1525-1531) have not run
      red; the loop is about /hub/term/a/... and /hub/mail/a/..., which the route does not touch.
    - serve::tests::every_seam_is_a_404_with_the_token_and_a_401_without_it, serve.rs:1565: left (200, "[]") right (404, "") for
      GET /hub/term. Fix: delete the ("GET", "/hub/term") entry at serve.rs:1552. The POST /hub/term/t1/input entry stays 404 (t1 does
      not exist); the 413 loop at serve.rs:1588-1590 passes.
    - The patched pins are derived from the failing assertions and the code; they were not run, since serve.rs is not edited here.

## Verification (final tree, 8e6233f)

- cargo test --offline --features test-bins --test term: 18 passed, 0 failed.
- cargo test --offline --features test-bins --no-fail-fast (whole suite): 849 passed, 2 failed. The 2 failures are the two serve.rs
  pins above (lib: 446 passed, 2 failed); every other binary passes.
- cargo clippy --offline --all-targets -- -D warnings: exit 0. With --features test-bins: exit 0.
- Live check (scratchpad review-T1b/live-T1b.sh): fake perch on 48113, copies of this slice's perch and fake_eidolon in ~/.cache/T1b-bin,
  the bearer sent from a 0600 header file, the login URL discarded. Checked: create 201 with size and title, listed size and title, no
  env in the list, input text/plain 415, input 'echo hi' + CR 204, input 'echo ab$((6*7))' 204, text shows hi and ab42, text lines=abc
  400, resize 200, hold on and off 200, close 202, text after close 404, no credential 401; --stop: port closed, perch gone, tree removed.
  Failures 0. Leftover descendants of the perch 0.
- Leftover `sleep 1000` processes and processes with a cwd under a test tempdir after the whole suite: 0 (counted, nothing killed).
