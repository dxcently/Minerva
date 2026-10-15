# T1b review (haiku) confirmed 19, rejected 1; critic merge-after-fixes

## input-hold-flush-fails-open [low] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\pty.rs:998
A failed hold flush is answered as a hold that took effect

Scenario: An agent line (for example `curl x | sh`) is already written to the PTY and unread while `make` runs in the foreground. The perch is at its fd limit (serve.rs:154-161 shows EMFILE is a known state), so the open of the slave in flush_input (pty.rs:828-831) fails. set_hold logs that at debug level only (pty.rs:998-1000) and the route answers 200 {"hold": true} (routes.rs:243-244). The operator believes the stop worked. When make exits, the shell reads the queued line and runs it. The at-risk window is larger than AGENT_AHEAD suggests: TIOCINQ counts only finished lines in canonical mode (pty.rs:833-835), so an unfinished agent line is not bounded by AGENT_AHEAD (pty.rs:66).

Fix: Return the flush result from Terminal::set_hold. On failure answer 503 with a message that unread input may remain, keep the hold on, and let the operator press hold again. Do not answer 200 for a stop that did not complete.

Verify: Confirmed. set_hold returns only the changed flag (pty.rs:996-1005), and a failed flush_input is logged at debug and dropped (pty.rs:998-1000). The route answers 200 on Ok(_) without seeing the flush result (routes.rs:243-244). README.md:177 states the flush as a guarantee with no failure case. The trigger is narrow: open_slave (pty.rs:819-824, O_NOCTTY|O_NONBLOCK open by name) must fail with EMFILE or ENFILE at the instant of the hold. serve.rs:146-161 shows fd exhaustion is a reachable state, and perch/src has no setrlimit, so the soft limit is never raised. The rare step is the fd table being full when the hold lands, so low is the right severity. Adjacent, not in this finding: under the same EMFILE, pending_input() returns None (pty.rs:836-840), and write_loop then sets allow to the whole chunk (pty.rs:1303-1304), so the AGENT_AHEAD throttle also turns off. That is T1a code and worth the orchestrator's notice.

## input-terms-json-overflow-resets-ids [low] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\mod.rs:883
terms.json can outgrow the loader's limit, and that restarts terminal ids at t1

Scenario: write_state serializes every ended record with no size bound (mod.rs:883-885). Each saved screen is up to 64 KiB (screen.rs:62), and JSON escaping doubles every quote and backslash, so 16 records of quote-dense screens come to about 2.1 MB, above MAX_FILE = 2 MiB (mod.rs:79). On the next start load_file sets the file aside (mod.rs:910-912), its `next` mark is lost, and Terms::new starts ids at t1 (mod.rs:527). The comment at mod.rs:524-526 promises ids are never reused across restarts. A page that still holds t2 from before the restart can then send POST /hub/term/t2/input after a new t2 is open, and the bytes land in a stranger's shell (routes.rs:184). The trigger needs 16 ended terminals whose saved screens are nearly all quotes or backslashes, so it is contrived.

Fix: Bound the serialized size at write time, or raise MAX_FILE above the worst case (16 x 2 x SAVED_TEXT_BYTES plus record overhead, about 2.2 MB). Also keep `next` in a small file that a set-aside never touches, so the id mark survives a bad terms.json.

Verify: Confirmed, with the trigger narrower than 'quote-dense' suggests. write_state serializes with no size bound (mod.rs:883-885) after truncating to MAX_TERMS=16 (mod.rs:59, 882). Each record's saved text is at most SAVED_TEXT_BYTES=64 KiB (screen.rs:62, mod.rs:203). serde escapes each quote or backslash to two bytes, so the file reaches MAX_FILE (2 MiB, mod.rs:79) only when nearly all 16 saved screens are escapable characters, with overhead pushing it over. Below that density the file is fine. Reaching it deliberately is easy (a shell printing long runs of quotes in 16 ended terminals), and then a restart sets the file aside (mod.rs:910-912), next becomes 0, and ids restart at t1 (mod.rs:527), against the comment at mod.rs:524-526. The 'stranger's shell' impact is overstated: agent reach for send is any web terminal (brief D5), so there is no agent escalation. The realistic harm is misdirected operator input into a reused id, which keeps severity low.

## input-hold-flush-drops-operator-bytes [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\pty.rs:996
The hold's flush also drops the operator's own unread typing

Scenario: The operator types `make install` while a program runs, then presses hold before the program reads it. flush_input discards every unread byte in the slave queue (pty.rs:826-831), including the operator's, and nothing tells the operator. The route's doc (routes.rs:230-231) says the operator's own typing is never held, which is true of the queue but not of the PTY. No security impact; the executor's inventory item 10 already records it.

Fix: State it in the route's doc comment and in the README. tcflush cannot separate the two kinds of byte, so there is no narrower fix.

Verify: Confirmed as stated. flush_input (pty.rs:826-831) calls tcflush(TCIFLUSH) on the slave, which discards every unread byte, including the operator's typed bytes that are already in the PTY. routes.rs:230-231 says 'the operator's own typing is never held', which is accurate about the hold gate and Terms::input but silent about the flush. The finding's README fix is already done: term/README.md:177 says the flush 'also takes the operator's unread bytes', and pty.rs:991-995 says the same. Only the route doc comment is incomplete. No security impact.

## input-spec-rewritten-by-executor [info] <old scratchpad>\slices\T1b.json:
The operator's T1b spec now says 401 where it said 403

Scenario: Not a runtime failure. The executor edited the settled slice spec so the cross-site-cookie expectation matches the code. The code is right: gate 1 refuses a cookie without same-origin with 401 (serve.rs:283-285 and 375-388), and tests/term.rs:462-467 checks it. But an operator decision was rewritten by an executor without the operator reviewing it.

Fix: Keep the code. The orchestrator should record the spec change and get the operator's acceptance before merge.

Verify: Confirmed. The plan still reads 'a cookie with Sec-Fetch-Site cross-site gets 403' (pass2-final.json:540). The slice spec now reads 'gets 401 (gate 1 does not count it; the bearer with cross-site gets 403)' (slices/T1b.json:12). The code matches the executor's version: a cookie needs same-origin (serve.rs:279-285), and gate 1 returns 401 before the fetch-metadata gate (serve.rs:377-385). tests/term.rs:462-467 asserts 401. So the code is right, but the operator's settled spec was edited by the executor and now disagrees with the plan. The brief says to touch only the files the spec owns. This is a process item for the orchestrator to record and get accepted before merge, with no runtime impact.

## input-chunked-cap-holds-one-frame [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\routes.rs:325
A chunked body over 64 KiB is refused one frame late

Scenario: Limited refuses the frame that crosses the cap (http-body-util 0.1.5 src/limited.rs:48-56), so that one frame is already in memory when the 413 is sent. Memory is bounded by hyper's read buffer rather than by the body size. The declared-length path refuses before any byte is read (routes.rs:326-328). No practical failure.

Fix: None needed.

Verify: Confirmed as stated. http-body-util 0.1.5 (the version in Cargo.lock) limited.rs poll_frame takes the frame from the inner body first and only then compares it with the remaining budget, so the crossing frame is already in memory. Memory is bounded by hyper's read buffer, not by the body size. The declared-length path is refused before any read (routes.rs:325-328). The serve.rs:443-444 comment 'nothing over the cap is ever held' overstates this by one frame. There is no practical failure and no fix is needed.

## input-content-type-lenient [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\routes.rs:342
Content-Type is matched without case and parameters, not exactly

Scenario: The spec says octet-stream only, and the code accepts `Application/Octet-Stream; charset=x`. This is not a security gap. The value is not a credential. A cross-origin page cannot send it without a preflight, which gets a 405 (serve.rs:415-421). A cross-site cookie never reaches the route (serve.rs:283-285).

Fix: Optional: compare the whole value if the operator wants the literal spec.

Verify: Confirmed as a deviation from the literal spec, with no security gap. is_octet_stream (routes.rs:342-346) accepts parameters after ';' and matches the media type case-insensitively. The plan says 'application/octet-stream only' (pass2-final.json:532). The choice is disclosed (routes.rs:340-341, term/README.md:285-286). It is not a CSRF gap: octet-stream is not a CORS-safelisted type, so a cross-origin page needs a preflight, which gets 405 with no CORS headers anywhere (serve.rs:412-421). A cross-site cookie never reaches the route (serve.rs:279-285, 377-385). Optional change: compare the whole value if the operator wants the literal spec.

## create-live-cwd-unfiltered [low] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\pty.rs:1039
Live terminal cwd skips the shown() filter that titles and reloaded records get

Scenario: pty.rs:1039 (Terminal::info) and pty.rs:1145 (record_with) set cwd to self.cwd.display().to_string() with no screen::shown filter. A directory under a --root whose name contains U+202E or U+2066 (bidi), a zero-width code point, ESC or a newline is returned verbatim by GET /hub/term (serde_json escapes only control characters, so bidi and zero-width code points pass) and written verbatim to terms.json. Anyone with write access under a root, including an agent that runs mkdir in a terminal it can send to (D5 reach), can make the operator's list show a spoofed or invisible folder. After a perch restart the same terminal shows the cleaned string, because Record::vetted (term/mod.rs:200) filters cwd on load. The title is cleaned at open (mod.rs:608), so only cwd is inconsistent.

Fix: Apply the same filter in Terminal::info and record_with, or use one helper for both: chars().filter(|c| screen::shown(*c)).take(4096). Add a test that a folder named with U+202E lists without it, both live and after a restart.

Verify: Confirmed. Paths below are relative to the worktree root C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1. Terminal::info (perch/src/term/pty.rs:1039) and record_with (pty.rs:1145) send self.cwd raw. Record::vetted (perch/src/term/mod.rs:200) runs screen::shown over cwd on load, so the live list and the list after a restart differ. screen::shown rejects the INVISIBLE table, which holds 0x200B-0x200F and 0x202A-0x202E (perch/src/term/screen.rs:274-275, 298-299), so U+202E in a folder name passes the live path. The title is cleaned at open (mod.rs:608), so the mismatch is cwd-only. Reachability: an explicit cwd is canonicalised and kept, so a bidi name in a folder under a --root survives into Info.cwd. Any local writer under a root can create that folder. The agent variant the reviewer describes is not live at this commit: the agent routes are the 404 stub in perch/src/term/agent.rs. Outcome is a display spoof in the operator's list, with no change of authority. Low stands.

## create-home-default-edges [low] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\mod.rs:352
Default cwd: HOME unset gives /, HOME empty or a deleted chat folder gives a 422 about a folder the caller never named

Scenario: Host::home (term/mod.rs:351-352) falls back to "/" when HOME is not in the perch's environment, and open (mod.rs:593-594) starts the shell there with 201. The README documents only $HOME, and "/" is not root-checked, so the operator gets no warning. HOME="" gives PathBuf::from(""), which fails is_dir (mod.rs:594), so every create that names no cwd (and every chat with no remembered folder) returns 422 "that folder is not a directory" (routes.rs:163), naming a folder the caller never gave. A relative HOME is resolved against the perch's working directory at spawn time, and Info.cwd shows the relative string. A chat whose remembered folder was deleted gets the same misleading 422, because known_cwd (sessions.rs:532) does not check existence. An explicit "cwd": "" is a 422 rather than the default.

Fix: Resolve the default once in Host::of: HOME must be a non-empty absolute directory, else refuse with a sentence that names HOME (or fall back to the scratch dir, if the operator agrees). Give the default case its own refusal text. Document the unset-HOME behaviour in term/README.md. Treat an empty cwd string as absent, or refuse it with a clear reason.

Verify: Confirmed by reading, with one correction. Host::home falls back to "/" when HOME is absent (perch/src/term/mod.rs:351-352). Nothing refuses it, and README:40-41 documents only $HOME. HOME="" gives an empty PathBuf. open's is_dir check (mod.rs:593-594) returns OpenError::Cwd, which routes.rs:163 maps to 422 "that folder is not a directory". The correction: that sentence names no path at all. It is misleading because it refers to a folder the caller never named, not because it prints one. known_cwd (sessions.rs:532-534) has no is_dir filter, while reopen_cwd (sessions.rs:552) does, so a deleted remembered folder gives the same 422 for a create with a chat. An explicit "cwd": "" goes through cwd_under_a_root (routes.rs:115-116), fails canonicalize (sessions.rs:498), and returns 422 rather than the default. Hazard: a surprising start directory and a misleading error, not authority. Low.

## create-cwd-refusal-echoes-canonical [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\sessions.rs:502
The 422 cwd sentence names the symlink-resolved path, not the one the caller sent

Scenario: cwd_under_a_root (sessions.rs:497-508) canonicalizes before the root test and formats "cwd {} is outside every --root" with the canonical path. A caller who names a link inside a root that points at /srv/other/private gets back "cwd /srv/other/private is outside every --root". The routes.rs header (line 5) says a refusal never names a path it was not given, and this one does. This is operator-only today: POST /hub/term needs the master bearer or the same-origin cookie, and agents reach only /hub/term/a/<verb>. The risk is reuse: a later caller that returns this sentence to a less-trusted party learns symlink targets and whether paths exist outside the roots.

Fix: Echo the caller's own spelling, or a fixed sentence, and log the canonical path instead. Keep the routes.rs:5 claim accurate.

Verify: Confirmed. cwd_under_a_root canonicalises (sessions.rs:497-498) and then formats path.display() in the 'outside every --root' sentence (502), the 'is not a directory' sentence (499) and the home-internal sentence (505). routes.rs:118 returns the anyhow chain as the 422 body. A caller who names a link inside a root that points outside therefore gets the link target in the answer. That contradicts routes.rs:5, which says a refusal never names a path it was not given. Reachability: POST /hub/term sits behind gate 1 (master bearer or same-origin cookie), so only the operator sees the sentence; the agent routes are a stub. Info. Reword the header or echo the caller's spelling.

## create-default-cwd-unchecked-inherited [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\routes.rs:116
Chat folders and HOME skip the root rule, and agent term_open will inherit that path

Scenario: An explicit cwd is checked (routes.rs:116). A create with a chat and no cwd starts in known_cwd(chat) (routes.rs:145-158; sessions.rs:532) with no root or home-internal check, and a chat with no folder starts in HOME. The body cannot choose these paths: known_cwd is written only by perch-side writers (mark_live from the door's hello cwd at sessions.rs:1011, insert at sessions.rs:1053, relocate.rs:152). So the unchecked surface is whatever those writers accept, which T1b does not own. D5 says an agent's term_open opens in the chat's cwd, so T2 will reuse the unchecked default unless it decides otherwise. The README section 'Where a shell starts' records the decision.

Fix: None for T1b. In T2, state in term_open's README whether its default goes through cwd_under_a_root and the home-internal rule, or not.

Verify: Confirmed as a fact about the code. routes.rs:120 uses the chat's folder without a root check. chat_folder (routes.rs:145-156) returns known_cwd unchecked. A chat with no folder starts in $HOME, also unchecked. README.md:276-279 records this as a decision. Writer note the finding omits: restart.rs:71 keeps the door's hello.cwd as given when cwd_under_a_root refuses it, so a path outside every root can become a remembered folder through the door's report. The reach adds nothing new, because a shell started in that folder gets the same tools the chat's door already runs there. Nothing to fix in T1b. T2 has to state whether term_open goes through the root check.

## create-cap-and-cwd-tests-missing [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\tests\term.rs:279
The cap and cwd edge cases the rule depends on have no test

Scenario: The cap code is right on reading (term/mod.rs:620-668): a terminal an operator opens with chat X is never counted against X's agent allowance (Owner::is_agents_of requires By::Agent), operator creates count toward MAX_TERMS, and they cannot evict ended terminals, so a full table is a 409 'close one first'. No test checks any of this with a chat: the 17th-create test (tests/term.rs:346) names no chat, and no test checks that X's four agent terminals still fit after operator creates linked to X. The cwd test (tests/term.rs:279-297) covers '/', '/does/not/exist' and one folder inside a root only. It does not pin a sibling named like the root (/root2 against /root), a link that leaves the root, a .. that leaves it, a plain file, a folder under <home>/sessions, or an empty or relative HOME. The code is right by reading (Path::starts_with is component-wise; canonicalize resolves links and ..), but a later change to a string-prefix check or to the canonicalize step would still pass the suite.

Fix: Add tests: a create with chat X does not reduce X's agent count; a root /x/root with a sibling /x/root2 gets 422; a link inside the root that points out gets 422; a create under <home>/sessions gets 422; HOME="" gives a clear refusal.

Verify: Partly refuted. The cap half is wrong: term/mod.rs:1648-1680 (an_agents_chat_holds_only_as_many_terminals_as_it_is_allowed) opens an operator terminal linked to chat c1 while c1 holds four agent terminals, checks it succeeds, and checks the chat's allowance and eviction afterwards. mod.rs:1095-1106 pins that an operator-owned terminal beside a chat is not the chat's agent's. mod.rs:630 maps By::Operator to chat None. The cwd half stands. tests/term.rs:279-300 covers only '/', '/does/not/exist' and one folder inside a root. sessions.rs:1700-1714 covers outside, nowhere and the plain file ctf-pwn-3.eid, which sessions_dir creates at 1337-1340. I found no test of a sibling root sharing a string prefix, a link leaving a root, a '..' leaving a root, or HOME empty or relative. tests/http.rs:1857 is tree listing, not cwd_under_a_root. A string-prefix regression in cwd_under_a_root would probably pass the suite. Drop the cap request from the fix; keep the cwd one.

## create-text-returns-shell-env [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\term\routes.rs:5
No-environment claim holds for the route's own fields, not for what a shell prints

Scenario: routes.rs:5-6 and term/README.md:254 say nothing an answer carries is the environment, a token or argv. The route's own fields are clean (Info has no env field, term/mod.rs:126). But GET /hub/term/<t>/text returns what the shell printed, and child_env (pty.rs:512-523) gives every shell the perch's environment minus MINERVA_* names containing TOKEN, SECRET, CODE or PASS. So *_API_KEY, SSH_AUTH_SOCK and cloud credentials reach every shell, and running env in any terminal returns them through /text. Because D5 lets agents read any web terminal, an agent can get those values with term_send then term_read. This is the T1a residual the code documents, but the T1b sentence claims more than the design keeps.

Fix: Reword both places to say the route adds no environment, token or argv, and that a shell can print what it has. Record in the D5 decision that agents can read it. If the operator wants it closed, strip *_API_KEY-style names in child_env (a T1a change).

Verify: Confirmed as a wording overclaim. child_env (perch/src/term/pty.rs about 512-523) drops only MINERVA_* names containing TOKEN, SECRET, CODE or PASS. Its own doc keeps SSH_AUTH_SOCK and *_API_KEY. README.md:117-119 and 226-228 document this. routes.rs:5-6 and README.md:254-255 say no answer carries the environment, yet GET /text returns whatever the shell prints, and running env in a terminal prints it. The agent chain the reviewer describes (term_send then term_read) is not live at 8e6233f, because the agent routes are a 404 stub (term/agent.rs). The master credential can already print env in any operator terminal, so there is no new exposure at this commit. Reword both sentences. Record agent read reach in the D5 decision when T2 lands.

## create-serve-pins-red [info] C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_2902c6f9-c37-1\perch\src\serve.rs:1552
Two serve.rs pins fail at this commit; the merge has to change them

Scenario: At 8e6233f, serve.rs:1552 still lists (GET, /hub/term) in the 404-with-token table, and serve.rs:1522 expects 404 for GET /hub/term with the master bearer. The route now answers 200 with [], so both assertions fail (the executor reports lib 446 passed, 2 failed). Merging the code without the pin edit leaves the suite red, and a later slice reading these pins as the contract would be misled.

Fix: At merge, change serve.rs:1522 (and the cookie check at 1523) to expect 200, and delete the (GET, /hub/term) entry at serve.rs:1552 from the 404 table, as the executor's inventory says.

Verify: Confirmed by reading; I did not run the suite. routes.rs:42-44 answers GET /hub/term with 200 and the list. serve.rs:1522 expects 404 for GET /hub/term with the master bearer, and serve.rs:1523 expects 404 with the same-origin cookie (the helper sets Sec-Fetch-Site: same-origin), so both now fail. serve.rs:1552 lists ("GET","/hub/term") in the every-seam 404 table. The diff stat has no serve.rs, so these pins stay red at 8e6233f. At merge: expect 200 at 1522 and 1523 (the finding's fix mentions only 1522) and delete the 1552 entry. This is a merge blocker, not a security defect.

## lifecycle-table-lock-across-proc [low] perch/src/term/mod.rs:568
info and list hold the table lock across each terminal's /proc reads

Scenario: A job-control pipeline whose first stage has exited leaves a foreground group with no live leader. Then Terminal::fg (pty.rs:1016-1020) falls through fg_leader (pty.rs:456-460) to session_members, a full read of /proc. Terms::info (mod.rs:567-569) calls Entry::info under the lock taken at mod.rs:568, and routes.rs:63 calls it for every /hub/term/<t>/... request before any work. Terms::list (mod.rs:562-565) does the same for all rows under the lock taken at mod.rs:564. While one request scans /proc on a runtime worker, input, close, text, get and create's reserve (mod.rs:620, 690, 756) on other terminals block their worker on the same std Mutex for the scan's duration. The normal cost is microseconds (leader alive), so this only bites in the pipeline case, and it grows with the process count.

Fix: Clone the Arc and Record entries out under the lock and call Entry::info after releasing it. Use a cheap existence probe at routes.rs:63 instead of info(), which runs fg(). Optionally cap the fg_leader fallback scan.

Verify: Confirmed. list (perch/src/term/mod.rs:562-565) and info (mod.rs:567-569) hold the one table mutex while mapping Entry::info; for a live entry that reaches Terminal::info (pty.rs:1030-1036), which calls fg() (pty.rs:1016-1023). fg() falls back to session_members, a full /proc read (pty.rs:382-401), only when fg_leader's first test fails (pty.rs:456-460: the group's leader must be alive, one /proc read). routes.rs:63 calls terms.info on the runtime for every /hub/term/<t>/... request before any work. The trigger is real: a foreground pipeline whose first stage has exited and been reaped leaves tcgetpgrp (pty.rs:812-815) naming a group with no leader. Impact is one /proc scan (milliseconds) under the table lock, stalling other terminals' table operations. No hang, no privilege change. Refinement: a foreground group with no members at all also takes the full scan on every call, so the trigger is wider than the finding says; severity does not change.

## lifecycle-text-resize-screen-wait [low] perch/src/term/routes.rs:273
text and resize wait on the screen lock with no bound and no cap on waiters

Scenario: A program floods output and a page or script issues many GET text or POST resize calls. Each parks a blocking thread in Screen::lock (screen.rs:953-955, a plain blocking lock) until the reader releases it. text_of's composition runs inside that lock (pty.rs:1089-1091, via view at pty.rs:1096), so the reader cannot parse while a text read is composed. Nothing bounds the wait (routes.rs:271-281), and serve.rs accepts connections with no limit (serve.rs:141-180). The reader releases the lock between reads and pauses 10 ms above 8 MiB/s (Throttle, pty.rs:340), so in practice the wait is bounded by the reader, not by the route. If the blocking pool saturates (512 threads, tokio builder.rs:306), then (a) Terms::close's hang-up (mod.rs:816-827) and Terms::shutdown's per-terminal hang-up (mod.rs:779-814) queue behind the waiters, so a shell is not signalled after its 202 or during shutdown until a thread frees; and (b) runtime drop at process exit waits for the blocking pool with no timeout (tokio 1.53.1 runtime/blocking/pool.rs:283-285, main.rs:104), so exit waits too. Runtime workers themselves are never blocked by this.

Fix: Give the screen wait a deadline and answer 503 when it runs out, as the try_lock loop in record_until already does. Compose the text after copying the rows out under the lock. Cap concurrent text and resize waiters per terminal.

Verify: Mechanism confirmed. text (routes.rs:271-281) and resize (routes.rs:223-227) wait on the screen lock with no deadline, via spawn_blocking; Screen::lock is a plain blocking lock (screen.rs:953-955); text_of composes under that lock through Terminal::view (pty.rs:1089-1100). Runtime workers are not blocked by this path: text and resize use spawn_blocking, and Terminal::info reads the atomic size (screen.rs:967-969; pty.rs:1031), so routes.rs:63 takes no screen lock. Close and shutdown hang-ups go through hang() on the blocking pool (mod.rs hang(), shutdown spawn_blocking), so at pool saturation they queue behind the waiters, as claimed; tokio's default is 512 (builder.rs:306). Exit: main.rs:104 is #[tokio::main] and main returns perch::run, so the runtime drops; tokio's BlockingPool Drop calls shutdown(None) and joins with no timeout (pool.rs drop). But the waits are bounded by the reader: it holds the screen lock for one read (at most 16 KiB, screen.rs feed) and releases it between reads, with a 10 ms pause above 8 MiB/s (pty.rs:75-77, Throttle at 340-358). Exit delay is the drain of those waiters, not an indefinite wait. Saturation needs about 512 simultaneous blocked requests from an authenticated caller, since serve.rs:141-180 has no connection cap. Severity low stands.

## lifecycle-sigkill-leaves-job-control-jobs [info] perch/src/term/pty.rs:625
a SIGKILLed perch can leave job-control background jobs running

Scenario: The operator's shell is an interactive login shell, so job control is on and `sleep 1000 &` sits in its own process group, not the foreground group. Under SIGKILL of the perch, end_session never runs. The master closes, and the kernel's tty hang-up signals the session leader and the foreground group, not a background group, so the job keeps running. The tests use /bin/sh (no job control), where the job shares the shell's group and is hung up. README.md:112-113 says only HUP-ignoring jobs outlive a SIGKILL, which understates the job-control case. Stated from my understanding of tty hang-up semantics; not run.

Fix: Document the job-control case in the README next to lines 112-113. Accept it as a known limit of SIGKILL, since no PDEATHSIG applies to a session.

Verify: The scenario is real for shells that do not forward SIGHUP, and refuted for bash. Probe, offline in WSL under $HOME/.cache/minerva-review-T1b: a Python holder owned the pty master (as the perch does), ran an interactive shell with 'sleep 1000 &' (job control on, own process group) and a 'nohup sleep 1001' control, then was SIGKILLed. With bash -i the job died and the nohup control survived (bash resends SIGHUP to its jobs). With /bin/sh (dash, job control on in a pty) the job-control job survived in its own group after the shell was gone; the control survived too. So the finding's mechanism holds for dash-type shells. The README (perch/src/term/README.md:112-113, not perch/README.md as cited) names only SIGHUP-ignoring jobs, which understates the dash case. The claim that the tests run a shell without job control is doubtful (dash -i on a tty had job control in the probe), though that is not verified for the perch's own spawn. Impact is orphaned processes after a perch crash, no privilege change, so info stands. Operator's login shell not verified; the probe did not touch the operator's machine or perch.

## lifecycle-exited-shell-jobs-stay-listed [info] perch/src/term/pty.rs:1336
an exited shell's background jobs keep running while the terminal reads alive:false

Scenario: `sleep 1000 & exit` ends the shell. wait_loop (pty.rs:1336-1346) calls ended() (pty.rs:1203), so the row shows alive:false, fg empty, and input answers 409. Nothing hangs the session up until close, the 30-minute prune (mod.rs:831-862) or shutdown. The job keeps running and keeps writing into the screen. A page that reads alive:false as nothing running is wrong. The T1a test at mod.rs:2358 pins this as intended.

Fix: Contract note for the page and T1c: alive:false means the shell is gone, not that the session is empty. Info carries no field that says a job is still running.

Verify: Confirmed. wait_loop (pty.rs:1336-1346) records the exit through ended (pty.rs:1203-1208) and never signals the session; the reap channel is taken only at hangup, abandon or drop (pty.rs:915, 935). Terminal::info reports alive from is_alive (pty.rs:970-972) and fg empty when not alive (pty.rs:1017-1019). Input answers 409 (routes.rs:194). The session is swept only through close, prune (mod.rs:831-862 path) or shutdown. The T1a test at mod.rs:2358 pins this as intended. Real, but a page-contract note, not a defect.

## lifecycle-ended-count-toward-cap [info] perch/src/term/mod.rs:620
exited terminals keep their slots against the 16 cap until closed or pruned

Scenario: Sixteen shells exit and stay listed. reserve (mod.rs:620-676) takes ended entries only for an agent's chat, so an operator create finds nothing to evict and answers 409 (routes.rs:160-161), though nothing is running. Slots come back only on close, or after KEEP_ENDED_MS (30 minutes, checked when list or open runs).

Fix: Contract note for the page: offer close on ended rows before offering a new terminal. Whether operator creates may evict the oldest ended row is an operator decision and a behaviour change, not for T1b.

Verify: Confirmed. In reserve, the ended list is built only when the owner is an agent's chat (mod.rs:633-649, under 'if let Some(chat)'). An operator create (chat None) gets an empty ended list, so by_table = (total + 1) - MAX_TERMS (mod.rs:650-654) with ended.len() == 0 returns OpenError::Cap (mod.rs:659-660). total counts listed ended entries (mod.rs:650). routes.rs:160-161 maps Cap to 409. Slots return on close, or by prune after KEEP_ENDED_MS (30 min, mod.rs:61), which runs on list and open (mod.rs:563, 591). Real behaviour; a contract note for the page.

## lifecycle-body-without-timeout [info] perch/src/serve.rs:166
no timer on the HTTP/1 connections, so a stalled request body is held open indefinitely

Scenario: http1::Builder is used without .timer(), so hyper's header_read_timeout (default 30 s, hyper-1.11.1 http1.rs:250) does not run, since it needs a Timer (http1.rs:346-347). A bearer or same-origin cookie holder can open an input or create request, send Content-Length and stop. The connection task waits forever (capped at serve.rs:516 has no timeout), and there is no connection cap (serve.rs:141-180). Shutdown still completes, because graceful.shutdown is bounded by RESPONSE_DRAIN (serve.rs:218), so this is resource use, not a hang.

Fix: Set a Timer and a header_read_timeout on the Builder, and a body read deadline on the /hub routes, or add a connection semaphore. This belongs to serve.rs's owner, not T1b.

Verify: The mechanism is real and predates T1b. serve.rs:166 builds http1::Builder::new() with no timer and no header_read_timeout; hyper-1.11.1 http1.rs:250 defaults the header timeout to 30 s but it needs a Timer (doc at http1.rs:346), so it is inert here. Body reads (serve.rs capped/body_of at about 509-535) have no timeout, and routes.rs read_body (325-333) inherits that. The accept loop (serve.rs:141-180) has no connection limit. Shutdown still completes because graceful.shutdown is bounded by RESPONSE_DRAIN (serve.rs:218). T1b does not touch serve.rs (per its inventory), and a header stall with no timer already applies to any connection before any route. T1b only adds body reads that inherit the hazard, so this belongs to serve.rs's owner, as the finding says.

## CRITIC
Test payload to isolate a schema problem. Verdict is merge-after-fixes. No high or medium gap survived the completeness pass.
- G1 merge gate: Suite red at the reviewed commit; not re-run. CHECK: Run cargo test and clippy at merge.
