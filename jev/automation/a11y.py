"""Accessibility-snapshot parsing -- docs/design/automation.md section 2,
"The two concrete cases" / "A Playwright accessibility snapshot".

`extensions/browser/service.py`'s `_snapshot` returns
`_head(url, title, scope=..., roles=..., chars=..., heading=...) + tree`,
where `tree` is Playwright's own `page.aria_snapshot(mode="ai")` text: one
element per line, nested by indentation, each carrying `[ref=eN]` when it
is addressable and `"name"` / `[level=N]` / a following `- /url: ...`
property line when it has them. `_head` is the head-block convention any
tool may follow (docs/design/observation.md, "Where an observation is
narrowed"): `url:` then `title:`, then zero or more further `key: value`
lines -- `scope:` (the landmark role a `within`-scoped snapshot is rooted
at), `roles:` (the role filter applied to the body, if any), `chars:` (the
body's own length, in code points, as counted by the producer), and
`heading:` (the page's own level-1 heading, read off the *unfiltered* tree
at the source -- docs/Decisions.md, "A page's identity is not one of its
elements") today, and any future one a later producer adds -- then a blank
line, then the body. `parse_head` reads the whole block generically;
`declared_length` reads `chars:` specifically, for `is_truncated` below.

`build_obs` is the one entry point the interpreter calls after every tool
result: it lifts the head lines out (so they are not paying twice for the
same bytes once they are broken out into `obs.url`/`obs.title`, which
matters against a 192-byte context window), prefers a `heading:` head line
for `obs.h1` when the producer stamped one, falling back to finding the
first `heading` `[level=1]` in the body itself -- for a producer that
predates `heading:`, or one that stamped it and genuinely found none, see
the fallback's own comment below -- and parses the remaining tree into
`{ref, role, name, level, url, landmark}` records -- lazily consumed by
`options.py`'s `refs` source, not eagerly attached to every observation,
because most tool outputs (a `bash` listing, for instance) are not an
accessibility tree at all and must not be scored against one.
"""
from __future__ import annotations

import re
from typing import TypedDict

LANDMARK_ROLES = frozenset(
    {"banner", "navigation", "main", "complementary", "contentinfo", "search", "region"}
)


class Ref(TypedDict):
    ref: str | None
    role: str
    name: str | None
    level: int | None
    url: str | None
    landmark: str | None
    text: str | None


_HEAD_RE = re.compile(
    r"^url: (?P<url>[^\n]*)\ntitle: (?P<title>[^\n]*)\n(?P<extra>(?:[a-z][a-z_]*: [^\n]*\n)*)\n"
)
_PROPERTY_RE = re.compile(r"^/([A-Za-z][A-Za-z0-9_-]*):\s*(.*)$")
_ELEMENT_RE = re.compile(
    r'^(?P<role>[A-Za-z][A-Za-z0-9_-]*)'
    r'(?:\s+"(?P<name>(?:[^"\\]|\\.)*)")?'
    r'(?P<attrs>(?:\s*\[[A-Za-z]+=[^\]]*\])*)'
    r'\s*:?\s*(?P<text>.*)$'
)
_ATTR_RE = re.compile(r"\[([A-Za-z]+)=([^\]]*)\]")

# When an accessible name contains a literal colon or an embedded double
# quote, Playwright cannot write it as the bare `role "name" [attrs]: text`
# line `_ELEMENT_RE` above reads -- either would be ambiguous with that
# grammar's own `: ` delimiter -- so it wraps the whole `role "name"
# [attrs]` head in single quotes instead, the way a YAML dumper quotes an
# ambiguous mapping key: `'<head>': <value-or-nothing>`. A literal single
# quote *inside* that head is then escaped by doubling it (`''`), the
# standard YAML single-quote escape -- confirmed against a real capture,
# "Ship''s Cat", "owner''s location" (jev/tests/fixtures/wiki/
# cat_citations_excerpt.txt, blocks 2-3). This is a second, independent
# layer wrapped *around* the same `role "name" [attrs]` grammar, not a
# variant of it, so it is matched as its own shape and the unwrapped head
# is handed back to `_ELEMENT_RE` -- the one regex that already knows how
# to read a name with an escaped `"` in it -- rather than duplicating that
# logic here.
_QUOTED_LINE_RE = re.compile(r"^'(?P<head>(?:''|[^'])*)'\s*:\s*(?P<rest>.*)$")

def parse_head(raw: str) -> tuple[dict | None, str]:
    """Split a head block -- `url: ...`, `title: ...`, then zero or more
    further `key: value` lines, then a blank line -- from whatever follows
    it. `(None, raw)` unchanged when the block is absent entirely: a tool
    that never adopted the convention (`bash`'s output, for instance) is
    not an error, just not this format. Otherwise a dict carrying at least
    `url` and `title`, plus one string entry per extra line (`scope`,
    `chars`, or any key a later producer adds -- this function does not
    need to know its name ahead of time to carry it), and the rest of
    `raw` past the blank line."""
    match = _HEAD_RE.match(raw)
    if match is None:
        return None, raw
    head: dict = {"url": match.group("url"), "title": match.group("title")}
    for line in (match.group("extra") or "").splitlines():
        key, _, value = line.partition(": ")
        head[key] = value
    return head, raw[match.end() :]


def lift_head(raw: str) -> tuple[str | None, str | None, str]:
    """The original three-field reading of `parse_head`, kept for callers
    that only ever wanted `url`/`title`/the rest -- `(None, None, raw)`
    unchanged when the head block is absent, same as `parse_head` itself."""
    head, rest = parse_head(raw)
    if head is None:
        return None, None, rest
    return head["url"], head["title"], rest


def declared_length(raw: str) -> int | None:
    """The producer's own `chars:` head line, as an int -- the body's
    length in code points, as counted by whatever wrote this result, before
    anything between there and here had a chance to change it. `None` when
    there is no head block at all, or there is one but it carries no
    `chars` line, or the value is not a bare non-negative integer -- any of
    which means there is nothing here to verify a body's length against."""
    head, _rest = parse_head(raw)
    if head is None:
        return None
    value = head.get("chars")
    if value is None or not value.isdigit():
        return None
    return int(value)


def is_truncated(raw_text: str) -> bool:
    """Whether `raw_text`'s body is a different length than its own
    producer declared it to be. The producer states the body's length in
    the head (`declared_length`, above); a body of any other length was
    shortened -- or otherwise changed -- by something between the producer
    and here, and this function does not need to know that something's
    name to say so, unlike the marker-text match this replaced (docs/
    design/observation.md, "A tool result is bounded by who reads it, not
    by how big it is"). An unstamped result (no `chars:` line at all --
    `declared_length` returns `None`) answers `False`: not proven whole,
    but not provably not, either; `run.py`'s `_run_tool_action` treats the
    two differently on purpose, and stores an unstamped result without a
    verdict on wholeness rather than refusing it."""
    declared = declared_length(raw_text)
    if declared is None:
        return False
    _head, rest = parse_head(raw_text)
    return len(rest) != declared


def _unwrap_quoted_value(value: str) -> str:
    """`'a-single-quoted-key': "a possibly double-quoted value"` -- strip
    one fully-wrapping pair of double quotes from the value half, when
    present, so a quoted trailing value (`": 11"`, wrapped because a bare
    `: 11` would itself look like another `key: value` pair) reads the same
    as an ordinary unquoted line's trailing text would. Not a general
    unescaper -- `Ref.text` carries no meaning downstream this module's own
    tests and every caller in this package do not already ignore, so this
    is a light touch, not a second parser."""
    if len(value) >= 2 and value[0] == '"' and value[-1] == '"':
        return value[1:-1]
    return value


def parse_refs(tree_text: str) -> tuple[list[Ref], int]:
    """Parse an indented `- role "name" [attr=val] [ref=eN]:` tree into a
    flat list of records plus a count of lines that looked like tree syntax
    (started with `-`, after indentation) but did not parse. A line that is
    not tree syntax at all -- ordinary command output, most of the time --
    is neither a record nor a skip; it is simply not this format, and the
    caller decides separately whether parsing it was appropriate.

    A line whose `role "name" [attrs]` head is wrapped in single quotes
    (`_QUOTED_LINE_RE` -- an ambiguous name, real on the wiki fixtures) is
    unwrapped first and then read by the exact same `_ELEMENT_RE`, so it
    becomes a normal record rather than a skip; see the module docstring
    and jev/tests/fixtures/wiki/cat_citations_excerpt.txt. What still
    increments `skipped` is a line that started with `- ` and matched
    neither shape -- genuinely unrecognised syntax, not one of this
    format's two known ones -- kept as a best-effort count rather than a
    raised error, because one unrecognised line must not fail an entire
    snapshot; callers that build a menu from these records surface a
    non-zero count as a warning instead of swallowing it (`options.py`'s
    `_from_refs`), which is the part that used to go missing.

    Nesting is read from indentation width directly (a stack of
    `(indent, record_or_None)`, popped while the top is not shallower than
    the current line) rather than assumed to be a fixed number of spaces
    per level, so this tolerates whatever width Playwright emits.
    """
    records: list[Ref] = []
    stack: list[tuple[int, Ref | None]] = []
    skipped = 0
    for raw_line in tree_text.splitlines():
        if not raw_line.strip():
            continue
        stripped = raw_line.lstrip(" ")
        indent = len(raw_line) - len(stripped)
        if not stripped.startswith("- "):
            continue  # not tree syntax -- not a record, not malformed either
        content = stripped[2:]
        while stack and stack[-1][0] >= indent:
            stack.pop()
        parent = stack[-1][1] if stack else None

        prop_match = _PROPERTY_RE.match(content)
        if prop_match is not None:
            key, value = prop_match.group(1), prop_match.group(2).strip()
            if parent is not None and key == "url":
                parent["url"] = value
            stack.append((indent, None))
            continue

        text_override = None
        element_src = content
        if content.startswith("'"):
            quoted_match = _QUOTED_LINE_RE.match(content)
            if quoted_match is not None:
                element_src = quoted_match.group("head").replace("''", "'")
                text_override = _unwrap_quoted_value(quoted_match.group("rest").strip())

        match = _ELEMENT_RE.match(element_src)
        if match is None:
            skipped += 1
            stack.append((indent, None))
            continue

        attrs = dict(_ATTR_RE.findall(match.group("attrs") or ""))
        level_raw = attrs.get("level")
        landmark = next(
            (anc["role"] for _, anc in reversed(stack) if anc is not None and anc["role"] in LANDMARK_ROLES),
            None,
        )
        text_value = text_override if text_override is not None else (match.group("text") or "").strip()
        record: Ref = {
            "ref": attrs.get("ref"),
            "role": match.group("role"),
            "name": match.group("name"),
            "level": int(level_raw) if level_raw is not None and level_raw.lstrip("-").isdigit() else None,
            "url": None,
            "landmark": landmark,
            "text": text_value or None,
        }
        records.append(record)
        stack.append((indent, record))
    return records, skipped


def build_obs(raw_text: str) -> dict:
    """The interpreter's one entry point: a tool's raw text result becomes
    `context.obs`, an object with `text` (the body, head lines stripped),
    `url`, `title`, `h1` (all `None` when the head-block convention was not
    followed), `scope` (the landmark role a `within`-scoped snapshot was
    rooted at, `None` for an unscoped one or a non-snapshot result),
    `section` (the heading text a `section`-scoped snapshot was narrowed
    to, `None` for an unsectioned one or a non-snapshot result -- lifted
    the same way `scope` is, off the same generic `parse_head` extra-line
    dict, so a producer that stamps a `section:` head line needs no parser
    change here to have it arrive), `chars`
    (the producer's own declared body length, `None` when unstamped -- see
    `declared_length`), and lazily-computable `refs` via
    `parse_refs(obs["text"])` -- not attached eagerly here, so a `bash`
    observation's `text` is not scanned for tree syntax it was never going
    to contain.
    """
    head, body = parse_head(raw_text)
    url = head.get("url") if head else None
    title = head.get("title") if head else None
    # A producer that stamped its own `heading:` head line (extensions/
    # browser's `_head`, `heading=`) is trusted first -- that line is read
    # off the *unfiltered* tree at the source, so it survives whatever
    # `roles` filter the body went through (docs/Decisions.md, "A page's
    # identity is not one of its elements -- it is a property of the
    # page"), the same reason `chars:`/`scope:`/`roles:` already live in
    # the head block rather than being re-derived from the body. `or None`
    # treats an empty value the same as an absent one, matching the body
    # scan's own `and r["name"]` truthiness check below.
    h1 = (head.get("heading") or None) if head else None
    if h1 is None and (url is not None or title is not None):
        # Falls through here for two different reasons that both want the
        # same answer: an OLDER producer that predates `heading:` entirely
        # (this scan is exactly what ran before that field existed -- kept
        # byte-for-byte so a mixed fleet of old and new services both keep
        # working), or a NEWER producer that scanned its own unfiltered
        # tree and genuinely found no level-1 heading (this scan will not
        # find one either, since filtering only removes lines, never adds
        # them -- so re-scanning here is redundant but not wrong). Only
        # worth the scan at all for something that announced itself as a
        # snapshot via the head convention -- see the module docstring.
        refs, _ = parse_refs(body)
        h1 = next(
            (r["name"] for r in refs if r["role"] == "heading" and r["level"] == 1 and r["name"]),
            None,
        )
    return {
        "text": body,
        "url": url,
        "title": title,
        "h1": h1,
        "scope": head.get("scope") if head else None,
        "section": head.get("section") if head else None,
        "chars": declared_length(raw_text),
    }
