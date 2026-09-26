# bench — the practice-image benchmark runner

`bench` runs a defender agent against a fresh, scored practice image and writes
one report row per run. It is the runner named `minerva bench` in
[`docs/design/triage-toolkit.md`](../docs/design/triage-toolkit.md) §7–§8; it
lives as a standalone crate until the `minerva` hub exists, then folds in as a
subcommand.

## What it does

```
reset ──▶ boot ──▶ wait_ready ──▶ baseline read ──▶ agent works (time box)
                                                          │
                                        ┌─────────────────┘
                                        ▼
                          agent stopped ──▶ authoritative score read ──▶ VM down ──▶ Row
```

The authoritative points are read over the qemu console **after** the agent is
stopped, so a guest the agent locked itself out of is still scored
(triage-toolkit.md §7.4.1). The runner never has a path to the answer key or the
writeup; the agent runs as a competitor.

## The four seams

The loop (`run::run_benchmark`) is generic over four traits so it runs against
fakes in tests and real infrastructure in the field:

| seam | trait | real impl | test impl |
|---|---|---|---|
| VM lifecycle | `VmControl` | `Vm` over `ShellHarness` → `bin/botforge-vm.sh` | `FakeHarness` |
| score read | `ScoreReader` | `Vm` runs `score_cmd`, `score::parse_score` parses | `FakeHarness` |
| the agent | `AgentRunner` | *(not wired)* — `NoopRunner` for now | `NoopRunner` |
| time | `Clock` | `SystemClock` | `FakeClock` |

`NoopRunner` drives nothing, which is the honest baseline: an agent that does
nothing leaves the image at its frozen 0/256. When eidolon + JEV + the brain
model are wired, they drop in behind `AgentRunner` with no change to the loop.

## Run it

```bash
cargo test                      # the loop, config, score parsing, guards
cargo run -- check --config bench.config.example
cargo run -- run   --config my-run.config     # needs a real VM + score_cmd
```

A run needs a `score_cmd` that prints the scorer's points over the console; see
`bench.config.example`. No `score_cmd`, no run — the runner refuses rather than
invent a zero.

## Not here yet

- The `AgentRunner` that starts eidolon + JEV against the brain model.
- Forensics grading, penalties, ladder metrics and tag accuracy in the row
  (fields are present, filled by the real runner).
- The library-snapshot hash (recorded from config today).

No network at runtime, no dependencies beyond the Rust toolchain — the crate is
meant to still build when nobody is maintaining it.
