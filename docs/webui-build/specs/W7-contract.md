# What W7 must know about R4's /hub/install (from R4's report, 10bd6ab)

## Responses
- GET /hub/install:
  - {installed:true, version, notice: null | {line, latest, stale, checked} (each text field may be null), plugins:[{name,version,how}], jev:{set_up,running,pid?}}
  - | {installed:false, why}
  - | {installed:false, managed:"nix", why}
- GET /hub/install/keys: a bare array of {id (upper hex), fingerprint ("0F7A 3A5C 447E C7EF"), state trusted|revoked|expired,
  source "compiled in"|"added by hand"|"added by a signed rotation"|"other", expires null|"YYYY-MM-DD"}.
- POST inspect {pubkey}: {id, fingerprint, comment}, or 422 {why}.
- POST add {pubkey, confirm}: {added: id}. POST remove {id, confirm}: {removed: id}.
- Errors are {why}, a sentence safe to show. 409s add `kind`: mismatch|revoked|collision|compiled|unsafe|not-found|failed|rotation.
  Key routes with no minerva: 404 {why} (kind "nix" under Nix).

## Statuses
400 bad input · 403 not armed · 409 refusal · 422 not a key · 429 a write is running or too many arms · 502 unreadable/unrunnable ·
503 reads busy · 504 timeout. After a 504 on add or remove, re-list to see whether it happened.

## The arm (D4)
- The 403 not-armed sentence is the exact terminal command to show: "in a terminal: hoot keys allow-web 0F7A 3A5C 447E C7EF, then add
  within 2 minutes". The page cannot arm (a cookie or any browser request gets 403); only the terminal can.
- POST /hub/install/keys/arm is bearer-only and refuses any request carrying Sec-Fetch-* or Origin.

## Flow
The person pastes the key -> inspect shows id, fingerprint, comment -> the person compares the fingerprint out of band and types it back
as confirm (any case or spacing) -> add. Offer remove only on "added by hand" rows. Refetch the list after added/removed.

## Residual the page must not hide
A script in the page's origin could swap the displayed fingerprint, so the UI tells the person to compare it with what the terminal
printed (hoot prints it), not only with what the page shows.

## From R4's security review (wf_504cd28a-134), W7's part
- output-3: never branch on `kind` for a 5xx. After an add/remove 502/504 the change may or may not have happened (kind "unknown"):
  re-list the keys and show what the list says.
- output-6: show the key `comment` in its own element with dir=auto, unicode-bidi: isolate, overflow: hidden and a fixed line height,
  apart from the fingerprint, so bidi runs or stacked combining marks cannot dress the fingerprint up.
- 409 kind "listed" (new): "that key is already in the list".
- arm-3: say plainly in the overlay that the terminal arm guards against a page alone, not against someone who holds the page's cookie
  once web terminals exist.
