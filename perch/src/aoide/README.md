# src/aoide/ — the Aoide runner

The one place that builds an `aoide` argv: a fixed set of shapes (`--version`, `mesh --json`, `mail outbox --json`, `mail read`,
`mail send`), `--flag=value` spelling, `--` before any text, a timeout that kills the child and a stdout cap.

```text
mod.rs    the runner                                                                                  (R2a)
```

**S0P-a built the seam.** Nothing here yet: the file only names its owner. The runner has no route and no `Ctx` field; `mesh` and
`mail` call it. Its inputs are already on `Options`: `--aoide` (the explicit binary, else `~/.local/bin/aoide` when it is executable,
never a `PATH` lookup), `--aoide-home`, `--aoide-daemon-socket` and `--aoide-events`.
