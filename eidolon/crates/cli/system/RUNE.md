# Writing Rune for Eidolon

Everything the operator can change about this harness is a Rune script —
Rune 0.14, a small dynamically-typed language with Rust's syntax and not
Rust's semantics. Four kinds of script run here, and the ones in this
directory are the ground truth for how each is written: read the nearest
one before writing a new one, and trust it over a search result — Rune is
obscure enough that a confident, fabricated "official" answer is a real
failure mode, and the sharp edges below were each found by a script that
compiled clean and crashed in use.

## Where a script lives and what it sees

| kind     | your copy                        | reference here             | checked by                       |
|----------|----------------------------------|----------------------------|----------------------------------|
| tool     | `../tools/<name>.rn`             | `tools/`                   | `eidolon tools`                  |
| policy   | `../policy.rn`                   | `policy.rn`                | `eidolon policy "<command>"`     |
| UI       | `../ui.rn`                       | `ui.rn`, `prelude.rn`      | `:reload_ui` in the TUI          |
| provider | `../providers/<name>.rn` (yours) | `providers/`, `examples/`  | `eidolon models`                 |

Every script sees the `json` module and an `eidolon` module — and nothing
else. There is no `fs`, `http`, `process`, `time` or `rand`: a script that
needs the filesystem goes through the host's primitives, which is what
puts every effect behind the policy gate and in the session log. What is
*in* `eidolon` depends on the kind of script; the UI script also sees a
small `ui` module.

A file that will not compile is a line on stderr and the shipped version
takes its place (`eidolon tools` prints the line); a `policy.rn` that
will not compile is a refusal to start, on purpose, since a gate that
silently reverted would leave you believing your edits were in force.

## A tool

```rune
pub fn manifest() {
    #{
        name: "read",
        description: "What the model reads to decide when to call it.",
        approval: "read_only",            // or "mutating": an input to the classifier, never the decision
        input_schema: #{ "type": "object", "properties": #{ … }, "required": ["path"] },
        // optional: guidance spliced into the system prompt while the tool is registered
        prompt: "Read a file before editing it …",
    }
}

pub async fn call(input) {
    eidolon::fs_read(input.path, input.get("offset"), input.get("limit")).await
}
```

`manifest()` runs once at load, keep it side-effect free. `call(input)` runs
per dispatch, on a thread of its own, with the model's JSON input as an
Object. `input.path` reads a required field; **a field that is not there
is an uncatchable runtime panic, not `()`** — read optional fields with
`input.get("offset")`, which is an `Option` and can be handed straight to
a primitive that takes one. `call` returns a `String`, or `#{ content,
is_error }`; a `Result::Err` returned through `?` becomes an error output,
which is why the built-ins hand a primitive's `Result` straight back.

The `eidolon` vocabulary in a tool — every one `async`, so `.await` it,
and every one answering a `Result`:

- `cwd()` — the dispatcher's live working directory.
- `fs_read(path, offset, limit)`, `fs_write(path, content)`,
  `fs_edit(path, old, new, replace_all)` — the last argument is a `bool`,
  not an `Option`: `match input.get("replace_all") { Some(b) => b == true, None => false }`.
- `shell(command, timeout_s)` — the command's rendered output, exit code
  and stderr inline; killed as a process group on timeout or cancel.
- `grep(pattern, path, glob, case_insensitive, max)` — `path:line: text`,
  gitignore-aware, capped.
- `web_fetch(url, max, timeout_s, raw)` — a page reduced to its text with
  links kept, or the bytes with `raw`.
- `choices_user(prompt, options)` — a multiple-choice question to the
  operator, routed through the dispatcher like any tool call; the only
  asking tool. A question that wants free text is asked by ending the turn
  with it in the reply text.
- With `[mneme]` configured: `mneme_rpc(function, args)` and
  `mneme_call(kind, args)` beneath it. With peers: `peers()`,
  `peer_send(to, text, wake)` and
  `swarm_call(kind, args)`. A tool that calls one of these compiles only
  where it exists — which is why `tools/peers.rn` is loaded on the
  built-in's terms and not as an ordinary user tool.

Structured answers cross the boundary as JSON text: `json::from_string`
them, and `match` the `Result` it returns.

## A provider

```rune
pub fn provider() {
    #{ name: "gw", wire: "openai", base_url: "https://…/v1",
       token_secret: "gw",                       // or token_file / token_env
       compat: #{ max_completion_tokens: true },
       models: [ #{ id: "…", name: "…", context: 200000, cost: #{ input: 2.5, output: 15.0 } } ] }
}
pub fn request(body) { body.remove("stream_options"); body }   // optional, per call, sync
pub fn models() { … }                                         // optional, at load
pub fn quota() { #{ plan: "lite", windows: [ … ] } }          // optional, when a panel asks
```

`provider()` runs once at load; `models()` too, merging into the declared
list; `request(body)` runs synchronously on every call with the outgoing
JSON as an Object, and returns it; `quota()` never runs at load. The
script sees `eidolon::http_get(path_or_url)` and nothing else — a `GET`
with the bearer added Rust-side, answered from a cache at launch and
refreshed behind the prompt. It returns a `Result`, and a `models()` that
must degrade to the empty list when the gateway is down cannot use `?`:
`match` it. `examples/providers/gateway.rn` is the annotated starting
point; `providers/zai.rn` is a shipped one with a `request` hook and a
`quota` reader.

## The policy table

`policy.rn` is the *leaf table* of the command safety classifier: Rust
takes a shell command apart — pipes, `&&`, substitutions, redirects — and
asks the table one already-decomposed simple command at a time. Five
`pub fn`s are called: `shell_arg(tool)`, `classify_tool(tool, approval,
input)`, `fs_verb_tier(prog)`, `bash_default(posture)` and
`classify_program(argv, ctx)`. Each is called once at load, so a missing
one is a startup error naming it. The two `classify_*` answer a verdict,
`#{ decision, reason, read_only }` with `decision` one of `allow`, `flag`
(ask) or `deny`; `fs_verb_tier` and `bash_default` answer a bare tier;
`shell_arg` names the argument that carries a tool's command line. The
file's own comments say what each decides. A table can change which
recognised command is silent, asked about or refused, and nothing about
composition or scoping — it never sees a pipe.

## The UI script

`ui.rn` owns layout, status line, key bindings and command parsing, and is
compiled together with `prelude.rn` (the builders — `column`, `panel`,
`transcript`, `prompt`, `send`, `command` …) into one namespace. Two
functions are required — `view(s)`, a layout tree from a per-frame
snapshot, and `keymap()`, a table of mode tables — and `on_submit(s,
text)` answers what `ret` does. `modes()`, `theme()` and `commands()` are
optional tables laid over the default's, so a script need only say what it
changes. The `ui` module is `k(n)` and `context(tokens, limit)` for the
status line, `parse_command(text)` (a real `Option` — `match` it),
`commands()` (the registry, for a menu of your own) and `roman(n)`. The
header of `ui.rn` documents the snapshot's fields and every action a
script can answer with. `:reload_ui` recompiles the file in place and
shows the error if there is one.

## The language, where it is not Rust

Each of these compiled clean and failed at runtime in a real script.

- **`let mut` does not compile.** Everything is mutable: `let out = [];
  out.push(x);`.
- **Missing methods and wrong arities compile fine and crash at runtime.**
  Instance functions resolve when called, so `s.find(…)` is a runtime
  "missing instance function" and a host function called with the wrong
  number of arguments fails on the call, not the compile. `String` has
  `split`, `split_once`, `contains`, `starts_with`, `ends_with`, `trim`,
  `replace`, `chars`, `parse`, `to_lowercase` — **no `find`, no `rfind`,
  no regex module**. Most `find`s are a `contains` or a `split_once`.
- **Values move.** Assignment shares, but a by-value method consumes:
  `content.split('\n')` moves `content`, and so does passing a variable
  straight into a host primitive; reuse afterwards is "value is moved".
  `.clone()` first when a value is needed again. `format!` and template
  literals do not consume what they read. `json::to_string(v)` consumes
  `v` — do any length check before serialising.
- **`Result`, `()`, `Object` and `Vec` cannot be formatted.** None has a
  display protocol, so `format!("{}", r)` and `` `${r}` `` on any of them
  is an uncatchable panic — and the failure branch, which is where a
  result is most likely to be one of these, is where it bites. Route
  anything that is not a known `String` or number through a helper:

  ```rune
  fn show(v) { match json::to_string(v) { Ok(s) => s, Err(_) => "(unserializable)" } }
  ```

- **`json::to_string`, `json::from_string` and `http_get` return `Result`.**
  `match` each; `?` is only legal in a function that itself returns a
  `Result` (a tool's `call` is one, a provider's `models` is not).
- **Host `Option`s are real `Option`s.** `input.get("x")` and
  `ui::parse_command(text)` hand back `Some`/`None`; `match` the variant.
  `x is String` is false on both arms of an `Option`, and `x == ()` on a
  possibly-unit value panics ("expected number, found Tuple") rather than
  evaluating false. Where a value may be a hit or a miss, branch on what a
  hit positively *is* — `if v is i64`, `if v is Object` — never on how a
  miss renders.
- **A bare `()` on the line after a closed `if { … }` is a call**, and
  crashes the VM ("Tuple cannot be called"); write `return ();`. Never
  start a statement with `(` after a `}`. A bare identifier as the tail
  after a `for` loop that reassigns it yields unit, not the variable —
  return `` `${out}` `` instead.
- **`match` has no `a | b` alternation.** The policy table's `in_list(x,
  list)` helper is the idiom.
- **Template literals**: a backtick anywhere inside one ends it (no
  escape, comments included), a lone `$` is a parse error, and `return`
  followed directly by a template literal does not parse — bind it first.
  `default` is a reserved word: name the parameter `fallback`.
- **No character-to-integer conversion.** Anything that depends on a
  character's code — percent-encoding, a hex digit — is a lookup table
  keyed by one-character strings.
- **A host future is not auto-awaited.** A tool's primitives are `async`:
  without `.await` the value is a `Future` and the next method call on it
  is a missing-instance-function crash. `.await` is legal only in an
  `async fn`, which is why `call` is one and `manifest` is not.
- **A failed primitive is an `Err`, not an exception**, and a
  `Result::Ok` from a primitive is what the host chose to report, not
  proof of the effect. Check what matters directly when it is cheap to.

## Checking a script

`eidolon tools` compiles every tool, prints its manifest and says which
file did not load and what stood in for it. `eidolon policy "<command>"`
runs the table over one command and prints the tier, the reason and what
the harness would do. `:reload_ui` recompiles `ui.rn` and reads the tables
again. `eidolon models` loads every provider live. A compile-clean script
has proved only that it parses: every trap above is a runtime one, so run
the path — a `bash` call through the tool, one command through the
table, one frame of the layout — before calling it done.
