# src/install/ — the install notice and update keys (`/hub/install`)

What the page shows about the install, and how an update key is added or removed from it. **This is the trust root of updates**: a key
added here is a key whose holder can have software installed on this machine by `minerva update`. So the module is small and closed, and
it is written to refuse. The perch only asks the installed `minerva`, with five fixed argv and the key on stdin. Page key changes use the
running perch's own `minerva`, so **restart the perch after an update**: a perch that has been updated underneath says so and does nothing.

```text
mod.rs    State (arms, one writer, readers), route(ctx, req, rest) -> Response<BoxBody>, the closed Verb enum, run() and run_write()   (R4)
```

```text
GET  /hub/install                         minerva status --json
       200 {installed: true, version, notice: null | {line, latest, stale, checked}, plugins: [{name, version, how}], jev: {set_up, running, pid?}}
       200 {installed: false, why}            no installed minerva for this perch: none beside an installed perch, none given with --minerva
       200 {installed: false, managed: "nix", why}    the perch (or the chosen minerva) is under /nix/store; nothing runs
       200 {installed: false, kind: "updated", why}   the perch's own files were removed or replaced by an update: start it again
GET  /hub/install/keys                    minerva keys list --json
       200 [{id, fingerprint, state, source, expires}]          a bare array; fingerprint is "0F7A 3A5C 447E C7EF"
POST /hub/install/keys/inspect {pubkey}   minerva keys inspect - --json           200 {id, fingerprint, comment} | 422 {why}
POST /hub/install/keys/add {pubkey, confirm}      keys inspect -, keys list, then keys add - --confirm FP --json
       200 {added: id} | 409 {why, kind} | 403 {why} (not armed) | 400 | 422 | 429 | 502 | 504
POST /hub/install/keys/remove {id, confirm}       keys list, then keys remove ID --confirm FP --json
       200 {removed: id} | 409 {why, kind} | 403 {why} | 400 | 429 | 502 | 504
POST /hub/install/keys/arm {fingerprint}  nothing runs        200 {armed: true, fingerprint, expires_in}    the bearer token only
```

Without a minerva every key route is a 404 `{why}` (a nix perch: `{why, kind: "nix"}`; an updated one: `{why, kind: "updated"}`). A path that
is not one of the six, the other method on a known path, and anything under it (`update`, `rotation`, `jev`, `remove-all`) is the empty 404.
A query string is a 400. A trailing slash is the bare path (that is `serve`'s rule, not this module's).

`kind` on a 409 is one of minerva's: `mismatch`, `revoked`, `collision`, `compiled`, `unsafe`, `not-found`, `failed` (a kind minerva does not
name is `failed`). This perch adds three of its own: `rotation` (a remove of a key that came with a signed rotation), `listed` (an add of a
key that is already in the list, by id or by fingerprint) and `unknown`. **`unknown` is the 502 or 504 of a write that was started and gave
no answer to trust** (it timed out, printed too much, died, or printed something this perch does not read): the change may have been made,
and the sentence always ends `; list the keys to see whether the change was made`. Refusals made before the write starts have no `kind`
(a read that failed) or one of the above (a refusal): never `unknown`.

## The step-up (D4)

```text
 terminal                         perch                                  page
 hoot keys allow-web <FP>  --->  POST /keys/arm {fingerprint}            (shows the key's fingerprint,
   (Authorization: Bearer)        one arm for FP, live 120 s              person types it back)
                                                                          |
                                  POST /keys/add {pubkey, confirm: FP} <--+
                                    armed(FP)?     no -> 403, nothing runs
                                    one writer?    no -> 429                         (the lock is now ours, an owned guard)
                                    keys inspect -   not a key -> 422; its own fingerprint is not FP -> 409 mismatch
                                    keys list        its id or its fingerprint is listed -> 409 listed
                                    spend the arm (single use) -> minerva keys add - --confirm FP --json  (key on stdin)
                                    minerva compares FP with the key's own fingerprint again
```

| Rule | What it is |
|---|---|
| who arms | the **bearer token in `Authorization` and nothing else**. The page's cookie is a 403, and so is any request carrying a `Sec-Fetch-Site`, `Sec-Fetch-Mode`, `Sec-Fetch-Dest` or `Origin` header: a page opened with `#token=` holds the bearer, and a browser cannot leave those headers out. `curl` sends none of them |
| one arm | per fingerprint; arming again moves the deadline. At most 16 fingerprints are armed at once (a 17th is a 429). Arms are in memory: a restart forgets them |
| 120 s | `now < armed_at + 120 s`: at 120.0 s the arm is gone (unit-tested on injected time, not by waiting) |
| single use | spent by the first add or remove of that fingerprint that **starts a write** in minerva, whatever minerva answers. A request the perch refuses before that leaves it alone: no arm, a busy writer, a text that is not a key, a fingerprint that is not the pasted key's, a key already listed, a key it will not remove. So a junk add never burns an arm |
| one key | the arm is for the fingerprint, and `--confirm <FP>` is what minerva checks against the key's own. An arm for X lets in only the key whose real fingerprint is X (the perch checks it first with `inspect`, minerva checks it again) |
| listed | an add of a key that is already in the list is a 409 `listed`, with the arm kept: the compiled-in key's 32 bytes under a fresh id, or a rotation key again, are the cases that matter. `minerva keys add` does not yet refuse a key whose bytes are already trusted under another id (an installer follow-up), so a page-made second entry for a trusted key would otherwise stand beside it |
| remove | only a key whose `source` is exactly `added by hand` (ruling 6). The real `minerva` removes a rotation key as well (it refuses only the compiled-in one), so **this check is the only guard for ruling 6**. The list is read first, under the same lock; `compiled in`, `added by a signed rotation` and anything else are refused with the arm kept |
| never | `update`, a rotation, `remove-all`, `jev-setup`: there is no argv for them, see below. `status --json` reads minerva's own files and asks no network |

## What may run

One enum, `Verb`, is the only thing `run` and `run_write` take, and its five arms are the five argv:

```text
status --json                                  keys list --json                       keys inspect - --json
keys add - --confirm <FP> --json               keys remove <ID> --confirm <FP> --json
```

| Rule | What it is |
|---|---|
| binary | the `minerva` beside the perch **only when the perch is an installed copy**: its canonical exe is `<prefix>/opt/minerva/<version>/bin/perch`, the shape the installer's `Layout::from_exe` knows. Any other perch (a development build, where `cargo build` of the installer leaves a stray `minerva` in `target/debug`) takes `--minerva` and nothing else. **Never a `PATH` lookup.** Beside an installed perch, the one beside wins over the flag. Under `/nix/store` (the raw exe path, the canonical one, or the flag's) is `managed: "nix"` and nothing runs |
| updated | an installed-shaped exe that has been deleted (`/proc/self/exe` reads `... (deleted)`) or no longer resolves is `kind: "updated"`: the sentence is `the server was updated; start it again`, and nothing runs. A development build rebuilt under itself is not an update |
| argv | the two values that vary, a fingerprint and a key id, are `Hex16`: exactly 16 hex digits, parsed from the request and re-spelled upper case by this crate, so neither can be an option, a path, a space or a `;`. Anything else is a 400 before a child exists |
| key | on **stdin**, never argv. 4096 bytes at most, not empty, no NUL |
| environment | cleared, then exactly `HOME`, `XDG_CONFIG_HOME`, `MINERVA_HOME` and `MINERVA_NO_UPDATE_CHECK` put back, and `MINERVA_FAKE_DIR` (only the test double reads it). Not a prefix: another of minerva's own settings, a provider key, `SSH_AUTH_SOCK`, a proxy or the runtime dir of the perch's shell does not reach it |
| process | working dir `/`, stderr to nowhere |
| reads | `status`, `keys list`, `keys inspect`: a 5 s timeout (504), at most 64 KiB + 1 byte of stdout (more is a 502), and the child is killed when it is not done in time, prints too much, or the request is dropped (`kill_on_drop`) |
| writes | `keys add`, `keys remove`: **never killed because the request ran long or went away.** On a fresh install there is no keys directory: minerva makes it and then writes `state.json`, and a kill between the two leaves an install where `minerva update` refuses every release until it is recovered. So the child and an owned writer lock go into a task of their own that waits for the exit; the request stops waiting at 5 s (a 504 of kind `unknown` that says to list the keys) or when its client leaves, and the task carries on. Output past 64 KiB is read and dropped, not a reason to stop. A hard cap of 60 s ends a child that is stuck (it is killed then, and only then) |
| concurrency | reads (status, list, inspect) four at once, a fifth is a 503; **one key write at a time**, a second is a 429. The lock is held from the first read of an add or remove until the write's child has exited, which can be after the request has been answered |

## What comes back is parsed, never relayed

Each answer is read into the typed shape the contract names (installer/README.md, "The perch contract"), every string is cleaned and the
page is sent a re-serialization. Extra fields are dropped, not forwarded.

Cleaning drops control characters, **every format character (general category Cf)**, the line and paragraph separators, and the
Default_Ignorable_Code_Point characters that draw nothing: the tag characters (a whole message nobody sees), `U+206A` to `U+206F`, the Hangul
and Khmer fillers, the combining grapheme joiner, and the variation selectors VS1 to VS14 and VS17 to VS256. VS15 and VS16 stay (they choose
how a character is drawn and hide nothing). The text is then trimmed and cut: a refusal sentence at 600 characters, a status line or stale
note at 300, a comment at 200, a version or plugin name at 64. **A cut text ends in `…`** (inside the limit), so a sentence that was
shortened does not read as a whole one.

| Answer | Taken as |
|---|---|
| `keys list` | strict: one row that is not `{id, fingerprint, state, source, expires}` (16-hex id and fingerprint, `state` one of `trusted` `revoked` `expired`, `expires` null or `YYYY-MM-DD`) makes the whole list a 502, because a list that quietly leaves a key out is worse than none. 256 rows at most. A `source` that is not one of the three labels spelled exactly is `other`, never one of them |
| `add`, `remove` | done only when the exit code and the JSON agree: exit 0 with `{added|removed: id}` and no `error`, or non-zero with `{error: {kind, sentence}}` and no success field. Exit 0 with an error, exit 1 with a success, empty output, non-JSON, a signal, a timeout, too much output, or a `removed` id other than the one asked is a 502 or 504 of kind `unknown` that claims nothing |
| `status` | exit 0 and the typed shape, else 502 |
| `inspect` | exit 0 with `{id, fingerprint, comment}`; a non-zero exit is 422 `that is not a public key minerva can read` |

## Inputs and secrets

| Input | Where it comes in | Bound |
|---|---|---|
| path | `rest` after `/hub/install`, matched whole against six literals | anything else is the empty 404; the query string is a 400 |
| method | the request line | the other method on a known path is the empty 404 |
| `Authorization` | the arm route only | `Bearer <token>`, compared in constant time (`gate::token_ok`); read nowhere else, never logged |
| `Sec-Fetch-*`, `Origin` | the arm route only | present means a browser made it: 403 |
| bodies | `serve::body_of` | 32 KiB on inspect, add and remove, 1 KiB on arm; JSON objects with exactly the named fields (`deny_unknown_fields`) |
| `pubkey` | inspect, add | 4096 bytes, not blank, no NUL; goes to the child's stdin and nowhere else |
| `confirm`, `fingerprint`, `id` | add, remove, arm | `Hex16` or a 400 |
| `--minerva`, `--knob install_*` | the perch's own command line | the operator's, not a request's; a knob can only shorten or take away |
| environment | the perch's own | only the five names above reach the child |
| files | none read or written by the perch | minerva reads and writes its own keys directory; the perch reads its own exe path to tell what it is |

| Secret | Where it can be | Where it cannot |
|---|---|---|
| the perch's bearer token | the arm request's `Authorization` header (compared) and `Ctx` | argv, the child's environment, a log line, any answer, any error |
| public keys, fingerprints, ids | the request body, the child's stdin and argv (fingerprint and id only), the answer | not secret. Logged: **the fingerprint of a key a terminal has armed** (`info`, so the log says which key a person allowed) and **the id of a key that was added or removed** (`info`). Never logged: key text, a comment, a status code |
| an arm | in memory, a fingerprint and a deadline | disk, an answer beyond `{armed, fingerprint, expires_in}`; its fingerprint is in the `info` line above |

## What it does not defend

* A script running **in the page's origin** that rewrites the fingerprint the page shows makes the person confirm the attacker's key. The
  person is told to compare the fingerprint with the one `hoot keys allow-web` printed, out of band; the perch cannot do that for them.
* Anything running as the perch's uid can run `minerva keys add` itself or read the bearer token from the perch's files. The arm raises
  the bar for a page and for a stolen cookie, not for the machine's own user.
* The arm is a bearer-token call: whoever holds the token and speaks without browser headers can arm. That is the same trust as the
  token's other calls, and the person still has to type the fingerprint back on the page.
* **Once the web terminals land (T1b), the arm has no wall of its own.** Whoever holds the page's cookie can open a shell in the page and run
  `hoot keys allow-web` there, and an agent's `term_send` can type it into a terminal it was given. The guard's term-text rows (G1) are a
  hint to a person reading the policy, not a gate. The step-up is a check that a person looked; it is not a boundary against the page.
* A restart drops the arms (safe). A crash of the perch while a write runs does not stop the write: the child is the task's, not the
  request's, and it is not killed with the perch either; it finishes on its own. Only the 60 s hard cap kills a write, and that is the one
  case where a write can be stopped partway.

## Test knobs and the stand-in

`--knob install_arm_ms=N`, `install_timeout_ms=N` and `install_write_cap_ms=N` (hidden) set the arm's life, a read's timeout and a write's
hard cap to N ms, **never longer than 120 s, 5 s and 60 s**; a value that is not a number is no knob. `--knob install_managed=nix` makes the
perch answer as a Nix install does (nothing runs); it can only take away. Tests use `src/bin/fake_minerva.rs` only: it answers the five
argv over a fixture directory, logs every call (argv, stdin, the env's names, never values) and refuses anything else with exit 2, so a route
that ran `update` would be seen. It misbehaves on request (`hang`, `slow`, `linger`, `big`, `garbage`, `empty`, `lie-ok`, `lie-error`, and
`write:<word>` for the two writes only), and it removes rotation keys as the real one does, so the perch's own check is what a test sees.
No test runs a real `minerva`. `tests/install.rs` is the HTTP seam (31 tests; the ones about which `minerva` is taken start a copy of the
perch binary in an installed-shaped temp dir), and the unit tests in `mod.rs` hold the parsers, the arm clock, the argv shapes and the
`resolve` rules.
