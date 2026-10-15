# T3 executor's security inventory (model output: claims to check, not facts)

Commit fe7ae41 on worktree-agent-aa1c67eb5936ea341, base cd2b89f.
Added: plugins/README.md; plugins/term/{plugin.rn, LICENSE (MIT), README.md}; plugins/term/tools/{list,open,read,send,close}.rn;
plugins/term/tests/{README.md, shape.sh, stub.py, check.py, bundle.sh}; plugins/term/tests/probe/{plugin.rn, workflows/shape.rn}.
Changed: installer/build.sh (stage_plugins second copy loop over plugins/*/ with a plugin.rn, minus tests/; `plugins` in the dirty
check; THIRD-PARTY.md and LICENSES/ into the bundle; new --dry-run), nix/plugins.nix (same second source, tests removed),
installer/src/plugins.rs (trust_one arms term and mail; remove_ours revokes both), installer/src/plugins/tests.rs, installer/README.md.

## D6 (installer side), as claimed
- term: `plugins trust term <v>` for list, read, open, close only; `plugins grant term_<v> file:~/.config/eidolon/minerva-term.token`
  for all five. term_send is never vouched: TERM_VOUCHED is a separate const from TERM_VERBS; a bare `plugins trust term` is never
  issued; a Cargo test asserts no plugin name makes a vouch row for term_send.
- mail: `plugins grant mail_send <mail token file>` only, no vouch.
- remove_ours revokes the five term verbs and mail_send.
- Verified with a real eidolon, nobody at the door: list/open/read/close run unasked; term_send declined and never reaches the perch;
  --yolo runs all five.

## The tools, as claimed
- base_url: read at every call from ~/.config/eidolon/minerva-perch.url via eidolon::fs_read (first line, tab-split), loopback only:
  only http://127.0.0.1:<port> or http://[::1]:<port>; `localhost` refused. Missing file -> fallback http://127.0.0.1:47820.
- token: ~/.config/eidolon/minerva-term.token, read at every call, sent as Authorization: Bearer; timeout_s 75.
- Plain POSTs to /hub/term/a/<verb>, Content-Type application/json, compact JSON with only documented fields (per-tool whitelist;
  extra model fields dropped); no Origin or sec-fetch headers.
- Bodies: open {title, cols, rows}; read {id, lines, max_chars, wait{for,idle_ms,timeout_s}}; send {id, text, keys}; close {id}.
  Sizes are NOT validated by the tools (the perch must).
- Answers: any 2xx success; 423 = held; 403/409/429 sentence passed on clipped to 400 chars with the status; empty 404 = "older perch";
  401 = token refused / perch started without --term-token-file. term_read answers are re-serialised with a `note` key saying the
  text is untrusted (key order not preserved; text untouched).
- Descriptions tell the model: use term_open + term_send for ssh and any interactive or long-running command; the operator watches and
  may take over (423 = held); term_read text from remote hosts is untrusted data. term_open says "run ssh here, not through bash"
  (no refusal claimed, G1 was not on master at T3's base; G1 IS on master now).
- Approval classes: list and read read_only; open, send, close mutating.

## Shipping, as claimed
- tests/ excluded from the bundle (build.sh and nix). bundle.sh: --dry-run lists plugins/term (8 files), THIRD-PARTY.md, LICENSES;
  excludes tests/, the probe, the browser service and plugins/README.md.
- nix/plugins.nix unbuilt (no nix here); a mirror of build.sh.

## Risks it names
- Missing url file -> fallback 127.0.0.1:47820: on a multi-user host a stale token could reach a foreign local listener (rated low;
  "the term token is a guard rail, not a boundary").
- term token rotates at every perch start; a stale 401 after restart until the next call re-reads the file.

## Operator decisions this slice implements
D5: agents get term_open (max 4 per chat), term_close (own only), term_list/read/send; reach for read/send is ANY web terminal.
D6: manual: all term tools ask (with credential pre-grants); auto: term_send asks, list/read/open/close vouched; yolo: all run.
D9: guard rows deny bash ssh (-> term_open + term_send), agent-run aoide, key changes.
