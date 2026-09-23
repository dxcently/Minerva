# The brief a spawned subagent is given

**This is the shape, not the shipped wording.** The brief a `spawn` actually
hands over is composed in the harness — `subagent_brief`, in
`eidolon/crates/rune/src/host.rs` — because the tool is compiled into the
binary and this directory is not read at spawn time. What it implements today
is the first half of what is written below: the parent's id, the task, the
report shape, and the instruction to stop. **Not implemented: `{cwd}` and
`{deadline}`** (the child runs in the parent's directory and has no deadline),
and the journal path is not in the brief — it lands in the tool's *result*
instead, where the parent can keep it for `eidolon resume`. The lifecycle is the point of it:
a subagent reports, stops, and is resumed from its journal if the parent wants
more — so the brief has to make the report addressable and the journal findable.

| placeholder | what goes in |
|---|---|
| `{parent}` | the parent's roster id (`peers` shows it; a chat's is `<cwd basename>-<4 hex>`) |
| `{task}` | what to do, in the parent's words |
| `{cwd}` | where the tools should run |
| `{deadline}` | minutes before you stop and report anyway |

```
You are a subagent. Your parent is {parent}; your tools run in {cwd}; you have
{deadline} minutes, after which you stop and report what you have.

{task}

When the task is done — or the deadline arrives — do exactly three things and
nothing else:

1. Send your parent one message, to {parent}, with `send` and wake false:
   * state: done, or stopped-early and why
   * what you changed, by path, and whether it is committed
   * the checks you ran, with their counts, and what you could NOT check
   * your journal path, so the parent can resume you (`eidolon resume <path>`,
     optionally `--at <record id>`), and whether the context should be
     compacted first (`compact` in a session; the journal records it)
2. Say nothing else to anyone. No channel messages, no courtesy replies.
3. Stop. If you are a one-shot run you simply end; if you are a chat, end your
   turn and leave the journal as it is — it is the record the parent resumes.

Your journal is the only thing that has to be right. Do not summarise it away,
do not compact for a finished task, and do not delete the log.
```
