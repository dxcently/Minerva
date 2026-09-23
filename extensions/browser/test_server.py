"""Tests for extensions/browser/service.py -- the eidolon extension service.

Run with this extension's own venv (see service.py's module docstring for
how it is built), plus the test-only dependency TestClient needs on this
Starlette version:

    C:\\Users\\dxcen\\Projects\\bonsai2\\extensions\\browser\\.venv\\Scripts\\python -m pip install -r requirements-test.txt
    C:\\Users\\dxcen\\Projects\\bonsai2\\extensions\\browser\\.venv\\Scripts\\python -m unittest test_server -v

`unittest`, not pytest -- there is no pytest on this box, same as jev's own
suite beside this one.

Two shapes of test here, for a reason found while writing this suite and
worth recording. FastAPI's `TestClient` gives each `.post()`/`.get()` call
its own ASGI transport lifetime, which jev's tests never notice because a
torch forward pass is plain sync code with no opinion about which loop calls
it -- but Playwright's async API genuinely is bound to the loop that started
it (see service.py's `Browser` docstring), so a real page opened on one
`TestClient` call turned out to be a dead connection by the next one
(confirmed directly against this service before this file existed:
`Page.aria_snapshot: 'NoneType' object has no attribute 'send'`). So: auth,
health and argument validation -- nothing that needs a page to survive past
one call -- go through `TestClient`, exactly like jev's suite. The one real,
multi-step browser walk (open, snapshot, click a ref, watch a stale one
fail, type, read, back) is a single `IsolatedAsyncioTestCase` method calling
`service._open` / `_snapshot` / ... directly, awaited in sequence inside one
coroutine -- one event loop by construction, never a second one.

That walk launches a real headless Chromium against a `data:` URL -- no
network dependency, no fixture server, and fast enough on this box (well
under a second cold, per the extension's report) that faking it would stop
testing the thing this service exists to do, the same reasoning jev gives
for running `choose` against its real checkpoint. It is skipped, not
failed, when this venv has no chromium installed yet, on the same check
`/health` itself reports.
"""
from __future__ import annotations

import asyncio
import ctypes
import hmac
import http.server
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

# service.py reads its port and token at import time -- both must be set
# before the only `import service` below. Forced, not `setdefault`, for the
# same reason as jev/test_server.py: a developer's own
# `$env:EIDOLON_SERVICE_TOKEN` from a manual smoke test (service.py's own
# docstring shows one) outlives that one command in the same shell, and must
# not leak into what this suite authenticates with.
os.environ.setdefault("EIDOLON_SERVICE_PORT", "8091")
os.environ["EIDOLON_SERVICE_TOKEN"] = "test-token"

import service  # noqa: E402  (must follow the environ mutations above)
from fastapi.testclient import TestClient  # noqa: E402

TOKEN = os.environ["EIDOLON_SERVICE_TOKEN"]
AUTH = {"authorization": f"Bearer {TOKEN}"}

# A self-contained fixture page, loaded by `data:` URL so this suite depends
# on neither the network nor a second process serving a file. One of each
# role docs/design/automation.md's "refs" source cares about: a link outside
# `main` (banner), a heading, a link and a button inside `main`, and a text
# field to exercise browser_type.
PAGE = (
    "data:text/html,"
    "<html><head><title>Ref Test</title></head><body>"
    "<header><a href='/home'>Home</a></header>"
    "<main><h1>Welcome</h1>"
    "<p><a href='https://example.com/next'>the next link</a></p>"
    "<button>Press me</button>"
    "<input type='text' placeholder='say something'>"
    "</main></body></html>"
)

# E5's fixture: one of each thing `roles`/`within` together must tell apart
# -- a link outside `main` entirely (Chrome, in `banner`), a link directly
# under `main` (Direct, `main` is its nearest landmark), a non-link element
# of the right container but wrong role (Press, a `button`), and a link
# that IS a descendant of `main` but is NOT nearest to it (Nested, wrapped
# in its own `nav`) -- the exact distinction docs/Decisions.md's "why no
# `depth` saves wiki-hop" names as the one a size knob cannot make: 2,741
# raw `link` records under a live `within: "main"` Cat capture, only 97
# with `landmark == "main"` directly.
ROLES_PAGE = (
    "data:text/html,"
    "<html><head><title>Roles</title></head><body>"
    "<header><a href='/home'>Chrome</a></header>"
    "<main><h1>T</h1>"
    "<a href='/direct'>Direct</a>"
    "<button>Press</button>"
    "<nav><a href='/nested'>Nested</a></nav>"
    "</main></body></html>"
)

# docs/design/judgement.md section 4's shape, in miniature: two landmarks
# inside `main`, each opened by a heading -- "Etymology" holding one link
# that a `section: "See also"` scope must exclude, "See also" holding two
# links it must keep plus a nested `navigation` ("Portals", real on the
# live article) whose own links it must also exclude, because their
# nearest landmark is the navigation, not the section. `role='region'` is
# explicit rather than relying on `<section>`'s own name-dependent implicit
# mapping, so this fixture cannot go dark in the accessibility tree over an
# unrelated HTML-AAM naming change.
SECTIONS_PAGE = (
    "data:text/html,"
    "<html><head><title>Sections</title></head><body>"
    "<main><h1>T</h1>"
    "<div role='region'><h2>Etymology</h2><a href='/word'>Word</a></div>"
    "<div role='region'><h2>See also</h2>"
    "<a href='/domestication'>Domestication</a>"
    "<a href='/felidae'>Felidae</a>"
    "<nav><a href='/portal1'>Portal1</a><a href='/portal2'>Portal2</a></nav>"
    "</div>"
    "</main></body></html>"
)


class HealthTests(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(service.app)

    def test_health_is_2xx_and_says_whats_on_disk(self):
        r = self.client.get("/health")
        self.assertEqual(r.status_code, 200)
        body = r.json()
        self.assertEqual(body["status"], "ok")
        self.assertIn("chromium_installed", body)

    def test_health_never_launches_a_browser(self):
        # Order-independent on purpose: some other test in this module may
        # already have launched a real page (see the module docstring), so
        # this checks that /health does not CHANGE that state either way,
        # rather than asserting it is None.
        before = service.STATE.page
        self.client.get("/health")
        self.assertIs(service.STATE.page, before)


class AuthTests(unittest.TestCase):
    """Same shape as jev/test_server.py's `AuthTests` -- this service copies
    jev's fixed auth check line for line (see service.py's `call`), and
    these are the tests that are the reason to believe that copy is exact.
    """

    def setUp(self):
        self.client = TestClient(service.app)

    def test_missing_token_is_401_and_runs_nothing(self):
        spy = mock.Mock()
        with mock.patch.dict(service.METHODS, {"read": spy}):
            r = self.client.post("/call", json={"method": "read", "args": {}})
        self.assertEqual(r.status_code, 401)
        self.assertEqual(r.json(), {"ok": False, "error": "bad token"})
        spy.assert_not_called()

    def test_wrong_token_is_401_and_runs_nothing(self):
        spy = mock.Mock()
        with mock.patch.dict(service.METHODS, {"read": spy}):
            r = self.client.post(
                "/call",
                json={"method": "read", "args": {}},
                headers={"authorization": "Bearer not-the-token"},
            )
        self.assertEqual(r.status_code, 401)
        spy.assert_not_called()

    def test_non_ascii_wrong_token_is_401_and_runs_nothing(self):
        # Pins the exact regression jev/server.py's own fix closes:
        # `hmac.compare_digest` raises `TypeError` on a non-ASCII `str`,
        # which would turn a guessed token with one stray high byte into an
        # unhandled 500 instead of an ordinary 401. httpx2 refuses to encode
        # a non-ASCII `str` header client-side, so the value goes in as
        # `bytes` here -- the same latin-1 bytes a real client puts on the
        # wire either way.
        spy = mock.Mock()
        with mock.patch.dict(service.METHODS, {"read": spy}):
            r = self.client.post(
                "/call",
                json={"method": "read", "args": {}},
                headers={"authorization": "Bearer café-guess-xyz".encode("latin-1")},
            )
        self.assertEqual(r.status_code, 401)
        self.assertEqual(r.json(), {"ok": False, "error": "bad token"})
        spy.assert_not_called()

    def test_raw_high_byte_token_is_401_and_runs_nothing(self):
        # A byte that was never meant to decode as text at all, not just
        # "non-ASCII text" -- the harsher half of the same regression.
        spy = mock.Mock()
        with mock.patch.dict(service.METHODS, {"read": spy}):
            r = self.client.post(
                "/call",
                json={"method": "read", "args": {}},
                headers={"authorization": b"Bearer \xff"},
            )
        self.assertEqual(r.status_code, 401)
        self.assertEqual(r.json(), {"ok": False, "error": "bad token"})
        spy.assert_not_called()

    def test_invalid_json_body_is_a_clean_400_not_a_500(self):
        r = self.client.post(
            "/call",
            headers={**AUTH, "content-type": "application/json"},
            content=b"{not json",
        )
        self.assertEqual(r.status_code, 400)
        self.assertFalse(r.json()["ok"])

    def test_unknown_method_is_an_ok_false_not_a_500(self):
        r = self.client.post("/call", headers=AUTH, json={"method": "nope", "args": {}})
        self.assertEqual(r.status_code, 200)
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("nope", body["error"])

    def test_a_json_body_that_is_not_an_object_is_a_clean_400(self):
        for payload in ([1, 2, 3], "just a string", 42, None):
            with self.subTest(payload=payload):
                r = self.client.post("/call", headers=AUTH, json=payload)
                self.assertEqual(r.status_code, 400)
                self.assertFalse(r.json()["ok"])

    def test_the_token_check_goes_through_compare_digest(self):
        # Pins the *code path*, not timing (too noisy to measure reliably
        # here): a future edit back to a plain `!=` fails this test instead
        # of only a security review.
        real = hmac.compare_digest
        calls = []

        def spy(a, b):
            calls.append((a, b))
            return real(a, b)

        with mock.patch("service.hmac.compare_digest", spy):
            self.client.post("/call", headers=AUTH, json={"method": "nope", "args": {}})
        self.assertEqual(len(calls), 1)


class ValidationTests(unittest.TestCase):
    """Argument checks that raise before `_ensure_page()` runs, in every
    method that touches a ref or a URL -- verified here by checking each
    fails over HTTP without needing a live page.

    That is NOT the same as proving the ordering itself (see service.py's
    `_click`/`_type` comments on why the check comes first), and an earlier
    version of this docstring wrongly claimed it did: not needing a live
    page for the *test* to pass says nothing about whether the *code under
    test* started one anyway before raising. Confirmed directly, as part of
    this review's break-it-to-prove-it pass: with `_ensure_page()` and
    `_resolve_ref()` swapped in `_click`, every test in this module
    -- these included -- still passed. `ClickCostOrderingTests` below is
    what actually proves the ordering; these three only prove which
    argument each method requires and what the error says about it."""

    def setUp(self):
        self.client = TestClient(service.app)

    def test_open_needs_a_url(self):
        r = self.client.post("/call", headers=AUTH, json={"method": "open", "args": {}})
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("url", body["error"])

    def test_click_needs_a_ref(self):
        r = self.client.post("/call", headers=AUTH, json={"method": "click", "args": {}})
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("ref", body["error"])

    def test_type_needs_text(self):
        r = self.client.post(
            "/call", headers=AUTH, json={"method": "type", "args": {"ref": "e1"}}
        )
        body = r.json()
        self.assertFalse(body["ok"])
        self.assertIn("text", body["error"])


class ClickCostOrderingTests(unittest.IsolatedAsyncioTestCase):
    """`_click` and `_type` both promise, in their own comments, that a
    rejected ref costs nothing and is in particular never the reason this
    service pays for its first Chromium launch. Found, while reviewing this
    file, to be an untested promise: `ValidationTests` checks the error
    message a bad ref produces, not what ran before it was raised, and the
    full suite stayed green with the two calls swapped (see the
    break-it-to-prove-it note now on that class). These patch
    `_ensure_page` itself and assert it is never called -- a signal the
    ordering's absence cannot produce by accident, unlike a shared error
    string two different orderings would both raise."""

    async def test_a_rejected_click_ref_never_launches_the_browser(self):
        with mock.patch.object(service.STATE, "known_refs", frozenset()):
            ensure_page = mock.AsyncMock()
            with mock.patch("service._ensure_page", ensure_page):
                with self.assertRaises(ValueError):
                    await service._click({"ref": "e1"})
            ensure_page.assert_not_called()

    async def test_a_rejected_type_ref_never_launches_the_browser(self):
        with mock.patch.object(service.STATE, "known_refs", frozenset()):
            ensure_page = mock.AsyncMock()
            with mock.patch("service._ensure_page", ensure_page):
                with self.assertRaises(ValueError):
                    await service._type({"ref": "e1", "text": "hi"})
            ensure_page.assert_not_called()


class SnapshotArgsTests(unittest.IsolatedAsyncioTestCase):
    """What `_snapshot` asks Playwright's `Page.aria_snapshot` for, not what
    a page replies -- so a fake page that only records its call is enough,
    and these do not need a real browser or a slow page to prove anything.

    Regression tests for a real defect this review found by running the
    service against a real page: `_snapshot` used to call
    `page.aria_snapshot(mode="ai")` with no `timeout` at all, so Playwright's
    own bare 30s default was the only ceiling there ever was and nothing
    could ask for more. `https://en.wikipedia.org/wiki/Felis` -- ordinary
    Wikipedia content, nothing adversarial -- exceeded even 170s on this box
    (docs/design/automation.md's own worked example clicks through to this
    exact page). `depth` is the other half: unbounded `mode="ai"` is what
    hung; the same mode with any depth limit, even 2, returned in well under
    a second against the same live page."""

    async def _snapshot_against_fake_page(self, args: dict):
        page = mock.AsyncMock()
        page.aria_snapshot.return_value = "url: https://example.com\ntitle: Example\n\n"
        page.url = "https://example.com"
        page.title = mock.AsyncMock(return_value="Example")
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            await service._snapshot(args)
        return page.aria_snapshot

    async def test_the_default_timeout_is_more_generous_than_playwrights_bare_default(self):
        aria_snapshot = await self._snapshot_against_fake_page({})
        aria_snapshot.assert_awaited_once()
        _, kwargs = aria_snapshot.call_args
        self.assertEqual(kwargs["mode"], "ai")
        self.assertEqual(kwargs["timeout"], service.SNAPSHOT_TIMEOUT_MS)
        self.assertGreater(service.SNAPSHOT_TIMEOUT_MS, 30_000, "no more generous than Playwright's own default")
        self.assertIsNone(kwargs.get("depth"), "depth must stay unset by default -- it changes what a graph can see")

    async def test_a_caller_can_ask_for_more_time_and_a_shallower_tree(self):
        aria_snapshot = await self._snapshot_against_fake_page({"timeout_s": 120, "depth": 5})
        _, kwargs = aria_snapshot.call_args
        self.assertEqual(kwargs["timeout"], 120_000)
        self.assertEqual(kwargs["depth"], 5)

    async def test_within_reaches_get_by_role_and_the_locators_own_aria_snapshot_runs(self):
        # docs/design/observation.md, "Where an observation is narrowed":
        # `within` must ask the *locator's* aria_snapshot, not the page's,
        # or the tree sent back was never actually narrowed. `page` is
        # AsyncMock, whose auto-created attributes are themselves
        # AsyncMock -- wrong for `get_by_role`, a synchronous
        # locator-constructor in the real API (confirmed against this
        # venv's installed Playwright: `Locator`/`Page.get_by_role` carries
        # no `async` in its signature, unlike `count`/`aria_snapshot`), so
        # it is overridden with a plain `Mock` here rather than left to the
        # parent's auto-speccing.
        page = mock.AsyncMock()
        target = mock.AsyncMock()
        target.count = mock.AsyncMock(return_value=1)
        target.aria_snapshot = mock.AsyncMock(return_value="- main [ref=e1]:\n  - text: hi\n")
        page.get_by_role = mock.Mock(return_value=target)
        page.url = "https://example.com"
        page.title = mock.AsyncMock(return_value="Example")
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            result = await service._snapshot({"within": "main"})
        page.get_by_role.assert_called_once_with("main")
        target.count.assert_awaited_once()
        target.aria_snapshot.assert_awaited_once()
        _, kwargs = target.aria_snapshot.call_args
        self.assertEqual(kwargs["mode"], "ai")
        page.aria_snapshot.assert_not_awaited()
        self.assertIn("scope: main", result)

    async def test_no_within_means_the_pages_own_aria_snapshot_runs_and_get_by_role_is_untouched(self):
        page = mock.AsyncMock()
        page.aria_snapshot.return_value = "url: https://example.com\ntitle: Example\n\n"
        page.url = "https://example.com"
        page.title = mock.AsyncMock(return_value="Example")
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            result = await service._snapshot({})
        page.aria_snapshot.assert_awaited_once()
        page.get_by_role.assert_not_called()
        self.assertNotIn("scope:", result)

    async def test_within_must_be_a_non_empty_string(self):
        # Same shape as `_type`'s `if not isinstance(text, str)` guard: a
        # scope has to be a role name Playwright's `get_by_role` can use,
        # not whatever JSON happened to arrive in that slot. Raised before
        # `get_by_role` is ever called, so the bare fake page from
        # `_snapshot_against_fake_page` (no `get_by_role` mock configured)
        # is enough.
        for bad in (123, "", []):
            with self.subTest(within=bad):
                with self.assertRaises(ValueError):
                    await self._snapshot_against_fake_page({"within": bad})

    async def test_roles_must_be_a_non_empty_list_of_role_names(self):
        # E5: docs/design/observation.md's "narrow at the source" extended
        # past `within`. Validated before either Playwright call, same
        # reasoning as `within` just above -- a bad `roles` argument must
        # not be the reason this service pays for a snapshot it is about
        # to refuse anyway (confirmed by `test_a_bad_roles_argument_never_
        # reaches_aria_snapshot` below, the ordering proof this class's own
        # docstring says a message-only check like this one cannot give).
        # A whitespace-only entry (`" "`) is deliberately not in this list:
        # `within`'s own check just above accepts one for the same reason
        # (`not within` reads a lone space as truthy, same as `not r`
        # would here), so rejecting it only for `roles` would be a new,
        # narrower standard invented for this argument alone rather than
        # the one this file already has.
        for bad in ("link", [], [1], [""], 123, {"link"}):
            with self.subTest(roles=bad):
                with self.assertRaises(ValueError):
                    await self._snapshot_against_fake_page({"roles": bad})

    async def test_max_must_be_a_positive_integer(self):
        for bad in (0, -1, 1.5, "64", True, False):
            with self.subTest(max=bad):
                with self.assertRaises(ValueError):
                    await self._snapshot_against_fake_page({"roles": ["link"], "max": bad})

    async def test_max_without_roles_is_refused(self):
        # `max` caps the list `roles` produces; asking for a cap on a whole
        # tree names a request that does not exist rather than silently
        # doing nothing with the extra argument.
        with self.assertRaises(ValueError) as cm:
            await self._snapshot_against_fake_page({"max": 5})
        self.assertIn("roles", str(cm.exception))

    async def test_section_requires_roles_and_within(self):
        # docs/design/judgement.md section 4: `section` narrows a
        # role-filtered list to one of `within`'s own nested landmarks, so
        # it needs both -- the same "requires" shape as `max` needing
        # `roles` just above, checked in the same place and just as early.
        with self.assertRaises(ValueError) as cm:
            await self._snapshot_against_fake_page({"section": "See also"})
        self.assertIn("roles", str(cm.exception))

        with self.assertRaises(ValueError) as cm2:
            await self._snapshot_against_fake_page({"roles": ["link"], "section": "See also"})
        self.assertIn("within", str(cm2.exception))

    async def test_section_must_be_a_non_empty_string(self):
        for bad in (123, "", [], ["See also"], True):
            with self.subTest(section=bad):
                with self.assertRaises(ValueError):
                    await self._snapshot_against_fake_page(
                        {"within": "main", "roles": ["link"], "section": bad}
                    )

    async def test_a_bad_roles_argument_never_reaches_aria_snapshot(self):
        # Same ordering proof `ClickCostOrderingTests` gives for `_click`/
        # `_type`'s ref check, applied here: a bare fake page with no
        # `aria_snapshot` configured is not enough to show the *order* of
        # events, only that this call raises. Patching `page.aria_snapshot`
        # itself and asserting it is never awaited is what actually proves
        # validation runs first.
        page = mock.AsyncMock()
        page.get_by_role = mock.Mock()
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            with self.assertRaises(ValueError):
                await service._snapshot({"roles": []})
        page.aria_snapshot.assert_not_awaited()

    async def test_roles_and_max_reach_the_projection_and_the_head_names_them(self):
        # What `_filter_roles` itself does with `tree`, `within` and `max`
        # is `RoleProjectionUnitTests`' job (fast, no browser); this proves
        # only that `_snapshot` actually calls it with what the caller
        # asked for, rather than validating `roles`/`max` and then
        # forgetting to use them. `section` and `url` are asserted `None`/
        # `page.url` here too -- not because this test is about `section`
        # (see `test_section_reaches_the_projection_and_the_head_names_it`
        # below for that), but because `_snapshot` always passes both, and
        # an exact-args assertion that only checked a prefix would stop
        # proving `_snapshot` forwards what it was actually given the
        # moment a sixth argument joined the other five.
        page = mock.AsyncMock()
        page.aria_snapshot.return_value = "- main [ref=e1]:\n  - link \"L\" [ref=e2]\n"
        page.url = "https://example.com"
        page.title = mock.AsyncMock(return_value="Example")
        filtered = mock.Mock(return_value="- main [ref=e1]:\n  - link \"L\" [ref=e2]\n")
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            with mock.patch("service._filter_roles", filtered):
                result = await service._snapshot({"roles": ["link"], "max": 5})
        filtered.assert_called_once_with(
            "- main [ref=e1]:\n  - link \"L\" [ref=e2]\n", frozenset({"link"}), None, 5, None, "https://example.com"
        )
        self.assertIn("roles: link", result)

    async def test_section_reaches_the_projection_and_the_head_names_it(self):
        # The `section` counterpart of the test just above: proves
        # `_snapshot` forwards `section` (and `within`, and `page.url`) to
        # `_filter_roles`, and stamps `section:` in the head -- not what
        # `_filter_roles` itself does with it, which is
        # `RoleProjectionUnitTests`' job. `within` is given (`section`
        # requires it), so `get_by_role` is overridden with a plain `Mock`
        # for the same reason
        # `test_within_reaches_get_by_role_and_the_locators_own_aria_snapshot_runs`
        # above does: it is a synchronous locator-constructor in the real
        # API, not the AsyncMock `page`'s auto-created attributes would be.
        page = mock.AsyncMock()
        target = mock.AsyncMock()
        target.count = mock.AsyncMock(return_value=1)
        target.aria_snapshot = mock.AsyncMock(
            return_value="- region [ref=e1]:\n  - heading \"See also\" [level=2]\n  - link \"L\" [ref=e2]\n"
        )
        page.get_by_role = mock.Mock(return_value=target)
        page.url = "https://example.com"
        page.title = mock.AsyncMock(return_value="Example")
        filtered = mock.Mock(return_value="- main [ref=e1]:\n  - link \"L\" [ref=e2]\n")
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            with mock.patch("service._filter_roles", filtered):
                result = await service._snapshot({"within": "main", "roles": ["link"], "section": "See also"})
        filtered.assert_called_once_with(
            "- region [ref=e1]:\n  - heading \"See also\" [level=2]\n  - link \"L\" [ref=e2]\n",
            frozenset({"link"}),
            "main",
            None,
            "See also",
            "https://example.com",
        )
        self.assertIn("section: See also", result)

    async def test_heading_is_read_before_the_roles_filter_projects_it_away(self):
        # The ordering this whole fix depends on: `_first_heading` must see
        # the tree Playwright actually returned, not whatever
        # `_filter_roles` projects it down to -- a `roles` filter that
        # excludes "heading" is exactly the case this exists for
        # (docs/Decisions.md, "A page's identity is not one of its
        # elements"). Mocking both and asserting what `_first_heading` was
        # actually called with -- not merely that it was called at all --
        # is what would catch a future refactor that quietly swapped the
        # two lines back to "filter first, then look for a heading in
        # whatever is left."
        page = mock.AsyncMock()
        unfiltered_tree = '- main [ref=e1]:\n  - heading "T" [level=1] [ref=e2]\n  - link "L" [ref=e3]\n'
        page.aria_snapshot.return_value = unfiltered_tree
        page.url = "https://example.com"
        page.title = mock.AsyncMock(return_value="Example")
        filtered_tree = '- main [ref=e1]:\n  - link "L" [ref=e3]\n'  # heading already projected away
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            with mock.patch("service._filter_roles", mock.Mock(return_value=filtered_tree)) as filtered:
                with mock.patch("service._first_heading", mock.Mock(return_value="T")) as heading:
                    result = await service._snapshot({"roles": ["link"]})
        heading.assert_called_once_with(unfiltered_tree)
        filtered.assert_called_once()
        self.assertIn("heading: T", result)

    async def test_no_roles_means_filter_roles_is_never_called(self):
        # The `roles is None` path must be a true no-op -- not "filter with
        # a wildcard roles set" reimplemented differently.
        page = mock.AsyncMock()
        page.aria_snapshot.return_value = "url: https://example.com\ntitle: Example\n\n"
        page.url = "https://example.com"
        page.title = mock.AsyncMock(return_value="Example")
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)):
            with mock.patch("service._filter_roles") as filtered:
                result = await service._snapshot({})
            filtered.assert_not_called()
        self.assertNotIn("roles:", result)


class RoleProjectionUnitTests(unittest.TestCase):
    """`_line_role`/`_filter_roles` -- the E5 mechanism itself -- against
    hand-built tree text, no browser needed: what a scoped-and-filtered
    call keeps, and why. `RoleFilteredSnapshotTests` below proves the same
    mechanism end to end, through `_snapshot`, against a real headless
    Chromium; these prove the projection in isolation, fast enough to run
    on every change to it, and precise enough to name exactly which line
    of a hand-built tree a given assertion is about."""

    def test_a_plain_role_line_needs_no_quoting_to_be_read(self):
        self.assertEqual(service._line_role('link "Home" [ref=e7]'), "link")
        self.assertEqual(service._line_role('heading "Cat" [level=1] [ref=e3]'), "heading")
        self.assertEqual(service._line_role('generic [ref=e5]:'), "generic")

    def test_a_property_line_has_no_role(self):
        # `/url: ...` -- a11y.py's own `_PROPERTY_RE` shape. Never a match
        # for `_ROLE_RE` (it does not start with a letter), so it is simply
        # not a role line -- the same way a11y.py's `parse_refs` treats it:
        # no record, but still a real line with a real stack effect.
        self.assertIsNone(service._line_role("/url: https://example.com"))

    def test_a_colon_bearing_name_is_read_through_the_real_quoted_wrap_shape(self):
        # A genuine line from jev/tests/fixtures/wiki/cat_citations_excerpt.txt
        # (block 2, read but not edited): a name containing a literal colon
        # forces Playwright to wrap the whole `role "name" [attrs]` head in
        # single quotes, with any literal `'` inside doubled. This is the
        # exact text, not a simplified stand-in -- if this ever misparses,
        # a real Wikipedia link shaped like it would silently vanish from
        # the option list rather than raising anything.
        line = '\'link "Toolbox: Ship\'\'s Cat on the Kalmar Nyckel" [ref=e5081] [cursor=pointer]\':'
        self.assertEqual(service._line_role(line), "link")

    def test_an_unparseable_line_has_no_role_and_does_not_raise(self):
        self.assertIsNone(service._line_role(": not a role at all"))
        self.assertIsNone(service._line_role("'unterminated"))

    def test_nearest_landmark_not_mere_descendance_is_what_within_keeps(self):
        # The core semantic this mechanism must reproduce
        # (docs/Decisions.md, 2026-09-19, "Why no depth saves wiki-hop"):
        # jev's own consumer-side filter keeps a link only when its
        # NEAREST landmark ancestor is `within`, not merely when it sits
        # somewhere under it. A link nested inside a `navigation` that is
        # itself inside `main` is a descendant of main but must NOT survive
        # `within: "main"`.
        tree = (
            '- main [ref=e1]:\n'
            '  - link "Direct" [ref=e2]\n'
            '  - navigation [ref=e3]:\n'
            '    - link "Nested" [ref=e4]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", None)
        self.assertIn('link "Direct" [ref=e2]', out)
        self.assertNotIn("Nested", out)
        self.assertTrue(out.startswith("- main [ref=e1]:\n"), out)

    def test_two_sibling_nested_landmarks_are_told_apart(self):
        # A stack pop using `>` instead of `>=` would leave the first
        # nested landmark's frame on the stack when a SECOND, sibling
        # landmark at the very same indent begins -- proved here by a link
        # that follows both: it must read `main` as its own nearest
        # landmark, not whichever of the two preceded it.
        tree = (
            '- main [ref=e1]:\n'
            '  - navigation [ref=e2]:\n'
            '    - link "InNav" [ref=e3]\n'
            '  - region [ref=e4]:\n'
            '    - link "InRegion" [ref=e5]\n'
            '  - link "Direct" [ref=e6]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", None)
        self.assertIn("Direct", out)
        self.assertNotIn("InNav", out)
        self.assertNotIn("InRegion", out)

    def test_roles_without_within_imposes_no_landmark_condition(self):
        tree = (
            '- banner [ref=e1]:\n'
            '  - link "Chrome" [ref=e2]\n'
            '- main [ref=e3]:\n'
            '  - link "Body" [ref=e4]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), None, None)
        # Exact, not just `assertIn`: with no scope to re-parent under,
        # this must be *only* the two matches, flush left -- no extra root
        # line, and no leftover indentation from wherever each one really
        # sat in the tree.
        self.assertEqual(out, '- link "Chrome" [ref=e2]\n- link "Body" [ref=e4]\n')

    def test_multiple_roles_are_all_kept(self):
        tree = (
            '- main [ref=e1]:\n'
            '  - link "L" [ref=e2]\n'
            '  - button "B" [ref=e3]\n'
            '  - heading "H" [level=1]\n'
        )
        out = service._filter_roles(tree, frozenset({"link", "button"}), "main", None)
        self.assertIn("L", out)
        self.assertIn("B", out)
        self.assertNotIn('"H"', out)

    def test_max_caps_at_the_first_n_in_document_order(self):
        tree = "- main [ref=e1]:\n" + "".join(
            f'  - link "L{i}" [ref=e{i + 2}]\n' for i in range(5)
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", 2)
        self.assertIn("L0", out)
        self.assertIn("L1", out)
        self.assertNotIn("L2", out)
        self.assertNotIn("L3", out)
        self.assertNotIn("L4", out)

    def test_max_is_a_cap_not_a_floor(self):
        # docs/Decisions.md, 2026-09-19, "Why no depth saves wiki-hop": a
        # previous brief had "enough real links -- the graph's max is 64"
        # read as a floor, and scored a genuinely working 58-option menu a
        # failure over a constraint nobody had actually set. Fewer real
        # matches than `max_n` must never be an error or padded here.
        tree = '- main [ref=e1]:\n  - link "Only" [ref=e2]\n'
        out = service._filter_roles(tree, frozenset({"link"}), "main", 64)
        self.assertEqual(out, '- main [ref=e1]:\n  - link "Only" [ref=e2]\n')

    def test_kept_lines_are_reproduced_byte_for_byte(self):
        tree = '- main [ref=e1]:\n  - link "Weird  spacing" [ref=e2] [cursor=pointer]\n'
        out = service._filter_roles(tree, frozenset({"link"}), "main", None)
        self.assertIn('link "Weird  spacing" [ref=e2] [cursor=pointer]', out)

    def test_no_match_under_a_scope_returns_just_the_scope_line(self):
        tree = '- main [ref=e1]:\n  - heading "T" [level=1]\n'
        out = service._filter_roles(tree, frozenset({"link"}), "main", None)
        self.assertEqual(out, '- main [ref=e1]:\n')

    def test_no_match_without_a_scope_returns_empty(self):
        tree = '- heading "T" [level=1]\n'
        out = service._filter_roles(tree, frozenset({"link"}), None, None)
        self.assertEqual(out, "")

    def test_an_empty_tree_is_returned_unchanged(self):
        self.assertEqual(service._filter_roles("", frozenset({"link"}), "main", None), "")

    def test_first_heading_finds_the_level_1_heading_by_name(self):
        tree = '- main [ref=e1]:\n  - heading "Cat" [level=1] [ref=e2]\n  - link "Felis" [ref=e3]\n'
        self.assertEqual(service._first_heading(tree), "Cat")

    def test_first_heading_ignores_headings_below_level_1(self):
        tree = '- heading "Section" [level=2] [ref=e1]\n'
        self.assertIsNone(service._first_heading(tree))

    def test_first_heading_is_none_when_there_is_no_heading_at_all(self):
        tree = '- link "A" [ref=e1]\n- button "Go" [ref=e2]\n'
        self.assertIsNone(service._first_heading(tree))

    def test_first_heading_returns_none_on_a_tree_a_roles_filter_already_excluded_it_from(self):
        # The bug this whole mechanism exists to close, reproduced
        # directly: once `roles` has already projected a tree down to just
        # links, scanning THAT tree for a heading finds nothing -- exactly
        # why `_snapshot` must call this on the tree Playwright returned,
        # before `_filter_roles` runs, never after.
        tree = '- main [ref=e1]:\n  - heading "Cat" [level=1] [ref=e2]\n  - link "Felis" [ref=e3]\n'
        already_filtered = service._filter_roles(tree, frozenset({"link"}), None, None)
        self.assertIsNone(service._first_heading(already_filtered))
        self.assertEqual(service._first_heading(tree), "Cat")  # the original tree still has it

    def test_first_heading_reads_a_colon_bearing_name_through_the_quoted_wrap_shape(self):
        # Same real shape as
        # test_a_colon_bearing_name_is_read_through_the_real_quoted_wrap_shape
        # above, applied to a heading instead of a link -- Playwright wraps
        # the whole `role "name" [attrs]` head in single quotes the same
        # way regardless of which role carries the colon-bearing name.
        line = "- 'heading \"Toolbox: Overview\" [level=1]': [ref=e9]\n"
        self.assertEqual(service._first_heading(line), "Toolbox: Overview")

    # --- `section` (docs/design/judgement.md section 4, "The landmark
    # predicate, and 'See also'") -----------------------------------------
    #
    # The Cat article's own shape, in miniature: `main` holds several
    # headed landmarks in document order; `section` must keep only the
    # one whose OWN first child is the named heading, exclude a nested
    # `navigation` inside it (the live "Portals" bar), and refuse by name
    # -- never a silent first match, never a silent empty result -- when
    # the heading is missing or ambiguous. `RoleFilteredSnapshotTests`
    # proves the same mechanism end to end against a real Chromium page;
    # these prove it in isolation, the same division of labour
    # `RoleProjectionUnitTests`' own class docstring already states for
    # `roles`/`within`.

    def test_a_section_keeps_only_links_whose_nearest_landmark_is_that_section(self):
        tree = (
            '- main [ref=e1]:\n'
            '  - region [ref=e2]:\n'
            '    - heading "Etymology" [level=2] [ref=e3]\n'
            '    - link "Cat word" [ref=e4]\n'
            '  - region [ref=e5]:\n'
            '    - heading "See also" [level=2] [ref=e6]\n'
            '    - link "Domestication" [ref=e7]\n'
            '    - link "Felidae" [ref=e8]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", None, "See also")
        self.assertNotIn("Cat word", out)
        self.assertIn("Domestication", out)
        self.assertIn("Felidae", out)

    def test_links_inside_a_navigation_nested_in_the_section_are_excluded(self):
        # The Portals shape, real on the live article (E5/judgement.md
        # section 4: 3 of "See also"'s 34 named links). The navigation's
        # nearest landmark is the section (it IS directly inside it), but
        # a link inside the navigation has the navigation itself as its
        # OWN nearest landmark -- the same "nearest, not mere descendance"
        # rule `within` already enforces, not a second mechanism.
        tree = (
            '- main [ref=e1]:\n'
            '  - region [ref=e2]:\n'
            '    - heading "See also" [level=2] [ref=e3]\n'
            '    - link "Domestication" [ref=e4]\n'
            '    - navigation [ref=e5]:\n'
            '      - link "Cats portal" [ref=e6]\n'
            '      - link "Animals portal" [ref=e7]\n'
            '  - region [ref=e8]:\n'
            '    - heading "Notes" [level=2] [ref=e9]\n'
            '    - link "Elsewhere" [ref=e10]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", None, "See also")
        self.assertIn("Domestication", out)
        self.assertNotIn("Cats portal", out)
        self.assertNotIn("Animals portal", out)
        self.assertNotIn("Elsewhere", out)  # a different section entirely

    def test_a_link_directly_under_within_is_excluded_even_though_it_equals_within(self):
        # The exclusion the docstring calls out by name: a line whose
        # nearest landmark is the IMPLICIT `within` (nothing between it and
        # the scope line -- lead/infobox territory on the real article)
        # would satisfy the plain `within` condition (`landmark == within`)
        # but must never satisfy `section`, because `within` itself is
        # never a section root.
        tree = (
            '- main [ref=e1]:\n'
            '  - link "Lead paragraph link" [ref=e2]\n'
            '  - region [ref=e3]:\n'
            '    - heading "See also" [level=2] [ref=e4]\n'
            '    - link "Domestication" [ref=e5]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", None, "See also")
        self.assertNotIn("Lead paragraph", out)
        self.assertIn("Domestication", out)

    def test_a_section_with_no_such_heading_is_refused_by_name(self):
        tree = (
            '- main [ref=e1]:\n'
            '  - region [ref=e2]:\n'
            '    - heading "Etymology" [level=2] [ref=e3]\n'
            '    - link "Cat word" [ref=e4]\n'
        )
        with self.assertRaises(ValueError) as cm:
            service._filter_roles(tree, frozenset({"link"}), "main", None, "See also")
        self.assertIn("See also", str(cm.exception))
        self.assertIn("no landmark", str(cm.exception))

    def test_two_sections_with_the_same_heading_are_refused_with_the_count(self):
        tree = (
            '- main [ref=e1]:\n'
            '  - region [ref=e2]:\n'
            '    - heading "Notes" [level=2] [ref=e3]\n'
            '    - link "First" [ref=e4]\n'
            '  - region [ref=e5]:\n'
            '    - heading "Notes" [level=2] [ref=e6]\n'
            '    - link "Second" [ref=e7]\n'
        )
        with self.assertRaises(ValueError) as cm:
            service._filter_roles(tree, frozenset({"link"}), "main", None, "Notes")
        self.assertIn("Notes", str(cm.exception))
        self.assertIn("2", str(cm.exception))
        self.assertIn("exactly one", str(cm.exception))

    def test_a_max_cap_does_not_hide_a_later_duplicate_section_heading(self):
        # The subtle failure mode a naive port of `max_n`'s early-exit
        # would have: `max: 1` reaches its cap inside the FIRST "Notes"
        # section, one link in. If the walk stopped there the way it does
        # without `section`, the second "Notes" landmark later in the
        # document would never be seen and the ambiguity would silently
        # read as unique. It must not -- ambiguity has to be provable, so
        # `section` disables the early exit and applies `max_n` only after
        # confirming there is exactly one root to cap.
        tree = (
            '- main [ref=e1]:\n'
            '  - region [ref=e2]:\n'
            '    - heading "Notes" [level=2] [ref=e3]\n'
            '    - link "First" [ref=e4]\n'
            '    - link "AlsoFirst" [ref=e5]\n'
            '  - region [ref=e6]:\n'
            '    - heading "Notes" [level=2] [ref=e7]\n'
            '    - link "Second" [ref=e8]\n'
        )
        with self.assertRaises(ValueError) as cm:
            service._filter_roles(tree, frozenset({"link"}), "main", 1, "Notes")
        self.assertIn("2", str(cm.exception))

    def test_section_still_honours_max_once_uniqueness_is_confirmed(self):
        tree = (
            '- main [ref=e1]:\n'
            '  - region [ref=e2]:\n'
            '    - heading "See also" [level=2] [ref=e3]\n'
            '    - link "First" [ref=e4]\n'
            '    - link "Second" [ref=e5]\n'
            '    - link "Third" [ref=e6]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", 2, "See also")
        self.assertIn("First", out)
        self.assertIn("Second", out)
        self.assertNotIn("Third", out)

    def test_a_heading_that_is_not_the_landmarks_first_child_does_not_make_it_a_section_root(self):
        # The spec is specifically "the FIRST element line nested directly
        # under it" (docs/design/judgement.md section 4), not "a heading
        # anywhere inside it": a landmark opened by something else first
        # must not become a section root merely because a matching heading
        # shows up later inside the same landmark.
        tree = (
            '- main [ref=e1]:\n'
            '  - region [ref=e2]:\n'
            '    - link "Not a heading" [ref=e3]\n'
            '    - heading "See also" [level=2] [ref=e4]\n'
            '    - link "Should be excluded" [ref=e5]\n'
            '  - region [ref=e6]:\n'
            '    - heading "See also" [level=2] [ref=e7]\n'
            '    - link "Should be kept" [ref=e8]\n'
        )
        out = service._filter_roles(tree, frozenset({"link"}), "main", None, "See also")
        self.assertNotIn("Should be excluded", out)
        self.assertNotIn("Not a heading", out)
        self.assertIn("Should be kept", out)

    def test_the_head_carries_section_between_roles_and_chars(self):
        head = service._head(
            "https://example.com", "Example", scope="main", roles=["link"], section="See also", chars=10
        )
        self.assertEqual(
            head,
            "url: https://example.com\ntitle: Example\nscope: main\nroles: link\nsection: See also\nchars: 10\n\n",
        )

    def test_the_section_head_key_matches_a11y_pys_head_line_grammar(self):
        # jev/automation/a11y.py's `parse_head` lifts extra head lines with
        # `_HEAD_RE`'s `(?:[a-z][a-z_]*: [^\n]*\n)*` -- mirrored here as a
        # literal, not imported (this service does not depend on jev, same
        # reasoning `_LANDMARK_ROLES` above already gives). The stakes are
        # not "this one line is dropped": `_HEAD_RE` anchors the whole head
        # block from `url:` on, so one extra line that fails this grammar
        # fails the match from the start and silently drops url/title too
        # -- confirmed directly against that regex while this field was
        # being named, which is why it is `section:` and not, say, `s1:`.
        self.assertRegex("section", r"^[a-z][a-z_]*$")


class RefResolutionTests(unittest.TestCase):
    """`_resolve_ref` never touches `STATE.page`, so it runs safely under a
    throwaway `asyncio.run` loop regardless of what loop `STATE.page` (if
    any, from another test in this module) happens to be bound to."""

    def test_a_ref_from_no_snapshot_yet_is_refused_by_name(self):
        with mock.patch.object(service.STATE, "known_refs", frozenset()):
            with self.assertRaises(ValueError) as cm:
                asyncio.run(service._resolve_ref({"ref": "e7"}))
        self.assertIn("e7", str(cm.exception))
        self.assertIn("browser_snapshot", str(cm.exception))

    def test_a_ref_still_in_the_known_set_resolves(self):
        with mock.patch.object(service.STATE, "known_refs", frozenset({"e7"})):
            ref = asyncio.run(service._resolve_ref({"ref": "e7"}))
        self.assertEqual(ref, "e7")

    def test_a_missing_ref_argument_is_refused(self):
        with self.assertRaises(ValueError):
            asyncio.run(service._resolve_ref({}))


class _RecordingAsyncLock:
    """`asyncio`'s answer to jev/test_server.py's `_RecordingLock`: wraps a
    real lock so a test can prove `/call`'s dispatch actually goes through
    `async with LOCK:` -- not just that a bare lock excludes, which would
    keep passing even if that line were deleted."""

    def __init__(self, real_lock: asyncio.Lock):
        self._real = real_lock
        self.acquire_count = 0

    async def __aenter__(self):
        await self._real.acquire()
        self.acquire_count += 1
        return self

    async def __aexit__(self, *exc):
        self._real.release()


class ConcurrencyTests(unittest.TestCase):
    def test_call_dispatch_goes_through_the_lock(self):
        # An *unknown* method returns before `async with LOCK:` is ever
        # reached (`fn is None` short-circuits in `call`), so this needs a
        # name `METHODS` actually has -- stubbed out so the assertion is
        # about the lock, not about a real page.
        async def fake_read(args):
            return {"text": "stub"}

        rec = _RecordingAsyncLock(service.LOCK)
        client = TestClient(service.app)
        with mock.patch.object(service, "LOCK", rec), mock.patch.dict(
            service.METHODS, {"read": fake_read}
        ):
            r = client.post("/call", headers=AUTH, json={"method": "read", "args": {}})
        self.assertEqual(r.status_code, 200)
        self.assertTrue(r.json()["ok"], r.json())
        self.assertEqual(rec.acquire_count, 1, "/call never acquired LOCK -- is the `with` still there?")


@unittest.skipUnless(service._chromium_installed(), "chromium is not installed in this venv")
class RealBrowserWalkTests(unittest.IsolatedAsyncioTestCase):
    """One coherent walk through a real headless Chromium, calling
    service's async methods directly rather than through TestClient (see
    the module docstring for why). This class exists specifically to prove
    the contract docs/design/automation.md relies on: a snapshot's refs
    stay valid across the calls that use them, and go stale -- legibly, by
    name, before Playwright is even asked -- the moment the page changes
    under them. Every assertion below runs against a real page, not a
    mock."""

    async def asyncTearDown(self):
        # Leaves no live Chromium behind once the suite finishes, the same
        # thing `eidolon ext stop browser` promises for a real session (see
        # the extension's report).
        if service.STATE.browser is not None:
            await service.STATE.browser.close()
        if service.STATE.playwright is not None:
            await service.STATE.playwright.stop()
        service.STATE.browser = None
        service.STATE.context = None
        service.STATE.page = None
        service.STATE.playwright = None
        service.STATE.known_refs = frozenset()

    async def test_open_snapshot_click_type_read_back(self):
        opened = await service._open({"url": PAGE})
        self.assertEqual(opened["title"], "Ref Test")

        tree = await service._snapshot({})
        # The two-line convention docs/design/automation.md documents: any
        # tool's output may lead with url:/title: lines and a blank line,
        # which the jev interpreter lifts into obs.url/obs.title.
        head, _, body = tree.partition("\n\n")
        self.assertIn("title: Ref Test", head)
        self.assertIn('link "Home"', body)
        self.assertIn('button "Press me"', body)
        self.assertTrue(service.STATE.known_refs, "a snapshot with real content produced no refs")

        # Found by role and name, the way docs/design/automation.md's own
        # a11y.py would, not by assuming a specific ref number.
        button_ref = re.search(r'button "Press me" \[ref=(e\d+)\]', body).group(1)

        clicked = await service._click({"ref": button_ref})
        self.assertEqual(clicked["ref"], button_ref)

        # The click invalidated the snapshot that minted button_ref: reusing
        # it must fail by name, not hang and not silently resolve to
        # whatever now sits at that internal id (docs/design/automation.md:
        # "a stale ref ... is an ERROR, not a click on whatever now has that
        # number").
        with self.assertRaises(ValueError) as cm:
            await service._click({"ref": button_ref})
        self.assertIn(button_ref, str(cm.exception))
        self.assertIn("browser_snapshot", str(cm.exception))

        # A fresh snapshot mints a usable ref for the text field.
        tree2 = await service._snapshot({})
        field_ref = re.search(r'textbox[^\[]*\[ref=(e\d+)\]', tree2).group(1)

        typed = await service._type({"ref": field_ref, "text": "hello ref", "submit": False})
        self.assertEqual(typed["ref"], field_ref)

        # `type` invalidates refs exactly like `click`: the link from that
        # same second snapshot, never itself used, is also refused now.
        link_ref = re.search(r'link "the next link" \[ref=(e\d+)\]', tree2).group(1)
        with self.assertRaises(ValueError):
            await service._click({"ref": link_ref})

        # browser_read: plain text, no refs, needs no snapshot first.
        read = await service._read({"max": 8000})
        self.assertIn("Welcome", read["text"])
        self.assertFalse(read["truncated"])

        capped = await service._read({"max": 5})
        self.assertTrue(capped["truncated"])
        self.assertEqual(len(capped["text"]), 5)

        # browser_back: opened once, so there is nowhere to go -- Playwright
        # reports that as no navigation rather than an error, and this
        # service passes that through as went_back: false instead of
        # raising.
        back = await service._back({})
        self.assertFalse(back["went_back"])

    async def test_a_ref_survives_a_second_navigation_not_just_the_first(self):
        """Regression test for a real defect this review found by running
        the service against a real page rather than trusting the design
        doc's illustration or a hand-typed fixture: Playwright 1.63.0 only
        numbers refs as a bare `e7` for a page's FIRST navigation in a
        process's lifetime. From the second navigation on -- reproduced with
        an ordinary same-tab link click, not just another browser_open --
        every ref is `f1e2`-style instead, then `f2e2`, and so on per
        navigation (confirmed directly against
        https://en.wikipedia.org/wiki/Cat -> click "Felis" on this box, and
        reproduced here with two plain local pages so the test needs no
        network). `test_open_snapshot_click_type_read_back` above never
        exercises a second navigation at all -- its one `browser_open`
        happens once, and clicking its button does not navigate -- which is
        exactly why this shape got past it. This test's job is only to
        prove a ref minted by the snapshot just taken is accepted, the same
        property proven above for a first navigation; the exact label a ref
        carries is an implementation detail this deliberately does not
        assert."""
        page_one = (
            "data:text/html,<html><head><title>One</title></head>"
            "<body><a href='/x'>first-page-link</a></body></html>"
        )
        page_two = (
            "data:text/html,<html><head><title>Two</title></head>"
            "<body><a href='/y'>second-page-link</a></body></html>"
        )
        await service._open({"url": page_one})
        await service._snapshot({})  # the first navigation's snapshot: bare `eN`
        await service._open({"url": page_two})  # the second navigation
        tree = await service._snapshot({})
        match = re.search(r'link "second-page-link" \[ref=([^\]]+)\]', tree)
        self.assertIsNotNone(match, f"no ref found on the second page's own snapshot:\n{tree}")
        ref = match.group(1)
        clicked = await service._click({"ref": ref})
        self.assertEqual(clicked["ref"], ref)


class OriginOfTests(unittest.TestCase):
    """`_origin_of`'s own contract, direct and cheap -- no browser, no
    mocking, just the function (docs/design/unattended.md section 3,
    "Confinement": "An origin is scheme, host and port, lowercase host, no
    path"). What each of these proves about the semantics this service uses
    for `confine`, stated once here rather than re-derived at every call
    site: scheme and host are compared case-insensitively (host lowercased,
    scheme lowercased); a scheme's own default port folds into the
    portless form, any other port does not; `http` and `https` never
    compare equal to each other, no matter the host; a subdomain is a
    different origin than its parent, and `localhost` is a different origin
    than `127.0.0.1` -- no family, no fuzz, the triple or nothing."""

    def test_default_https_port_is_dropped(self):
        self.assertEqual(service._origin_of("https://example.com:443/x"), "https://example.com")

    def test_default_http_port_is_dropped(self):
        self.assertEqual(service._origin_of("http://example.com:80/x"), "http://example.com")

    def test_a_non_default_port_is_kept(self):
        self.assertEqual(service._origin_of("http://127.0.0.1:5173/x"), "http://127.0.0.1:5173")

    def test_host_is_lowercased(self):
        self.assertEqual(service._origin_of("https://EN.Wikipedia.ORG/wiki/Cat"), "https://en.wikipedia.org")

    def test_http_and_https_are_different_origins(self):
        self.assertNotEqual(service._origin_of("http://example.com"), service._origin_of("https://example.com"))

    def test_localhost_and_127_0_0_1_are_different_origins(self):
        self.assertNotEqual(
            service._origin_of("http://localhost:8000"), service._origin_of("http://127.0.0.1:8000")
        )

    def test_a_subdomain_is_a_different_origin_than_its_parent(self):
        self.assertNotEqual(service._origin_of("https://www.example.com"), service._origin_of("https://example.com"))

    def test_path_query_and_fragment_are_dropped(self):
        self.assertEqual(service._origin_of("https://example.com/a/b?x=1#y"), "https://example.com")


class ConfineValidationTests(unittest.IsolatedAsyncioTestCase):
    """`_confine`'s own input validation -- no browser needed for any of
    these, since every case here is refused before `_ensure_browser` (and
    therefore before any real launch) ever runs; confirmed by the absence
    of any mock or fake here at all, unlike `OpenConfineDispatchTests`
    below."""

    async def test_confine_must_be_a_list(self):
        with self.assertRaises(ValueError) as cm:
            await service._confine("main")
        self.assertIn("confine", str(cm.exception))

    async def test_confine_must_not_be_empty(self):
        with self.assertRaises(ValueError):
            await service._confine([])

    async def test_confine_rejects_a_non_string_element(self):
        with self.assertRaises(ValueError):
            await service._confine(["https://example.com", 7])

    async def test_confine_rejects_a_non_http_scheme(self):
        with self.assertRaises(ValueError):
            await service._confine(["ftp://x"])

    async def test_confine_rejects_file_scheme(self):
        # `file://` has no host and is not a confinable origin -- ORIGIN_RE
        # admits only `https?`, so this is refused the same way any other
        # non-http(s) scheme is, not given special-cased handling.
        with self.assertRaises(ValueError):
            await service._confine(["file:///etc/passwd"])

    async def test_confine_rejects_an_origin_with_a_path(self):
        with self.assertRaises(ValueError):
            await service._confine(["https://x/path"])


class OpenConfineDispatchTests(unittest.IsolatedAsyncioTestCase):
    """`_open`'s own confine/no-confine branch -- which of `_confine`/
    `_unconfine` it reaches, and with what argument, proven against fakes
    the same way `SnapshotArgsTests` proves what `_snapshot` asks of
    Playwright without needing a real page (see that class's own
    docstring): this is a claim about what `_open` itself does, not about
    what a real browser does in response to it -- `ConfinementTests` below
    is where that is proven, against a real one."""

    async def asyncTearDown(self):
        service.STATE.confined = None

    async def _open_against_fakes(self, args: dict):
        page = mock.AsyncMock()
        page.url = args.get("url", "")
        page.title = mock.AsyncMock(return_value="Example")
        confine_fn = mock.AsyncMock()
        unconfine_fn = mock.AsyncMock()
        with mock.patch("service._ensure_page", mock.AsyncMock(return_value=page)), \
                mock.patch("service._confine", confine_fn), \
                mock.patch("service._unconfine", unconfine_fn):
            await service._open(args)
        return confine_fn, unconfine_fn

    async def test_a_present_confine_reaches_confine_and_not_unconfine(self):
        confine_fn, unconfine_fn = await self._open_against_fakes(
            {"url": "https://example.com", "confine": ["https://example.com"]}
        )
        confine_fn.assert_awaited_once_with(["https://example.com"])
        unconfine_fn.assert_not_awaited()

    async def test_an_absent_confine_reaches_unconfine_and_not_confine(self):
        confine_fn, unconfine_fn = await self._open_against_fakes({"url": "https://example.com"})
        unconfine_fn.assert_awaited_once()
        confine_fn.assert_not_awaited()

    async def test_unconfine_is_a_no_op_when_nothing_is_confined(self):
        # The common case -- an ordinary open, most of the time -- costs
        # nothing extra: no context replaced, no page recreated, when there
        # was never anything to undo.
        self.assertIsNone(service.STATE.confined)
        service.STATE.context = "sentinel"
        try:
            await service._unconfine()
            self.assertEqual(service.STATE.context, "sentinel", "unconfine touched context when nothing was confined")
        finally:
            service.STATE.context = None


def _start_fixture_server(routes: dict) -> http.server.ThreadingHTTPServer:
    """A real `http.server.ThreadingHTTPServer` on an ephemeral loopback
    port -- `routes` maps a path to `(status, headers, body)`.
    `server.hits` is every path this server actually received, in order,
    the same diff-a-real-listing discipline `_list_playwright_temp_names`
    above uses instead of trusting an in-process claim: what proves a
    request never happened is this list never gaining an entry for it, not
    this service saying so. Threading, not the plain single-connection
    `HTTPServer` an earlier version of this fixture used: `service.py`'s
    confined route handler (`_route_confined`) makes its OWN outbound
    request per intercepted request via `route.fetch()`, which can be a
    second, separate connection to the same loopback server the browser
    itself is also talking to. Reproduced directly, isolated from this
    test file entirely (a standalone script, not this suite): a
    single-threaded `HTTPServer` plus a `route.fetch()`-based handler hung
    a plain listed-origin navigation indefinitely; switching that same
    script to `ThreadingHTTPServer` fixed it outright, same request,
    same assertions, well under a second. What this fixture does NOT
    claim: that the single-threaded server was also the cause of an
    earlier, separate hang seen in this suite under an OLDER version of
    `_route_confined` (a regex-match-and-abort route that never called
    `route.fetch()` for an allowed request at all, so this specific
    contention could not have been why THAT one hung) -- that earlier
    hang's own root cause was never separately isolated, and is recorded
    as an open question rather than folded silently into this docstring's
    own story."""
    hits: list[str] = []

    class _Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, format, *args):  # noqa: A002 -- stdlib's own name
            pass  # keep test output quiet; failures below carry their own message

        def do_GET(self):
            hits.append(self.path)
            entry = routes.get(self.path)
            if entry is None:
                self.send_response(404)
                self.end_headers()
                return
            status, headers, body = entry
            self.send_response(status)
            for key, value in headers.items():
                self.send_header(key, value)
            self.end_headers()
            if body:
                self.wfile.write(body)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    server.hits = hits  # type: ignore[attr-defined]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


@unittest.skipUnless(service._chromium_installed(), "chromium is not installed in this venv")
class ConfinementTests(unittest.IsolatedAsyncioTestCase):
    """`browser_open`'s `confine` (docs/design/unattended.md section 3,
    "Confinement"), proved against a real headless Chromium and two real
    loopback `http.server` instances on two real ephemeral ports -- never
    mocked, and never asked only what it was already going to answer: every
    method below either drives a real navigation/click/fetch and reads a
    real server's own hit log, or drives a second real Playwright context
    directly. Skipped, not failed, on a venv with no chromium installed,
    the same condition every other real-browser class in this file checks.

    Origin A is `127.0.0.1:{port_a}`, the one origin every test confines
    to; origin B is `127.0.0.1:{port_b}`, always unlisted. `/a.html` is the
    one fixture page carrying one of each shape a confined page can try:
    a same-origin link (`same`), a direct cross-origin link (`other`), a
    same-origin link that 302s cross-origin (`away`), a same-origin
    same-tab download (`dl`), a cross-origin `<img>` subresource, and a
    cross-origin `fetch()` from an inline `<script>` -- the D1r shape."""

    async def asyncSetUp(self):
        self.server_b = _start_fixture_server({
            "/x.html": (200, {"Content-Type": "text/html"}, b"<html><head><title>X</title></head><body>x</body></html>"),
            "/probe": (200, {"Content-Type": "text/plain"}, b"probed"),
            "/pixel.png": (200, {"Content-Type": "image/png"}, b"\x89PNG\r\n\x1a\n"),
        })
        self.port_b = self.server_b.server_address[1]
        self.origin_b = f"http://127.0.0.1:{self.port_b}"

        a_html = (
            "<html><head><title>A</title></head><body><main><h1>A</h1>"
            "<a href='/b.html'>same</a>"
            f"<a href='{self.origin_b}/x.html'>other</a>"
            "<a href='/redirect'>away</a>"
            "<a href='/d.bin' download>dl</a>"
            f"<img src='{self.origin_b}/pixel.png'>"
            f"<script>fetch('{self.origin_b}/probe')</script>"
            "</main></body></html>"
        ).encode()
        self.server_a = _start_fixture_server({
            "/a.html": (200, {"Content-Type": "text/html"}, a_html),
            "/b.html": (200, {"Content-Type": "text/html"}, b"<html><head><title>B-page-on-A</title></head><body>b</body></html>"),
            "/redirect": (302, {"Location": f"{self.origin_b}/x.html"}, b""),
            "/d.bin": (
                200,
                {"Content-Type": "application/octet-stream", "Content-Disposition": 'attachment; filename="d.bin"'},
                b"binary-payload",
            ),
        })
        self.port_a = self.server_a.server_address[1]
        self.origin_a = f"http://127.0.0.1:{self.port_a}"

    async def asyncTearDown(self):
        for server in (getattr(self, "server_a", None), getattr(self, "server_b", None)):
            if server is not None:
                server.shutdown()
                server.server_close()
        # Same full reset every other real-browser class in this file uses.
        if service.STATE.browser is not None:
            await service.STATE.browser.close()
        if service.STATE.playwright is not None:
            await service.STATE.playwright.stop()
        service.STATE.browser = None
        service.STATE.context = None
        service.STATE.page = None
        service.STATE.playwright = None
        service.STATE.known_refs = frozenset()
        service.STATE.confined = None
        service.STATE.last_blocked = None

    async def _ref_for(self, tree: str, name: str) -> str:
        match = re.search(rf'link "{name}" \[ref=([^\]]+)\]', tree)
        self.assertIsNotNone(match, f"no {name!r} link in snapshot:\n{tree}")
        return match.group(1)

    # -- 1: a top-level navigation to a listed origin succeeds -----------

    async def test_open_to_a_listed_origin_succeeds(self):
        opened = await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        self.assertEqual(opened["title"], "A")
        self.assertEqual(opened["confined"], [self.origin_a])
        self.assertIn("/a.html", self.server_a.hits)

    async def test_a_confined_click_within_the_listed_origin_navigates(self):
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        tree = await service._snapshot({})
        ref = await self._ref_for(tree, "same")
        clicked = await service._click({"ref": ref})
        self.assertTrue(clicked["url"].endswith("/b.html"))
        self.assertEqual(clicked["confined"], [self.origin_a])

    # -- 2: a navigation to an unlisted origin is aborted -----------------

    async def test_an_open_to_an_unlisted_origin_is_refused_before_navigating(self):
        with self.assertRaises(ValueError) as cm:
            await service._open({"url": f"{self.origin_b}/x.html", "confine": [self.origin_a]})
        message = str(cm.exception)
        self.assertIn(self.origin_b, message)
        self.assertIn(self.origin_a, message)
        self.assertEqual(self.server_b.hits, [], "B must never have been asked at all")

    async def test_a_confined_click_to_an_unlisted_origin_is_refused_naming_it(self):
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        tree = await service._snapshot({})
        ref = await self._ref_for(tree, "other")
        with self.assertRaises(ValueError) as cm:
            await service._click({"ref": ref})
        message = str(cm.exception)
        self.assertIn(self.origin_b, message)
        self.assertIn(self.origin_a, message)
        # A blocked TOP-LEVEL navigation does not leave the page on its
        # previous document -- Chromium replaces it with its own internal
        # failed-navigation page (`chrome-error://chromewebdata/`), the same
        # as any other net::ERR_* on a main-frame navigation -- observed
        # directly while diagnosing this test, not assumed. What actually
        # matters, and what the ValueError message and the hit log above
        # already prove, is that the page never got to B.
        self.assertFalse(service.STATE.page.url.startswith(self.origin_b), "page ended up on B")
        self.assertEqual(self.server_b.hits, [], "B must never have recorded a hit")

    # -- 3: a subresource from an unlisted origin is aborted while the
    #       page still loads (the case a naive implementation misses --
    #       it only guards navigation) --------------------------------

    async def test_a_subresource_from_an_unlisted_origin_is_aborted_while_the_page_still_loads(self):
        opened = await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        # The page loaded fully -- title included -- despite carrying an
        # <img> pointed at a blocked origin; a blocked subresource does not
        # fail the page around it.
        self.assertEqual(opened["title"], "A")
        await asyncio.sleep(0.3)  # generous margin past `load` for the image's own settled fate
        self.assertNotIn("/pixel.png", self.server_b.hits, "an <img> subresource reached the blocked origin")

    # -- 4: a fetch() to loopback from page script is aborted (D1r) -------

    async def test_an_in_page_fetch_to_an_unlisted_origin_is_aborted(self):
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        await asyncio.sleep(0.3)
        self.assertNotIn("/probe", self.server_b.hits, "an in-page fetch() reached the blocked origin -- the D1r shape")

    # -- 5: a redirect from a listed origin to an unlisted one is
    #       aborted -- the request that starts legal and ends elsewhere ---

    async def test_a_click_whose_target_redirects_off_the_listed_origin_is_refused_naming_it(self):
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        tree = await service._snapshot({})
        ref = await self._ref_for(tree, "away")
        with self.assertRaises(ValueError) as cm:
            await service._click({"ref": ref})
        message = str(cm.exception)
        self.assertIn(self.origin_b, message)
        # Same reasoning as the direct-cross-origin-link test above: a
        # blocked top-level navigation lands on Chromium's own
        # chrome-error:// page, not back on A -- the hit logs below are
        # what actually prove where the request did and did not go.
        self.assertFalse(service.STATE.page.url.startswith(self.origin_b), "page ended up on B")
        self.assertIn("/redirect", self.server_a.hits, "the first, legal leg to A must have gone through")
        self.assertNotIn("/x.html", self.server_b.hits, "the redirect's target must never have been requested")

    async def test_an_open_whose_own_navigation_redirects_off_the_listed_origin_is_aborted(self):
        # Deliberately this test method's first call into the service at
        # all -- STATE.browser is None going in (see `_ensure_browser`) --
        # so this also exercises `_confine` standing up a browser from
        # nothing, not just replacing a context that already existed.
        self.assertIsNone(service.STATE.browser)
        with self.assertRaises(Exception) as cm:
            await service._open({"url": f"{self.origin_a}/redirect", "confine": [self.origin_a]})
        self.assertIn("BLOCKED_BY_CLIENT", str(cm.exception).upper())
        self.assertIn("/redirect", self.server_a.hits)
        self.assertNotIn("/x.html", self.server_b.hits, "the redirect's target must never have been requested")

    # -- 6: confinement is per-context -- an unconfined snapshot
    #       elsewhere still works -----------------------------------------

    async def test_confinement_is_scoped_to_its_own_context_not_the_whole_browser(self):
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        self.assertEqual(service.STATE.confined, (self.origin_a,))

        # A second, independent context in the SAME browser process -- the
        # architecture here is one shared browser with one context active
        # at a time (docs/design/unattended.md section 3), so "elsewhere"
        # is proven the way it is actually implemented: against
        # Playwright/Chromium's own guarantee that a context-level route
        # does not leak to a sibling context, not against this service's
        # own bookkeeping, which never runs two contexts at once and so
        # cannot be what such a leak would even mean.
        extra_context = await service.STATE.browser.new_context()
        try:
            extra_page = await extra_context.new_page()
            resp = await extra_page.goto(f"{self.origin_b}/x.html", wait_until="load")
            self.assertIsNotNone(resp)
            self.assertEqual(resp.status, 200)
            self.assertIn("/x.html", self.server_b.hits, "the unconfined sibling context could not reach B")
        finally:
            await extra_context.close()

        # STATE's own confined context is still exactly as confined as it
        # was -- driven directly, bypassing `_open`, so this cannot be
        # mistaken for `_open`'s own dispatch logic doing the proving.
        self.assertEqual(service.STATE.confined, (self.origin_a,))
        with self.assertRaises(Exception):
            await service.STATE.page.goto(f"{self.origin_b}/x.html", timeout=5000)
        # Same chrome-error:// landing as every other blocked top-level
        # navigation in this class -- the exception above and B's own hit
        # log (unchanged since the sibling context's request, checked
        # above) are what prove this context is still confined.
        self.assertFalse(service.STATE.page.url.startswith(self.origin_b), "page ended up on B")

    # -- getting the comparison wrong looks identical to getting it right,
    #    until someone crafts a hostname -----------------------------------

    async def test_a_lookalike_host_with_the_listed_origin_as_a_prefix_is_still_blocked(self):
        # `127.0.0.1.evil.example` carries origin_a's host as a literal
        # string prefix but is a different host entirely, and resolves
        # nowhere -- a naive prefix match would let this through to a real
        # DNS lookup, which would then fail on ITS OWN merits
        # (net::ERR_NAME_NOT_RESOLVED), not on confinement -- so asserting
        # the specific abort reason below, not just "some exception", is
        # what actually distinguishes a working boundary check from a
        # broken one that happens to fail here anyway.
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        lookalike = f"http://127.0.0.1.evil.example:{self.port_a}/x"
        with self.assertRaises(Exception) as cm:
            await service.STATE.page.goto(lookalike, timeout=5000)
        self.assertIn("BLOCKED_BY_CLIENT", str(cm.exception).upper())

    # -- the rest of docs/design/unattended.md's own ConfinementTests list -

    async def test_a_confining_open_starts_with_no_cookies(self):
        await service._open({"url": f"{self.origin_a}/a.html"})  # plain, unconfined context first
        await service.STATE.context.add_cookies(
            [{"name": "sid", "value": "abc123", "url": self.origin_a}]
        )
        before = await service.STATE.context.cookies()
        self.assertTrue(before, "setup did not actually set a cookie")

        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        after = await service.STATE.context.cookies()
        self.assertEqual(after, [])

    async def test_a_download_link_under_confinement_saves_nothing(self):
        """The design doc's own two suggested proofs (docs/design/unattended.md,
        the ConfinementTests bullet list) are offered as equivalent -- "assert
        via `page.expect_download` timing out, or the context's
        `accept_downloads` being `False`" -- but they are not equivalent on
        this Playwright (1.63.0), confirmed with a standalone script before
        rewriting this test, not assumed: the `download` event still fires
        (with a real `suggested_filename`), so `expect_download(timeout=...)`
        never times out. What actually happens is the doc's second
        alternative in substance: because the confined context is
        `new_context(accept_downloads=False)`, the fired download is a stub
        that immediately fails -- `download.failure()` returns Playwright's
        own message telling the caller to pass `accept_downloads=True`, and
        no artifact (so no file on disk) is ever created for it. Asserting
        that failure is live, non-vacuous proof that nothing was saved --
        stronger than either of the doc's two suggestions, and true."""
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        tree = await service._snapshot({})
        ref = await self._ref_for(tree, "dl")
        page = service.STATE.page
        async with page.expect_download(timeout=2000) as dl_info:
            await service._click({"ref": ref})
        download = await dl_info.value
        self.assertEqual(download.suggested_filename, "d.bin")
        failure = await download.failure()
        self.assertIsNotNone(failure, "a confined download must fail -- accept_downloads=False")
        self.assertIn("accept_downloads", failure)

    async def test_an_open_without_confine_after_a_confined_one_unconfines(self):
        await service._open({"url": f"{self.origin_a}/a.html", "confine": [self.origin_a]})
        self.assertEqual(service.STATE.confined, (self.origin_a,))

        opened = await service._open({"url": f"{self.origin_b}/x.html"})
        self.assertEqual(opened["title"], "X")
        self.assertIsNone(opened["confined"])
        self.assertIsNone(service.STATE.confined)


@unittest.skipUnless(service._chromium_installed(), "chromium is not installed in this venv")
class ScopedSnapshotTests(unittest.IsolatedAsyncioTestCase):
    """`within` (docs/design/observation.md, "Where an observation is
    narrowed") against a real headless Chromium, not a fake -- the claim
    this class exists to prove is about Playwright's own behaviour, not this
    service's: that a `Locator.aria_snapshot(mode="ai")` renders its root
    element's own line (M3) and mints refs `aria-ref=` still resolves
    afterward (M2). Both were reasoned, not observed, from
    driver/package/lib/coreBundle.js and `Locator.aria_snapshot`'s own
    docstring (docs/design/observation.md, "Why the ref contract survives")
    before this class ran. Skipped, not failed, on a venv with no chromium
    installed, same condition `RealBrowserWalkTests` checks.

    The M2 test deliberately runs past a page's FIRST navigation before
    doing its real work: service.py's own `REF_RE` comment records a ref
    minted on a page's first navigation ever, an ordinary same-tab link
    click included -- so a test that only ever navigates once cannot tell
    `within`'s own refs apart from that regime, and wiki-hop itself is
    never on its first navigation past the opening page. Reproduced here
    with two plain `data:` pages, no network, the same technique
    `test_a_ref_survives_a_second_navigation_not_just_the_first` above
    uses.
    """

    async def asyncTearDown(self):
        if service.STATE.browser is not None:
            await service.STATE.browser.close()
        if service.STATE.playwright is not None:
            await service.STATE.playwright.stop()
        service.STATE.browser = None
        service.STATE.context = None
        service.STATE.page = None
        service.STATE.playwright = None
        service.STATE.known_refs = frozenset()

    async def test_a_scoped_snapshot_renders_the_scope_as_its_root_line_and_nothing_outside_it(self):
        """M3."""
        page_html = (
            "data:text/html,"
            "<html><head><title>Scope</title></head><body>"
            "<nav><a href='/skip'>Skip</a></nav>"
            "<main><h1>T</h1><a href='/in'>In</a></main>"
            "</body></html>"
        )
        await service._open({"url": page_html})
        result = await service._snapshot({"within": "main"})
        head, _, body = result.partition("\n\n")
        self.assertIn("scope: main", head)
        match = re.search(r"^chars: (\d+)$", head, re.MULTILINE)
        self.assertIsNotNone(match, f"no chars: line in head:\n{head}")
        self.assertEqual(int(match.group(1)), len(body))
        self.assertTrue(body.startswith("- main"), f"tree did not start with '- main':\n{body}")
        self.assertNotIn("Skip", body)
        self.assertNotIn("navigation", body)

    async def test_a_ref_from_a_scoped_snapshot_is_clickable_and_replaces_the_previous_snapshots_refs(self):
        """M2. See the class docstring for why this burns a first
        navigation before the part that matters."""
        throwaway = (
            "data:text/html,<html><head><title>Zero</title></head>"
            "<body><a href='/z'>zero</a></body></html>"
        )
        page_two = (
            "data:text/html,"
            "<html><head><title>Two</title></head><body>"
            "<nav><a href='/skip'>Skip</a></nav>"
            "<main><h1>T</h1><a href='/in'>In</a></main>"
            "</body></html>"
        )
        await service._open({"url": throwaway})
        await service._snapshot({})  # first navigation's snapshot: bare `eN` -- spent, not needed
        await service._open({"url": page_two})  # second navigation: f1e2-style refs from here on

        full = await service._snapshot({})
        nav_match = re.search(r'link "Skip" \[ref=([^\]]+)\]', full)
        self.assertIsNotNone(nav_match, f"no Skip ref in unscoped snapshot:\n{full}")
        nav_ref = nav_match.group(1)

        scoped = await service._snapshot({"within": "main"})
        in_match = re.search(r'link "In" \[ref=([^\]]+)\]', scoped)
        self.assertIsNotNone(in_match, f"no In ref in scoped snapshot:\n{scoped}")
        in_ref = in_match.group(1)

        # The scoped snapshot's own ref is clickable -- minted by a
        # `Locator` snapshot, resolved through the same `aria-ref=` engine
        # `_click` always uses, on the whole page, not some scoped view of
        # it (there is no such thing; `within` only ever changes what
        # `aria_snapshot` walks, never what `_click` resolves against).
        clicked = await service._click({"ref": in_ref})
        self.assertEqual(clicked["ref"], in_ref)

        # The scoped snapshot replaced known_refs wholesale, same as every
        # snapshot does: the nav ref from the PREVIOUS (unscoped) snapshot
        # of this same document is refused by name, not silently resolved
        # against whatever now holds that internal id.
        with self.assertRaises(ValueError) as cm:
            await service._click({"ref": nav_ref})
        self.assertIn(nav_ref, str(cm.exception))
        self.assertIn("browser_snapshot", str(cm.exception))

    async def test_a_scope_that_matches_nothing_is_refused_by_name(self):
        await service._open({"url": "about:blank"})
        with self.assertRaises(ValueError) as cm:
            await service._snapshot({"within": "main"})
        message = str(cm.exception)
        self.assertIn("main", message)
        self.assertIn("0", message)
        self.assertIn("about:blank", message)

    async def test_a_scope_that_matches_more_than_one_element_is_refused_with_the_count(self):
        page_html = (
            "data:text/html,"
            "<html><head><title>Two Regions</title></head><body>"
            "<section role='region' aria-label='A'>a</section>"
            "<section role='region' aria-label='B'>b</section>"
            "</body></html>"
        )
        await service._open({"url": page_html})
        with self.assertRaises(ValueError) as cm:
            await service._snapshot({"within": "region"})
        self.assertIn("2", str(cm.exception))

    async def test_an_unscoped_snapshot_stamps_chars_and_carries_no_scope_line(self):
        page_html = "data:text/html,<html><head><title>Plain</title></head><body><p>hi</p></body></html>"
        await service._open({"url": page_html})
        result = await service._snapshot({})
        head, _, body = result.partition("\n\n")
        self.assertNotIn("scope:", head)
        match = re.search(r"^chars: (\d+)$", head, re.MULTILINE)
        self.assertIsNotNone(match, f"no chars: line in head:\n{head}")
        self.assertEqual(int(match.group(1)), len(body))


@unittest.skipUnless(service._chromium_installed(), "chromium is not installed in this venv")
class RoleFilteredSnapshotTests(unittest.IsolatedAsyncioTestCase):
    """`roles` (E5: docs/design/observation.md's "narrow at the source"
    extended past `within`, past the point where consumer-side filtering
    stopped being merely slow and became impossible -- 762,764 characters
    do not fit down a 262,144-character pipe) against a real headless
    Chromium, not a fake -- the same reasoning `ScopedSnapshotTests` gives
    for why `within` earns this class rather than only mocks: the claim
    here is about a real Playwright/Chromium round trip (a scoped
    `aria_snapshot`, refs minted by it, refs resolved through `aria-ref=`
    against a page this process still holds) a fake page cannot stand in
    for.

    `test_a_roles_filtered_ref_survives_a_second_navigation` is the one
    that matters most. docs/Decisions.md, 2026-09-19 records that the
    `within` ref-survival proof deliberately ran past a page's first
    navigation, because wiki-hop itself is never on its first navigation
    past the opening page and the easy case would have proved nothing
    about the regime that matters. That proof does not automatically
    transfer here: it covered a bare `Locator.aria_snapshot`, and this
    mechanism reads and rewrites that call's own text afterward -- a step
    the earlier proof never exercised, and the one this review's brief
    specifically asked to be re-measured rather than assumed."""

    async def asyncTearDown(self):
        if service.STATE.browser is not None:
            await service.STATE.browser.close()
        if service.STATE.playwright is not None:
            await service.STATE.playwright.stop()
        service.STATE.browser = None
        service.STATE.context = None
        service.STATE.page = None
        service.STATE.playwright = None
        service.STATE.known_refs = frozenset()

    async def test_roles_and_within_together_keep_only_direct_named_links(self):
        await service._open({"url": ROLES_PAGE})
        result = await service._snapshot({"within": "main", "roles": ["link"]})
        head, _, body = result.partition("\n\n")
        self.assertIn("scope: main", head)
        self.assertIn("roles: link", head)
        match = re.search(r"^chars: (\d+)$", head, re.MULTILINE)
        self.assertIsNotNone(match, f"no chars: line in head:\n{head}")
        self.assertEqual(int(match.group(1)), len(body))
        self.assertIn('link "Direct"', body)
        self.assertNotIn("Chrome", body)  # outside `within` entirely (banner)
        self.assertNotIn("Press", body)  # right container, wrong role (button)
        self.assertNotIn("Nested", body)  # inside main, but nearest landmark is navigation

    async def test_roles_without_within_keeps_every_link_regardless_of_landmark(self):
        await service._open({"url": ROLES_PAGE})
        result = await service._snapshot({"roles": ["link"]})
        head, _, body = result.partition("\n\n")
        self.assertNotIn("scope:", head)
        self.assertIn("roles: link", head)
        self.assertIn("Chrome", body)
        self.assertIn("Direct", body)
        self.assertIn("Nested", body)
        self.assertNotIn("Press", body)

    async def test_heading_survives_a_roles_filter_that_excludes_it(self):
        # The regression this class exists to close, end to end through a
        # real headless Chromium rather than a mock: `ROLES_PAGE`'s own
        # `<h1>T</h1>` sits inside `main`, same as every link this class
        # already tests. A `roles: ["link"]` snapshot -- wiki-hop.json's
        # own call shape -- must still name it in the head block, even
        # though "heading" was never in the requested roles and the body
        # itself carries no heading line at all.
        await service._open({"url": ROLES_PAGE})
        result = await service._snapshot({"within": "main", "roles": ["link"]})
        head, _, body = result.partition("\n\n")
        self.assertIn("heading: T", head)
        self.assertNotIn('heading "T"', body)  # roles=[link] still keeps the BODY link-only

    async def test_heading_is_present_on_an_unfiltered_snapshot_too(self):
        await service._open({"url": ROLES_PAGE})
        result = await service._snapshot({"within": "main"})
        head, _, body = result.partition("\n\n")
        self.assertIn("heading: T", head)
        self.assertIn('heading "T"', body)  # unfiltered: still in the body too, same as always

    async def test_max_caps_a_real_pages_option_list_in_document_order(self):
        page_html = (
            "data:text/html,<html><head><title>Many</title></head><body><main>"
            + "".join(f"<a href='/{i}'>L{i}</a>" for i in range(10))
            + "</main></body></html>"
        )
        await service._open({"url": page_html})
        result = await service._snapshot({"within": "main", "roles": ["link"], "max": 3})
        _, _, body = result.partition("\n\n")
        self.assertEqual(body.count('- link "L'), 3, body)
        self.assertIn('"L0"', body)
        self.assertIn('"L1"', body)
        self.assertIn('"L2"', body)
        self.assertNotIn('"L3"', body)
        self.assertNotIn('"L9"', body)

    async def test_a_roles_filtered_ref_is_clickable(self):
        await service._open({"url": ROLES_PAGE})
        result = await service._snapshot({"within": "main", "roles": ["link"]})
        ref = re.search(r'link "Direct" \[ref=([^\]]+)\]', result).group(1)
        clicked = await service._click({"ref": ref})
        self.assertEqual(clicked["ref"], ref)

    async def test_a_ref_the_role_filter_excluded_is_not_in_known_refs(self):
        await service._open({"url": ROLES_PAGE})
        # The button's ref, from an unfiltered snapshot of the same
        # document first...
        unfiltered = await service._snapshot({"within": "main"})
        button_ref = re.search(r'button "Press" \[ref=([^\]]+)\]', unfiltered).group(1)
        # ...then the roles-filtered snapshot, which replaces known_refs
        # wholesale with only what IT showed -- a ref this call never
        # displayed is refused exactly like a ref from any other prior
        # snapshot is, by the same existing rule, not a new one.
        await service._snapshot({"within": "main", "roles": ["link"]})
        with self.assertRaises(ValueError) as cm:
            await service._click({"ref": button_ref})
        self.assertIn(button_ref, str(cm.exception))
        self.assertIn("browser_snapshot", str(cm.exception))

    async def test_a_roles_filtered_ref_survives_a_second_navigation_not_just_the_first(self):
        """See the class docstring for why this burns a first navigation
        before the part that matters, and why the earlier `within` proof of
        this same property does not automatically cover this mechanism
        too."""
        throwaway = (
            "data:text/html,<html><head><title>Zero</title></head>"
            "<body><a href='/z'>zero</a></body></html>"
        )
        await service._open({"url": throwaway})
        await service._snapshot({})  # first navigation's snapshot: bare `eN` -- spent, not needed

        await service._open({"url": ROLES_PAGE})  # second navigation: f1e2-style refs from here on
        full = await service._snapshot({})
        chrome_ref = re.search(r'link "Chrome" \[ref=([^\]]+)\]', full).group(1)

        scoped = await service._snapshot({"within": "main", "roles": ["link"]})
        _, _, scoped_body = scoped.partition("\n\n")
        # Checked against the BODY specifically, not the whole result: the
        # head's own `url:` line is this page's `data:` URL, which is the
        # page's literal HTML source and so contains the substring
        # "Nested" regardless of what the body's own filtering kept --
        # asserting against `scoped` whole was a false positive waiting to
        # happen, caught by actually running this rather than trusting the
        # earlier `within`-only version of this same check.
        self.assertNotIn("Nested", scoped_body)
        direct_ref = re.search(r'link "Direct" \[ref=([^\]]+)\]', scoped_body).group(1)

        # The scoped-and-filtered snapshot's own ref is clickable -- minted
        # by the same `Locator.aria_snapshot(mode="ai")` call `within`
        # alone already proved this for, past this same regime, unaffected
        # by this call also having rewritten the text that ref arrived in.
        clicked = await service._click({"ref": direct_ref})
        self.assertEqual(clicked["ref"], direct_ref)

        # The filtered snapshot replaced known_refs wholesale, same as
        # every snapshot does: the unscoped snapshot's own ref, from the
        # call immediately before this one on this same document, is
        # refused by name now, not silently resolved.
        with self.assertRaises(ValueError) as cm:
            await service._click({"ref": chrome_ref})
        self.assertIn(chrome_ref, str(cm.exception))
        self.assertIn("browser_snapshot", str(cm.exception))

    async def test_a_scope_with_no_role_matches_is_not_an_error(self):
        page_html = (
            "data:text/html,<html><head><title>Empty</title></head>"
            "<body><main><h1>Only a heading</h1></main></body></html>"
        )
        await service._open({"url": page_html})
        result = await service._snapshot({"within": "main", "roles": ["link"]})
        head, _, body = result.partition("\n\n")
        lines = body.splitlines()
        self.assertEqual(len(lines), 1, body)
        self.assertTrue(lines[0].startswith("- main"), body)
        match = re.search(r"^chars: (\d+)$", head, re.MULTILINE)
        self.assertEqual(int(match.group(1)), len(body))

    async def test_depth_and_roles_compose_without_error(self):
        await service._open({"url": ROLES_PAGE})
        result = await service._snapshot({"within": "main", "roles": ["link"], "depth": 1})
        _, _, body = result.partition("\n\n")
        self.assertIn("Direct", body)
        self.assertNotIn("Nested", body)  # pruned by depth before roles ever sees it

    async def test_a_section_scoped_snapshot_of_a_real_page_yields_that_sections_refs_and_they_click(self):
        # docs/design/judgement.md section 4, proved end to end through a
        # real headless Chromium rather than the hand-built trees
        # `RoleProjectionUnitTests` uses: `SECTIONS_PAGE` reproduces the
        # live article's shape in miniature -- two headed landmarks, the
        # second holding a nested `navigation` ("Portals", real on Cat) --
        # and this asserts both what a section-scoped call keeps and that
        # the refs it mints are the genuine article, clickable through the
        # same `aria-ref=` engine every other ref in this file is.
        await service._open({"url": SECTIONS_PAGE})
        result = await service._snapshot({"within": "main", "roles": ["link"], "section": "See also"})
        head, _, body = result.partition("\n\n")
        self.assertIn("scope: main", head)
        self.assertIn("roles: link", head)
        self.assertIn("section: See also", head)
        match = re.search(r"^chars: (\d+)$", head, re.MULTILINE)
        self.assertEqual(int(match.group(1)), len(body))
        self.assertIn("Domestication", body)
        self.assertIn("Felidae", body)
        self.assertNotIn("Word", body)  # Etymology, a different section
        self.assertNotIn("Portal1", body)  # nested navigation inside the section
        self.assertNotIn("Portal2", body)

        ref = re.search(r'link "Domestication" \[ref=([^\]]+)\]', body).group(1)
        clicked = await service._click({"ref": ref})
        self.assertEqual(clicked["ref"], ref)

    async def test_a_section_scoped_ref_survives_a_second_navigation_not_just_the_first(self):
        """Same reasoning as
        test_a_roles_filtered_ref_survives_a_second_navigation_not_just_the_first
        above, extended to `section`: a live capture against
        https://en.wikipedia.org/wiki/Cat (docs/Build-Log.md) proved this
        by hand, past a same-tab click and into a *third* navigation's
        refs, not only the second -- this fixture-based version is what
        runs on every future change to this mechanism, not just once."""
        throwaway = (
            "data:text/html,<html><head><title>Zero</title></head>"
            "<body><a href='/z'>zero</a></body></html>"
        )
        await service._open({"url": throwaway})
        await service._snapshot({})  # first navigation's snapshot: bare `eN` -- spent, not needed

        await service._open({"url": SECTIONS_PAGE})  # second navigation: f1e2-style refs from here on
        result = await service._snapshot({"within": "main", "roles": ["link"], "section": "See also"})
        _, _, body = result.partition("\n\n")
        self.assertNotIn("Word", body)  # the exclusion still holds past the first navigation too
        ref = re.search(r'link "Domestication" \[ref=([^\]]+)\]', body).group(1)
        self.assertRegex(ref, r"^f\d+e\d+$")  # the regime that matters, not bare `eN`

        clicked = await service._click({"ref": ref})
        self.assertEqual(clicked["ref"], ref)


class LifespanWiringTests(unittest.TestCase):
    """Confirms the ASGI plumbing itself -- that `app`'s lifespan really
    does reach `_close_browser` on shutdown -- separately from what
    `_close_browser` actually does, which `RealCleanupTests` below proves
    against a real browser. A mistake in `app = FastAPI(..., lifespan=...)`
    (wrong function, `startup` instead of `shutdown`, or the decorator
    dropped entirely) would not be caught by a test that only ever calls
    `_close_browser` directly."""

    def test_asgi_shutdown_calls_close_browser(self):
        called = []

        async def fake_close():
            called.append(True)

        with mock.patch("service._close_browser", fake_close):
            with TestClient(service.app):
                self.assertEqual(called, [], "shutdown ran before the client's `with` block even exited")
            # TestClient.__exit__ is what sends the ASGI `lifespan.shutdown`
            # message (Starlette's own testclient.py, `__enter__`/`__exit__`
            # -- confirmed by reading it directly: plain, non-`with`
            # `TestClient(app)` construction, used everywhere else in this
            # file, never triggers lifespan at all).
        self.assertEqual(called, [True])


class PidLivenessTests(unittest.TestCase):
    """`_pid_alive_posix` is reached only ever with `windows=False`, from
    `_profile_locked`'s POSIX branch -- on a POSIX host this suite does not
    run on (this box is Windows; see service.py's module docstring). So
    these exercise its interpretation of `os.kill`'s documented exception
    contract by mocking `os.kill` itself, rather than trusting a real
    POSIX process to behave as documented -- the same "reasoned, not
    observed" split `_profile_locked`'s own docstring names."""

    def test_a_process_lookup_error_means_dead(self):
        with mock.patch("service.os.kill", side_effect=ProcessLookupError()):
            self.assertFalse(service._pid_alive_posix(999999))

    def test_no_error_means_alive(self):
        with mock.patch("service.os.kill", return_value=None):
            self.assertTrue(service._pid_alive_posix(1))

    def test_a_permission_error_still_means_alive(self):
        # Exists, just not ours to signal -- still a live process, not a
        # gone one; treating this as "dead" would be the dangerous
        # direction to be wrong in.
        with mock.patch("service.os.kill", side_effect=PermissionError()):
            self.assertTrue(service._pid_alive_posix(1))

    def test_an_unexpected_os_error_is_read_as_alive_not_dead(self):
        # The safe failure mode when the check itself cannot complete:
        # assume alive, leave the directory, let a later sweep retry.
        with mock.patch("service.os.kill", side_effect=OSError("weird")):
            self.assertTrue(service._pid_alive_posix(1))


def _win_exclusive_open(path: Path):
    """Open `path` the way Chromium opens its own `lockfile` -- share mode
    0, i.e. genuinely exclusive -- via raw `CreateFileW`. Python's portable
    `os.open` cannot request this: confirmed directly on this box that two
    independent `os.open(path, os.O_RDWR)` calls on the same file do NOT
    conflict with each other at all, so a test that used `os.open` on both
    sides to simulate "held open" would pass without proving anything.
    Returns the raw handle; the caller closes it with
    `ctypes.windll.kernel32.CloseHandle`. Windows-only, stdlib-only (no new
    dependency for a test-only need)."""
    GENERIC_READ = 0x80000000
    GENERIC_WRITE = 0x40000000
    CREATE_ALWAYS = 2
    handle = ctypes.windll.kernel32.CreateFileW(
        str(path), GENERIC_READ | GENERIC_WRITE, 0, None, CREATE_ALWAYS, 0, None
    )
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError()
    return handle


@unittest.skipUnless(sys.platform == "win32", "exercises the Windows lockfile branch specifically")
class WindowsProfileLockTests(unittest.TestCase):
    """`_profile_locked`'s Windows branch (`windows=True`), against a real
    exclusive file handle -- not a mock -- confirmed to actually conflict
    the way Chromium's own open of the same file does (see
    `_win_exclusive_open` for why a plain `os.open` could not be used to
    prove this)."""

    def test_a_missing_lockfile_is_not_locked(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertFalse(service._profile_locked(Path(d), windows=True))

    def test_an_exclusively_held_lockfile_is_locked(self):
        with tempfile.TemporaryDirectory() as d:
            lock_path = Path(d) / "lockfile"
            handle = _win_exclusive_open(lock_path)
            try:
                self.assertTrue(service._profile_locked(Path(d), windows=True))
            finally:
                ctypes.windll.kernel32.CloseHandle(handle)

    def test_a_released_lockfile_is_not_locked(self):
        with tempfile.TemporaryDirectory() as d:
            lock_path = Path(d) / "lockfile"
            handle = _win_exclusive_open(lock_path)
            ctypes.windll.kernel32.CloseHandle(handle)
            self.assertFalse(service._profile_locked(Path(d), windows=True))


class PosixProfileLockTests(unittest.TestCase):
    """`_profile_locked`'s POSIX branch (`windows=False`) -- reached for
    real on this Windows box (the branch itself runs; nothing here is
    skipped), with `os.readlink` and `_pid_alive_posix` mocked at the
    boundary where this box genuinely cannot go further: this account
    cannot create the real `SingletonLock` symlink Chromium would
    (confirmed directly: `os.symlink` here raises `WinError 1314`, no
    privilege held), and `os.kill(pid, 0)` does not carry POSIX's meaning
    on Windows regardless. What IS real here: the parsing of the link
    target and the decision each outcome leads to."""

    def test_a_missing_singleton_lock_is_not_locked(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertFalse(service._profile_locked(Path(d), windows=False))

    def test_a_dead_pid_in_the_link_target_is_not_locked(self):
        with tempfile.TemporaryDirectory() as d:
            with mock.patch("service.os.readlink", return_value="somehost-424242"):
                with mock.patch("service._pid_alive_posix", return_value=False) as alive:
                    self.assertFalse(service._profile_locked(Path(d), windows=False))
                alive.assert_called_once_with(424242)

    def test_a_live_pid_in_the_link_target_is_locked(self):
        with tempfile.TemporaryDirectory() as d:
            with mock.patch("service.os.readlink", return_value="somehost-1"):
                with mock.patch("service._pid_alive_posix", return_value=True):
                    self.assertTrue(service._profile_locked(Path(d), windows=False))

    def test_an_unparseable_link_target_is_not_locked(self):
        with tempfile.TemporaryDirectory() as d:
            with mock.patch("service.os.readlink", return_value="not-shaped-like-a-pid-"):
                self.assertFalse(service._profile_locked(Path(d), windows=False))


class SweepLogicTests(unittest.TestCase):
    """`_sweep_orphaned_profiles`'s own decisions -- which names in
    `tmp_dir` qualify at all, whether they are old enough, and whether a
    profile directory's lock is even checked (an artifacts directory's
    never is -- see the function's docstring for why) -- against a fully
    isolated fake `tmp_dir`, never the real one. `_profile_locked` is
    mocked throughout: what it returns is `WindowsProfileLockTests` and
    `PosixProfileLockTests`'s job, not this one's."""

    @staticmethod
    def _mkold(base: Path, name: str, age_s: float) -> Path:
        p = base / name
        p.mkdir()
        stamp = time.time() - age_s
        os.utime(p, (stamp, stamp))
        return p

    def test_a_name_matching_neither_prefix_is_left_alone(self):
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            self._mkold(base, "some-other-temp-dir-abc123", 3600)
            with mock.patch("service._profile_locked", return_value=False):
                removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base, min_age_s=60)
            self.assertEqual(removed, [])
            self.assertTrue((base / "some-other-temp-dir-abc123").exists())

    def test_a_file_is_never_swept_even_with_a_matching_prefix(self):
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            f = base / f"{service.PROFILE_PREFIX}notadir"
            f.write_text("x")
            stamp = time.time() - 3600
            os.utime(f, (stamp, stamp))
            # `shutil.rmtree` on a non-directory path raises `NotADirectoryError`
            # (an `OSError`), which `ignore_errors=True` alone happens to
            # swallow, and the post-condition check would then also see the
            # file untouched -- so a broken `is_dir()` filter can hide
            # behind both of those and still leave this test green (found
            # directly: deleting the filter here did not turn this red).
            # `rmtree.assert_not_called()` targets the filter itself,
            # independent of what `rmtree` happens to do with a bad path.
            with mock.patch("service.shutil.rmtree") as rmtree:
                removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base, min_age_s=60)
            rmtree.assert_not_called()
            self.assertEqual(removed, [])
            self.assertTrue(f.exists())

    def test_too_young_is_kept_without_even_asking_whether_it_is_locked(self):
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            self._mkold(base, f"{service.PROFILE_PREFIX}young", 5)
            with mock.patch("service._profile_locked", return_value=False) as locked:
                removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base, min_age_s=60)
            self.assertEqual(removed, [])
            locked.assert_not_called()

    def test_old_and_unlocked_profile_dir_is_removed(self):
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            name = f"{service.PROFILE_PREFIX}dead"
            self._mkold(base, name, 3600)
            with mock.patch("service._profile_locked", return_value=False):
                removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base, min_age_s=60)
            self.assertEqual(removed, [name])
            self.assertFalse((base / name).exists())

    def test_old_but_locked_profile_dir_is_kept(self):
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            name = f"{service.PROFILE_PREFIX}alive"
            self._mkold(base, name, 3600)
            with mock.patch("service._profile_locked", return_value=True):
                removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base, min_age_s=60)
            self.assertEqual(removed, [])
            self.assertTrue((base / name).exists())

    def test_old_artifacts_dir_is_removed_without_ever_checking_a_lock(self):
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            name = f"{service.ARTIFACTS_PREFIX}dead"
            self._mkold(base, name, 3600)
            with mock.patch("service._profile_locked") as locked:
                removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base, min_age_s=60)
            locked.assert_not_called()
            self.assertEqual(removed, [name])

    def test_a_directory_that_survives_rmtree_is_not_reported_as_removed(self):
        # Regression test: `shutil.rmtree(..., ignore_errors=True)` can
        # partially fail -- one straggler file still held open -- and say
        # nothing about it either way. An earlier version of this function
        # trusted it unconditionally and reported the directory removed
        # regardless; caught by RealCleanupTests' crash test racing a real
        # process tree's last file releasing after its lockfile already
        # had. Reproduced here without a real process: `shutil.rmtree`
        # mocked to a no-op, so the directory provably still exists
        # afterward.
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            name = f"{service.PROFILE_PREFIX}stubborn"
            self._mkold(base, name, 3600)
            with mock.patch("service._profile_locked", return_value=False):
                with mock.patch("service.shutil.rmtree") as rmtree:
                    removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base, min_age_s=60)
            rmtree.assert_called_once()
            self.assertEqual(removed, [], "reported a directory removed that rmtree never actually touched")
            self.assertTrue((base / name).exists())

    def test_the_windows_flag_passed_in_reaches_the_lock_check_unchanged(self):
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            self._mkold(base, f"{service.PROFILE_PREFIX}x", 3600)
            with mock.patch("service._profile_locked", return_value=False) as locked:
                service._sweep_orphaned_profiles(windows=False, tmp_dir=base, min_age_s=60)
            _, kwargs = locked.call_args
            self.assertEqual(kwargs["windows"], False)

    def test_the_default_min_age_is_the_documented_constant(self):
        # No min_age_s override here -- this is SWEEP_MIN_AGE_S itself, end
        # to end, not a stand-in for it.
        with tempfile.TemporaryDirectory() as d:
            base = Path(d)
            self._mkold(base, f"{service.PROFILE_PREFIX}tooyoung", service.SWEEP_MIN_AGE_S - 5)
            self._mkold(base, f"{service.PROFILE_PREFIX}oldenough", service.SWEEP_MIN_AGE_S + 5)
            with mock.patch("service._profile_locked", return_value=False):
                removed = service._sweep_orphaned_profiles(windows=True, tmp_dir=base)
            self.assertEqual(removed, [f"{service.PROFILE_PREFIX}oldenough"])


def _list_playwright_temp_names() -> set[str]:
    """A full-`%TEMP%`-style listing, narrowed to the two prefixes this
    section cares about -- the diff-before-and-after technique
    docs/Decisions.md's own "A cleanup claim is a testable assertion" entry
    names as how this project verifies a cleanup claim at all, applied
    here instead of trusting the process table."""
    tmp = Path(tempfile.gettempdir())
    return {
        p.name for p in tmp.iterdir()
        if p.name.startswith(service.PROFILE_PREFIX) or p.name.startswith(service.ARTIFACTS_PREFIX)
    }


def _snapshot_profile_files(profile_dir: Path) -> set[str]:
    """Every file under `profile_dir`, as paths relative to it. Exists
    because a top-level `profile_dir.exists()` check is not enough to prove
    a live profile survived a sweep untouched: found directly, by breaking
    the lock check on purpose during this suite's own development,
    `shutil.rmtree(profile_dir, ignore_errors=True)` against a directory
    Chromium is actively using did NOT remove the directory itself (one
    file -- observed as the top-level `lockfile`, or one of the several
    per-store LevelDB `LOCK` files underneath -- stayed exclusively open
    and blocked it), but it silently deleted 61 of the 124 files inside it
    anyway (Crashpad settings, cache data, several `Default/*/LOCK` and
    `*/CURRENT` files) while the browser kept running and `page.title()`
    kept answering. A directory-level `.exists()` check, and even a
    still-responds smoke check, both pass right through that. Comparing
    this snapshot before and after a sweep is what actually catches it."""
    return {
        str(p.relative_to(profile_dir)) for p in profile_dir.rglob("*") if p.is_file()
    }


def _find_driver_pid(profile_dir_name: str) -> int | None:
    """Windows-only test helper: the pid of the Playwright driver (node.exe)
    that launched the Chromium using `profile_dir_name`, found by locating
    the one chrome.exe whose command line names that directory and carries
    no `--type=` (the main process -- every helper process this service's
    one launch produces, crashpad-handler/gpu-process/utility/renderer, has
    one; confirmed directly against a real launch on this box) and reading
    its ParentProcessId. Shells out to `powershell.exe`/CIM rather than
    adding a process-inspection dependency this service does not otherwise
    need, for a need that belongs to this test, not to the shipped code."""
    ps = (
        "Get-CimInstance Win32_Process -Filter \"Name='chrome.exe'\" | "
        "Select-Object ProcessId, ParentProcessId, CommandLine | ConvertTo-Json"
    )
    out = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command", ps],
        capture_output=True, text=True, timeout=20,
    )
    try:
        data = json.loads(out.stdout or "null")
    except json.JSONDecodeError:
        return None
    if data is None:
        return None
    if isinstance(data, dict):
        data = [data]
    for row in data:
        cmd = row.get("CommandLine") or ""
        if profile_dir_name in cmd and "--type=" not in cmd:
            return row.get("ParentProcessId")
    return None


def _force_kill_tree(pid: int) -> None:
    """The Windows half of exactly what `eidolon_tools::shell::kill_group`
    (crates/tools/src/shell.rs) does to stop this service in real
    operation -- `taskkill /T /F`, a forceful, uncatchable tree kill.
    Used here to make the crash this section proves against a real one,
    not a stand-in: no exit handler in the driver, no `close` event, no
    `removeFolders` -- the same absence `docs/Decisions.md`'s "a profile
    directory outlives the process that made it by design" describes."""
    subprocess.run(
        ["taskkill", "/PID", str(pid), "/T", "/F"],
        capture_output=True, text=True, timeout=20,
    )


@unittest.skipUnless(service._chromium_installed(), "chromium is not installed in this venv")
@unittest.skipUnless(sys.platform == "win32", "the crash simulation shells out to taskkill/CIM -- observed on Windows only, the same split _profile_locked's own docstring names")
class RealCleanupTests(unittest.IsolatedAsyncioTestCase):
    """Non-vacuous proof for both halves of the cleanup contract, against a
    real headless Chromium, real OS processes, and real directories under
    `tempfile.gettempdir()` -- not the process table. This project has
    already been burned by trusting the process table once
    (docs/Decisions.md, 2026-09-19, "A cleanup claim is a testable
    assertion": 13 directories outlived work that checked the process table
    and reported zero orphans), which is the whole reason this class
    diffs a real directory listing before and after instead."""

    async def asyncTearDown(self):
        # Whatever a test above left in STATE gets a real chance to close
        # cleanly, exactly like the other real-browser classes in this
        # file already do -- this class kills processes by pid directly,
        # never through STATE, so this is very rarely the thing that
        # actually cleans up, but it is what keeps a failed assertion
        # midway through a test from leaking a live Chromium past it.
        if service.STATE.browser is not None:
            try:
                await service.STATE.browser.close()
            except Exception:
                pass
        if service.STATE.playwright is not None:
            try:
                await service.STATE.playwright.stop()
            except Exception:
                pass
        service.STATE.browser = None
        service.STATE.context = None
        service.STATE.page = None
        service.STATE.playwright = None
        service.STATE.known_refs = frozenset()

    async def test_shutdown_cleanup_removes_a_directory_that_existed_during_the_session(self):
        """Half 1, proven the way the brief asks: a directory exists during
        a session and is gone after it -- the directory itself, before and
        after, not merely that `close()` was awaited without raising."""
        before = _list_playwright_temp_names()
        await service._open({"url": "data:text/html,<html><body>one</body></html>"})
        created = _list_playwright_temp_names() - before
        self.assertTrue(created, "no new playwright temp dir appeared -- nothing to prove cleanup against")
        for name in created:
            self.assertTrue((Path(tempfile.gettempdir()) / name).exists(), f"{name} missing DURING the session")

        await service._close_browser()

        # Playwright's own cleanup runs off the driver noticing chromium's
        # exit, asynchronously to our own await above -- fast in isolation
        # (well under two seconds, observed while designing this test) but
        # not bounded, and this suite launches many real Chromiums back to
        # back (this file's own module docstring, plus every other
        # real-browser class above); a fixed short sleep here was observed
        # to flake under that load once the suite grew this large. Poll
        # instead of guessing a single wait long enough for the busiest
        # case, with a generous ceiling -- this is still the same
        # assertion, just not gambling on exact timing to make it.
        deadline = time.monotonic() + 15
        still_there = created & _list_playwright_temp_names()
        while still_there and time.monotonic() < deadline:
            await asyncio.sleep(0.5)
            still_there = created & _list_playwright_temp_names()

        self.assertEqual(still_there, set(), f"still on disk after a graceful close: {still_there}")

    async def test_a_forcefully_killed_browser_leaves_a_real_orphan_and_the_next_sweep_removes_it(self):
        """Half 2, proven the way the brief asks: actually kill a browser
        without letting it clean up, confirm the orphan is there, then show
        the next start removes it."""
        before = _list_playwright_temp_names()
        await service._open({"url": "data:text/html,<html><body>two</body></html>"})
        created = _list_playwright_temp_names() - before
        profile_names = [n for n in created if n.startswith(service.PROFILE_PREFIX)]
        self.assertEqual(len(profile_names), 1, created)
        profile_dir = Path(tempfile.gettempdir()) / profile_names[0]

        driver_pid = _find_driver_pid(profile_names[0])
        self.assertIsNotNone(driver_pid, "could not find the driver process behind this launch")

        # Detach STATE from what is about to die -- this IS the crash: no
        # close(), no stop(), asyncTearDown above has nothing left to do
        # for this session. A real crash would not leave a Python object
        # around to null out either; this is the closest an in-process test
        # gets to that, without it changing what actually gets killed.
        service.STATE.browser = None
        service.STATE.context = None
        service.STATE.page = None
        service.STATE.playwright = None

        # Killing the driver out from under Playwright's own connection
        # breaks a pipe a background asyncio task is still reading from;
        # this test already let go of every reference above (the same
        # shape as a real crash, where nothing is left holding on either),
        # so that task's own exception has nowhere to go and asyncio logs
        # it to stderr as unretrieved. Expected noise from the crash
        # itself, not a failure -- silenced for this window so it does not
        # read as one, and restored immediately after.
        loop = asyncio.get_running_loop()
        previous_handler = loop.get_exception_handler()
        loop.set_exception_handler(lambda _loop, _context: None)
        try:
            _force_kill_tree(driver_pid)
            # `taskkill /T /F` (confirmed directly, with its own -v output
            # captured while designing this test) reports SUCCESS for every
            # pid in the tree as soon as it has called TerminateProcess on
            # each -- not the same moment Windows finishes tearing each one
            # down and releasing its handle on `lockfile`. Measured directly
            # against this box, under this suite's own load (many real
            # Chromium launches before this one): as long as ~4 seconds for
            # one launch's full tree to actually stop holding it. Poll the
            # exact signal the sweep itself depends on, rather than guess a
            # fixed wait -- this doubles as confirming `_profile_locked`
            # correctly observes the transition, not just the sweep.
            deadline = time.monotonic() + 30
            while service._profile_locked(profile_dir, windows=True) and time.monotonic() < deadline:
                await asyncio.sleep(0.5)
        finally:
            loop.set_exception_handler(previous_handler)

        self.assertFalse(
            service._profile_locked(profile_dir, windows=True),
            "the killed tree still held its lock after a generous wait -- it did not actually die",
        )

        orphaned = created & _list_playwright_temp_names()
        self.assertEqual(
            orphaned, created,
            f"expected a real orphan; some entries vanished on their own anyway: {created - orphaned}",
        )

        # `_profile_locked` returning False proves `lockfile` itself is
        # free; it does not promise every other file a slower helper
        # process was still touching is too (`_sweep_orphaned_profiles`'s
        # own comment on `ignore_errors=True`, added after this test first
        # caught the gap: an `rmtree` racing that last straggler can
        # partially fail, and now correctly leaves the name out of
        # `removed` rather than over-claiming). A production sweep never
        # meets this race -- it only ever looks at directories already past
        # `SWEEP_MIN_AGE_S`, minutes to hours old by the brief's own numbers,
        # not seconds -- so retrying here a few times is this test making up
        # for the margin `min_age_s=0` deliberately gave away, not a gap in
        # the sweep itself.
        # Accumulated across attempts, not just the last one: once a name
        # is actually gone, a later call's own `tmp_dir.iterdir()` never
        # sees it again to report it a second time (an earlier version of
        # this loop compared only the latest call's return value against
        # `created` and could fail even after both directories were gone,
        # one per attempt, exactly the reporting discipline the fix above
        # is for).
        removed_total: set[str] = set()
        deadline = time.monotonic() + 15
        while not (removed_total >= created) and time.monotonic() < deadline:
            removed_total |= set(
                service._sweep_orphaned_profiles(
                    windows=True, tmp_dir=Path(tempfile.gettempdir()), min_age_s=0
                )
            )
            if not (removed_total >= created):
                await asyncio.sleep(0.5)
        self.assertTrue(
            removed_total >= created,
            f"sweep did not remove the orphan: removed={removed_total} orphan={created}",
        )
        self.assertEqual(created & _list_playwright_temp_names(), set())

    async def test_the_sweep_does_not_remove_a_directory_a_live_browser_is_using(self):
        """The negative case the brief calls out as the one that matters:
        a second, genuinely live browser -- standing in for a second
        eidolon process on the same box, named in the brief as a real case,
        not a hypothetical -- must keep its locked profile directory
        through a sweep aggressive enough to catch a real crash.
        `min_age_s=0` is deliberately the setting used here, not the
        default: if age were the only thing protecting this directory,
        `min_age_s=0` would remove it regardless of how alive it is. Only
        `_profile_locked` can save it under this setting, so surviving
        proves the lock check itself, not the grace window standing in for
        it.

        The paired artifacts directory is a deliberately different story,
        proven here rather than assumed: it carries no lock of its own
        (`_sweep_orphaned_profiles`'s docstring -- Chromium does not know it
        exists, and this service never writes into it), so under
        `min_age_s=0` specifically, with age no longer protecting anything
        either, sweeping it even though this browser is alive is accepted,
        documented behaviour, not a bug -- caught by an earlier, wrong
        version of this test that asserted otherwise. What has to be true
        instead, and is checked directly: the browser itself still works
        after its artifacts directory is gone.

        Directory-level survival is not trusted as proof by itself here --
        `_snapshot_profile_files`'s own docstring explains why a top-level
        `.exists()` check passed straight through a real partial-deletion
        bug during this suite's development. This test snapshots every
        file under the profile directory before and after the sweep and
        requires the set to be identical, which is what actually would
        have caught that.
        """
        before = _list_playwright_temp_names()
        second = service.Browser()
        with mock.patch("service.STATE", second):
            await service._open({"url": "data:text/html,<html><body>three</body></html>"})
        try:
            created = _list_playwright_temp_names() - before
            profile_names = [n for n in created if n.startswith(service.PROFILE_PREFIX)]
            self.assertEqual(len(profile_names), 1, created)
            profile_name = profile_names[0]
            profile_dir = Path(tempfile.gettempdir()) / profile_name
            files_before = _snapshot_profile_files(profile_dir)

            removed = service._sweep_orphaned_profiles(
                windows=True, tmp_dir=Path(tempfile.gettempdir()), min_age_s=0
            )

            # The one directory that actually holds this session's state
            # must survive -- the point of the whole check.
            self.assertNotIn(profile_name, removed)
            self.assertTrue(profile_dir.exists())

            # Not just present, but untouched: no file inside it was lost
            # to a partial `rmtree` that stopped short of the top level.
            # A subset check, not equality -- confirmed necessary, not just
            # cautious: a still-live browser legitimately writes new files
            # of its own in this same window (observed directly here, once:
            # a new `GrShaderCache` entry) which is normal activity, not
            # sweep damage, and must not fail this test. Only something
            # that existed before and is gone after is the failure this
            # test exists to catch.
            files_after = _snapshot_profile_files(profile_dir)
            missing = files_before - files_after
            self.assertEqual(missing, set())

            # Not just the directory surviving: the browser itself still
            # actually works after the sweep ran past it (its artifacts
            # directory included, whether or not the sweep took it).
            title = await second.page.title()
            self.assertIsInstance(title, str)
        finally:
            if second.browser is not None:
                await second.browser.close()
            if second.playwright is not None:
                await second.playwright.stop()


if __name__ == "__main__":
    unittest.main()
