# src/guard/ — guard rows

The deny rows the perch writes into the policy every chat runs under, in all three modes, and the status the page shows for them. An agent's
own shell tool cannot run the `aoide` CLI, cannot `ssh`, and cannot change the update keys; each refusal carries a sentence that says what to
do instead. It is a speed bump in front of an honest mistake or an obvious request, and this file says where it stops.

```text
mod.rs     the file, the status, the probe, and the same rows in Rust            (G1)
rows.rn    the rows, as fixed Rune text (`include_str!`); nothing in it is ever built from a request or a file
```

## What is refused, and what the agent is told

| Row | Refused | Sentence the agent reads |
|---|---|---|
| 1 | the `aoide` / `aoided` program (with or without `.exe`) | agents send Aoide mail with `mail_send` (opt-in per chat); the aoide CLI is not for agents here |
| 2 | the `ssh`, `ssh.exe`, `sshpass`, `autossh` and `mosh` programs | ssh through a terminal, `term_open` then `term_send`, so the operator watches live |
| 3 | `minerva keys <verb>` for any verb but `list` / `inspect`; `--keys-arm` with `minerva` or `perch-open.sh`; `hoot keys` (also `hoot.ps1`, `hoot.cmd`, through a shell or powershell) | update keys are added by the operator, from the page or a terminal, not by agents |
| 4 | any word of the command that names `minerva/update-keys` (either slash, any case), as an argument as much as a program | the update-keys directory is the operator's; keys are added or removed from the page or a terminal, not through files an agent writes |

"The program" means the first word after the wrappers are peeled (`env` and `env -S`, `nohup`, `timeout`, `xargs`, `nice`, `stdbuf`,
`command`, `exec`, `setsid`, `time`, `ionice`, `flock`, `unbuffer`, `chrt`, `taskset`, `watch`, `strace`, their options and the one
positional some take), past `NAME=value` prefixes and shell keywords (`!`, `if`, `then`, `elif`, `else`, `do`, `while`, `until`). The code
string of a shell is read the same way, as shell words: `bash -c`, and every spelling of it (`-lc`, `-ec`, `-cx`, `--command`, `-c --`),
`script -c`, `pwsh` / `powershell -Command`, `cmd /c` and `/k`; every candidate code option of one command is judged, not the first. The
code of `python -c`, `perl -e` and `node -e` is read only where it starts a command (`system(`, `exec(`, `popen(`, `subprocess`, a
backtick), so `print(aoide)` is not a command. `eval` and `find -exec` are read too. Nesting is read six levels deep, a code string over
64 KiB is not read, and one command line reads at most 256 KiB of code text in all: past any of those is a miss, never a hang. A word that
merely appears (`ls ~/src/aoide`, `grep -r aoide .`, `echo ssh`, `scp a b:c`) is not the program.

## What the rows reach, and what they do not

eidolon decomposes a command line and hands the table one simple command at a time. The rows are asked about those.

```text
reached     a flat command            ssh h uptime
            each stage of a pipeline  cat f | ssh h
            each part of a chain      true && ssh h      true; ssh h
            the body of a while loop  while test -f flag; do ssh h; done
            one level of $(...)       echo $(ssh h uptime)
missed      a for / if / case, a subshell ( ... ), a { ...; } group, a function body, a command in the background (... &),
            an assignment of $(...), a redirect that leaves the workspace; the algebra judges the whole line by its shape
            ("Deny (shape, not merits)"), which auto asks about and yolo answers: it RUNS, unasked
            the file verbs (cp, mv, tee, touch, mkdir, ln): "workspace file op", decided by the algebra itself, which reads
            ~/x as a path inside the workspace; the table is never asked, and it runs silently in auto too
not seen    any eidolon an agent starts itself (eidolon --yolo -p "ssh host uptime"), and a subagent's children: they run under the
            member's config, without the rows
```

So in yolo `for h in a b; do ssh $h uptime; done` runs, and the rows are in the config of the chat's OWN tool calls only. `tests/guard.rs`
pins the measured shapes (`PINNED_KNOWN_MISSES`, asserted against the real eidolon in yolo), so an eidolon that starts to hand one to the
table shows up as a failing test and a decision, not as a surprise. They are not patched in the table: the table never sees them.

Not seen either, and not claimed: a program word made by `$(...)` (`$(printf ao)ide x` reaches the rows as an empty word), a shell that
reads its program from standard input or a heredoc (`echo "ssh h" | bash`, `bash <<EOF`), a script on disk, an encoded payload, a redirect
target (`echo k > ~/.config/minerva/update-keys/k.pub` is not a word of any command).

## The file

```text
~/.config/eidolon/policy.rn                    the member's own table (or eidolon's, unpacked on a first run)   never written
~/.config/eidolon/minerva-guard-<8 hex>.rn     the same table with classify_program renamed minerva_stock_classify_program, the
                                               rows, and a classify_program that asks them first   0600, rewritten at every start
~/.config/eidolon/policy.permits.rn            the vouch and grant store, shared: it is "beside the policy file", and these are in
                                               one directory
~/Minerva/config.toml                          [policy] path = the guard file, enabled = true
~/Minerva/policy-manual.rn                     the manual wrapper over the member's table (all four entry points), with the same rows
```

The `<8 hex>` is the first eight of `sha256(canonical --home)`, so two perches over one eidolon config each have a file of their own, in
the one directory (the permits store is still found beside it), and one failing to start never removes the other's.

Rune has no import, so wrapping is a text edit: each of the four entry points (`classify_tool`, `classify_program`, `fs_verb_tier`,
`bash_default`) must be found exactly once at the start of a line, and `classify_program` is renamed; a table that does not fit, or that is
already one of ours, is refused and the guard is off. Only `classify_program` is renamed in the guard: the other three stay the member's own
where eidolon reads them, so `eidolon policy-rows` (which compares the string arms of `pub fn classify_tool` with the shipped table) reads
the member's rows, and says "identical" for auto and yolo as it does for the member's own config. Manual renames all four.
`classify_program` asks the rows first and a hit is a `deny` with the row's sentence; everything else is the member's table's answer, reason
text included (eidolon's vouches key on the stock fallthrough reason).

Permits: `policy.permits.rn` is compiled in front of the table, in one namespace. The rows are all named `minerva_guard_*` or
`MINERVA_GUARD_*`, which cannot meet `VOUCHE`, `GRANT`, `ATTEST`, `permit_rows`, `vouched`, `granted`, `attested`. A vouch from
`eidolon plugins trust` and a credential grant from `eidolon plugins grant` are honoured under the guarded config, and under a config in
another directory that names only the guarded `[policy] path`; `tests/guard.rs` runs both with no question.

Posture: a `[policy]` entry changes what a command no table names gets (no entry is `allow`; an entry with no `default` is `flag`). The
generated config writes `default = "allow"` only when the member had no entry (or had switched the gate off); a member's own entry, with
its `default`, `judge` and `yolo`, is kept. A member whose gate was off (`enabled = false`) gets it on, without the `judge` and `yolo` they
wrote for a gate that never ran, and the note in `why` says so.

## When it is off

`status()` is `{on, file, why}` and `/hub/meta/policy` shows it. `on` means the rows are in the config this perch's own chats start with
(and the start-up probe saw them refuse); it does not say that nothing else can run `ssh`, see above. It is off, with a sentence, when: there
is no `--home`; the member's policy path is relative or its directory is missing or unwritable (on the default path the directory is made
by eidolon's own first-run unpack, never by the perch); the member's table cannot be wrapped; or the start-up probe does not pass.

The probe is `eidolon --config <home>/config.toml policy -- aoide mail send x`, 6 s at most (`perch-open.sh` waits 20): it must come back
`Deny`, with the guard's sentence, from the `table:` the guard file names. After a pass the member's table is read again; if eidolon synced
it while starting, the guard is made again from the new text and probed once more. When the probe fails the config goes back to the
member's own, and only then is the guard file removed: if that config cannot be written the perch does not start, and the file stays.
A start that fails is logged at error level.

## Text typed into a terminal

`refuses_term_text(text)` is the same rows (1, 3 and 4; not ssh, which is what `term_open` and `term_send` are for) over one `term_send`
text, the first 8 KiB of it, split at `; & | ( ) { } \` newline`. It is unconditional: a guard that is off in the policy is still on here.
It is a hint, not a gate: a command split across sends, typed with DEL or tab completion, built by a variable, or sent after the first
8 KiB passes. `tests/guard.rs` pins those.

## ATT&CK

| Technique | Row | How far |
|---|---|---|
| T1059 Command and Scripting Interpreter (an agent driving the Aoide CLI) | 1 | the CLI spellings the rows read; a script on disk, a program built by `$(...)` or a payload on stdin gets past |
| T1021.004 Remote Services: SSH (an agent opening its own sessions the operator cannot see) | 2 | the shapes the rows reach; yolo runs the missed shapes above |
| T1098.004 Account Manipulation: SSH Authorized Keys | 3, 4 | not a mitigation. The key CLI spellings and a command word naming `minerva/update-keys` are refused, but `cp` / `mv` / `tee` (file verbs the algebra decides), the write and edit tools (the member's table), a shell redirect, a script and the missed shapes can still put a file in the directory |

The keys rows cover CLI spellings and any command word that names the directory (`rsync evil.pub ~/.config/minerva/update-keys/`, `ls
~/.config/minerva/update-keys` are refused). `cp evil.pub ~/.config/minerva/update-keys/` is a workspace file operation: measured against the
real eidolon it is "workspace file op", `Allow`, silent, in auto and in yolo, because the algebra decides the file verbs itself and never
asks the table; the `write` and `edit` tools there are the member's table's too (`classify_tool` is not ours), and a redirect target is not
a command word. Keeping an agent out of that directory is not something rows in a policy table can do; it takes the directory's own
permissions or a consumer that wants a confirmation for a key that arrives as a file. `tests/guard.rs` pins the `cp` and `mv` shapes.

## Tests

`tests/guard.rs` compiles the generated tables with Rune and runs the real `eidolon policy`, `policy-rows` and `workflow run` against a temp
tree (HOME and every XDG directory pinned inside it, swarm off, mock provider only). The real eidolon is `MINERVA_TEST_EIDOLON=/abs/path`,
else the first on `PATH`; with `MINERVA_REQUIRE_EIDOLON=1` and none the tests fail instead of passing unlooked, and each test that does look
prints `REAL-EIDOLON-RAN <name>` under `--nocapture`. `perch::guard` has in-crate tests for the word check, the runaway bounds and the
bounded runner.
