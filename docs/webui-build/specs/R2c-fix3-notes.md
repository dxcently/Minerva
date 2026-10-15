# R2c follow-ups (fold into the aoide re-probe round; review wf_a36fab1c-ff2 at 0754f2f: merge-after-fixes, 0 high/medium)

- critic gap R10: the start-record message is kept uncapped and unscrubbed; README says it is a listen URL "as observed" but no checked writer
  produces that. Confirm the real start line against CURRENT aoide main (re-probe), else run the start message through the url rule
  (without_userinfo + drop on '@') and a length cap.
- leak-4 (low): webui/term/renderers/jev.js:21 MARKS lacks U+061C (ARABIC LETTER MARK); reachable via a pair-request gate url. Shared
  renderer, outside R2c: one-char fix + a test (orchestrator at merge, or the T1/page hardening slice).
- leak-6 (info): query-string credential passes the url scrub (http://host?token=abc). Hardening: drop a url whose query has a key like
  token/key/secret/pass, or drop the query.
- leak-8 (info): GET /hub/mesh registry `via` forwarded unscrubbed (not drawn). Apply without_userinfo to via.
- files-1 (low): feeds_apart runs at start only; a post-start same-uid link swap routes aoide's audit appends into aoided's feed (aoide
  audit.rs opens without O_NOFOLLOW -> Aoide-side fix belongs upstream in Aoide main). Re-check (dev,ino) of both paths per aoide call and
  per events poll; README says start-only until then.
- files-2..8 (low/info): unresolved spellings when the audit folder is missing; inode reuse after recreate; folder links followed (O_NOFOLLOW
  is last component only); daemon default diverges from aoided's; HOME unset -> no audit path; refused_start does not drain pipes; first
  poll error -> cursor 0 replay. Document or fix in the same round.
