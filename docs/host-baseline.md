# Host baseline — recorded before any change

Recorded by: agent session, unattended host-prep task.
Purpose: this box is being prepared so the agent's `bash` tool resolves a working
interpreter. Step 3 of the procedure (`bash` starting to work) looks identical to
a coincidence unless the broken state is written down first. This file is that
record. **Do not edit it after the fix lands** — a baseline that is updated stops
being a baseline.

## 1. Observed shell failure (verbatim)

Command attempted:

```
echo hello
```

Result, exactly as returned by the tool:

```
[exit code 1]
W i n d o w s   S u b s y s t e m   f o r   L i n u x   h a s   n o   i n s t a l l e d   d i s t r i b u t i o n s .
 ...
Use 'wsl.exe --list --online' to list available distributions
and 'wsl.exe --install <Distro>' to install.
```

(The error text comes back NUL-interleaved, i.e. UTF-16LE bytes rendered as
spaced characters — a WSL stub artifact, not a transcription error. Rendered as
text it reads: `Windows Subsystem for Linux has no installed distributions.`)

Every other command attempted failed identically, including `find` and `ls`:

| command | exit code | output |
|---|---|---|
| `echo hello` | 1 | WSL "no installed distributions" message |
| `pwd && find . -name '*.txt' -not -path './.git/*'` | 1 | same |
| `ls *.txt` | 1 | same |
| `find . -name '*.txt' -not -path './.git/*' \| sort` | 1 | same |

Not transient: four calls, two of them separated by other tool activity, all
identical. The stub is present; no distro is installed behind it.

## 2. Repository-documented host facts (cited, not remembered)

Gathered with `grep` while `bash` was broken.

- **WSL is not installed on this box.**
  `docs/Build-Log.md:1872`: "the direct-service fallback. Windows 11 throughout;
  no WSL on this box."
  Corroborated later in the same file: "WSL remains uninstalled and gh remains
  unauthenticated" (`docs/Build-Log.md:3607`, within the ollama/agent-run entry).

- **PATH shape: `Git\cmd` present, `Git\bin` absent.**
  `docs/Build-Log.md:2163-2167`: "`Git\cmd` is on PATH and `Git\bin`, which holds
  `bash.exe`, is not; verified by prepending it and watching `browser` come up on
  :53106. It deliberately did **not** patch PATH in the launcher, because that
  would hide a real `crates/tools` defect and deepen the Windows-only hole."
  `docs/Build-Log.md:2298-2301` repeats the shape for the S34 acceptance run
  ("Git\cmd present, Git\bin and Git\usr\bin absent") and adds the warning used in
  step 2 below: that run deliberately used the *operator's* PATH "rather than in
  the Bash tool's PATH, which Git Bash itself pollutes at shell-init".

- **`EIDOLON_BASH` and the resolution order.**
  `docs/Tasklist.md:253` (S34, fixed 2026-09-19): "`bash` **is** genuinely needed
  (`extension.rn`'s `command` is real POSIX shell with control flow); the defect
  was `Command::new("bash")` with no resolution. Now resolves via `EIDOLON_BASH` →
  `PATH`+`PATHEXT` → beside `git` → well-known roots → an honest failure naming
  every place it looked."
  `docs/Decisions.md:3061-3067` gives the same order in prose and states the
  override's failure mode: a bad `EIDOLON_BASH` "warns and falls through — wrong
  is not absent".
  `docs/Build-Log.md:2291-2296` locates it: both spawn sites in
  `crates/tools/src/shell.rs` (`run()` at 89, `run_background_with_env()` at 271)
  resolve through `bash_path()` (599), a per-process `OnceLock` over
  `find_bash()` (632).

- **Where the real Git Bash is expected.**
  `bin/watch-agent.ps1:21-22`: `$gitBashDir = 'C:\Program Files\Git\bin'`, and if
  `bash.exe` exists there it is prepended to the child `PATH`.

- **The fix location is not in this working directory.**
  The implementation lives at `eidolon/crates/tools/src/shell.rs`
  (`docs/design/linux.md:52`), outside this repo checkout. `grep` for
  `find_bash`, `bash.exe`, `PATHEXT` or `Command::new("bash")` across `*.rs` here
  returns no matches, which is consistent with that: the Rust source is not
  present to read, so the fallback root lists cannot be inspected from here.

## 3. Binary-existence probes (calibrated, `read`-based)

`read` discriminates precisely, but only after calibration with known-absent
controls — this was checked, not assumed:

| path | observed | reading |
|---|---|---|
| `C:\Program Files\DefinitelyNotHere-xyzzy-42` (control) | os error 2, "cannot find the file specified" | absent |
| `C:\Program Files\Git\bash.exe` (control) | os error 2 | absent — error 2 is the "absent" signal |
| `C:\Program Files\Git` | os error 5, "Access is denied" | exists (a directory) |
| `C:\Program Files\Git\bin` | os error 5 | exists (a directory) |
| `C:\Program Files\Git\bin\bash.exe` | "looks binary (47448 bytes); not rendering it" | **exists — Git Bash is installed at the documented path** |
| `C:\Windows\System32\bash.exe` | os error 2 | **absent** |
| `C:\Windows\System32\wsl.exe` | "looks binary (274432 bytes)" | exists — the WSL feature is present |
| `C:\Users\dxcen\AppData\Local\Microsoft\WindowsApps\bash.exe` | os error 1920, "cannot be accessed by the system" | exists but cannot be opened — app-execution-alias reparse point |
| `config.toml` (repo root) | os error 2 | not present in this checkout |

`grep` is **not** usable for this: it returns `[no matches]` for a nonexistent path
exactly as for an existing file with no matching line, so it cannot probe existence
at all. Two of its results in this file's first draft were drawn on that basis and
carried no information.

## 4. Reconstructed cause — evidence, not inference

The first version of this file guessed the stub was `System32\bash.exe`.
**That guess was wrong**: the probe above shows that path is absent. Corrected
here rather than left standing.

The evidence points instead to `...\WindowsApps\bash.exe`, the Windows
app-execution-alias stub that forwards to WSL. It exists, it cannot be opened
directly (os error 1920), and invoking it with no distro installed yields exactly
the message in §1. The WSL feature itself is present (`System32\wsl.exe` exists)
with no distro behind it — which is what `docs/Build-Log.md:1872` and `:3607`
record of this box.

So the shape is: a stub named `bash.exe` sits on `PATH`; the resolver's PATH step
matches it and stops; the beside-git step — where a working
`C:\Program Files\Git\bin\bash.exe` genuinely exists — is never reached.

**Still unverified:** that the tool resolved *this* file (its `PATH` cannot be read
from here), and whether `WindowsApps` precedes `Git\bin` in the environment the
tool inherits. Message text plus these artifacts are consistent with that
mechanism — consistent, not proven.

## 5. A discrepancy worth flagging

`docs/Build-Log.md:2298-2314` records S34's acceptance test passing *in the
operator's broken PATH* — `ext start jev` on :53279 and `ext start browser` on
:57950 both coming up, which requires `find_bash()` to have reached the beside-git
fallback. That implies the PATH step matched nothing at the time. If the
`WindowsApps` alias identified above is now on PATH, it would match first. Either
the alias appeared after 2026-09-19, or the operator's PATH and the tool's PATH
differ on this point — cf. the warning at `docs/Build-Log.md:2298-2301`.
Recorded, not resolved.

## 4. Non-listing evidence gathered for comparison

Before any fix, `.txt` files visible *by content search only* (a `grep` with a
`*.txt` glob, pattern `.`):

- `extensions/browser/requirements.txt`
- `extensions/browser/requirements-test.txt`
- `jev/tests/fixtures/wiki/cat_citations_excerpt.txt`
- plus `cat_full_capture.txt`, named in that fixture's header comments

This is **not a listing** and is not a substitute for the step 3 acceptance test:
files whose contents match nothing never appear, so empty `.txt` files are
invisible to it. Kept here only as the "before" picture the real listing is
compared against — success criterion 2 of step 3.
