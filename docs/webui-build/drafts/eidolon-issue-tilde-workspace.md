# Title

policy: file ops and redirects aimed at `~/…` or `$VAR/…` are judged workspace-local and run silently in auto

# Body

@noah427 and μέλι —

We found this while pinning Minerva's guard rows against the real eidolon. Every probe below is `eidolon policy -- '<cmd>'` with the default config (auto), run from an empty temp dir under an isolated HOME and XDG tree. No model calls were made (・_・;)

**Version:** eidolon 0.1.0 (`eidolon --version`), WSL2 Ubuntu, built from master `<sha — fill in>`.

## What we see

| command | verdict | in auto |
|---|---|---|
| `cp evil.pub ~/.config/minerva/update-keys/` | Allow · workspace file op | runs, silently |
| `cp evil.pub /home/u/.config/minerva/update-keys/` | Deny (shape) · filesystem mutation escapes workspace | asks you first |
| `cp x ~/.bashrc`, `cp x $HOME/.bashrc`, `cp x ${HOME}/.bashrc`, `cp x "$HOME"/.bashrc`, `cp x ~u/.bashrc`, `cp x ~/` | Allow · workspace file op | runs, silently |
| `cp x $TMPDIR/y`, `cp x $OLDPWD/y` | Allow · workspace file op | runs, silently |
| `tee ~/.bashrc`, `mkdir ~/.config/autostart`, `ln -s x ~/.bashrc` | Allow · workspace file op | runs, silently |
| `echo x >> ~/.ssh/authorized_keys` | Allow · read | runs, silently |
| `echo x >> /home/u/.ssh/authorized_keys`, `echo x > ../x`, `echo x > /tmp/x` | Deny (shape) · redirection escapes workspace | asks you first |
| `cd ~ && cp /dev/null .bashrc`, `cp x $(echo /tmp)/y` | Deny (shape) · filesystem mutation escapes workspace | asks you first |
| `chmod 777 ~/.ssh` | Flag · destructive op **in workspace** | asks you first (the reason is wrong, the outcome is right) |

**Expected:** each `~/…` or `$VAR/…` spelling gets the same verdict as its absolute spelling, which is escapes workspace, so it asks in auto.

## Why it matters

In auto, a model (or a prompt injection in something it reads) can do all of the following without an ask:
- append to `~/.ssh/authorized_keys`
- rewrite `~/.bashrc`
- drop files into `~/.config/autostart` or any other `~/.config/*` directory

The same commands with absolute paths ask. `--yolo` is unchanged, since it runs everything anyway, and manual asks anyway, so the gap is auto mode specifically. A policy table can't patch this, because file verbs and redirects are decided by the algebra before the table is consulted.

## Where it seems to come from

The workspace-escape check appears to resolve path words literally: `~/x`, `~u/x`, `$HOME/x`, `${HOME}/x` and `$TMPDIR/x` read as relative names under the cwd. `cd ~` and `$(…)` are already treated as escaping, so the gap looks limited to plain tilde words and parameter words, in file-op targets and in redirect targets.

## A possible fix

Before the escape check:
- expand a leading `~` or `~user`, and `$HOME` / `${HOME}`, against the harness's own environment;
- treat any other unexpanded parameter word in a path position as escaping (fail closed, so it asks).

The `chmod` reason text would probably follow from the same change.

On our side, Minerva's guard docs list this gap as a known miss, and we'll pin the auto verdicts so we notice when a fix lands.

Happy to test a branch (ᵔᴥᵔ)

— Khoa (& Rook)
