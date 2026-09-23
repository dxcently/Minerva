"""browser extension service -- serves Playwright-backed browser automation
behind eidolon's `service_call`.

WHY THIS EXISTS: a Rune tool script sees `json` and `eidolon` and nothing
else -- no sockets, no filesystem beyond the host primitives, no Python (see
eidolon/docs/extensions.md, "Why a service at all"). A real browser has to be
launched once and driven many times, so it lives here, behind
`GET /health` and `POST /call` with `{"method": ..., "args": {...}}`, gated on
a bearer token the extension host mints and hands this process in the
environment -- never on the command line, never logged. Full contract:
eidolon/docs/extensions.md. The Rune side is `extensions/browser/tools/*.rn`,
which register as `browser_open`, `browser_snapshot`, `browser_click`,
`browser_type`, `browser_read` and `browser_back`.

Run only through the extension host -- `eidolon ext start browser`, or
`eidolon ext enable browser` once and every later session adopts the same
process (see the "Lifetime: one service per machine" section of the doc
above: one Chromium, shared, not one per chat). It reads
EIDOLON_SERVICE_PORT and EIDOLON_SERVICE_TOKEN from the environment the host
starts it with; there is no other way in. For a manual smoke test set both
by hand, same idiom as jev/server.py:

    EIDOLON_SERVICE_PORT=8090 EIDOLON_SERVICE_TOKEN=dev python service.py

THE INTERPRETER: this extension's manifest (extension.rn) runs whichever of
`.venv/bin/python` or `.venv/Scripts/python.exe` exists, rather than naming
either -- see that file's comment. Set the venv up once with:

    python -m venv .venv
    .venv/bin/pip install -r requirements.txt          # POSIX
    .venv\\Scripts\\pip install -r requirements.txt      # Windows
    PLAYWRIGHT_BROWSERS_PATH=0 .venv/bin/python -m playwright install chromium --no-shell

`--no-shell` skips Playwright's separate chromium-headless-shell build
(about 270 MB on this box): `channel="chromium"` below launches the regular
Chrome-for-Testing binary in its own headless mode, which is the only
binary this service ever asks for. `PLAYWRIGHT_BROWSERS_PATH=0` is a
sentinel Playwright recognizes, not a path -- it keeps the downloaded
browser inside this venv's own site-packages rather than a machine-wide
cache (`~/.cache/ms-playwright` or `%LOCALAPPDATA%\\ms-playwright`), so the
whole extension, browser included, lives and dies with this one directory.
"""
from __future__ import annotations

import asyncio
import hmac
import os
import re
import shutil
import sys
import tempfile
import time
import urllib.parse
from contextlib import asynccontextmanager
from pathlib import Path

# Must run before `playwright` is imported, here or transitively -- the
# resolver that decides where to look for an installed browser reads this
# once, at import time. See the module docstring for what "0" means.
os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", "0")

import playwright  # noqa: E402  (see PLAYWRIGHT_BROWSERS_PATH above)
from fastapi import FastAPI, Request  # noqa: E402
from fastapi.responses import JSONResponse  # noqa: E402
import uvicorn  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402
from playwright.async_api import TimeoutError as PlaywrightTimeoutError  # noqa: E402

# Same reasoning as jev/server.py's own PORT/TOKEN: `os.environ[...]`, not
# `.get(...)`. A service that cannot get a real token has no business
# binding a port at all, so it dies on import rather than serving
# unauthenticated.
PORT = int(os.environ["EIDOLON_SERVICE_PORT"])
TOKEN = os.environ["EIDOLON_SERVICE_TOKEN"]

# `app` itself is constructed further down, past `_lifespan` -- it is the
# one thing here that needs that function to already exist as a real object
# (FastAPI's constructor takes `lifespan=` directly; there is no public
# setter to hand it over after the fact), everything else here is only
# resolved at call time and does not care where in the file it is defined.

# Short on purpose. By the time `_click`/`_type` reach Playwright at all,
# `_resolve_ref` has already checked the ref against the last snapshot's
# known set -- a Playwright-side timeout past that point means the page
# changed in a way the snapshot did not predict (same-page script mutating
# the DOM without a navigation), and the caller needs that news quickly,
# not after Playwright's own 30s default. `NAV_TIMEOUT_MS` is longer because
# a real page load, unlike clicking something already on screen, can
# legitimately take a while.
ACTION_TIMEOUT_MS = 8_000
NAV_TIMEOUT_MS = 30_000
# `aria_snapshot`'s own default (Playwright's Page.aria_snapshot(), verified
# against this venv's installed 1.63.0) is 30_000ms, which a real,
# moderately complex page can exceed -- observed directly against
# https://en.wikipedia.org/wiki/Felis on this box: `TimeoutError:
# Page.aria_snapshot: Timeout 30000ms exceeded.` on the very next snapshot
# after an ordinary link click, with nothing else unusual about the page.
# Doubled rather than left at Playwright's own default, and overridable per
# call the same way `_open`'s `timeout_s` is, because there is no way to
# know a page's tree-walk cost ahead of taking the snapshot that needs it.
SNAPSHOT_TIMEOUT_MS = 60_000

# What a ref looks like in Playwright's own `aria_snapshot(mode="ai")` text
# -- `[ref=e7]`, one per interactive or structurally meaningful node, EXCEPT
# that "e7" is only the whole story for the first snapshot a page's current
# document ever produces. Verified live on this box (Playwright 1.63.0,
# Windows): a fresh page's first `aria_snapshot(mode="ai")` after its first
# navigation numbers refs `e1`, `e2`, ...; every snapshot after any *later*
# navigation -- including an ordinary same-tab link click, not just another
# browser_open -- numbers them `f1e2`, `f1e3`, ..., then `f2e2`, ... on the
# navigation after that, incrementing per navigation rather than per frame
# actually present (reproduced with plain single-frame `data:` pages, no
# iframes involved). A regex anchored on `e\d+` right after `ref=` matches
# none of those and silently leaves `known_refs` empty, so `_resolve_ref`
# rejects every real ref past the first navigation as "not in the current
# snapshot" -- indistinguishable, from the caller's side, from a genuinely
# stale one. `automation/a11y.py` (jev's parser for this exact format)
# already extracts the ref generically for this reason; this regex now does
# the same rather than assuming a shape Playwright does not keep to.
REF_RE = re.compile(r"\[ref=([^\]]+)\]")

# What a `confine` list entry must look like (docs/design/unattended.md
# section 3, "Confinement"). `https?` only -- `file://` has no host and is
# not a confinable origin, so it is refused rather than silently matching
# nothing later. `[a-z0-9.\-]+` is deliberately lowercase-only: the graph
# lint that authors a `confine` list already requires lowercase hosts (same
# document), and this service lowercases every entry itself before checking
# it against this pattern (see `_confine`), so the two sides can never read
# the same literal differently over a case difference one side normalized
# and the other did not. No path, no userinfo, no IPv6 brackets -- an origin
# is scheme+host+port, nothing past the third slash, and `_origin_of` below
# never produces any of those either.
ORIGIN_RE = re.compile(r"^https?://[a-z0-9.\-]+(:\d+)?$")

# scheme -> the port that scheme means when none is written -- so
# `https://example.com` and `https://example.com:443` compare equal as the
# same origin, the same equivalence every browser's own same-origin check
# makes, rather than a stricter one that would make a graph author guess
# which spelling this service wants.
_DEFAULT_PORTS = {"http": 80, "https": 443}


class Browser:
    """One Chromium, one page, shared by every call -- see
    eidolon/docs/extensions.md's "Lifetime: one service per machine": two
    chats must not mean two Chromiums. Launch is lazy, on first real use,
    never on `/health` (see `health()` below); everything else runs under
    `LOCK`, held for the whole of one `/call` (see `call()` below), because
    Playwright's async API is bound to the event loop that started it -- a
    second thread calling into it would not just race, it would be talking
    to the wrong loop -- and because the ref contract below only means
    something if "the current snapshot" cannot change out from under a call
    already in flight.
    """

    def __init__(self):
        self.playwright = None
        self.browser = None
        self.context = None
        self.page = None
        # A ref is only ever meaningful against the snapshot that minted it
        # (docs/design/automation.md, "A Playwright accessibility snapshot":
        # "Playwright renumbers on every snapshot"). This set is replaced
        # wholesale by every `browser_snapshot` and cleared by every call
        # that can change the page -- open, back, click, type -- so a ref
        # from an earlier snapshot, or from before the last action, is
        # refused *by this service*, by name, before Playwright is even
        # asked. That is what "invalidated legibly" means here: not a
        # generic timeout, not a coincidental match on whatever now
        # occupies that internal id, but a named refusal pointing at the
        # fix (call browser_snapshot again).
        self.known_refs: frozenset[str] = frozenset()
        # Set only by `_confine`, cleared only by `_unconfine` and
        # `_close_browser` -- the tuple of origins the *current* context is
        # confined to, or `None` when it is the plain, unconfined context
        # `_ensure_page` makes. Read by `_open` (to refuse an out-of-list
        # target before navigating), by `_click`/`_type` (to know whether a
        # blocked navigation should become a refusal), and echoed back in
        # those three answers so a caller never has to track it itself
        # (docs/design/unattended.md section 3, "Confinement").
        self.confined: tuple[str, ...] | None = None
        # The most recent request `_route_confined` aborted, as
        # `(url, is_navigation_request)` -- overwritten by every abort, read
        # and reset around one `_click`/`_type` call at a time (see those
        # functions). Not a log: this service does not need one to answer
        # "did the thing this call just did get blocked", only the last
        # word on it.
        self.last_blocked: tuple[str, bool] | None = None


STATE = Browser()
LOCK = asyncio.Lock()


async def _ensure_browser():
    """The launch half of `_ensure_page` below, on its own: `_confine` (see
    "Confinement" further down) has to be able to stand up a browser before
    any page has ever been opened -- `browser_open {url, confine}` as a
    session's very first call is ordinary, and at that point `STATE.browser`
    is still `None`, nothing launched yet. Split out so both callers share
    one launch rather than `_confine` growing a second copy of it that could
    drift from this one. A no-op past the first call, from either caller."""
    if STATE.browser is not None:
        return STATE.browser
    STATE.playwright = await async_playwright().start()
    # channel="chromium": the full Chrome-for-Testing binary Playwright
    # downloads for the plain `chromium` browser, run in its own headless
    # mode -- NOT chromium-headless-shell, a second, separate ~270 MB
    # binary Playwright otherwise prefers for `headless=True` by default.
    # This venv's setup (see the module docstring) never installs that
    # second copy, so leaving `channel` unset here would fail every launch
    # with "Executable doesn't exist" instead of loading a page -- proved
    # by running both ways against this exact venv.
    #
    # No `--no-sandbox`, no `chromium_sandbox=False`: a page is the largest
    # untrusted-input surface this project has (see the extension's own
    # report), and the browser process's own OS sandbox is the boundary for
    # whatever a visited page's script tries -- not something to hand away
    # for convenience. A Linux host that cannot satisfy the sandbox's
    # namespace requirements (root inside some containers) needs a runtime
    # fix, not this flag; see the report for what a Linux deployment needs.
    STATE.browser = await STATE.playwright.chromium.launch(headless=True, channel="chromium")
    return STATE.browser


async def _ensure_page():
    if STATE.page is not None:
        return STATE.page
    await _ensure_browser()
    # A fresh, non-persistent context: no `storage_state`, no user-data
    # directory. Cookies, logins and local storage all vanish when this
    # process stops, on purpose -- the shared, machine-scoped service
    # otherwise a quiet place for one session's credentials to leak into
    # another's.
    STATE.context = await STATE.browser.new_context()
    STATE.page = await STATE.context.new_page()
    return STATE.page


def _invalidate_refs() -> None:
    STATE.known_refs = frozenset()


def _origin_of(url: str) -> str:
    """scheme+host+port, and nothing past the third slash -- path, query,
    fragment, userinfo and credentials are none of them part of an origin
    (docs/design/unattended.md section 3, "Confinement": "An origin is
    scheme, host and port, lowercase host, no path"). `urlsplit` already
    lowercases `.hostname` itself; done again here explicitly so this
    function's own contract does not quietly depend on that, matching
    `ORIGIN_RE`'s comment above. `http` and `https` are never folded into
    each other -- the scheme is part of the triple, not a detail -- so a
    `confine` list that means to cover both must list both, the same as a
    graph that needs `localhost` and `127.0.0.1` must list both (they are
    different hosts, not aliases). A port is dropped only when it is the
    literal default for that scheme (`_DEFAULT_PORTS`), so
    `https://example.com` and `https://example.com:443` compare equal but
    `https://example.com:8443` does not fold into either -- and a subdomain
    is never treated as covered by its parent: `https://en.wikipedia.org` in
    a `confine` list matches only that host, not `https://www.wikipedia.org`
    or any other `*.wikipedia.org`, because they are different hosts, full
    stop -- the same reasoning this function applies to scheme and port
    applies to host: no fuzzing, no implied family, an origin means exactly
    the triple it names."""
    parts = urllib.parse.urlsplit(url)
    scheme = (parts.scheme or "").lower()
    host = (parts.hostname or "").lower()
    port = parts.port
    if port is not None and _DEFAULT_PORTS.get(scheme) == port:
        port = None
    origin = f"{scheme}://{host}"
    if port is not None:
        origin += f":{port}"
    return origin


async def _settle(page) -> None:
    """Best-effort quiet-down after a click or a type that may or may not
    have started a navigation. `wait_for_load_state` returns at once if the
    page is already at that state, so this costs nothing when nothing
    happened; the `except` below covers only the case where something did
    start and is still loading a slow page -- swallowed because the
    caller's next `browser_snapshot` is the thing that actually needs to
    wait, and this call already told the truth about whether the click or
    type itself landed."""
    try:
        await page.wait_for_load_state("domcontentloaded", timeout=ACTION_TIMEOUT_MS)
    except PlaywrightTimeoutError:
        pass


def _head(
    url: str,
    title: str,
    *,
    scope: str | None = None,
    roles: list[str] | None = None,
    section: str | None = None,
    chars: int | None = None,
    heading: str | None = None,
) -> str:
    """The head is a block of `key: value` lines ending in a blank line
    (docs/design/observation.md, "Where an observation is narrowed"). `url`
    and `title` come first, in that order, for every reader that predates
    this block -- the jev interpreter lifts them into `obs.url`/`obs.title`
    without reparsing Playwright's own tree for them. `scope` is the
    landmark role the tree is rooted at, present only for a `within`-scoped
    snapshot. `roles` names the role filter applied to the body, present
    only when the caller asked for one (docs/design/observation.md's E5:
    "narrow at the source" extended from `within` to `roles`) -- read
    generically by `a11y.parse_head` like any other extra head line, so an
    older reader that has never heard of it is unaffected. `section` names
    the heading a `section`-scoped call narrowed to (docs/design/
    judgement.md section 4, "The landmark predicate, and 'See also'"),
    present only when the caller asked for one -- same generic-head-line
    reasoning as `roles`, stamped between `roles` and `chars` for no reason
    a reader needs to depend on (`parse_head` does not care about order
    among the extra lines, only that each one matches its `key: value`
    grammar). `section` is already all lowercase letters, so unlike
    `heading` below it needs no renaming to satisfy `a11y.parse_head`'s
    `[a-z][a-z_]*` key grammar. `heading` is the
    page's own level-1 heading -- read by `_snapshot` off the *unfiltered*
    tree, before any `roles` filter runs, for the same reason `roles`
    itself extended "narrow at the source" from `within`: a `roles` filter
    that excludes `heading` must not silently take the page's own identity
    with it (docs/Decisions.md, "A page's identity is not one of its
    elements -- it is a property of the page"; jev/automation/a11y.py's
    `build_obs` reads this line into `obs.h1` in preference to scanning the
    body itself, and falls back to that scan when an older producer never
    stamped this line at all). Named `heading:`, not `h1:`, so it still
    matches `a11y.parse_head`'s own `[a-z][a-z_]*` key grammar -- a digit
    right after the first letter does not (confirmed against that regex
    directly: `h1:` fails to parse as a head line at all, which would have
    silently truncated the whole head block one line early, README-worthy
    the way this comment now is instead). `chars` is the length of the body
    that follows the blank line, counted in code points by this producer,
    so a consumer can verify it received the whole thing rather than
    something a layer in between shortened -- the body's own length after
    any `roles`/`section` filtering has already happened, since that is the
    body actually being declared whole. Only `_snapshot` calls this: `_read`
    answers a JSON object with its own `url`/`title` fields and carries no
    head block, so the claim that the two share it -- true once, false
    since `_read` grew its own shape -- is not repeated here."""
    head = f"url: {url}\ntitle: {title}\n"
    if heading is not None:
        head += f"heading: {heading}\n"
    if scope is not None:
        head += f"scope: {scope}\n"
    if roles is not None:
        head += f"roles: {', '.join(roles)}\n"
    if section is not None:
        head += f"section: {section}\n"
    if chars is not None:
        head += f"chars: {chars}\n"
    return head + "\n"


# --- pushing the role filter down to the source -------------------------
#
# docs/design/observation.md section 2 ruled "narrow at the source" and
# only half-delivered it: `within` reaches Playwright's own `get_by_role`
# above, but `roles` -- wiki-hop's `choose.from.refs.roles` -- stayed a
# consumer-side filter in jev/automation/options.py's `_from_refs`, applied
# only after the *entire* `within`-scoped tree had already crossed the one
# boundary `MAX_BODY` guards. Measured live against
# https://en.wikipedia.org/wiki/Cat on this box: a `within: "main"` tree is
# 762,764 characters, 2.9x over `MAX_BODY`, so that call is refused before
# jev ever gets to filter it (docs/Build-Log.md, 2026-09-19, E5) -- and of
# its 2,741 `link`-role records, only 97 carry `landmark == "main"`
# directly (docs/Decisions.md, same date): the rest sit inside Wikipedia's
# own per-section `region` landmarks (Vector 2022 wraps every H2 body
# section in one) and a `navigation` landmark for the page's own UI chrome,
# so a size knob (`depth`) cannot fix what is a selectivity problem --
# proven separately by E3's depth sweep, flat at 58 options from depth 5
# through 7 and only reaching 87 unlimited.
#
# `aria_snapshot` has no filtered-list mode of its own -- mode="ai" always
# answers a tree (docs/design/observation.md's own words), never a list --
# so what follows is a projection done here, on the text Playwright already
# built for `within`/`depth`, not a second Playwright call and not one call
# per matching element. A second call per element was considered and
# rejected: `_click`'s own contract, and the comment on `Browser.known_refs`
# below it, both rest on "a scoped snapshot invalidates the previous
# snapshot's refs" -- confirmed for `within` by `ScopedSnapshotTests` in
# test_server.py -- so calling `aria_snapshot` on N individual link locators
# in a loop would invalidate every ref but the last one it minted, which is
# the opposite of a usable option list.
#
# This mirrors jev/automation/a11y.py's `parse_refs` -- the same
# `LANDMARK_ROLES`, the same indentation-stack nearest-ancestor walk, the
# same quoted-line unwrap for a name carrying a literal colon (real on this
# article: `'link "Toolbox: Ship''s Cat on the Kalmar Nyckel"'` is a genuine
# line in jev/tests/fixtures/wiki/cat_citations_excerpt.txt) -- independently
# rather than imported, for the same reason `REF_RE` above is this module's
# own rather than a11y.py's: this service does not depend on jev, and a
# format both sides read independently is the only way a drift between them
# is caught rather than silently propagated. jev/ is not this change's to
# edit (docs/design/observation.md names the boundary; this review's brief
# repeats it), so `_from_refs`'s own `roles`/`within` filter is left exactly
# as it is -- it keeps running downstream, redundantly and harmlessly, on a
# body that already satisfies it.
_LANDMARK_ROLES = frozenset(
    {"banner", "navigation", "main", "complementary", "contentinfo", "search", "region"}
)
# The role token opening a tree line, after its own `- ` marker -- the same
# grammar a11y.py's `_ELEMENT_RE` anchors on: a bareword, never quoted
# itself even when the name beside it is.
_ROLE_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_-]*")
# a11y.py's `_QUOTED_LINE_RE`, trimmed to just what finding a line's role
# needs: the wrapped `role "name" [attrs]` head, with any doubled `''`
# still doubled -- unescaped below, the same way a11y.py's own
# `.replace("''", "'")` does, before the role is read off the front of it.
_QUOTED_WRAP_RE = re.compile(r"^'((?:''|[^'])*)'\s*:")
# a11y.py's `_ELEMENT_RE`/`_ATTR_RE`, mirrored independently the same way
# `_LANDMARK_ROLES`/`_ROLE_RE`/`_QUOTED_WRAP_RE` above already do -- trimmed
# to the three groups `_first_heading` below actually reads (role, name,
# attrs); it has no use for that regex's trailing `text` group, so it is
# not reproduced here.
_ELEMENT_RE = re.compile(
    r'^(?P<role>[A-Za-z][A-Za-z0-9_-]*)'
    r'(?:\s+"(?P<name>(?:[^"\\]|\\.)*)")?'
    r'(?P<attrs>(?:\s*\[[A-Za-z]+=[^\]]*\])*)'
)
_ATTR_RE = re.compile(r"\[([A-Za-z]+)=([^\]]*)\]")


def _line_role_and_name(content: str) -> tuple[str | None, str | None]:
    """`_line_role`'s exact quoted-wrap unwrap, plus the element's own
    accessible name when it carries one -- what `section` needs in order to
    tell "this landmark's first child is the heading named X" from "is
    anything else", which the role alone cannot answer. Only called on a
    landmark's first child line (see `_filter_roles`'s pending-child check),
    never on every line the way `_line_role` is, so `section`'s cost stays
    the one extra regex per landmark the docstring below promises, not one
    per line."""
    if content.startswith("'"):
        wrapped = _QUOTED_WRAP_RE.match(content)
        if wrapped is None:
            return None, None
        content = wrapped.group(1).replace("''", "'")
    match = _ELEMENT_RE.match(content)
    if match is None:
        return None, None
    return match.group("role"), match.group("name")


def _line_role(content: str) -> str | None:
    """The role token starting `content` -- one tree line's text, already
    past its own `- ` marker -- or `None` when the line does not open with
    one (a `/url:`-style property line, or a line this format's grammar
    does not otherwise cover; a11y.py's `parse_refs` treats both the same
    way this does, as structurally present but not a record). A name
    containing a literal colon forces Playwright to wrap the whole
    `role "name" [attrs]` head in single quotes instead of the bare
    `role "name" [attrs]: text` line `_ROLE_RE` alone can read, because an
    unwrapped colon inside the name would be ambiguous with that grammar's
    own trailing `: text` delimiter -- so that wrapper is peeled first when
    present (`content.startswith("'")`) and the role is read from what it
    exposes, never from the raw wrapped text, which starts with `'` and
    matches no bare role token at all."""
    if content.startswith("'"):
        wrapped = _QUOTED_WRAP_RE.match(content)
        if wrapped is None:
            return None
        content = wrapped.group(1).replace("''", "'")
    match = _ROLE_RE.match(content)
    return match.group(0) if match else None


def _filter_roles(
    tree: str,
    roles: frozenset[str],
    within: str | None,
    max_n: int | None,
    section: str | None = None,
    url: str | None = None,
) -> str:
    """Project `tree` down to just the lines whose role is in `roles`,
    computing each line's nearest-landmark ancestor the way
    jev/automation/a11y.py's `parse_refs` would and keeping a match only
    when that landmark equals `within` (when a scope was asked for at all
    -- `within is None` imposes no landmark condition, same as an unscoped
    call imposes none today). `max_n`, when given, caps the kept list at
    its own first N in document order -- **a cap, not a floor**: an earlier
    brief on this same project said "enough real links to choose from --
    the graph's `max` is 64" and had that read as a requirement, scoring a
    58-option menu a failure over a constraint nobody actually set
    (docs/Decisions.md, 2026-09-19, "Why no `depth` saves wiki-hop"). Fewer
    real matches than `max_n` is not an error here either; it is the page.

    Every kept line is reproduced byte-for-byte from `tree` -- only which
    lines survive changes, never their own text -- so a ref inside one is
    exactly the ref Playwright minted for it, resolvable through the same
    `aria-ref=` engine `_click` always uses, same as an unfiltered
    snapshot's refs are today (proved for this exact mechanism by
    `RoleScopedSnapshotTests` in test_server.py, including past a second
    navigation in the `f<N>e<M>` ref regime, not only the easy first-
    navigation case).

    What changes is nesting: every survivor is re-parented as a direct
    child of the kept scope line (one level of indentation under it),
    regardless of how deep it actually sat in the real tree, because
    wiki-hop's own consumer-side check (`options.py`'s `_from_refs`,
    `record.get("landmark") != within`) still runs on whatever this
    returns -- so a kept link's landmark must still read `within` when jev
    re-parses this projection, not merely when this function decided it
    did. Without `within` there is no scope line to re-parent under, so a
    match is emitted at the top level and carries no landmark either way,
    the same as it would if jev parsed an equivalent hand-filtered list
    today.

    `section`, when given (docs/design/judgement.md section 4, "The
    landmark predicate, and 'See also'"), narrows further and differently:
    a landmark becomes a **section root** iff the first element line nested
    directly under it is `heading` with `name == section`, and a survivor
    must have a section root as its OWN nearest landmark -- never `within`
    itself, even though `landmark == within` would satisfy the plain
    `within` condition above. That exclusion is not a separate check; it
    falls out of using `landmark_frame is not None` as part of the
    condition, because a line directly under `within` with no landmark
    between it and the scope line has no frame at all to be a root, the
    same reason the lead and infobox (`within`'s own direct children) are
    exactly what E5's "See also" ruling says a `section` scope must
    exclude. A section root is found the same nearest-landmark way `within`
    already is (so a `navigation` "Portals" bar nested inside "See also" is
    its own nearest landmark for anything inside it, and is never a section
    root itself unless ITS OWN first child is a matching heading) -- this
    is what keeps the Portals bar's links out of "See also"'s 31 without a
    second, different mechanism.

    Ambiguity has to be provable, so when `section` is given the walk never
    takes the `max_n` early exit -- a second same-named heading later in
    the document, past where an early `max_n` break would have stopped,
    must still be found, or "no landmark whose first heading is that" could
    misreport a real duplicate as unique. `max_n` is instead applied to
    `kept` once the walk finishes and exactly one root was found -- the
    same cap, the same document order, one comparison later. Zero roots or
    more than one is refused by name, the same shape `within`'s own
    element-count check already has:
    `section {section!r}: no landmark whose first heading is that on {url}`
    or `section {section!r}: {n} landmarks whose first heading is that on
    {url}; a section names exactly one` -- `url` is threaded in from
    `_snapshot` (`page.url`, already resolved there) purely for this
    message; nothing else here reads it.
    """
    lines = tree.splitlines()
    if not lines:
        return tree
    scope_line = lines[0] if within is not None else None
    body_lines = lines[1:] if within is not None else lines

    # The implicit ancestor for anything that finds no landmark among its
    # own tracked ancestors: `within`, when there is a kept-but-unwalked
    # scope line standing in as everything's ultimate parent (exactly what
    # a11y.py's own walk would find were it parsing `tree` whole, root line
    # included); `None`, when there is no scope at all.
    implicit_landmark = within

    # Stack entries are `[indent, role, pending, is_section_root]` -- a
    # list, not a tuple, only because `section` needs to flip the last two
    # fields in place once a landmark's first child is seen. `pending` is
    # true only for a landmark role, and only until its own first child is
    # processed (see the loop body); `is_section_root` starts false and is
    # set at that same moment. Both are dead weight when `section is None`
    # (always false, never read), which is why the `section is None` path
    # below never differs from what this function returned before `section`
    # existed.
    stack: list[list] = []
    kept: list[str] = []
    section_roots = 0
    for raw_line in body_lines:
        stripped = raw_line.lstrip(" ")
        if not stripped.startswith("- "):
            continue  # blank, or not tree syntax -- no stack effect, same as a11y.py
        indent = len(raw_line) - len(stripped)
        content = stripped[2:]
        while stack and stack[-1][0] >= indent:
            stack.pop()

        role = _line_role(content)

        if section is not None and stack and stack[-1][2]:
            # `stack[-1]` is a landmark still awaiting its first child, and
            # this line -- the first one not yet popped since that landmark
            # was pushed -- is it, by construction of the same indentation
            # stack the nearest-landmark walk already relies on. Settle it
            # now, once, whatever it turns out to be: a second line nested
            # under the same landmark is never its first child again.
            stack[-1][2] = False
            _, name = _line_role_and_name(content)
            if role == "heading" and name == section:
                stack[-1][3] = True
                section_roots += 1

        landmark_frame = next((f for f in reversed(stack) if f[1] in _LANDMARK_ROLES), None)

        if section is None:
            landmark = landmark_frame[1] if landmark_frame is not None else implicit_landmark
            landmark_ok = within is None or landmark == within
        else:
            # Never satisfied by the implicit `within` (landmark_frame is
            # None) -- see the docstring above for why that is not merely
            # this function's default but the exclusion E5's ruling names.
            landmark_ok = landmark_frame is not None and landmark_frame[3]

        if role is not None and role in roles and landmark_ok:
            kept.append(stripped)
            if max_n is not None and section is None and len(kept) >= max_n:
                # Stop walking, not just stop keeping: a page with far more
                # matches than `max_n` should not pay to finish walking a
                # tree whose tail it will only discard. Not taken when
                # `section` is given -- see the docstring above: ambiguity
                # past this point must still be found.
                break

        stack.append([indent, role, section is not None and role in _LANDMARK_ROLES, False])

    if section is not None:
        if section_roots == 0:
            raise ValueError(f"section {section!r}: no landmark whose first heading is that on {url}")
        if section_roots > 1:
            raise ValueError(
                f"section {section!r}: {section_roots} landmarks whose first heading is that on {url}; "
                "a section names exactly one"
            )
        if max_n is not None:
            kept = kept[:max_n]

    if scope_line is None:
        return "\n".join(kept) + ("\n" if kept else "")
    return "\n".join([scope_line, *(f"  {line}" for line in kept)]) + "\n"


def _first_heading(tree: str) -> str | None:
    """The name of the first `heading` role element at `[level=1]` in
    `tree` -- read here, on the *unfiltered* tree `_snapshot` just got back
    from Playwright, before `_filter_roles` (if `roles` was asked for at
    all) ever projects it down to a subset that may or may not still
    include `heading`. This is what lets a page's own identity survive any
    `roles` filter the same way `url`/`title` already do, rather than being
    a casualty of whichever filter a particular graph happens to ask for
    (docs/Decisions.md, "A page's identity is not one of its elements").

    Deliberately does no landmark walk of its own, unlike `_filter_roles`:
    a `within`-scoped call already handed this function a tree rooted at
    that landmark (Playwright's own `target.aria_snapshot`, in `_snapshot`
    below), so every line in it is already inside `within` by construction,
    same as an unscoped call's tree is the whole page by construction --
    either way, "first heading, level 1, with a name, anywhere in this
    tree" is exactly jev/automation/a11y.py's own `build_obs` semantics
    before this change existed (no landmark check there either), mirrored
    independently rather than imported for the reason given at the top of
    `_filter_roles` above: this service does not depend on jev.
    """
    for raw_line in tree.splitlines():
        stripped = raw_line.lstrip(" ")
        if not stripped.startswith("- "):
            continue
        content = stripped[2:]
        if content.startswith("'"):
            wrapped = _QUOTED_WRAP_RE.match(content)
            if wrapped is None:
                continue
            content = wrapped.group(1).replace("''", "'")
        match = _ELEMENT_RE.match(content)
        if match is None or match.group("role") != "heading":
            continue
        attrs = dict(_ATTR_RE.findall(match.group("attrs") or ""))
        if attrs.get("level") != "1":
            continue
        name = match.group("name")
        if name:
            return name
    return None


# --- the six methods ----------------------------------------------------
# --- confinement ----------------------------------------------------------
#
# docs/design/unattended.md section 3, "Confinement": a warrant can name
# which origins a run may touch, but the gate layer (crates/rune) only ever
# sees a tool call's name and arguments -- never a page's subresource loads,
# its redirects, or a script's own fetch(). Only the browser process can see
# those, so this is the one layer that can actually enforce the bound, and
# it enforces it the same way for every shape a request can take: a fresh,
# isolated context with one route installed, matched against the FULL
# request URL Chromium is about to send, before that send happens. A
# request this route matches never leaves the browser process -- there is
# no "warn and continue" here, only "abort before the socket opens" and
# "never routed at all," and the tests beside these functions prove both
# outcomes against a real Chromium rather than asserting them.


def _record_block(request, url: str) -> None:
    """Shared by every abort path below: the last request confinement
    refused, and whether it was itself trying to leave the page --
    `is_navigation_request()` is what lets `_click`/`_type` tell "the thing
    you just did tried to leave the page and could not" from "some
    unrelated subresource also got blocked" (see those two functions).
    `url` is taken as a parameter rather than always read off `request.url`
    because the redirect case below needs to name the HOP that was refused
    (the Location a 3xx pointed at), not the original, allowed request that
    carried it."""
    STATE.last_blocked = (url, request.is_navigation_request())


async def _route_confined(route) -> None:
    """The confined context's only route handler, installed on `"**/*"` --
    every request, not only the ones an origin check would refuse. That is
    not caution for its own sake: a plain "match the disallowed ones,
    `route.abort` them, leave everything else unrouted" handler -- this
    function's first implementation, and docs/design/unattended.md's own
    step 4 text -- was tested against a real redirect and failed. Confirmed
    against a real Chromium on this box, not merely read about (matches a
    known, filed Playwright limitation: github.com/microsoft/playwright/
    issues/34994): a context-level route is invoked for a request's FIRST
    leg, but Playwright's own driver continues a server-side redirect by
    calling Fetch.continueRequest directly against Chromium, with no
    RouteImpl constructed for the hop that follows -- so a route matching
    only "not a listed origin" never sees the redirect target at all, and a
    confined page that opened `{A}/redirect` sailed straight through to
    `{B}/x.html`, hit B for real, and the calling test caught it (not the
    other way around).

    So every request is taken over here, whole. `route.request`'s own
    origin is checked first, exactly as before; a request already off the
    list is aborted right there, before anything is sent
    (`_record_block`/`route.abort`). One that passes is fetched BY HAND,
    `route.fetch(max_redirects=0)` -- Playwright's own escape hatch for
    this exact shape, found by reading the limitation above rather than
    guessing past it -- which performs the real request but does NOT let
    Chromium auto-follow a redirect the way an un-intercepted request
    would. The response that comes back is inspected before it is ever
    shown to the page: a 3xx's `Location` is resolved and origin-checked
    the same way the original request was, and if it is off the list this
    hop is aborted here too, with the redirect TARGET as the thing named in
    `STATE.last_blocked` -- B never sees a byte either way, whether it was
    the request's own destination or where a legal request's own server
    tried to send it next. A 3xx whose target IS listed, or any non-3xx
    response, is handed to the page via `route.fulfill(response=...)` --
    which, for a redirect, makes the BROWSER issue the next hop as its own
    fresh navigation, landing back on this same handler and re-checked
    again, so a multi-hop chain is validated hop by hop rather than only at
    its ends. `route.fetch()` replays the original request's method,
    headers, cookies and body unchanged when given no overrides beyond
    `max_redirects`, and `route.fulfill(response=...)` passes its
    `Set-Cookie` and every other header through to the browser exactly as
    a real network response would -- both confirmed live, not assumed
    (`ConfinementTests.test_a_confining_open_starts_with_no_cookies`,
    which depends on cookies still working at all under this path, not
    only on there being none to start)."""
    request = route.request
    origins = STATE.confined
    origin = _origin_of(request.url)
    if origins is None or origin not in origins:
        _record_block(request, request.url)
        await route.abort("blockedbyclient")
        return
    try:
        response = await route.fetch(max_redirects=0)
    except Exception:
        # The real request itself failed (DNS, connection refused, ...) --
        # not a confinement decision, but the route still must be resolved
        # one way or another, and `abort` is the honest answer: this
        # service never actually delivered anything to the page either.
        _record_block(request, request.url)
        await route.abort("failed")
        return
    if 300 <= response.status < 400:
        location = response.headers.get("location")
        if location:
            target_url = urllib.parse.urljoin(request.url, location)
            target_origin = _origin_of(target_url)
            if target_origin not in origins:
                _record_block(request, target_url)
                await route.abort("blockedbyclient")
                return
    await route.fulfill(response=response)


async def _close_extra_page(page) -> None:
    """Installed on the confined context's own `"page"` event -- fired for
    any page beyond the one `_confine` itself opens, i.e. a `target=_blank`
    link or a `window.open()` call from inside a confined page. Playwright's
    event emitter for this object is `AsyncIOEventEmitter`
    (`playwright/_impl/_connection.py`, `ChannelOwner`), confirmed directly
    against this venv's installed source: passing an `async def` here is not
    just documentation-shaped, `_emit_run` (`pyee/asyncio.py`) calls the
    handler, sees a coroutine come back, and schedules it with
    `ensure_future` itself -- there is no separate sync wrapper this needs.
    Closing rather than leaving it open matters because `_route_confined` is
    installed on the CONTEXT, not this one page -- a second page in the same
    confined context is just as confined as the first, but a run that
    thinks it is driving one page should never quietly gain a second one it
    never asked for and never snapshots."""
    if page is not STATE.page:
        try:
            await page.close()
        except Exception:
            pass  # a close racing the page's own teardown must not crash the handler


async def _confine(origins) -> None:
    """`browser_open {url, confine}`'s implementation. Validates, then
    **replaces the shared context wholesale**: closes whatever context is
    current (confined or not), opens a fresh `new_context(accept_downloads=
    False)` -- no cookies, no storage, no downloads, inherited from nothing
    -- installs the one route (`_route_confined`) that resolves every
    request itself and aborts one outside the listed origins (navigations,
    subresources and redirect hops alike -- see that function's own
    docstring for why a hop is not automatically safe just because the
    request that carried it was), wires
    `_close_extra_page` so a popup cannot sidestep it, and only then opens
    the one page `_ensure_page` will hand back to everything else. `_open`
    (the only caller) still does the per-navigation origin check and the
    actual `page.goto` itself; this function's job ends at "the context this
    session's page lives in cannot reach anywhere but the list," proved
    against a real Chromium by `ConfinementTests` below.

    Origin comparison, stated plainly because a wrong answer here is
    invisible until someone crafts a hostname for it: an origin is
    scheme+host+port (`ORIGIN_RE`, `_origin_of`). `http` and `https` are
    different origins, never folded together. A default port
    (`_DEFAULT_PORTS`) is optional in the list and in a request alike, but
    that is the ONLY normalization -- no subdomain match, no parent-domain
    match, no case-fuzzing past a plain `.lower()`. `_route_confined` below
    never compares strings against each other at all -- every request URL
    is parsed by `_origin_of` down to its own scheme+host+port before the
    check, an exact tuple-membership test against `STATE.confined` -- so
    `https://example.com` in the list cannot be confused with a request to
    `https://example.com.evil.com`: `_origin_of` reads that request's host
    as `example.com.evil.com`, a different string, full stop, never a
    prefix match on the raw URL that a crafted hostname could exploit.
    `ConfinementTests.test_a_lookalike_host_with_the_listed_origin_as_a_
    prefix_is_still_blocked` below exists because "reasoned to be correct"
    and "watched actually fail against a real browser" are not the same
    claim -- this docstring's own first version described a regex
    boundary-character guard for exactly this case, written before
    `_route_confined` existed in its current form and left behind by the
    redirect fix above; the exact-parse comparison here replaced it
    entirely rather than sitting beside it, and the test was re-run against
    the replacement, not merely trusted to still apply."""
    if not isinstance(origins, list) or not origins or not all(isinstance(o, str) and o for o in origins):
        raise ValueError(f"confine must be a list of origins like https://en.wikipedia.org; got {origins!r}")
    normalized = [o.lower() for o in origins]
    for o in normalized:
        if not ORIGIN_RE.match(o):
            raise ValueError(f"confine must be a list of origins like https://en.wikipedia.org; got {origins!r}")

    await _ensure_browser()  # may be a session's very first call -- see that function's own docstring
    if STATE.context is not None:
        await STATE.context.close()
    STATE.context = await STATE.browser.new_context(accept_downloads=False)

    # Set before the route is installed or any page exists, not after: the
    # route reads `STATE.confined` at call time (see `_route_confined`)
    # rather than closing over a local, so there is no window -- even in
    # principle -- where a route could fire against a stale or absent
    # value.
    STATE.confined = tuple(normalized)
    await STATE.context.route("**/*", _route_confined)

    # `new_page()` BEFORE `.on("page", ...)` is installed, not after --
    # found live, not reasoned: the context's own `"page"` event
    # (`_on_page`, `playwright/_impl/_browser_context.py`) fires for EVERY
    # page created in the context, `new_page()`'s own included, and it does
    # so before this call's `await` below returns and assigns `STATE.page`.
    # Registered in the other order, `_close_extra_page` sees this very
    # page arrive while `STATE.page` still holds the PREVIOUS session's
    # value (or `None`), the `page is not STATE.page` check reads it as an
    # extra page, and it closes the one page `_open`'s `page.goto` is about
    # to use -- observed directly as `net::ERR_ABORTED; maybe frame was
    # detached?` on every single confined open, including to a listed
    # origin, before this ordering fix (docs/design/unattended.md's own
    # step 4 text lists `.on("page", ...)` first; that ordering does not
    # survive contact with a real browser).
    STATE.page = await STATE.context.new_page()
    STATE.context.on("page", _close_extra_page)
    STATE.last_blocked = None
    _invalidate_refs()  # a new context; any ref from the old one names nothing on it


async def _unconfine() -> None:
    """The other half of `_open`'s `confine`/no-`confine` branch (see
    `_open` below): a no-op unless the current context actually is confined,
    so an ordinary unconfined `browser_open` -- including a session's very
    first call ever -- costs nothing extra and leaves `_ensure_page`'s own
    lazy launch exactly as it was before this section existed. When it does
    have something to undo: same replace-the-context shape as `_confine`,
    minus `accept_downloads=False` and the route -- an ordinary shared
    context, reachable from anywhere, same as before confinement existed at
    all."""
    if STATE.confined is None:
        return
    if STATE.context is not None:
        await STATE.context.close()
    STATE.context = await STATE.browser.new_context()
    STATE.page = await STATE.context.new_page()
    STATE.confined = None
    STATE.last_blocked = None
    _invalidate_refs()


async def _open(args: dict) -> dict:
    url = (args.get("url") or "").strip()
    if not url:
        raise ValueError("open needs a url")
    timeout_s = args.get("timeout_s") or 30
    confine = args.get("confine")
    if confine is not None:
        await _confine(confine)
    else:
        await _unconfine()
    page = await _ensure_page()
    if STATE.confined is not None:
        origin = _origin_of(url)
        if origin not in STATE.confined:
            raise ValueError(f"open {url}: origin {origin} is not in confine {list(STATE.confined)}")
    await page.goto(url, timeout=float(timeout_s) * 1000, wait_until="load")
    _invalidate_refs()  # a new document; any ref from before names nothing on it
    return {
        "url": page.url,
        "title": await page.title(),
        "confined": list(STATE.confined) if STATE.confined else None,
    }


async def _snapshot(args: dict) -> str:
    page = await _ensure_page()
    timeout_s = args.get("timeout_s") or (SNAPSHOT_TIMEOUT_MS / 1000)
    # `depth` is Playwright's own escape hatch for the failure mode above:
    # unset (the default, and the only behaviour before this review) walks
    # the whole tree, which is what hung on the Felis page; every depth
    # tried against that same page in isolation -- 5, even 2 -- returned in
    # under a second. Left unset by default because cutting the tree changes
    # what a graph's chooser can see, which is a decision for whoever writes
    # the graph, not a default this service should pick silently -- but a
    # caller that hits the hang has no way to ask for less tree at all
    # without this. `within` is the other escape hatch, and the one a graph
    # should reach for first, because it changes what crosses the wire
    # rather than how deep the walk goes -- the service does not know what
    # `main` is, the browser does, through `get_by_role` (see
    # docs/design/observation.md, "Where an observation is narrowed").
    depth = args.get("depth")
    within = args.get("within")
    if within is not None:
        if not isinstance(within, str) or not within:
            raise ValueError("within must be a landmark role name, e.g. \"main\"")

    # `roles` is the third escape hatch, beside `depth` and `within` above,
    # and the one that answers what neither of the other two can: `depth`
    # trims the tree uniformly and cannot fix a selectivity problem (a real
    # Cat-article sweep found the option count flat at 58 from depth 5
    # through 7, only reaching 87 with no limit at all -- a size knob
    # spending characters without buying options); `within` narrows to one
    # landmark's whole subtree, which on this article is still 762,764
    # characters, 2.9x over MAX_BODY. `roles` narrows what `within` cannot:
    # which *kind* of node survives, computed and applied here rather than
    # after the tree has already crossed the wire (docs/design/
    # observation.md, "Where an observation is narrowed", extended by E5 --
    # see `_filter_roles` above for the mechanism and what it costs).
    # Validated before either Playwright call below, same reasoning as
    # `within`'s own check just above: a caller's bad argument should not
    # be the reason this service pays for a snapshot it is about to refuse
    # anyway.
    roles = args.get("roles")
    max_n = args.get("max")
    # `section` is the fourth escape hatch (docs/design/judgement.md
    # section 4, "The landmark predicate, and 'See also'"): where `roles`
    # narrows by kind, `section` narrows by which of `within`'s own nested
    # landmarks a line sits in, found by the heading that opens it --
    # "See also" holds 34 named links behind 763 others in document order,
    # a `max: 64` cap and document order reach none of them, and no
    # landmark predicate does either (the lead and infobox are `within`'s
    # own direct children, not a section's). It narrows what `roles`
    # already narrows, so it requires `roles` for the same reason `max`
    # does just above; it additionally requires `within`, because a
    # section is one of `within`'s nested landmarks and there is none to
    # be inside without a `within` to nest inside. Both requirements are
    # validated here, before either Playwright call, same reasoning as
    # `roles`'s own checks below -- a bad argument should not be the
    # reason this service pays for a snapshot it is about to refuse
    # anyway. The refusal for a heading that does not exist, or exists
    # more than once, cannot be checked this early: it depends on the
    # tree Playwright is about to hand back, so `_filter_roles` raises it
    # once that tree is in hand (see its own docstring).
    section = args.get("section")
    if roles is None:
        if max_n is not None:
            raise ValueError(
                "max requires roles -- it caps the filtered list; there is no such cap for a whole tree"
            )
        if section is not None:
            raise ValueError(
                "section requires roles -- it narrows a role-filtered list; there is no such list for a whole tree"
            )
    else:
        if not isinstance(roles, list) or not roles or not all(isinstance(r, str) and r for r in roles):
            raise ValueError('roles must be a non-empty list of role names, e.g. ["link"]')
        if max_n is not None and (not isinstance(max_n, int) or isinstance(max_n, bool) or max_n <= 0):
            raise ValueError("max must be a positive integer")
        if section is not None:
            if not isinstance(section, str) or not section:
                raise ValueError('section must be a non-empty string naming a heading, e.g. "See also"')
            if within is None:
                raise ValueError(
                    "section requires within -- a section is one of within's own nested landmarks, "
                    "and there is none to be inside without within"
                )

    if within is not None:
        target = page.get_by_role(within)
        n = await target.count()
        if n != 1:
            raise ValueError(
                f"within={within!r}: {n} elements with that role on {page.url}; a scope names exactly one"
                + (" -- use an unscoped snapshot, or depth" if n > 1 else "")
            )
        tree = await target.aria_snapshot(mode="ai", timeout=float(timeout_s) * 1000, depth=depth)
    else:
        tree = await page.aria_snapshot(mode="ai", timeout=float(timeout_s) * 1000, depth=depth)

    # Read off the tree Playwright actually returned, before `roles` (if
    # given) gets a chance to project headings out of it -- see
    # `_first_heading`'s own docstring for why this has to happen here,
    # ahead of the `_filter_roles` call just below, and not after it.
    heading = _first_heading(tree)

    if roles is not None:
        tree = _filter_roles(tree, frozenset(roles), within, max_n, section, page.url)

    # Replaced wholesale from whatever this call is actually about to hand
    # back -- the filtered body when `roles` narrowed it, same as an
    # unfiltered call's known_refs has always been every ref in *its* own
    # returned body. A ref this call excluded (a button, when the caller
    # asked only for links; a link outside `within`'s own subtree) was
    # never shown, and the existing rule already refuses anything the
    # current snapshot did not show, by name, before Playwright is even
    # asked -- narrowing what counts as "current" narrows what that rule
    # protects without changing the rule itself.
    STATE.known_refs = frozenset(REF_RE.findall(tree))
    return _head(
        page.url, await page.title(), scope=within, roles=roles, section=section, chars=len(tree), heading=heading
    ) + tree


async def _resolve_ref(args: dict) -> str:
    ref = args.get("ref")
    if not ref or not isinstance(ref, str):
        raise ValueError("needs a ref from the current browser_snapshot")
    if ref not in STATE.known_refs:
        raise ValueError(
            f"ref {ref!r} is not in the current snapshot; call browser_snapshot again"
        )
    return ref


async def _click(args: dict) -> dict:
    # Checked before `_ensure_page()`, not after: a ref rejected by
    # `_resolve_ref` should cost nothing, and in particular should never be
    # the reason this service pays for its first Chromium launch.
    ref = await _resolve_ref(args)
    page = await _ensure_page()
    # Reset before this click's own action starts, not after -- so the
    # check below, past `_settle`, can only ever see a block THIS click
    # caused, never one left over from an earlier call
    # (docs/design/unattended.md section 3, "Confinement").
    STATE.last_blocked = None
    locator = page.locator(f"aria-ref={ref}")
    try:
        await locator.click(timeout=ACTION_TIMEOUT_MS)
    except PlaywrightTimeoutError:
        raise ValueError(
            f"ref {ref!r} was in the last snapshot but the page would not resolve it to a "
            f"clickable element; it may have changed underneath -- call browser_snapshot again"
        )
    finally:
        # Whether the click landed or not: a click can rewrite the DOM even
        # when it does not navigate, so whatever refs the last snapshot
        # handed out are not trusted past this point. This is the same rule
        # docs/design/automation.md's wiki-hop graph is built around ("why
        # `reading` re-enters and snapshots again after every click rather
        # than clicking twice from one observation"), enforced here instead
        # of left to that graph's discipline.
        _invalidate_refs()
    await _settle(page)
    # A confined click that tried to navigate off the list already failed
    # at the network layer (`_route_confined` aborted it before it
    # happened); this turns that into a refusal a caller can act on, naming
    # where it tried to go and what it was allowed to reach -- the same
    # shape `_open`'s own pre-navigation refusal uses. A blocked SUBRESOURCE
    # that is not itself a navigation (an image, a script's own fetch) is
    # not this click's business to report -- `is_navigation_request()` is
    # what tells the two apart (see `_route_confined`/`_record_block`).
    if STATE.confined is not None and STATE.last_blocked is not None:
        blocked_url, was_navigation = STATE.last_blocked
        if was_navigation:
            blocked_origin = _origin_of(blocked_url)
            raise ValueError(
                f"click {ref}: navigation to {blocked_url} was blocked -- "
                f"origin {blocked_origin} is not in confine {list(STATE.confined)}"
            )
    return {
        "ref": ref,
        "url": page.url,
        "title": await page.title(),
        "confined": list(STATE.confined) if STATE.confined else None,
    }


async def _type(args: dict) -> dict:
    text = args.get("text")
    if not isinstance(text, str):
        raise ValueError("type needs text")
    # Same ordering as `_click`, same reason: a rejected ref must not be
    # what pays for this service's first Chromium launch.
    ref = await _resolve_ref(args)
    page = await _ensure_page()
    STATE.last_blocked = None  # same reasoning as `_click`
    submit = bool(args.get("submit"))
    locator = page.locator(f"aria-ref={ref}")
    try:
        # `.fill()`, not keystroke-by-keystroke typing: Playwright's own
        # guidance is to fill unless a test specifically needs real key
        # events, and nothing here needs that.
        await locator.fill(text, timeout=ACTION_TIMEOUT_MS)
        if submit:
            await locator.press("Enter", timeout=ACTION_TIMEOUT_MS)
    except PlaywrightTimeoutError:
        raise ValueError(
            f"ref {ref!r} was in the last snapshot but the page would not resolve it to a "
            f"fillable element; it may have changed underneath -- call browser_snapshot again"
        )
    finally:
        _invalidate_refs()  # same reasoning as `_click`
    await _settle(page)
    # `submit` is the only way `_type` can navigate (Enter on a form
    # field); same check and same reasoning as `_click`'s, immediately
    # above it in this file.
    if STATE.confined is not None and STATE.last_blocked is not None:
        blocked_url, was_navigation = STATE.last_blocked
        if was_navigation:
            blocked_origin = _origin_of(blocked_url)
            raise ValueError(
                f"type {ref}: navigation to {blocked_url} was blocked -- "
                f"origin {blocked_origin} is not in confine {list(STATE.confined)}"
            )
    return {
        "ref": ref,
        "url": page.url,
        "title": await page.title(),
        "confined": list(STATE.confined) if STATE.confined else None,
    }


async def _read(args: dict) -> dict:
    page = await _ensure_page()
    max_chars = int(args.get("max") or 8000)
    text = await page.locator("body").inner_text()
    return {
        "url": page.url,
        "title": await page.title(),
        "text": text[:max_chars],
        "truncated": len(text) > max_chars,
    }


async def _back(args: dict) -> dict:
    page = await _ensure_page()
    resp = await page.go_back(timeout=NAV_TIMEOUT_MS, wait_until="load")
    _invalidate_refs()  # a different document under the same tab, same as `_open`
    return {"went_back": resp is not None, "url": page.url, "title": await page.title()}


METHODS = {
    "open": _open,
    "snapshot": _snapshot,
    "click": _click,
    "type": _type,
    "read": _read,
    "back": _back,
}


def _chromium_installed() -> bool:
    """A stat and a glob, not a launch -- see `health()`. Playwright's own
    layout under `PLAYWRIGHT_BROWSERS_PATH=0` names the executable
    `chrome.exe` on Windows and `chrome` (no extension) on POSIX inside a
    `chromium-<build>/chrome-<platform>/` directory; observed directly on
    this Windows box (`chrome-win64/chrome.exe`), reasoned by the same
    Chrome-for-Testing build-naming convention for the POSIX branch, not
    observed there."""
    base = Path(playwright.__file__).resolve().parent / "driver" / "package" / ".local-browsers"
    exe = "chrome.exe" if sys.platform == "win32" else "chrome"
    return any(base.glob(f"chromium-*/chrome-*/{exe}"))


# --- profile-directory cleanup -------------------------------------------
#
# `_ensure_page` above launches Chromium through a plain, non-persistent
# `browser_type.launch()` -- no `user_data_dir` argument exists on that call
# in Playwright's own Python API (only `launch_persistent_context` takes
# one; see this venv's playwright/_impl/_browser_type.py). So this module
# never names or picks a profile directory itself. Playwright's own driver
# does, silently, on every launch
# (driver/package/lib/coreBundle.js, `_prepareToLaunch`):
#
#     userDataDir = await fs.promises.mkdtemp(path.join(os.tmpdir(),
#         `playwright_${this._name}dev_profile-`))   // "chromium" here
#     artifactsDir = await fs.promises.mkdtemp(path.join(os.tmpdir(),
#         "playwright-artifacts-"))
#
# -- confirmed live on this box, twice, against a real launch: e.g.
# `%TEMP%\playwright_chromiumdev_profile-pmBKLx` paired with
# `%TEMP%\playwright-artifacts-PxbrYk`. Both get pushed into a
# `tempDirectories` list the driver removes (`removeFolders(tempDirectories)`,
# same file) once it sees the Chromium process it spawned actually exit --
# which is what `_close_browser`'s graceful path below relies on, and why it
# does not need to know either path itself: Playwright already does.
#
# A process that never gets a chance to run that code -- `eidolon ext stop
# browser`, or a real crash -- leaves both behind: nothing under
# `tempfile.gettempdir()` ever deletes them on its own. That is the leak
# `_sweep_orphaned_profiles` closes, on the next process that starts.
PROFILE_PREFIX = "playwright_chromiumdev_profile-"
ARTIFACTS_PREFIX = "playwright-artifacts-"

# How old a candidate must be before the sweep will even consider it.
# Generous on purpose: this only has to outlast "another process on this
# box just created this a moment ago and has not opened its lock file yet"
# (observed on this box: well under a second, every time), and the orphans
# this exists for are the opposite kind -- minutes to hours old, from a
# session that is long gone (docs/Decisions.md, 2026-09-19, "A cleanup
# claim is a testable assertion": the 13 that outlived that day's earlier
# work were timestamped 05:24-05:53, found well after).
SWEEP_MIN_AGE_S = 60.0


async def _close_browser() -> None:
    """Best-effort graceful shutdown: close what `_ensure_page` opened, the
    same sequence test_server.py's own real-browser test classes already
    use in `asyncTearDown`. Closing `STATE.browser` asks Chromium to exit
    over CDP; the Playwright driver's own process-exit handling removes the
    temp directories it made for this launch once it sees Chromium actually
    exit (see the section comment above) -- so this function does not track
    or remove any path itself, on purpose: Playwright already knows what it
    made, and closing is what lets it act on that.

    Reachability, stated plainly rather than assumed: this only runs if
    something gives this process a chance to run its own code before it
    dies -- the ASGI shutdown below (`_lifespan`, wired to uvicorn's own
    SIGINT/SIGTERM handling), or a direct call, as the tests do. It is
    NOT what cleans up after `eidolon ext stop browser`: `ext::stop` in
    crates/rune/src/ext.rs calls `eidolon_tools::shell::kill_tree`, which
    (crates/tools/src/shell.rs, `kill_group`) sends the whole process group
    `SIGKILL` on POSIX or runs `taskkill /T /F` on Windows -- both terminate
    every process in this service's tree, this one included, before any of
    them can run a line of their own shutdown code, every time, not as an
    occasional race. That path leaves a real orphan by design of the kill,
    not by a bug here, which is exactly why `_sweep_orphaned_profiles`
    exists as the half that depends on nobody getting a graceful chance to
    run at all. This function still earns its place: a developer's Ctrl+C
    against the manual smoke test this module's own docstring shows, a bare
    `SIGTERM` from anything else that might one day stop this process, and
    every test below that calls it directly, all reach it.
    """
    if STATE.browser is not None:
        try:
            await asyncio.wait_for(STATE.browser.close(), timeout=10)
        except Exception:
            pass  # a close that fails must not be why shutdown hangs
    if STATE.playwright is not None:
        try:
            await asyncio.wait_for(STATE.playwright.stop(), timeout=10)
        except Exception:
            pass
    STATE.browser = None
    STATE.context = None
    STATE.page = None
    STATE.playwright = None
    STATE.known_refs = frozenset()
    # Confinement is a property of the context this function just closed;
    # nothing downstream should see a stale `confined`/`last_blocked` from
    # a session that no longer has a browser at all.
    STATE.confined = None
    STATE.last_blocked = None


@asynccontextmanager
async def _lifespan(_app: FastAPI):
    """The modern replacement for `@app.on_event("shutdown")`, which this
    installed FastAPI (0.141.1) already marks deprecated in favour of this
    shape. Nothing runs before the `yield`: this service's one piece of
    startup work that must not block a health probe is the sweep below,
    which runs from `__main__`, before `uvicorn.run`, not from here."""
    yield
    await _close_browser()


def _pid_alive_posix(pid: int) -> bool:
    """`os.kill(pid, 0)` sends no signal -- it only asks whether `pid`
    exists and is reachable. Reasoned from Python's documented POSIX
    semantics, not observed: this venv runs on Windows (see the module
    docstring), where `os.kill` does not carry the same meaning, so this
    function is only ever reached with `windows=False` (see
    `_profile_locked`) and is exercised in this suite with `os.kill` itself
    mocked, never against a real POSIX process. `ProcessLookupError` is the
    one unambiguous "gone" answer; anything else -- it exists but is not
    ours to signal (`PermissionError`), or a check that could not complete
    -- is read as alive, the direction that leaves a directory rather than
    risks deleting one still in use."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError:
        return True
    else:
        return True


def _profile_locked(profile_dir: Path, *, windows: bool) -> bool:
    """Is a live Chromium still holding this profile directory? Mirrors
    Playwright's own `isProfileLocked` -- the function its own MCP server
    uses to decide whether a profile is already in use before reusing it --
    in logic, not just in spirit (driver/package/lib/coreBundle.js, compiled
    from packages/playwright-core/src/tools/mcp/browserFactory.ts, present
    in this exact installed version):

        function isProfileLocked(userDataDir) {
          const lockFile = process.platform === "win32" ? "lockfile" : "SingletonLock";
          ...
        }

    Windows (`lockfile`): confirmed live on this box, twice, against a real
    launch -- opening it while Chromium holds it raises `PermissionError:
    [WinError 32] The process cannot access the file because it is being
    used by another process`; opening it after a graceful close succeeds.
    So: try to open the file for read+write and close it again.
    `FileNotFoundError` means the profile never got far enough to be locked
    at all -- not locked. Any other failure is Windows' own mandatory file
    locking refusing us, which only happens while something else has the
    file open -- locked. No error means we just opened and closed it
    ourselves: nothing else had it -- not locked.

    POSIX (`SingletonLock`): reasoned from Playwright's own source quoted
    above, not observed against a real launch -- this box is Windows.
    Chromium leaves this as a symlink to `<hostname>-<pid>`, not a real file
    with content to open. Unreadable (missing, or not a symlink) means not
    locked. Readable means checking whether `<pid>` -- the text after the
    last `-`, matching Playwright's own `target.split("-").pop()` -- still
    exists, via `_pid_alive_posix`. The hostname half of the target is not
    checked separately: a dead pid is safe to sweep regardless of whose
    hostname is on the link, and a live one is skipped on pid alone, so the
    hostname never changes the answer either way.
    """
    lock_name = "lockfile" if windows else "SingletonLock"
    lock_path = profile_dir / lock_name
    if windows:
        try:
            fd = os.open(str(lock_path), os.O_RDWR)
        except FileNotFoundError:
            return False
        except OSError:
            return True
        else:
            os.close(fd)
            return False
    try:
        target = os.readlink(lock_path)
    except OSError:
        return False
    pid_str = target.rsplit("-", 1)[-1]
    if not pid_str.isdigit():
        return False
    return _pid_alive_posix(int(pid_str))


def _sweep_orphaned_profiles(
    *, windows: bool, tmp_dir: Path, min_age_s: float = SWEEP_MIN_AGE_S
) -> list[str]:
    """Run once, at process start (see `__main__` below), before this
    process's own `_ensure_page` has created anything -- so every candidate
    this finds necessarily belongs to some OTHER, past or still-running,
    launch, never this one. Returns the names it removed, for the startup
    log line.

    Sweepable means: named like one of ours (`PROFILE_PREFIX` /
    `ARTIFACTS_PREFIX` -- see the section comment above for where those
    names come from), old enough that a launch racing this sweep elsewhere
    on this box could not still be creating it (`min_age_s`), and -- for a
    profile directory specifically -- not `_profile_locked`. An artifacts
    directory carries no lock of its own (Chromium does not know it exists;
    only Playwright's driver does, for downloads/traces/HAR/video this
    service never asks for -- `_ensure_page` passes none of those options),
    so age is the only test available for one; this service never writes
    into it either way, so sweeping a live-but-untouched one costs nothing
    real.

    What could still go wrong, and why it is accepted: `PROFILE_PREFIX` /
    `ARTIFACTS_PREFIX` are Playwright's own generic names, not namespaced to
    eidolon -- an unrelated Playwright-based tool on the same box, using the
    same channel, would be indistinguishable from this service by name
    alone, and an old-enough directory of its, unlocked at the moment this
    checks, would be removed. Accepted because there is no stronger identity
    available through the public, non-persistent `launch()` API this
    service deliberately uses (no `user_data_dir` parameter exists on it --
    only `launch_persistent_context` takes one, and switching to it would
    mean Chromium writing cookies and local storage to disk for the whole
    life of every session, a real change to what this module's own
    `_ensure_page` comment promises about session data never touching disk
    -- out of scope for a directory-cleanup fix, and a worse trade than the
    one made here). The failure mode this leans away from is the opposite
    one, and the one that actually matters: `_profile_locked` and
    `_pid_alive_posix` both err toward "leave it" on anything they cannot
    positively prove dead, because the cost of a missed orphan is a retry
    at the next start and the cost of a wrong deletion is a live browser's
    profile pulled out from under it.
    """
    removed: list[str] = []
    try:
        entries = list(tmp_dir.iterdir())
    except OSError:
        return removed
    now = time.time()
    for entry in entries:
        name = entry.name
        is_profile = name.startswith(PROFILE_PREFIX)
        is_artifacts = name.startswith(ARTIFACTS_PREFIX)
        if not (is_profile or is_artifacts) or not entry.is_dir():
            continue
        try:
            age = now - entry.stat().st_mtime
        except OSError:
            continue
        if age < min_age_s:
            continue
        if is_profile and _profile_locked(entry, windows=windows):
            continue
        shutil.rmtree(entry, ignore_errors=True)
        # `ignore_errors=True` is deliberate -- one file a slower-to-exit
        # helper process (or, in principle, an unrelated antivirus/backup
        # scan) still has open must not abort the whole sweep over one
        # candidate -- but it also means `rmtree` can partially succeed and
        # say nothing about it either way. Checked here rather than trusted:
        # only a candidate actually gone is reported as removed, so a
        # partial failure is retried at the next start instead of being
        # counted as done when it was not (observed directly while writing
        # this suite's crash test: a freshly-dead tree's `lockfile` can
        # release a moment before every one of its other files does, and
        # trusting `rmtree` unconditionally here once reported a directory
        # as removed while a piece of it was still on disk).
        if not entry.exists():
            removed.append(name)
    return removed


app = FastAPI(title="browser extension service", lifespan=_lifespan)


@app.get("/health")
async def health():
    """2xx means only "this process is up", never "a browser is running".
    Chromium launches lazily on first real use (`_ensure_page`) specifically
    so this never has to be paid by a probe that only wants to know the
    process is alive -- the host's adoption probe gets a 2 second budget
    (eidolon/docs/extensions.md), and a cold Chromium launch is easily an
    order of magnitude past that. This handler never awaits `LOCK` and never
    touches `STATE.page`, so a slow navigation in flight on `/call` cannot
    make this one wait for it either."""
    return {"status": "ok", "chromium_installed": _chromium_installed()}


@app.post("/call")
async def call(request: Request):
    # Checked before anything else runs, including reading the body -- same
    # ordering as eidolon/docs/extensions.md's reference service and
    # jev/server.py's own fix. `hmac.compare_digest` over `bytes`, not
    # `str`: a plain `!=` short-circuits on the first differing byte, which
    # leaks how much of a guessed token was right, and `compare_digest`
    # itself raises `TypeError` on a non-ASCII `str` -- comparing the raw
    # latin-1 bytes both headers arrive as (Starlette decodes every header
    # with `latin-1`, a decode that is total over byte values 0-255) turns
    # what would otherwise be a stray-byte 500 into an ordinary 401.
    supplied = (request.headers.get("authorization") or "").encode("latin-1")
    expected = f"Bearer {TOKEN}".encode("latin-1")
    if not hmac.compare_digest(supplied, expected):
        return JSONResponse({"ok": False, "error": "bad token"}, status_code=401)
    try:
        body = await request.json()
        if not isinstance(body, dict):
            raise ValueError("the body must be a JSON object")
    except Exception:
        return JSONResponse({"ok": False, "error": "invalid JSON body"}, status_code=400)
    method = body.get("method")
    args = body.get("args") or {}
    fn = METHODS.get(method)
    if fn is None:
        return {"ok": False, "error": f"unknown method {method!r}"}
    try:
        # Awaited directly on this event loop, not `asyncio.to_thread` --
        # unlike jev's CPU-bound torch calls, Playwright's async API is
        # bound to the loop that started it (see `Browser`'s docstring), so
        # offloading to a worker thread would hand it to the wrong one.
        # `LOCK` is what actually serializes calls onto one page: held for
        # the whole method, not just the browser's own lazy launch, so a
        # `browser_snapshot` can never interleave with the
        # `browser_click`/`browser_type` that would invalidate the refs it
        # is busy reading.
        async with LOCK:
            result = await fn(args)
    except Exception as e:  # a bad ref or a Playwright failure should say why, not 500 silently
        return {"ok": False, "error": f"{type(e).__name__}: {e}"}
    return {"ok": True, "result": result}


if __name__ == "__main__":
    # The crash-sweep half of the cleanup contract: see
    # `_sweep_orphaned_profiles` for what makes a directory sweepable. Runs
    # once, here, before this process's own first launch -- never from
    # `/health` or anywhere request-shaped, so a slow %TEMP% (a box with a
    # lot on it, which this one has had) is never on the hook for a health
    # probe's 2-second budget. Best-effort like everything else at startup:
    # a broken sweep must not be why the service itself fails to come up.
    try:
        swept = _sweep_orphaned_profiles(
            windows=(sys.platform == "win32"), tmp_dir=Path(tempfile.gettempdir())
        )
    except Exception as e:
        swept = []
        print(f"profile sweep failed, continuing anyway: {type(e).__name__}: {e}")
    if swept:
        print(f"swept {len(swept)} orphaned Playwright temp dir(s) from an earlier crash: {', '.join(swept)}")
    print(f"browser extension service on http://127.0.0.1:{PORT} (chromium {'[#]' if _chromium_installed() else '[ ]'})")
    uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")
