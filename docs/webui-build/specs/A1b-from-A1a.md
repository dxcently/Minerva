# What A1b and A2 must know from A1a (watch.rs, as merged)

- `Sink::ask(&self, key, ask: &AskNote)`. AskNote {ask_id, kind, call_id, tool, structural, origin (snapshot at open), seen_seq} has no
  input and no reason: read them from `AskView::ask(id)`, which returns an `OpenAsk` with `input()` (parses), `input_json: Arc<str>`
  (at most 64 KiB) and `reason`. That also gives the LIVE origin: a matcher must re-read the origin with `AskView::ask(id)` at match
  time, never trust the AskNote snapshot.
- `AskView` has `asks()`, `ask(id)`, `seq()`, `max_ask_id()` and `is_closed()`. After the watcher ends, `ask()` is None and `asks()`
  is empty: check `is_closed()` and the DoorKey before acting.
- `seen_seq == 0` means the ask pre-dates the watcher (first seen in a reconnect replay before caught-up). Rule matching must require
  `ask.seen_seq > rule.stamp`, so 0 never matches. A rule can also reject asks whose id is at or below its `max_ask_id()` stamp.
- Origin `None` means unknown or disputed and must never match. A Split origin is sticky.
- `Closed` is always told, once, and last. It is not an event in the queue; `deliver` calls `sink.closed(key)` on a blocking thread.
- A sink that panics in `hello` or `watching` makes `open` return Err (the door is SIGTERMed).
- A goodbye now stops the watcher, so the registry must let the reap settle the row.
- `watch::open` still takes `(key, token, within, sink)`. `open_paced` is private.
- The fake (fake_eidolon/asks.rs): ask ids are unique per process; `on_answer` returns 409 for a second answer, 422 for an answer not in
  `answers` or the option labels (the ask stays open), and logs `answer-refused ask_id=N status=422`. A question with no options takes
  any answer. FAKE_LAG sends a `lagged` frame and does not model loss.
- Upstream wish list: an `origin` on the ask frame and `running` in `hello`.
