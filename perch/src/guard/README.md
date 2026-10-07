# src/guard/ — guard rows

The deny rows the perch writes into the policies it generates (no agent-run `aoide`, no `bash ssh`, no `minerva keys add`), and the
status the page shows for them.

```text
mod.rs    GuardStatus, status(), refuses_term_text(text)                                              (G1)
```

**S0P-a built the seam.** `status()` is `{on: false, file: None, why: "not built yet"}` and `refuses_term_text` returns `None`, so
`meta` and the terminal's `send` route can call them now and get the real answer when G1 lands.
