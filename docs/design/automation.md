# Automation

The statechart, its learned transition function, and the loop that makes the
transition function better.

This gives precise shape to the decision of 2026-09-18, *Automation is a
statechart, and jev is its learned transition function* (Decisions.md). It is
the document D2 (the interpreter), D3 (the editor) and D4 (the first graphs)
implement from, and it states one contract D1 (`ext/browser`) has to meet.
It builds on B1 as landed: `jev/server.py` is the extension service, `choose`
and `entail` are its methods, and `%LOCALAPPDATA%\eidolon\extensions\jev\` is
where it keeps what must outlive a session. It writes no code beyond a schema
and two graphs.

## The model in one page

A task is a graph of states. A state is one decision, taken over and over
until a guard says the state is finished:

1. **Observe.** The state's entry actions run. An entry action is usually one
   tool call — `browser_snapshot`, `bash` — and its result is written into the
   run's context as the observation.
2. **Check.** The state's `always` transitions are tried in order. Each has a
   guard: a deterministic test on the context, or a sentence that openjev
   checks against the observation. The first guard that passes takes its
   transition, and the cycle starts again in the target state.
3. **Choose.** If no guard passed, the state produces its option list — the
   links in the snapshot, a fixed menu of commands, the lines of a listing, or
   the state's own outgoing transitions — and jevlike scores every option in
   one pass. If the top option clears the state's confidence floor it is
   taken. If not, the run parks and asks whoever is driving it.
4. **Act.** The taken option is an event. The transition for that event runs
   its actions — the click, the command — and lands in a state, usually this
   same one, where the cycle begins again with a fresh observation.

Every choice, however it was made, is appended to a log as
`{context, options, label}`, which is the row jevlike trains on. The run ends
when it reaches a final state, when a budget runs out, when it is stopped, or
when an action fails and nothing in the graph catches it. Either way it hands
back one report.

Six parts, and who owns each:

| part | what it is | where it lives |
|---|---|---|
| the graph | one JSON file, an XState v5 subset: what jev *can* do | `extensions/jev/graphs/<id>.json`; drawn in the Svelte Flow page |
| the interpreter | holds a run and decides its next request; **never executes anything** | the jev service, `jev/automation/` |
| the driver | dispatches each request through eidolon's chokepoint and returns the result | `extensions/jev/tools/run.rn` |
| the chooser | jevlike: context + options → a probability each | in the service |
| the checker | openjev: premise + hypothesis → entails / contradicts / neutral | in the service |
| the escalation | whoever is driving: the session's model, a person, or nobody — then the run parks | the driver's caller |

Three sentences carry the whole design. **The service decides; the script
does; the gate rules** — the interpreter names a tool call, the Rune driver
dispatches it, and eidolon's policy hook sees the real command with its real
arguments, exactly as it would for a model's `tool_use`. **The graph shapes
the menu** — a 41,280-parameter byte model cannot read a page, but it can
learn which of sixty link titles tends to lead toward a target, and it can say
when it does not know. **The log is the product** — every run, escalated or
not, leaves rows in the shape the trainer reads, and a person can correct a
row after the fact; the next checkpoint is a command away.

## 1. The schema

**Choice.** A strict subset of XState v5's JSON config, with everything
jev-specific under `meta`, so every graph loads in XState's own `createMachine`
unchanged. Guards and actions are a closed set of *kinds* named by `type`,
never functions.

**Rejected.** A format of our own (a node list with `edges`): it would need
its own editor model, its own validator and its own documentation, and the
first thing it would grow is the hierarchy and history XState already has.
XState's full config: `parallel` regions, `invoke`, `after`, `spawn`, deep
history and `raise` each bring semantics a weak chooser has no use for and an
interpreter would have to get exactly right.

**Why `meta`.** XState reserves `meta` for what the machine's own runtime
ignores. Putting the choose block, the floor and the budget there means the
Svelte Flow page can lint a graph with XState's parser, a conformance test can
run a graph's state trajectory under XState with the tools stubbed, and no key
ever collides with one XState later claims.

### What is kept, what is dropped

| XState feature | kept? | why |
|---|---|---|
| atomic, compound and final states; `initial` | yes | the hierarchy is the "phase" structure of a triage |
| `on` transitions with `target`, `guard`, `actions`, `description` | yes | the edge |
| `always` (eventless) transitions | yes | this is how a guard is checked after every observation — "stay until the guard passes" |
| `entry` / `exit` actions | yes | observing on entry is the observe step; clearing a scratch list on exit is the common cleanup |
| `reenter: true` | yes | the difference between "loop and re-observe" and "stay and act" |
| shallow `history` | yes | resume a phase where it was after a recovery detour |
| `onDone` on a compound state | yes | "phase complete" without an event name |
| `output` on final states | yes | it becomes the report's `output` |
| `context` and `input` | yes | the run's memory and its arguments |
| `#id.path` and sibling targets | yes | enough to reach any state |
| `parallel` | **no** | one browser, one shell, one driver loop; two regions choosing at once has no meaning for one chooser |
| `invoke` / actors / `spawn` | **no** | the tool action *is* the invocation; a second actor model would be mechanics on the canvas |
| `after` (delays) | **no** | waiting for a page is the browser tool's job; a timer in the graph hides a tool that should block |
| deep history, `raise`, `sendTo`, `tags` | **no** | never needed by a graph two levels deep |
| guards and actions as functions | **no** | a graph must be shareable as data; a closed set of kinds is what an editor can offer in a dropdown |

### Semantics the interpreter guarantees

These are XState's, restated so an implementer need not read XState's source,
with one deliberate strengthening.

- **A `tool` action blocks.** In XState an action cannot return a value;
  here a `tool` action suspends the machine until the driver returns the
  result, then writes it into `context.<into>` (default `obs`). Under XState's
  own interpreter the same graph fires the tool and moves on, so the
  conformance test stubs `tool` as a synchronous action with a scripted
  result — enough to check trajectories, not timing. This is the one place
  the subset means more than XState does, and it exists so that one node on
  the canvas is one decision rather than three. A `tool` action may also
  carry `expect`, a guard from the same closed grammar every other guard
  site uses, checked once the result is already believed whole and
  successful; failing it refuses the result before it is ever written to
  `context`, the same as a failed dispatch (`docs/design/observation.md`
  section 4).
- **Order.** Entry actions run in list order, each tool completing before the
  next starts. Then `always` transitions are tried in document order and the
  first whose guard passes is taken. Only when none passes does the state
  choose. An event's transitions are looked up in the active state first,
  then each ancestor, then the root `on`; within a state, document order.
- **Re-entry.** A self-transition with `reenter: true` runs `exit`, then the
  transition's actions, then `entry` again, and counts one more visit. A
  transition with no `target` is internal: its actions run, the state's
  `always` list is re-checked, nothing else happens.
- **History.** A `history` state resolves to its parent's last active child,
  or to its `target` if the parent was never entered.
- **Reserved events.** `PICK` is raised by a choice from a `refs`, `menu` or
  `lines` source, carrying the option as `event.option`. `EMPTY` is raised
  when a source produced no options; if nothing handles it the run escalates
  with the empty menu. `ERROR` is raised when an action's result fails one
  of the three beliefs an observation must earn before it is stored, guarded
  over or scored (`docs/design/observation.md` section 4) — the tool's own
  error, a declared length that disagrees with what arrived, an `expect`
  guard that does not pass, or a `PICK` naming a ref no longer current —
  carrying `event.error`, `event.reason` (one of `failed`, `truncated`,
  `unexpected`, `stale`) and `event.tool` (the action's own `name`, or
  `"pick"` for a stale ref); if nothing handles it the run ends with outcome
  `error`. A `transitions` source raises the event it names.
- **Budgets end a run**, never a state: `steps`, `actions`, `wall_s` and
  `escalations` from `meta.jev.budget`, and `visits` per state. Exhaustion
  is outcome `exhausted` and the report names the budget.
- **Templates.** A string anywhere in `input`, `params`, `context` templates
  or `output` may contain `{{path}}`: `context.*`, `input.*`, `event.*`,
  `state.description`, `run.id`, `run.step`, and `option.*` inside
  `choose.label` only. A list renders newline-joined, an object as JSON, a
  missing path as the empty string with a warning in the run record. There
  is no logic in a template; logic is a guard.

### The schema

`jev/automation/schema.json`, verbatim:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "hoot:automation/1",
  "title": "Hoot automation graph: an XState v5 subset",
  "type": "object",
  "required": ["id", "initial", "states", "meta"],
  "additionalProperties": false,
  "properties": {
    "id": { "type": "string", "pattern": "^[a-z][a-z0-9-]{0,63}$" },
    "version": { "type": "string" },
    "description": { "type": "string" },
    "initial": { "$ref": "#/$defs/name" },
    "context": { "type": "object" },
    "states": { "$ref": "#/$defs/states" },
    "on": { "$ref": "#/$defs/transitions" },
    "meta": {
      "type": "object",
      "required": ["jev"],
      "properties": {
        "jev": { "$ref": "#/$defs/jev" },
        "editor": { "type": "object" }
      }
    }
  },
  "$defs": {
    "name": { "type": "string", "pattern": "^[a-z][a-zA-Z0-9_-]{0,63}$" },
    "event": { "type": "string", "pattern": "^[A-Z][A-Z0-9_]{0,31}$" },
    "template": { "type": "string" },
    "path": { "type": "string", "pattern": "^(context|input|event|run)(\\.[A-Za-z0-9_-]+)*$" },

    "jev": {
      "type": "object",
      "required": ["schema"],
      "additionalProperties": false,
      "properties": {
        "schema": { "const": 1 },
        "requires": { "type": "array", "items": { "type": "string" }, "default": [] },
        "input": { "type": "object", "description": "A JSON Schema for the run's input." },
        "defaults": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "floor": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.5 },
            "margin": { "type": "number", "minimum": 0, "maximum": 1, "default": 0.1 },
            "visits": { "type": "integer", "minimum": 1, "default": 20 },
            "threshold": { "type": "number", "minimum": 0.34, "maximum": 1, "default": 0.6 },
            "max": { "type": "integer", "minimum": 2, "default": 64 }
          }
        },
        "budget": {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "steps": { "type": "integer", "minimum": 1, "default": 50 },
            "actions": { "type": "integer", "minimum": 1, "default": 100 },
            "wall_s": { "type": "integer", "minimum": 1, "default": 600 },
            "escalations": { "type": "integer", "minimum": 0, "default": 10 }
          }
        },
        "warrant": {
          "type": "object",
          "required": ["tools"],
          "additionalProperties": false,
          "properties": {
            "tools": { "type": "array", "minItems": 1, "items": { "type": "string", "pattern": "^[a-z][a-z0-9_]*$" } },
            "origins": { "type": "array", "items": { "type": "string", "pattern": "^https?://[a-z0-9.\\-]+(:[0-9]+)?$" }, "default": [] },
            "commands": { "type": "array", "items": { "type": "string" }, "default": [] }
          }
        }
      }
    },

    "states": {
      "type": "object",
      "minProperties": 1,
      "propertyNames": { "pattern": "^[a-z][a-zA-Z0-9_-]{0,63}$" },
      "additionalProperties": { "$ref": "#/$defs/state" }
    },

    "state": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "type": { "enum": ["atomic", "compound", "final", "history"] },
        "description": { "type": "string" },
        "initial": { "$ref": "#/$defs/name" },
        "states": { "$ref": "#/$defs/states" },
        "history": { "const": "shallow" },
        "target": { "type": "string" },
        "entry": { "$ref": "#/$defs/actions" },
        "exit": { "$ref": "#/$defs/actions" },
        "always": { "$ref": "#/$defs/transitionOrList" },
        "on": { "$ref": "#/$defs/transitions" },
        "onDone": { "$ref": "#/$defs/transitionOrList" },
        "output": { "type": "object" },
        "meta": { "$ref": "#/$defs/stateMeta" }
      },
      "allOf": [
        {
          "if": { "properties": { "type": { "const": "final" } }, "required": ["type"] },
          "then": { "properties": { "states": false, "initial": false, "on": false, "always": false, "onDone": false } }
        },
        {
          "if": { "properties": { "type": { "const": "history" } }, "required": ["type"] },
          "then": {
            "required": ["history"],
            "properties": { "states": false, "on": false, "always": false, "entry": false, "exit": false, "meta": false }
          }
        },
        {
          "if": { "required": ["states"] },
          "then": { "required": ["initial"] }
        },
        {
          "if": { "required": ["output"] },
          "then": { "required": ["type"], "properties": { "type": { "const": "final" } } }
        }
      ]
    },

    "stateMeta": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "choose": { "$ref": "#/$defs/choose" },
        "floor": { "type": "number", "minimum": 0, "maximum": 1 },
        "margin": { "type": "number", "minimum": 0, "maximum": 1 },
        "visits": { "type": "integer", "minimum": 1 },
        "ask": { "$ref": "#/$defs/template" },
        "editor": { "type": "object" }
      }
    },

    "choose": {
      "type": "object",
      "required": ["from"],
      "additionalProperties": false,
      "properties": {
        "from": {
          "oneOf": [
            { "const": "transitions" },
            {
              "type": "object", "required": ["refs"], "additionalProperties": false,
              "properties": {
                "refs": {
                  "type": "object", "additionalProperties": false,
                  "properties": {
                    "of": { "$ref": "#/$defs/path", "default": "context.obs" },
                    "roles": { "type": "array", "items": { "type": "string" }, "default": ["link", "button"] },
                    "within": { "type": "string" },
                    "named": { "type": "boolean", "default": true },
                    "url": { "type": "string", "description": "A regular expression the option's snapshot url must match; an option with no url is dropped." }
                  }
                }
              }
            },
            {
              "type": "object", "required": ["menu"], "additionalProperties": false,
              "properties": { "menu": { "$ref": "#/$defs/menu" } }
            },
            {
              "type": "object", "required": ["lines"], "additionalProperties": false,
              "properties": {
                "lines": {
                  "type": "object", "additionalProperties": false,
                  "properties": {
                    "of": { "$ref": "#/$defs/path", "default": "context.obs.text" },
                    "skip": { "type": "integer", "minimum": 0, "default": 0 }
                  }
                }
              }
            }
          ]
        },
        "also": { "$ref": "#/$defs/menu" },
        "exclude": { "$ref": "#/$defs/template" },
        "max": { "type": "integer", "minimum": 2 },
        "context": { "$ref": "#/$defs/template" },
        "label": { "$ref": "#/$defs/template" }
      }
    },

    "menu": {
      "type": "array",
      "minItems": 1,
      "items": {
        "type": "object",
        "required": ["label"],
        "properties": {
          "label": { "type": "string", "minLength": 1 },
          "event": { "$ref": "#/$defs/event" }
        },
        "additionalProperties": true
      }
    },

    "transitions": {
      "type": "object",
      "propertyNames": { "pattern": "^[A-Z][A-Z0-9_]{0,31}$" },
      "additionalProperties": { "$ref": "#/$defs/transitionOrList" }
    },

    "transitionOrList": {
      "oneOf": [
        { "type": "string" },
        { "$ref": "#/$defs/transition" },
        {
          "type": "array", "minItems": 1,
          "items": { "oneOf": [ { "type": "string" }, { "$ref": "#/$defs/transition" } ] }
        }
      ]
    },

    "transition": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "target": { "type": "string" },
        "reenter": { "type": "boolean", "default": false },
        "guard": { "$ref": "#/$defs/guard" },
        "actions": { "$ref": "#/$defs/actions" },
        "description": { "type": "string" },
        "meta": { "type": "object" }
      }
    },

    "nli": {
      "type": "object",
      "required": ["hypothesis"],
      "additionalProperties": false,
      "properties": {
        "premise": { "$ref": "#/$defs/template", "default": "{{context.obs.text}}" },
        "hypothesis": { "$ref": "#/$defs/template" },
        "threshold": { "type": "number", "minimum": 0.34, "maximum": 1 },
        "window": { "enum": ["head", "tail"], "default": "head" },
        "max_chars": { "type": "integer", "minimum": 1, "default": 1500 }
      }
    },

    "guard": {
      "type": "object",
      "required": ["type", "params"],
      "additionalProperties": false,
      "properties": { "type": { "type": "string" }, "params": { "type": "object" } },
      "oneOf": [
        { "properties": { "type": { "const": "equals" },
            "params": { "type": "object", "required": ["path", "value"], "additionalProperties": false,
              "properties": { "path": { "$ref": "#/$defs/path" }, "value": {} } } } },
        { "properties": { "type": { "const": "contains" },
            "params": { "type": "object", "required": ["path", "value"], "additionalProperties": false,
              "properties": { "path": { "$ref": "#/$defs/path" }, "value": { "$ref": "#/$defs/template" } } } } },
        { "properties": { "type": { "const": "matches" },
            "params": { "type": "object", "required": ["path", "pattern"], "additionalProperties": false,
              "properties": { "path": { "$ref": "#/$defs/path" }, "pattern": { "type": "string" } } } } },
        { "properties": { "type": { "const": "exists" },
            "params": { "type": "object", "required": ["path"], "additionalProperties": false,
              "properties": { "path": { "$ref": "#/$defs/path" } } } } },
        { "properties": { "type": { "const": "count" },
            "params": { "type": "object", "required": ["path"], "additionalProperties": false,
              "properties": { "path": { "$ref": "#/$defs/path" },
                "gte": { "type": "integer", "minimum": 0 }, "lte": { "type": "integer", "minimum": 0 } } } } },
        { "properties": { "type": { "const": "entails" }, "params": { "$ref": "#/$defs/nli" } } },
        { "properties": { "type": { "const": "contradicts" }, "params": { "$ref": "#/$defs/nli" } } },
        { "properties": { "type": { "const": "not" },
            "params": { "type": "object", "required": ["guard"], "additionalProperties": false,
              "properties": { "guard": { "$ref": "#/$defs/guard" } } } } },
        { "properties": { "type": { "const": "and" },
            "params": { "type": "object", "required": ["guards"], "additionalProperties": false,
              "properties": { "guards": { "type": "array", "minItems": 2, "items": { "$ref": "#/$defs/guard" } } } } } },
        { "properties": { "type": { "const": "or" },
            "params": { "type": "object", "required": ["guards"], "additionalProperties": false,
              "properties": { "guards": { "type": "array", "minItems": 2, "items": { "$ref": "#/$defs/guard" } } } } } }
      ]
    },

    "actions": {
      "type": "array",
      "minItems": 1,
      "items": { "$ref": "#/$defs/action" }
    },

    "action": {
      "type": "object",
      "required": ["type", "params"],
      "additionalProperties": false,
      "properties": { "type": { "type": "string" }, "params": { "type": "object" } },
      "oneOf": [
        { "properties": { "type": { "const": "tool" },
            "params": { "type": "object", "required": ["name"], "additionalProperties": false,
              "properties": {
                "name": { "type": "string", "pattern": "^[a-z][a-z0-9_]*$" },
                "input": { "type": "object" },
                "into": { "type": "string", "pattern": "^[a-z][A-Za-z0-9_]*$", "default": "obs" },
                "timeout_s": { "type": "integer", "minimum": 1 },
                "expect": { "$ref": "#/$defs/guard" } } } } },
        { "properties": { "type": { "const": "assign" },
            "params": { "type": "object", "minProperties": 1 } } },
        { "properties": { "type": { "const": "push" },
            "params": { "type": "object", "required": ["path", "value"], "additionalProperties": false,
              "properties": { "path": { "type": "string" }, "value": {} } } } },
        { "properties": { "type": { "const": "inc" },
            "params": { "type": "object", "required": ["path"], "additionalProperties": false,
              "properties": { "path": { "type": "string" }, "by": { "type": "integer", "default": 1 } } } } }
      ]
    }
  }
}
```

Three lints run after schema validation, because a schema cannot see across
keys: every `target` resolves; a state with `meta.choose` whose source is not
`transitions` handles `PICK` (and one whose source *is* `transitions` has at
least two non-reserved events in `on`); and every `menu` label, in bytes, fits
the loaded checkpoint's `option_tokens` — a label the chooser cannot see the
end of is a label it cannot learn, and the lint names the state.

### Worked example: wiki-hop

Reach a target Wikipedia article from a start article by following links. One
real state, `reading`, does all the work: it snapshots the page, checks
whether it has arrived, and if not picks a link and comes straight back.

```json
{
  "id": "wiki-hop",
  "version": "1",
  "description": "Reach a target Wikipedia article from a start article by following article links.",
  "initial": "open",
  "context": { "goal": "", "start": "", "visited": [], "path": [] },
  "meta": {
    "jev": {
      "schema": 1,
      "requires": ["browser", "openjev"],
      "input": {
        "type": "object",
        "required": ["start", "goal"],
        "properties": { "start": { "type": "string" }, "goal": { "type": "string" } }
      },
      "defaults": { "floor": 0.35, "margin": 0.05, "visits": 30 },
      "budget": { "steps": 30, "actions": 70, "wall_s": 900, "escalations": 30 },
      "warrant": {
        "tools": ["browser_open", "browser_snapshot", "browser_click", "browser_back"],
        "origins": ["https://en.wikipedia.org"]
      }
    }
  },
  "on": { "ERROR": "failed" },
  "states": {
    "open": {
      "description": "Open the start article.",
      "entry": [
        { "type": "assign", "params": { "goal": "{{input.goal}}", "start": "{{input.start}}" } },
        { "type": "tool", "params": { "name": "browser_open",
            "input": { "url": "https://en.wikipedia.org/wiki/{{context.start}}",
              "confine": ["https://en.wikipedia.org"] } } }
      ],
      "always": "reading"
    },
    "reading": {
      "description": "On an article; pick the link most likely to lead toward the goal.",
      "entry": [
        { "type": "tool", "params": { "name": "browser_snapshot", "input": { "within": "main", "roles": ["link"] }, "into": "obs" } },
        { "type": "push", "params": { "path": "path", "value": "{{context.obs.h1}}" } }
      ],
      "always": [
        {
          "description": "the exact title",
          "guard": { "type": "equals", "params": { "path": "context.obs.h1", "value": "{{context.goal}}" } },
          "target": "arrived"
        },
        {
          "description": "a redirect, or a variant of the title",
          "guard": { "type": "entails", "params": {
            "premise": "Title: {{context.obs.title}}. Heading: {{context.obs.h1}}.",
            "hypothesis": "This is the encyclopedia article about {{context.goal}}.",
            "threshold": 0.8 } },
          "target": "arrived"
        }
      ],
      "meta": {
        "choose": {
          "from": { "refs": { "roles": ["link"], "within": "main", "url": "^(?:https://en\\.wikipedia\\.org)?/wiki/[^:#?]+$" } },
          "exclude": "{{context.visited}}",
          "max": 64,
          "context": "Target article: {{context.goal}}\nCurrent article: {{context.obs.h1}}\n{{context.obs.text}}",
          "label": "{{option.name}}"
        },
        "floor": 0.35,
        "ask": "Which link leads toward the article \"{{context.goal}}\"?"
      },
      "on": {
        "PICK": {
          "target": "reading",
          "reenter": true,
          "actions": [
            { "type": "push", "params": { "path": "visited", "value": "{{context.obs.h1}}" } },
            { "type": "tool", "params": { "name": "browser_click", "input": { "ref": "{{event.option.ref}}" } } }
          ]
        },
        "EMPTY": {
          "target": "reading",
          "reenter": true,
          "actions": [ { "type": "tool", "params": { "name": "browser_back" } } ]
        }
      }
    },
    "arrived": {
      "type": "final",
      "description": "The goal article is open.",
      "output": { "path": "{{context.path}}" }
    },
    "failed": {
      "type": "final",
      "description": "A browser action failed.",
      "output": { "error": "{{event.error}}", "reason": "{{event.reason}}", "path": "{{context.path}}" }
    }
  }
}
```

Read it top to bottom. `open` copies the run's input into context and opens
the page; its `always` is unguarded, so it moves on at once. `reading` takes a
snapshot on every entry and appends the page's heading to `path`. Two guards
decide arrival: the cheap exact one first, and only if that fails a ten-token
openjev check that catches a redirect landing on "Bicycle" when the goal was
"Bicycles". The menu is the `link` refs inside the page's `main` landmark,
minus pages already visited, capped at 64 in document order — which on
Wikipedia means the lead section's links, the most general ones, a bias
stated rather than hidden. The chooser's context puts the goal in the first
line because the checkpoint reads 192 bytes; the format is the one
`jevlike.data.build_wikispeedia` writes, so rows from this graph and rows
from the public Wikispeedia set train the same model. A pick clicks the ref
and re-enters `reading`, which snapshots again. A page with nothing left to
click goes back. The floor of 0.35 is a *bootstrap* floor: against the
synthetic checkpoint almost every step will escalate, which is the point —
the first runs are a person clicking with jev watching, and every click is a
human-verified row. `escalations: 30` equals `steps` for that reason.

### Worked example: sandbox triage

Dropped on an unfamiliar Linux box with a fixed menu of read-only commands:
find out what it is, what it exposes, what is running, and flag anything out
of place. Three phases inside one compound state, a recovery detour that
returns through history, and a final judgement over the lines of a listing.

```json
{
  "id": "triage-linux",
  "version": "1",
  "description": "On an unfamiliar Linux box: what it is, what it exposes, what is running, and what looks out of place, from a fixed menu of read-only commands.",
  "initial": "investigate",
  "context": { "notes": [], "tried": [], "flagged": [] },
  "meta": {
    "jev": {
      "schema": 1,
      "requires": [],
      "defaults": { "floor": 0.5, "margin": 0.15, "visits": 8 },
      "budget": { "steps": 40, "actions": 40, "wall_s": 600, "escalations": 10 },
      "warrant": {
        "tools": ["bash"],
        "commands": [
          "cat /etc/os-release",
          "uname -a",
          "hostname; uptime",
          "who",
          "ss -tulpn 2>/dev/null || netstat -tulpn",
          "ss -tnp state established",
          "ip -br addr",
          "nft list ruleset 2>/dev/null || iptables -S",
          "ps -eo comm,pid,user,pcpu,etime --sort=-pcpu | head -30"
        ]
      }
    }
  },
  "on": { "ERROR": "recover" },
  "states": {
    "investigate": {
      "description": "Three questions in order, each answered from its own menu.",
      "initial": "identify",
      "onDone": "report",
      "states": {
        "identify": {
          "description": "What is this box?",
          "exit": [ { "type": "assign", "params": { "tried": [] } } ],
          "always": [
            {
              "guard": { "type": "entails", "params": {
                "premise": "{{context.notes}}",
                "hypothesis": "The operating system, its version and the hostname are all stated.",
                "threshold": 0.7, "window": "tail", "max_chars": 1200 } },
              "target": "network"
            },
            {
              "guard": { "type": "count", "params": { "path": "context.tried", "gte": 3 } },
              "target": "network"
            }
          ],
          "meta": {
            "choose": {
              "from": { "menu": [
                { "label": "os release", "command": "cat /etc/os-release" },
                { "label": "kernel and arch", "command": "uname -a" },
                { "label": "hostname and uptime", "command": "hostname; uptime" },
                { "label": "who is logged in", "command": "who" }
              ] },
              "exclude": "{{context.tried}}",
              "context": "Triage: identify the box.\nKnown:\n{{context.notes}}"
            },
            "ask": "Which command answers what this box is?"
          },
          "on": {
            "PICK": { "actions": [
              { "type": "push", "params": { "path": "tried", "value": "{{event.option.label}}" } },
              { "type": "tool", "params": { "name": "bash",
                  "input": { "command": "{{event.option.command}}", "timeout_s": 20 } } },
              { "type": "push", "params": { "path": "notes", "value": "{{event.option.label}}: {{context.obs.text}}" } }
            ] }
          }
        },
        "network": {
          "description": "What does it expose?",
          "exit": [ { "type": "assign", "params": { "tried": [] } } ],
          "always": [
            {
              "guard": { "type": "entails", "params": {
                "premise": "{{context.obs.text}}",
                "hypothesis": "The output lists the ports the machine is listening on.",
                "threshold": 0.7, "max_chars": 1200 } },
              "target": "processes"
            },
            {
              "guard": { "type": "count", "params": { "path": "context.tried", "gte": 3 } },
              "target": "processes"
            }
          ],
          "meta": {
            "choose": {
              "from": { "menu": [
                { "label": "listening sockets", "command": "ss -tulpn 2>/dev/null || netstat -tulpn" },
                { "label": "established connections", "command": "ss -tnp state established" },
                { "label": "interfaces", "command": "ip -br addr" },
                { "label": "firewall rules", "command": "nft list ruleset 2>/dev/null || iptables -S" }
              ] },
              "exclude": "{{context.tried}}",
              "context": "Triage: what the box exposes.\nKnown:\n{{context.notes}}"
            },
            "ask": "Which command shows what this box exposes?"
          },
          "on": {
            "PICK": { "actions": [
              { "type": "push", "params": { "path": "tried", "value": "{{event.option.label}}" } },
              { "type": "tool", "params": { "name": "bash",
                  "input": { "command": "{{event.option.command}}", "timeout_s": 20 } } },
              { "type": "push", "params": { "path": "notes", "value": "{{event.option.label}}: {{context.obs.text}}" } }
            ] }
          }
        },
        "processes": {
          "description": "What is running? One listing, then judge it.",
          "entry": [
            { "type": "tool", "params": { "name": "bash",
                "input": { "command": "ps -eo comm,pid,user,pcpu,etime --sort=-pcpu | head -30", "timeout_s": 20 },
                "expect": { "type": "matches", "params": { "path": "context.obs.text", "pattern": "^COMMAND\\s+PID\\s+USER" } } } },
            { "type": "push", "params": { "path": "notes", "value": "processes: {{context.obs.text}}" } }
          ],
          "always": "judge"
        },
        "judge": {
          "description": "Does any process look out of place?",
          "meta": {
            "choose": {
              "from": { "lines": { "of": "context.obs.text", "skip": 1 } },
              "also": [ { "label": "nothing looks wrong", "event": "CLEAN" } ],
              "max": 32,
              "context": "Triage: flag a process that does not belong on a server.\nKnown:\n{{context.notes}}"
            },
            "floor": 0.6,
            "ask": "Which process looks out of place, if any?"
          },
          "on": {
            "PICK": {
              "target": "done",
              "actions": [ { "type": "push", "params": { "path": "flagged", "value": "{{event.option.label}}" } } ]
            },
            "CLEAN": "done"
          }
        },
        "done": { "type": "final" },
        "hist": { "type": "history", "history": "shallow", "target": "identify" }
      }
    },
    "recover": {
      "description": "A command failed. Carry on where the investigation was, or stop.",
      "entry": [ { "type": "push", "params": { "path": "notes", "value": "error: {{event.error}}" } } ],
      "meta": {
        "choose": { "from": "transitions", "context": "Triage: a command failed.\n{{event.error}}" },
        "floor": 0.7,
        "ask": "A command failed. Continue the investigation or stop?"
      },
      "on": {
        "CONTINUE": { "target": "#triage-linux.investigate.hist", "description": "continue where it left off" },
        "ABORT": { "target": "aborted", "description": "stop and report what is known" }
      }
    },
    "report": {
      "type": "final",
      "description": "Triage complete.",
      "output": { "notes": "{{context.notes}}", "flagged": "{{context.flagged}}" }
    },
    "aborted": {
      "type": "final",
      "description": "Stopped after a failure.",
      "output": { "notes": "{{context.notes}}" }
    }
  }
}
```

`identify` and `network` are the same shape: a menu of commands, a `PICK`
that runs one and records what it said, no `target` so the state stays put
and re-checks its guards. The first guard asks openjev whether the notes so
far answer the phase's question; the second is the loop-breaker — three
commands tried, move on regardless. `exit` clears `tried` so the next phase
starts fresh. The chooser never sees a command line: it sees `os release`,
`kernel and arch` — short labels that fit its 32-byte window — and the
command rides on the option as data the `PICK` action reads. The gate sees
the command. `processes` needs no choice, so it has no `choose`: one
listing, then `judge`, whose options are the listing's own lines (the
command name first, because that is the part of each line the chooser
reads) plus a fixed `nothing looks wrong` that raises its own event. A
`bash` failure anywhere inside `investigate` bubbles to the root's `ERROR`,
lands in `recover`, and the chooser — or, at a floor of 0.7, almost always
the person — picks between the two transitions; `CONTINUE` goes to the
history state, which resumes whichever phase was interrupted.

**Files.** `jev/automation/schema.json`; `jev/automation/graph.py` (load,
validate, lint, resolve targets); `extensions/jev/graphs/wiki-hop.json` and
`extensions/jev/graphs/triage-linux.json` (the two graphs above, verbatim).

**Tests** (`jev/tests/test_schema.py`): both graphs validate; a graph with
`"type": "parallel"` is refused naming the state; an unknown guard or action
`type` is refused; a state with `meta.choose` from `refs` and no `on.PICK`
fails the lint; a `history` state outside a compound state is refused; an
unresolvable `target` fails the lint naming both ends; a `menu` label longer
than the checkpoint's `option_tokens` fails the lint naming the state; a
`final` state with `on` is refused; both graphs load under XState's
`createMachine` unchanged (a `node` test, skipped when `node` or `xstate` is
absent).

## 2. What a node is

**Choice.** A node is a state with a `choose` block. Its option list comes
from one of four sources, all producing the same thing — an ordered list of
labelled options, each carrying data — and the taken option becomes an event
whose transition does the work. The service produces the list, scores it, and
*requests* an action; the Rune driver dispatches the action through the
chokepoint and returns the result; the result is the next observation.

**Rejected.** Executing actions inside the service. A `bash` run by the
Python process would be a shell command no policy hook ever saw, which is the
one thing the architecture forbids ("every effect goes through a host
primitive, behind the policy gate, into the session log"). Also rejected:
executing them from the Rune driver with `eidolon::shell` directly — that
primitive runs the command without dispatching it, so the gate would have
classified only `jev_run`'s input (a graph name) and never the command the
graph chose.

### The four sources

| `choose.from` | options | the event | the option's data |
|---|---|---|---|
| `"transitions"` | the state's own `on` events, minus the reserved three, labelled by each transition's `description` | the event itself | nothing |
| `{ "refs": … }` | elements of an accessibility snapshot in `context.obs`, filtered by role and landmark | `PICK` | `ref`, `role`, `name`, `url` when the snapshot gave one |
| `{ "menu": [...] }` | a fixed list written in the graph | `PICK`, or the item's own `event` | every other key of the item |
| `{ "lines": … }` | the non-empty lines of a text in context, after `skip` | `PICK` | `line`, `index` |

`also` appends fixed items to any source; `exclude` drops options whose
label is in the rendered list; `max` caps the list in source order. The
label is what jevlike sees and what the log stores; `choose.label` can render
it from the option's data (`{{option.name}}`), and for `refs` that is the
default.

### The chooser's context

`choose.context` is a template; the default is
`{{context.goal}}\n{{state.description}}\n{{context.obs.text}}` when a `goal`
exists and `{{state.description}}\n{{context.obs.text}}` otherwise. The
rendered string is stored whole in the log row. The checkpoint reads its
first `context_tokens` bytes — 192 on this box — and each option's first
`option_tokens` — 32. So a context template begins with what decides the
choice (the goal, the phase), never with the observation, and a label is a
name, never a command line. The interpreter reads both numbers off the
loaded checkpoint and the lint uses them; a checkpoint retrained with
`--context-tokens 512` widens the window with no change to any graph,
because the rows already hold the full string.

### The two concrete cases

**A Playwright accessibility snapshot.** D1's `browser_snapshot` returns
text in Playwright's own `ariaSnapshot` form with refs, one element per line,
nested by indentation:

```text
url: https://en.wikipedia.org/wiki/Bicycle
title: Bicycle - Wikipedia

- banner [ref=e2]:
  - link "Main page" [ref=e5]:
    - /url: /wiki/Main_Page
- main [ref=e40]:
  - heading "Bicycle" [level=1] [ref=e42]
  - paragraph [ref=e50]: A bicycle, also called a pedal cycle, is a
    - link "vehicle" [ref=e51]:
      - /url: /wiki/Vehicle
```

The head block is a convention any tool may follow: `url:` then `title:`,
then zero or more further `key: value` lines, then a blank line, then the
body — `docs/design/observation.md`, "Where an observation is narrowed",
which is also where `scope` and `chars` are ruled on. The interpreter lifts
`url`/`title` into `obs.url`/`obs.title` and every extra line into a field
of its own (`obs.scope`, the landmark role a `within`-scoped snapshot is
rooted at; `obs.chars`, the body's own length as its producer counted it,
in code points) regardless of which extra keys a given producer sends —
`jev/automation/a11y.py`'s `parse_head` reads the block generically rather
than assuming a fixed line count. `obs.chars`, when present, is what
`_run_tool_action` checks the arrived body's own length against before
storing anything (the whole belief, `docs/design/observation.md` section
4); an unstamped result (no `chars:` line) is stored without a verdict on
wholeness. The first `heading … [level=1]` becomes `obs.h1`.
`jev/automation/a11y.py` parses the rest into `{ref, role, name, level,
url, landmark}` records, where `landmark` is the nearest enclosing
`banner`, `navigation`, `main`, `complementary`, `contentinfo`, `search` or
`region`; `refs.within` matches it. A ref is valid only for the snapshot it
came from — Playwright renumbers on every snapshot — which is why `reading`
re-enters and snapshots again after every click rather than clicking twice
from one observation. The interpreter enforces it: a `PICK` whose `ref` is
not in the current `obs` is an `ERROR` (`event.reason: "stale"`), not a
click on whatever now has that number.

**A shell menu.** The graph lists the commands. The label is short and
descriptive; the command is data on the item; the `PICK` action is
`bash {command: "{{event.option.command}}"}`. What reaches the policy hook is
`bash` with that command line, classified the way any model's `bash` call is:
`cat /etc/os-release` is silent, something the table flags is asked about,
`sudo` is refused on the merits. The graph can only ever *narrow* what the
gate permits.

### The service–driver contract

Four methods on the jev service, beside `choose` and `entail`:

| method | args | answers |
|---|---|---|
| `automation.start` | `graph` (an id under `graphs/`, or an inline object), `input`, `ask` | `{run, request}` |
| `automation.step` | `run`, `request` (the id being answered), `result` (`{text, error}`) | `{request}` |
| `automation.answer` | `run`, `pick` (index or label) *or* `stop` (a reason), `by`, `note` | `{request}` |
| `automation.stop` / `.status` / `.runs` | `run`, a reason | the run record |

A request is one of three kinds. `act`: `{id, tool, input, timeout_s}` —
dispatch this, bring back the result. `escalate`: the payload of section 4 —
the run is parked. `final`: the report of section 4 — the run is over. A
`step` that answers a request id already consumed is ignored and the current
request returned, so a driver that retried after a dropped connection cannot
double-act. The run record is written to
`%LOCALAPPDATA%\eidolon\extensions\jev\runs\<run-id>.json` after every
request and every answer, so a service restart loses no run: every run that
was live becomes `orphaned`, which is an escalation whose two options are
*retry the pending action* and *stop* — the service cannot know whether a
click happened before its driver died, so it asks.

The driver, `extensions/jev/tools/run.rn`, is a loop and nothing else:
`start`, then while the request is `act`, `eidolon::dispatch(tool, input)`
and `step` with the result; on `escalate` or `final`, return the payload as
the tool's result. It observes the turn's cancellation token on every call,
as every host primitive already does. `jev_resume` is the same loop entered
through `answer`. Every action the run takes is therefore a nested dispatch:
policy-checked, journaled as its own `ToolResult`, published on the event
bus, and drawn live in the TUI and the web page under the `jev_run` call
that owns it.

**One host addition.** `eidolon::dispatch(name, input)` — a Rune primitive
that dispatches a registered tool by name through `Dispatcher::dispatch`,
exactly as `choices_user`, `peers` and `peer_send` already do internally via
`host.dispatch`. A script names a tool, never a URL; the policy hook sees the
inner call with its real input; an `Ask` goes to the `UserIo`; `NoUser`
declines and the driver receives an error result, which the graph sees as
`ERROR`. Origin is the script's, with the `script_…` ids `fresh_id` already
mints. Documented in `crates/cli/system/RUNE.md` beside `service_call`.

**Files.** `jev/automation/options.py` (the four sources, filters, labels,
the chooser context, forced and empty menus); `jev/automation/a11y.py`;
`jev/automation/run.py` (`Run`: the request queue, persistence, orphan
handling); `jev/automation/template.py`; `extensions/jev/tools/run.rn`,
`resume.rn`, `runs.rn`, `stop.rn`; `crates/rune/src/host.rs` (`dispatch`);
`crates/cli/system/RUNE.md`. D1 adds `browser_back` and emits the two head
lines from `browser_snapshot`.

**Tests.** `test_a11y.py`: a fixture snapshot parses to the expected refs,
roles, names, levels, urls and landmarks; `within: main` keeps only the
article's links; `h1`, `title`, `url` are lifted; malformed lines are skipped
and counted. `test_options.py`: `exclude` drops by label; `max` keeps source
order; a menu with one option left is *forced* (no chooser call, a row marked
`forced`); no options raises `EMPTY`; `also` items with `event` raise that
event; the `transitions` source omits `PICK`, `EMPTY`, `ERROR`; the default
context puts the goal first; the stored context is the full string, not the
192-byte prefix; a stale `ref` in a `PICK` is an `ERROR`. `test_run.py`: a
`step` with a consumed request id is ignored; a run survives a serialise /
deserialise round trip with its pending request; a run live at service start
becomes `orphaned` with the two-option escalation. Rust, `crates/rune`: an
inner `bash` dispatched from a script reaches a recording policy hook with
the command text, is journaled as a `ToolResult`, and under `NoUser` an `Ask`
returns an error result; cancelling the outer call cancels the inner one.
End to end, `eidolon run --provider mock` against a stub service (the
`http.server` fixture from the extension host's own tests) scripting a
three-action run: the session log shows one `jev_run` and three nested
results in order.

## 3. Guards through openjev

**Choice.** A guard is a closed kind. The deterministic kinds run in
microseconds over the context. The two NLI kinds, `entails` and
`contradicts`, build one premise from a template, one hypothesis from a
template, and ask the service's `entail` — which reaches openjev only through
`OpenJevCrossEncoder.predict`. A guard passes when the probability of its
named label meets its threshold; `neutral` never passes anything.

**Rejected.** Every guard through openjev. A 4B model in float32 on this
CPU answers in seconds per call, proportional to the premise length — measure
it before quoting a number, but it is seconds, not milliseconds — and "the
title equals the goal" or "three commands have been tried" is not a question
for a language model. Also rejected: a `neutral` verdict as a soft pass or a
"stay" signal. The 0.72-neutral probe in Decisions.md is the argument: a
model that answers either way is exactly the danger, and a transition taken
on "the observation does not say" is that danger made policy.

### Premise and hypothesis

The premise is `params.premise` rendered (default `{{context.obs.text}}`),
cut to `max_chars` from the `head` or the `tail`. Head for a page, whose
opening is its subject; tail for a command's output, whose end is where the
error is. The hypothesis is a present-tense declarative sentence *about the
premise text* — "The output lists the ports the machine is listening on" —
never about the world. openjev scores textual entailment; a hypothesis that
needs knowledge the premise does not contain is scored as neutral, and
neutral fails. Both strings go to the service as they are; the service, and
only the service, formats
`Premise: {premise}\nHypothesis: {hypothesis}` through the vendor class. No
other code path exists, and the test that pins the Decisions.md probe is what
keeps it that way.

### Thresholds and order

`threshold` defaults to `meta.jev.defaults.threshold` (0.6); the schema's
floor of 0.34 is the point below which a three-way softmax's argmax means
nothing. `entails` passes iff `p[entailment] ≥ threshold`; `contradicts`
passes iff `p[contradiction] ≥ threshold`. The three probabilities are
written to the run record with the guard's rendered strings, so a threshold
can be tuned from evidence rather than guessed. All NLI guards on one state's
`always` list are sent in one `entail` call — one batch, one round trip —
and evaluated in document order once the batch returns; the deterministic
guards before them in the list are evaluated first and can short-circuit
the batch entirely.

`contradicts` exists for the case entailment cannot express: routing on a
refusal. "The article does not exist" against the hypothesis "This is an
encyclopedia article" is a contradiction, and a graph can send that to a
recovery state instead of waiting for `visits` to run out.

**Files.** `jev/automation/guards.py`; the `entail` method already in
`jev/server.py` is the only NLI entry point and gains a batch argument.

**Tests** (`test_guards.py`): each deterministic kind on fixtures, including
`count` on a list and `matches` on a multi-line text; `and`/`or`/`not` nest;
document order and first-pass-wins; an `entails` guard passes only when
`p[entailment] ≥ threshold` and fails on neutral and on contradiction;
`contradicts` passes on contradiction only; `window: tail` keeps the end;
`max_chars` is applied after rendering; with a mocked cross-encoder, the
service formats through `predict` and never through `tok(premise,
hypothesis)`; all NLI guards on a state go in one batch; marked slow and
run against the real weights: "the server returned 500 on every request"
versus "the service is healthy" scores contradiction ≥ 0.85, the probe from
Decisions.md, pinned.

## 4. Escalation and budget

**Choice.** The floor is per node, with a graph-wide default. Escalation
returns the run's question *as the result of the tool call that was driving
it*, and the run parks in the service until `jev_resume` answers. Who answers
is whoever was driving: the session's model, a person, or — for a headless
run — nobody yet, and the report says so. A budget is four numbers on the
graph and one per node; any of them ending the run produces the same report.

**Rejected.** A single global floor: a two-option "continue or stop" node
and a sixty-link page need different absolutes, and one number makes one of
them either mute or noisy. A dedicated escalation model wired into the
service: it would need an LLM client the service does not have or a host
primitive that does not exist, and it would put the decision somewhere no
transcript shows it. `eidolon send`'s doorbell as *the* human channel: it
delivers into a running session's inbox framed as external tool input, which
is the right door for waking an agent and the wrong one for reaching a
person — and while a run is live its driver is inside a tool call, so the
session it would wake is the one already waiting. An Open WebUI channel: a
second notification system that depends on the face being up, when every
surface the box has already renders a tool result to a person.

### The floor

A choice is taken when `top1 ≥ floor` and `top1 − top2 ≥ margin`. The
absolute catches a menu the chooser has never seen; the margin catches a
menu where two options tie. Per node: `meta.floor`, `meta.margin`; default:
`meta.jev.defaults`. The floor is *measured*: `jevlike-eval` prints expected
calibration error and per-bin accuracy, and the floor for a graph is the
confidence bin above which accuracy on that graph's held-out rows is what
the author will accept. Until a graph has rows, its floor is high and the
chooser is a suggestion, which is the bootstrap mode described under
wiki-hop.

### The payload

One rendering, read by a model and a person alike, because they are looking
at the same evidence:

```json
{
  "kind": "escalate",
  "run": "r_01J9XQ4M7T8V2N",
  "graph": "wiki-hop@1",
  "state": "reading",
  "step": 7,
  "why": "top 0.21 under floor 0.35",
  "question": "Which link leads toward the article \"Philosophy\"?",
  "evidence": { "goal": "Philosophy", "h1": "Bicycle", "url": "https://en.wikipedia.org/wiki/Bicycle", "excerpt": "…first 1500 chars of obs.text…" },
  "options": [ { "index": 0, "label": "vehicle", "p": 0.21 }, { "index": 1, "label": "pedal", "p": 0.17 } ],
  "budget": { "steps": "7/30", "actions": "8/70", "wall_s": "41/900", "escalations": "3/30" },
  "warrant": {
    "id": "w_b0506f840829",
    "graph": "wiki-hop@1",
    "sha256": "ca573dd96be230e64c2a6b1f46005dd5cacb8cbe7767f5c0a5971903d0df2ea0",
    "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
    "origins": ["https://en.wikipedia.org"],
    "commands": [],
    "actions": 70,
    "wall_s": 900
  },
  "answer": "jev_resume {run, pick: <index or label>, warrant: <this payload's warrant>} or {run, stop: <reason>}"
}
```

`why` is one of: under the floor, under the margin, the menu was empty and
nothing handled `EMPTY`, `visits` exhausted, the run was orphaned. The
options are all of them, in menu order, with probabilities — a person wants
the whole menu and a model that is asked to pick from it must see it whole.

### The answers

Two forms and no third. `pick` — an index or a label — takes that option as
if the chooser had, logs the row with `source` and `verified` naming who
answered, and continues. `stop` ends the run with outcome `stopped` and the
reason in the report. An answer outside the menu is not accepted: the menu
is the contract, and if the right move is not on it the graph is wrong, and
the correction is to the graph in the editor, not to the run. A structural
escape ("give up", "go back") is put on the menu with `also` so the chooser
can learn it too.

### The tiers, by who is driving

`jev_run` takes `ask`: `return` (default) or `user`.

- **`return`.** The escalation is the tool result. A session driven by a
  model reads it and calls `jev_resume` — the model is the second tier, at
  no new cost, and the transcript shows what it decided; it can pass the
  question to the operator by ending its turn with it, the house idiom for a
  free-text question. A headless session prints the payload and exits with
  the run parked; `jev_runs` lists it; any later session on the box resumes
  it. Melete driving a rented box sees the payload in `job_status` and its
  own agent and Telegram are the tiers.

  A warrant changes what the gate does, not what these tiers do. Passed to
  `jev_run` (`jev_warrant`, `docs/design/unattended.md`), the block is the
  operator's one written authorization for the run's tool set, origins,
  commands, count and wall clock; the gate checks each dispatched action
  against it instead of asking, so it is silent for the run's mechanics.
  The chooser's own uncertainty still escalates through `jev_resume` exactly
  as above -- the model is still the second tier for that -- because a
  warrant authorizes the envelope, not the picks inside it.
- **`user`.** The driver puts the question through `choices_user` instead
  — a dialog in the TUI, assistant text answered by the next message in Open
  WebUI through A1's shim — and the answer is `verified: human` by
  construction, since only a person can answer `choices_user`. Under
  `NoUser` the question is declined and the run parks as under `return`.
  This is the bootstrap mode: a person driving with jev suggesting.

A prompt-processing rate of 15–33 tok/s on this box puts a 1,000-token
payload at 30–60 s before the 27B model answers; bonsai-8b is several times
faster. The escalation budget bounds the spend.

### Budget

`meta.jev.budget`: `steps` (decisions: chooser calls, forced picks and
escalations), `actions` (tool dispatches), `wall_s` (from `start`, parked
time excluded), `escalations`. `meta.visits` per state. Tokens are not the
run's to count: the only model that spends any is the session's, and its
turns are already on eidolon's usage ledger. What stops a run: a top-level
final state (`reached`); a budget (`exhausted`); a `stop` answer or
`jev_stop` (`stopped`); an unhandled `ERROR` (`error`); cancellation of the
driving turn (`cancelled`, with the run kept, resumable). The report:

```json
{
  "kind": "final",
  "run": "r_01J9XQ4M7T8V2N",
  "graph": "wiki-hop@1",
  "outcome": "reached",
  "final_state": "arrived",
  "output": { "path": ["Bicycle", "Vehicle", "Transport", "Philosophy"] },
  "steps": 4, "actions": 5, "escalations": 1, "chooser_calls": 3, "entail_calls": 4,
  "wall_s": 96,
  "exhausted": null,
  "path": ["open", "reading", "reading", "reading", "reading", "arrived"],
  "log": "%LOCALAPPDATA%\\eidolon\\extensions\\jev\\decisions\\wiki-hop.jsonl",
  "record": "%LOCALAPPDATA%\\eidolon\\extensions\\jev\\runs\\r_01J9XQ4M7T8V2N.json",
  "warrant": {
    "id": "w_b0506f840829",
    "graph": "wiki-hop@1",
    "sha256": "ca573dd96be230e64c2a6b1f46005dd5cacb8cbe7767f5c0a5971903d0df2ea0",
    "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
    "origins": ["https://en.wikipedia.org"],
    "commands": [],
    "actions": 70,
    "wall_s": 900
  }
}
```

**Files.** `jev/automation/run.py` (floor and margin, the payload, the two
answers, budgets, outcomes, the report); `extensions/jev/tools/resume.rn`
with a manifest that tells a model to name `by: model` and never claim
`human`.

**Tests** (`test_run.py`): a top under the floor escalates with exactly the
keys above; a top over the floor but under the margin escalates naming the
margin; a node floor overrides the default; `pick` by index and by label
continue the run and log `verified`; a label that matches no option is
refused; `stop` ends with `stopped`; `visits` exceeded escalates naming it;
each of the four budgets ends the run with `exhausted` naming which; parked
time is not wall time; an unhandled `ERROR` ends with `error`; a handled one
transitions; `ask: user` routes through the driver's `choices_user` path and
the row reads `verified: human`; a cancelled driver leaves a resumable run.

## 5. The decision log and the correction path

**Choice.** One JSONL file per graph under
`%LOCALAPPDATA%\eidolon\extensions\jev\decisions\`, append-only, whose rows
are jevlike's native `{context, options, label}` with `label` an integer
index and every other key metadata the trainer ignores. A correction is a
new row with the same `id` and a new `label`, never an edit. Export merges
by id, last row wins, filters by who vouched, and splits by run so no run
leaks across train and test.

**Rejected.** The row B1 writes today, with `label` as the option's *text*:
`jevlike.data.validate` refuses it ("label must be an option index"), so
those rows train nothing until re-derived; D2 changes `label` to the index
and adds `chosen` for the text, and B1's ad-hoc `jev_choose` rows move to
`decisions/adhoc.jsonl` in the same shape. Rewriting rows in place: the file
is the evidence, and a log that can be edited is a log that cannot be
trusted about what the chooser actually did. A database: the trainer reads
JSONL, the correction page reads a few thousand rows, and the export is a
merge; nothing here needs a query planner.

### The row

```json
{"id":"r_01J9XQ4M7T8V2N-0007","context":"Target article: Philosophy\nCurrent article: Bicycle\n- main [ref=e40]: …","options":["vehicle","pedal","chain drive","Karl von Drais"],"label":0,
 "chosen":"vehicle","probs":[0.21,0.17,0.09,0.08],"source":"human","verified":"human","floor":0.35,"margin":0.05,
 "run":"r_01J9XQ4M7T8V2N","graph":"wiki-hop@1","state":"reading","step":7,"ts":"2026-09-18T19:02:11Z",
 "ckpt":"synthetic.pt@3f9a1c2d7e4b","action":{"tool":"browser_click","input":{"ref":"e51"}}}
```

`source` is who made the pick that was taken: `jevlike`, `model`, `human`,
`forced`. `verified` is who vouches for the label: `null` (the chooser's own,
unverified), `outcome` (the run later reached its goal without a `stop`),
`model`, `human`. A forced row — a menu with one option — is written for the
record and never exported. `ckpt` is the checkpoint file and the first twelve
hex of its SHA-256, so a row can be attributed to the chooser that made it.
A correction row is
`{"id": "…-0007", "label": 2, "verified": "human", "by": "operator", "ts": "…", "note": "the article's own link, not the disambiguation"}`
and nothing else; the exporter refuses a correction whose id it has not seen
in full. A run's end appends
`{"run": "…", "outcome": "reached", "ts": "…"}`, which is what lets the
exporter promote that run's unverified rows to `outcome`.

### The correction path

The `/jev` page eidolon serves (D3's editor, one tab of it) lists runs, and a
run's decisions as they were shown at escalation: the evidence, the menu, the
probabilities, what was taken. A click on another option posts
`decisions.relabel {id, label, note}`, which appends the correction row. The
same method is reachable from a chat through `jev_relabel`, so a model that
reviewed a run can propose a correction — written as `verified: model`, never
`human`. The page's JSON routes (`crates/web/src/jev.rs`) reach the service
through the same `Endpoint` the host holds; the page never learns a port or
a token.

### From log to checkpoint

1. `decisions.export {graph, verified: ["human", "model"], out}` writes
   `train.jsonl`, `validation.jsonl`, `test.jsonl` — merged by id, filtered
   by `verified`, split 80/10/10 *by run id*, the grouping rule jevlike's
   README insists on. `outcome` rows join with `verified: ["outcome", …]`
   once a graph has more successful runs than a person can review; they are
   weakly positive and stated as such.
2. `jevlike-train train.jsonl --validation validation.jsonl --output
   runs/wiki-hop.pt --context-tokens 512` — a wider window than the synthetic
   checkpoint's, because the rows already carry the full context.
3. `jevlike-eval runs/wiki-hop.pt test.jsonl` prints top-1, top-3, ECE and
   the shuffled-context control. A checkpoint that does not beat the control
   is not deployed. The per-bin accuracy sets `meta.jev.defaults.floor`.
4. `JEVLIKE_CKPT` names the new file; `eidolon ext stop jev` and `start jev`
   load it; every row from then on carries its hash.

No automatic retraining. Four commands, each producing a number a person
reads, is the right size of loop for a checkpoint a person is responsible
for; the day the numbers are boring is the day to schedule it.

**Files.** `jev/automation/decisions.py` (the writer, `relabel`, the
exporter and its split); `extensions/jev/tools/relabel.rn`;
`crates/web/src/jev.rs`; the decisions tab in D3's page; a change to
`jev/server.py`'s `_log_decision`.

**Tests** (`test_decisions.py`): a row has exactly the keys above and
`label` is an index; `jevlike.data.validate` — the real function, imported —
accepts every exported row; a forced row is written and not exported; a
correction appends and the export takes the last row per id; a correction
for an unseen id is refused; an outcome row promotes that run's `null` rows
to `outcome` and no other run's; `verified` filtering; the split never puts
two rows of one run in different files and is stable under re-export; the
ad-hoc `jev_choose` path writes the same shape to `adhoc.jsonl`; a `relabel`
through the tool writes `verified: model` whatever `by` claims.

## 6. Deployment

**Choice.** The graph travels alone; the interpreter travels once per box.
A box that runs graphs needs the `eidolon` binary, the `extensions/jev`
directory with a Python that has torch and `jevlike` installed and the
checkpoint beside it, and whichever extensions the graph's `meta.jev.requires`
names. openjev does not travel unless the profile says so. Remote triage
boxes are rented, reached and scheduled by Melete; the seam is one Melete
skill that ships the bundle, runs `eidolon do` headless with the graph, and
brings the report and the decision rows home.

**Rejected.** Shipping the JSON alone: it is a script, and a script needs
its interpreter — the same way an `.rn` needs `eidolon`. Shipping a second
scheduler or a daemon of our own on the box: Melete already schedules, rents
and reaches boxes, and a triage run is a job like any other. Shipping openjev
by default: 9 GB on disk and 16 GB resident in float32 is more than a rented
triage box has, and a guard the box cannot evaluate must fail at start, not
mid-run.

### What a box needs

| item | size | why |
|---|---|---|
| `eidolon` binary | one file | the chokepoint, the journal, the gate |
| `extensions/jev/` + `jev/` | small | the interpreter and the driver |
| Python 3.11+, `torch` CPU wheel, `jevlike` | ~200 MB | the chooser's runtime |
| the checkpoint | 169 KB today | the chooser |
| `extensions/browser/` + Playwright + Chromium | ~300 MB | only when `requires` names `browser` |
| openjev weights | 9 GB | only when `requires` names `openjev` and the profile allows it |
| a `policy.rn` for the box, and its config's `[[warrants]]` entry | — | the sandbox's posture (below) |

`automation.start` refuses a graph whose `requires` the box cannot meet,
naming the guard or tool that needs it, before the first action. A graph for
sandboxes therefore uses deterministic guards where it can, and its
`entails` guards are the ones worth a remote answer: the profile may name an
`entail_upstream` — the home box's jev service over an SSH tunnel Melete
already holds — so the premise leaves the sandbox and the verdict comes
back. That is a choice per deployment, because the observation is the
sandbox's contents, and it is stated in the report when it was made.

### The gate on a sandbox

On a triage box the run is headless, `NoUser` declines every question by
design, and a decline on a menu command would end the run. So a sandbox's
config gains `[[extensions]] name = "jev" approval = "trust"` and
`[[warrants]] graph = "triage-linux@1"` (`docs/design/unattended.md`,
"Headless"): `jev_run` and every command the graph's own warrant block lists
are `Warranted` by that standing entry and never ask, and the three refusals
on the merits — privilege escalation, powering down, destroying a
filesystem — still refuse, untouched by any warrant. A command the table
flags that the warrant does not cover is declined by `NoUser` and the run
reaches `recover` — the true outcome of that command on this box, not an
ended run. The menu is read-only by construction; the gate is what makes
that a guarantee rather than a habit.

### The Melete seam

Melete reaches a fleet box as `ssh_exec(project, command)` and finds it by
its `project:<name>` tag; rents one through `lib_rpc vultr_instance_*`; runs
a registered skill with `run_skill_task`, later with `schedule_skill_task`,
chained `after` another job, or on a cadence with `schedule_recurring`. The
seam is a skill in Melete's own `skills/` tree, `hoot-triage`, taking
`{project, graph, input}`:

1. ensure the bundle above is on the box, from a release tarball the skill
   knows the URL of; that `eidolon ext list` shows `jev` enabled; and that
   its config carries `[[extensions]] name = "jev" approval = "trust"` and
   `[[warrants]] graph = "triage-linux@1"`, writing them if not;
2. `ssh_exec` → `eidolon do --call '{"name":"jev_run","input":{"graph":"triage-linux","input":{},"warrant":<jev_warrant "triage-linux">}}'`
   — the typed form of `eidolon do`, the same JSON `/api/do` takes (C4);
   step 1's standing entry answers the one question the call raises, so
   nothing blocks; the report is the process's stdout;
3. copy `decisions/<graph>.jsonl` and `runs/<run>.json` home into
   `%LOCALAPPDATA%\eidolon\extensions\jev\decisions\inbox\<project>\`, where
   the exporter reads them like local rows;
4. post the report; Melete's Telegram stream carries it, and `job_status`
   holds it.

A parked run on a box is a report with `outcome: parked` and the escalation
payload; Melete's agent, reading it from `job_status`, is the second tier and
answers with another `ssh_exec` of `jev_resume`; its operator on Telegram is
the third. Nothing new is built for either.

**Files.** `bin/hoot.ps1` gains `hoot bundle` (the tarball of the table
above, per platform); `crates/cli/src/main.rs` gains `eidolon do --call` if
C4 has not already; Melete-side, `skills/hoot-triage/`.

**Tests.** `automation.start` on a graph requiring `openjev` against a
service reporting `openjev: false` in its health fails before any action,
naming the first `entails` guard; a graph requiring `browser` on a box with
no such extension fails at start naming it; the triage graph runs end to end
in a Linux container against the real checkpoint and a real shell, reaching
`report` with `flagged` present, under `--yolo`; the wiki-hop graph runs over
recorded snapshots (fixtures under `jev/tests/fixtures/wiki/`) with one
scripted human answer and reaches `arrived`; `hoot bundle` produces a
tarball that, unpacked on a clean container with the Python wheel cached,
passes `eidolon ext start jev`; `hoot-triage --dry-run` on Melete prints the
exact `ssh_exec` commands and copies nothing.

## What this does not solve

- **The chooser stays weak until it has rows.** Nothing here makes a
  41,280-parameter byte model understand language. It learns which labels
  tend to follow which contexts on this box's graphs, and the floor is what
  keeps it honest while it does. The first hundred runs of any graph are a
  person's.
- **Screenshot computer use.** Out of v1 by decision. `pywinauto`'s
  enumerated controls are the same option-list shape as `refs` and will be a
  fifth source; the interpreter needs no change for it, but nothing here
  designs it.
- **Extraction.** openjev checks a claim; it does not pull a hostname out of
  a paragraph. A "fact" in a triage report is the command's output, windowed
  — text a person reads, not a schema.
- **Concurrency.** One run per driver, one driver per tool call, no parallel
  regions. Two runs on one box are two sessions.
- **Detached runs.** A run lives inside the tool call that drives it. A run
  that outlives its session — started by a cron-fired chat, parked for a
  day, resumed by the doorbell waking an idle session — needs the loop in
  Rust with its own record kinds, and `eidolon send` becomes the right door
  then. The service-side design does not change for it; the driver does.
- **Time.** No `after`. A page that has not finished loading is the browser
  tool's problem to block on; a command that takes too long is `timeout_s`.
- **The editor.** D3 owns how a `choose` block is drawn and edited. This
  document fixes only what it must round-trip: every key here, `meta.editor`
  for layout, and unknown keys preserved.
- **A Python-free sandbox.** The chooser is small enough to run under candle
  inside `eidolon` itself, which would drop torch from the box. Worth doing
  once the graph format has stopped moving; not before.
- **Who is a human.** `verified: human` is by construction on the
  `choices_user` and page paths and by claim on `jev_resume`; a model
  instructed not to claim it will mostly not. The session journal's
  `CallOrigin` could be threaded into the driver's input to close that gap;
  it is not here.
- **B1's rows.** The `label`-as-text rows written before D2 lands are
  re-derivable, since each carries its options and the chosen text in
  `label`; the exporter does it on the fly rather than anyone rewriting the
  file.
