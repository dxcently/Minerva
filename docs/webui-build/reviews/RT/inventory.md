# RT security inventory

Final HEAD 8435c9a ("perch: a chat tag is read as raw JSON, so 1e400 and a lone surrogate load") on branch worktree-wf_ce3f2350-5aa-1, base b761462. Commits since b761462: 0717fde (the slice), f885903 (a non-string chat tag reads as no tag), e771c46 (the README's odd-tag wording, no code change), 8065661 (a chat tag of any depth is no tag), 8435c9a (the tag is read as raw JSON: RawValue with the raw_value feature replaces the visitor, so 1e400, -1e400 and a lone surrogate are no tag instead of setting runs.json aside; tests/README.md's runs.rs row is 0717fde's text again, as granted). Claims for a reviewer to check against the code and tests. Line numbers are for 8435c9a.

## Files changed

- perch/Cargo.toml: the serde_json line only (line 58), `{ version = "1", features = ["raw_value"] }`. No new crate: perch/Cargo.lock is unchanged (`git diff --stat -- perch/Cargo.lock` is empty).
- perch/src/runs/state.rs: Run.chat with deserialize_with tag_or_none (156-157); tag_or_none (doc 185-188, fn 189-192: `Option<Box<RawValue>>`, kept only when `serde_json::from_str::<String>` reads the raw text); the `RawValue` import; Run::new (161); row() shows chat only when it passes sessions::id_ok (405-406); route() (491) passes the query to list; chat_query (506); list (514); create: chat shape (556-559), graph name, workflow file, home, chat existence (575-584), holder, health gate, begin (592), 202 chat (612-613); chat_known (621); busy with holder chat (626-630); CHAT_SHAPE (63); the eidolon paragraph and route table in the module doc (1-40); unit tests the_list_takes_one_chat_or_no_query (901), a_chat_is_in_the_same_write... (928), a_chat_that_is_not_a_string_is_no_tag_and_the_file_still_loads (940-956), a_chat_nested_past_the_depth_limit_is_no_tag_and_the_file_still_loads (958-972), a_tag_serde_json_cannot_read_as_a_value_is_no_tag_and_the_file_still_loads (974-996). The serde::de visitor imports are gone (none were used elsewhere).
- perch/src/runs/README.md: routes (line 22: `a chat= with no id`), the row, and "The chat tag" (lines 47-60): a tag that is not a JSON string is no tag, of any type, depth or range; nothing about the tag sets the file aside.
- perch/tests/runs.rs: non-stand-in chat tests at 409-544 (the odd-tag test at 516); the new integration test a_tag_of_any_json_shape_loads_and_only_a_chat_id_answers (546-572); stand-in tests in mod flow (1461-1583 area, unchanged).
- perch/tests/README.md: the runs.rs row is 0717fde's text again ("R1a, RT ... the chat tag and `?chat=` (RT)"). Granted and kept, per RT-fix3.md.
- Not touched by RT: sessions.rs, term/*, proc.rs, serve.rs, statefile.rs, lib.rs, main.rs, tests/common, tests/http.rs, Cargo.lock.

## Inputs and answers

POST /hub/runs, field `chat`:
- absent or JSON null: no tag. 202 is exactly {rid, state}; row, detail and runs.json carry no chat key.
- not a string (number, bool, object, array): 400, sentence CHAT_SHAPE, nothing made.
- a string that fails sessions::id_ok (empty, leading non-alphanumeric, any char outside [A-Za-z0-9._-], over 128 bytes, `/`, `%`, `=`, space, newline, non-ASCII): 400, same sentence, nothing made.
- a well-formed string the perch does not know (not remembered in chat-cwds or a door it holds, and not in registry.list()): 404 {error: "no such chat"}. Nothing made; no health probe; no holder check reached.
- a known chat: 202 {rid, state, chat}. The tag is written in the same write as the new row (begin, 318, and the write it makes).
- Check order in create() (538): body object, graph string, args object, args cap (64 KiB), chat shape (400), graph name (404), --plugins-dir (503), workflow file (404), --home (503), chat existence (404), holder (409), health gate (503), begin.
- 409 body gains `chat` = the holder's stored tag, only when it passes id_ok (busy(), 626-630). Sentence unchanged.

GET /hub/runs?chat=<id>:
- Only the exact query `chat=<value>` with the raw value passing id_ok (chat_query, 506-512). No percent-decoding: `%` is not an id character, so `chat=a%2Fb` is 400.
- Empty query (`/hub/runs?`) is no filter. Anything else (no chat, repeated chat, another key, `chat=a=b`, `chat=a&x=1`, `chat=..`, `chat=`) is 400 with a sentence.
- Filter runs before take(LISTED), so one chat's runs keep their own newest 50 (list, 514).
- Unknown or deleted chat: 200 []. No existence check on reads.
- Other verbs ignore the query, as before (verb(), 462, reads method and path only; serve.rs routes by path).

Reads of stored state:
- row() (389) adds `chat` only when the stored value passes id_ok (405-406), so a hand-edited runs.json cannot put a path or markup in the page's list, detail, cancel or abandon answer.
- statefile::load does no field check (unchanged). It calls serde_json::from_slice::<BTreeMap<String, Run>> directly (statefile.rs:22), and no type in perch/src/runs/ or statefile.rs has #[serde(flatten)] or an untagged enum (a grep for both is empty; the only "flatten" hits are Option and iterator calls), so RawValue is never handed buffered content. The load path is proven by the unit test through Store::open.
- tag_or_none (189-192) reads the chat as raw JSON text. RawValue checks syntax only and builds no value, so a number past f64's range (1e400, -1e400) is no error, and a nesting past 128 levels is no error either (serde_json skips the raw value iteratively). The text is kept only when `from_str::<String>` reads it, so a lone surrogate (`"\ud800"`) is no tag. A string that decodes to an id (`"aA"` is `aA`, `"ok1"`) is the tag. Every other JSON type (number, bool, null, object, array) is no tag.
- A string that is not an id is kept on the struct and left off every answer by row().
- One odd tag does not set runs.json aside. Only a file that is not valid JSON at all does, as before for any field.
- Guarded by unit tests a_chat_that_is_not_a_string_is_no_tag_and_the_file_still_loads (940), a_chat_nested_past_the_depth_limit_is_no_tag_and_the_file_still_loads (958) and a_tag_serde_json_cannot_read_as_a_value_is_no_tag_and_the_file_still_loads (974, through Store::open, eleven shapes), and integration tests a_chat_that_is_not_a_string_is_no_tag_and_one_odd_tag_sets_no_run_aside (tests/runs.rs:516) and a_tag_of_any_json_shape_loads_and_only_a_chat_id_answers (546, the eleven shapes through the real perch; `ok1` and `aA` answer a chat, the rest none, and `?chat=aA` and `?chat=ok1` list exactly r5 and r6).
- Revert check (scratch copy, not in the commit): with the commit's tests and the 8065661 reader (visitor, serde_json = "1"):
  - 1e400 and -1e400 each make serde refuse the whole file: the unit test goes red at `assert!(file(home.path()).exists(), "runs.json is where it was, not set aside")` (state.rs:1025 in the scratch copy); the integration test goes red at `every run is listed: []` (left 0, right 11).
  - the lone surrogate does the same: runs.json set aside, 0 runs, the same two assertions.
  - a per-shape probe (scratch only) confirmed: 1e400, -1e400 and "\ud800" each set runs.json aside with 0 runs listed; "aA", "ok1", 7, a 200-deep array, [1], {"x":1}, true and null each load, with the expected chat.
  - the same probe on 8435c9a: all eleven shapes load, runs.json kept, two runs listed, only the id strings answer r1.
- Edit, resume, give_back, abandon and cancel clone the whole record, so the tag survives them (unit test a_chat_is_in_the_same_write... covers edit).
- Consequence: a non-string tag, of any type, depth or range, is dropped from runs.json on this perch's next write (the file is written whole). A string that is not a chat's id is kept on disk and left off every answer, and the next write keeps it. Not a leak: a non-string tag was never a valid label, and a string that is not an id never reaches an answer.

## Decisions applied (operator and RT-fix3.md)

- Known chat = term's rule (known_cwd, then list). chat_known (621) is a twin of term::routes::chat_folder (term/routes.rs:145-156): same order, same id_ok first. No shared Registry::knows yet.
- Holder stays the perch's 409 authority; no holder check on reads; /hub/runs/health unchanged.
- The tag is a label: never proof or authority, nothing granted because of it.
- The tag never reaches eidolon: not in start_argv, resume_argv, config.toml, args.json, the child environment, any path or file name, or a log line. proc.rs is untouched.
- Pruning at KEPT (500) is across all chats (documented).
- A perch from before RT drops tags on its next write (documented; write() rewrites the file whole).
- RT-fix3 item 1 (option B): the raw_value feature on serde_json, no new crate, Cargo.lock unchanged; the visitor is gone.
- RT-fix3 item 3: runs/README.md's odd-tag sentence and the tag_or_none doc say what the code does now; README:22 reads `a chat= with no id`.
- RT-fix3 grant: perch/tests/README.md's runs.rs row kept (0717fde's text).

## What is never exposed

- The chat id is never in an eidolon argv (fake_records check in a_run_started_beside_a_chat_keeps_the_chat_on_every_answer), nor in any file under runs/<rid>/ (walk-and-grep test).
- No file path is ever derived from the chat. It is compared as a string and echoed in JSON only (the only joins are home/runs/<rid>, and rid is checked by rid_ok).
- A refused start writes nothing: no runs/ folder, no runs.json, no child, no health probe (tests at 409-544).
- No bearer, login code or login URL is in any file or report. The test harness uses a temp HOME; nothing reads ~/.aoide, ~/Minerva or ~/.config.
- No raw tag text is ever echoed: a non-string or non-id tag is never in an answer, a log or an error sentence.

## Risks seen (for the reviewer to weigh)

1. A tagged start calls registry.list() under spawn_blocking (which reads the sessions directories, including eidolon's read-only one) only when the chat is not remembered by known_cwd (chat_known short-circuits). Cost is at most one /hub/sessions-sized listing per tagged start for an unremembered chat; behind the bearer. Low.
2. 404 "no such chat" versus 202 tells a bearer holder whether a chat id is known. This is the same information /hub/sessions gives. Low.
3. The tag is a page claim. Any future agent route for runs must filter by agent::Caller.chat, never by a body or query value. Not enforced by code today (no such route exists).
4. A tag can name any known chat, including one the user did not open beside it, and a chat in the trash whose folder is still remembered (U1, accepted).
5. Chat ids are millisecond stamps: an id reused by a later chat would inherit older runs' tag (U7, accepted).
6. Pruning across chats can remove one chat's old runs when other chats are busy (documented).
7. RawValue copies a tag's raw text, and from_str::<String> copies it again. runs.json is 0600 and written by this perch, so the size is bounded by what the perch wrote. Low.
8. OUT OF SCOPE, seen, not changed: journal_of and answer trust the `session` path stored in runs.json, without proc.rs decide's check that it is inside the run's folder (proc.rs ~495-501). A hand-edited runs.json could steer a resume's --session at another file. Follow-up for the runs owner.
9. OUT OF SCOPE, seen, not changed: term's Record::vetted filters chat with screen::shown, weaker than id_ok (term/mod.rs ~200-202). Follow-up for the term owner.
10. Tag persists in runs.json (0600), alongside chat ids already listed in sessions. No new secret is stored.
11. A syntactically invalid runs.json still sets the whole file aside, as before. A hand-edited file that is not JSON loses its runs. Pre-existing; not changed.
12. Pre-existing, unrelated to RT: tests/guard.rs nested_find_exec_and_repeated_flags_are_linear_and_bounded asserts a 60 s wall-clock bound. The final suite run on 8435c9a passed it in 27.45 s. It exercises perch::guard only; RT changed no guard code.
13. OUT OF SCOPE, pre-existing, not changed: the `args` field is Option<Value>. serde_json refuses it past 128 levels, so a hand-edited args nested 200 deep still sets runs.json aside, on master and on this branch. The chat tag no longer has this limit. Follow-up for the runs owner if the same guarantee is wanted for args.
