# R4 executor's security inventory (model output: claims to check, not facts)

Commit 10bd6ab on worktree-agent-adba1e34b92716086, base cc0da12. Files: perch/src/install/mod.rs, perch/src/install/README.md,
perch/src/bin/fake_minerva.rs, perch/tests/install.rs. The README carries the same input table.

## Inputs accepted and how each is bounded
- Path: matched whole against six literals; anything else, or the other method on a known path, is the empty 404 (update, rotation,
  jev and remove-all are nobody's). A query string is a 400. A trailing slash is the bare path (serve's rule).
- Authorization (arm only): Bearer compared in constant time through gate::token_ok; never logged or relayed. The install module
  re-parses Authorization itself (serve.rs's bearer_ok is private).
- Sec-Fetch-Site, Sec-Fetch-Mode, Sec-Fetch-Dest, Origin (arm only): any present = a browser made it = 403.
- Bodies via serve::body_of: 32 KiB for inspect/add/remove, 1 KiB for arm. JSON objects with exactly the named fields
  (deny_unknown_fields); anything else 400.
- pubkey: not blank, no NUL, at most 4096 bytes; to the child's stdin only, never argv.
- confirm, fingerprint, id: parsed to Hex16 (exactly 16 hex digits after dropping ASCII whitespace, re-spelled upper case; max 64
  chars input). Only a Hex16 can reach argv.
- Perch CLI: --minerva, hidden --knob install_arm_ms and install_timeout_ms; knobs only shorten (never beyond 120 s and 5 s).
- Perch environment: only HOME, XDG_CONFIG_HOME and MINERVA_* reach the child (env cleared). The perch reads and writes no files.
- Which minerva: <canonical exe dir>/minerva if an executable file, else --minerva. Never PATH. Under /nix/store (raw exe, canonical
  exe, or the flag) it is {installed:false, managed:"nix"} and nothing runs.
- Run bounds: fixed argv from a closed Verb enum (five shapes), cwd /, stderr null, kill_on_drop, 5 s timeout (504), 64 KiB stdout cap
  (read cap+1 then kill, 502). 4 readers at once (fifth 503). One key write at a time (429), lock held across a remove's list read.
  At most 16 armed fingerprints (17th 429).
- Answers from minerva are untrusted: parsed into typed shapes, strings cleaned (control, bidi, zero-width dropped, length-cut), the
  page gets a re-serialisation. keys list strict: one bad row = 502, capped at 256 rows. Unknown source label = "other". add/remove
  done only when exit code and JSON agree; a removed id must equal the asked id.

## Secrets
- Bearer only in the arm request's Authorization header and Ctx for the compare. Never in argv, child env, logs, answers, errors.
- Public keys, fingerprints and ids are not secret; logged only as the id of an added/removed key plus a status code.
- An arm is memory only (fingerprint + deadline); a restart forgets it.

## Deviations it claims
- Arm also refuses any Sec-Fetch-*/Origin request (beyond D4's bearer-only).
- An arm is spent only by an add/remove that reaches minerva, whatever minerva answers; perch-side refusals (no arm, busy writer,
  remove of a non-"added by hand" key, wrong fingerprint) keep it.
- Child gets an env allowlist. Knobs only shorten.
- Remove adds a perch-made kind "rotation" for rotation keys; an inexact source label and unknown minerva kinds are "failed".
- Status JSON re-serialised: keys alphabetical, extra fields dropped; jev.pid only when running.

## Residual it names
- A script in the page's origin could swap the displayed fingerprint; the UI should tell the person to compare it with what the
  terminal printed.

## Operator decision this slice implements (D4)
Key add/remove from the page is armed by `hoot keys allow-web <FP>` in a terminal: one add/remove of that FP within 120 s; the
fingerprint is always typed back.
