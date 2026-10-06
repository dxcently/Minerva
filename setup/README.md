# setup: the Minerva first-run wizard

`minerva-setup` (the `minerva setup` of the plan) asks three things, with defaults shown so Enter,
Enter, Enter works, and writes `<home>/minerva.toml`:

```text
  home folder       not asked: always $MINERVA_HOME or ~/Minerva (hoot looks nowhere else)
  default model     chatgpt:gpt-5.6-luna | ollama:deepseek-v4.1-flash | mock | ...   "other..." takes any id
  project roots     ~, the Windows profile, <home>   plus "add another folder"; each must exist and be a folder
        |
        v
  summary -> confirm -> <home>/minerva.toml (0600, atomic), <home>/{sessions,scratch} (0700), projects.json "[]"
```

It never touches `~/.config/eidolon`. Run again, it shows the saved values as the defaults and rewrites
only when you confirm a change. Keys it does not know in `minerva.toml` are kept.

The model list is a short built-in one, extended with up to six ids parsed from `eidolon models`
(a row whose first word is `provider:model`); `eidolon models` is given six seconds and, when it is
missing or slow, the built-in list is used and the wizard says so. The Windows profile comes from
`cmd.exe /c echo %USERPROFILE%` run in `/mnt/c`, mapped with `wslpath -u`; it is left out when not found.

Without a terminal on stdin it prints one line and exits 2. Esc or Ctrl-C exits 130 and writes nothing.

## Build and run

In WSL (the prompts are [cliclack](https://docs.rs/cliclack), a port of `@clack/prompts`):

```sh
cd setup && CARGO_TARGET_DIR=$HOME/setup-target cargo build --release
```

From Windows: `hoot init` runs it, and a bare `hoot` runs it the first time (no `<home>/minerva.toml`
yet). From WSL: `bin/minerva-setup.sh`. Both look for the binary at `$MINERVA_SETUP_BIN`, else
`$HOME/setup-target/release/minerva-setup`, and say how to build it when it is missing. A first-run
`hoot` goes on with the defaults when the wizard is not built; `hoot init` does not.

## For the launcher: `--print-roots`

`minerva-setup --print-roots` asks nothing and prints the saved `roots` of `$MINERVA_HOME/minerva.toml`
(default `~/Minerva`), one folder per line, skipping (with a note on stderr) any that is no longer a
folder. `bin/perch-open.sh` passes each as a `--root` to the perch, so bash never parses TOML. No
file, no built wizard or an empty list keeps the old defaults: `$HOME` and the Windows profile.

## Layout

```text
src/main.rs   the prompts and the exit codes (0, 2 no terminal, 130 cancelled)
src/lib.rs    everything testable: read/write minerva.toml, the /mnt/ and folder checks, the model list
```

Tests: `CARGO_TARGET_DIR=$HOME/setup-target-dev cargo test`.

## The server role step (not built)

The deploy plan (`logs/agents/deploy-PLAN.md`) wants the wizard to also set up a server: a role
(mgmt, range, club Mneme/Melete) and its secrets, stored with `eidolon secret set` over stdin. That is
not approved, so it is not here. The place for it is marked in `src/main.rs`: the `Role` enum (only
`Desktop` today) and `wizard()`, which would ask the role first and call one `ask_*` function per role.
Secrets must go through `cliclack::password` and the child's stdin, never into `Answers`, argv, the
environment or a file this crate writes.

MIT, like the rest of Minerva. No `eidolon-*` or `aoide-*` dependency (the latter are GPL-3.0).
