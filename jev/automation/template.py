"""`{{path}}` templates -- docs/design/automation.md section 1, "Templates".

A template is a plain string that may contain one or more `{{path}}`
placeholders; there is no logic here on purpose ("logic is a guard"). A path
is dotted (`context.obs.h1`, `run.step`) and is resolved against whatever
`scope` dict the caller builds for the occasion -- the schema restricts a
*guard's* or *action's* own `path` field to `context|input|event|run` roots
(see the `$defs/path` regex in schema.json), but a free-form *template*
string (`choose.context`, `choose.label`, a state's `output`, an NLI
`premise`/`hypothesis`) is allowed to also read `state.*` and, inside
`choose.label` only by convention, `option.*` -- this module does not police
which roots a caller may put in `scope`; the caller decides by what it hands
in.

Two entry points:

- `render(template, scope, warnings=None)` always returns a `str`, per the
  design's stringification rules (a list joins with newlines, an object goes
  through `json.dumps`, a missing path renders as `""`).
- `render_value(template, scope, warnings=None)` returns the *raw* Python
  value when the whole (trimmed) template is exactly one placeholder and
  nothing else -- so `choose.exclude: "{{context.visited}}"` gets the real
  list back for membership tests rather than a newline-joined string of it.
  Anything else falls back to `render`'s stringified behaviour.
"""
from __future__ import annotations

import json
import re
from typing import Any

_PLACEHOLDER = re.compile(r"\{\{\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}")
_WHOLE_PLACEHOLDER = re.compile(r"^\{\{\s*([A-Za-z_][A-Za-z0-9_.]*)\s*\}\}$")

MISSING = object()  # distinct from a real `None` stored in context; public
# so guards.py and options.py can tell "resolved to null" apart from "did
# not resolve" without reaching for an underscore-prefixed name.


def resolve_path(path: str, scope: dict) -> Any:
    """Walk a dotted path (`"context.obs.h1"`) into `scope`. Returns
    `MISSING` -- not `None` -- when any segment is absent, so a caller can
    tell "resolved to null" apart from "did not resolve" if it cares to;
    every caller in this module collapses the two the way the design says
    to (empty string, or "does not exist" for the `exists` guard).
    """
    segments = path.split(".")
    if not segments or not segments[0]:
        return MISSING
    current: Any = scope
    for segment in segments:
        if isinstance(current, dict):
            if segment not in current:
                return MISSING
            current = current[segment]
        elif isinstance(current, (list, tuple)):
            if not segment.lstrip("-").isdigit():
                return MISSING
            index = int(segment)
            if not -len(current) <= index < len(current):
                return MISSING
            current = current[index]
        else:
            return MISSING
    return current


def stringify(value: Any) -> str:
    """The design's three rendering rules: a list is newline-joined, an
    object goes through JSON, everything else is its plain text -- `None`
    (a resolved-but-null value) and a genuinely missing path both render as
    the empty string; only the caller-facing warning distinguishes them."""
    if value is MISSING or value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, (list, tuple)):
        return "\n".join(stringify(item) for item in value)
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, dict):
        return json.dumps(value, ensure_ascii=False, sort_keys=True)
    return json.dumps(value, ensure_ascii=False, default=str)


def render(template: str, scope: dict, warnings: list[str] | None = None) -> str:
    """Interpolate every `{{path}}` in `template` and return the resulting
    string. A template with no placeholder at all is returned unchanged."""

    def _sub(match: re.Match) -> str:
        path = match.group(1)
        value = resolve_path(path, scope)
        if value is MISSING and warnings is not None:
            warnings.append(f"unresolved template path: {path}")
        return stringify(value)

    return _PLACEHOLDER.sub(_sub, template)


def render_value(template: str, scope: dict, warnings: list[str] | None = None) -> Any:
    """Like `render`, but when `template` (trimmed) is exactly one
    `{{path}}` and nothing else, hand back the raw resolved value instead of
    its stringification -- `choose.exclude`'s `"{{context.visited}}"` needs
    the real list for membership tests, not a newline-joined string of it.
    A missing path in this whole-template form resolves to `None`, not the
    empty string `render` would give it, because callers of this form
    (exclude lists, `equals`'s literal-or-template `value`) want "nothing
    there" to behave like Python's `None`, not like `""`."""
    match = _WHOLE_PLACEHOLDER.match(template.strip())
    if match is None:
        return render(template, scope, warnings)
    path = match.group(1)
    value = resolve_path(path, scope)
    if value is MISSING:
        if warnings is not None:
            warnings.append(f"unresolved template path: {path}")
        return None
    return value
