# POSIX blocker inventory — bonsai2 (Windows → Linux / Nix flake)

Read-only survey. No file in the project was created, modified, or deleted; this
document is the only file written.

* **Root surveyed:** `C:\Users\dxcen\Projects\bonsai2`
* **Directories in scope:** `jev/`, `extensions/`, `bin/`, `models/`
* **Out of scope by instruction:** `eidolon/` (Rust), and every `.venv/`,
  `__pycache__/`, `node_modules/`, `target/`, `.git/`
* **Method:** exhaustive pattern grep over every file in the four directories
  (patterns listed in the Coverage section), then every reported line was read
  back individually and the text below is copied from that read.

**Legend**

* `path:line` — real line numbers, each one read back before being written down.
  Every line-numbered citation in the ten category tables was then re-checked
  mechanically by reading that exact line out of the file and asserting the
  quoted text is a substring of it; all of them pass.
* *Offending text* is verbatim **with leading indentation removed**. Everything
  else on the line — quotes, escapes, trailing `|` — is exactly as it appears.
  Three markdown escapes are used so the tables render, and nothing else is
  altered: a literal `|` is written `\|`, a literal trailing backtick (a
  PowerShell line continuation) is written `` \` ``, and a quote that itself
  begins or ends with a backtick is padded by one space inside the code span.
* `Cats` — every category the line belongs to, not only the table it sits in.
  A line can be an instance of two categories, so **category row counts sum to
  more than the distinct-line count**; both are given at the end.
* Severity: **BLOCKER** = Linux cannot run this at all · **DEGRADED** = runs but
  behaves differently · **COSMETIC** = no runtime effect on Linux.
* Rows marked *(docstring)*, *(comment)* or *(test)* are inside a docstring,
  comment, or test fixture. Their execution status is stated in the row. None of
  them is counted as a runtime blocker on that basis alone.

---

## Category 1 — Absolute Windows paths (drive letters, raw backslash paths)

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `jev/server.py:62` | `os.getenv("JEVLIKE_ROOT", r"C:\Users\dxcen\Projects\cms-agent\models\jevlike")` | 1,2 | **BLOCKER** | Drop the default (use `os.getenv("JEVLIKE_ROOT")`) — `_jevlike()` already raises a clear `RuntimeError` naming the variable. |
| `jev/server.py:66` | `OPENJEV_DIR = Path(os.getenv("OPENJEV_DIR", r"C:\Users\dxcen\Projects\bonsai2\models\openjev"))` | 1 | **BLOCKER** | Default to `Path(__file__).resolve().parent.parent / "models" / "openjev"`. |
| `extensions/jev/extension.rn:44` | `command: "'C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv/Scripts/python.exe' -u server.py",` | 1,2,7 | **BLOCKER** | Copy the platform-branch idiom from `extensions/browser/extension.rn:31` (`test -f "{dir}/../.venv/bin/python" && … \|\| …`). |
| `bin/hoot.ps1:125` | `$shim = Join-Path $env:USERPROFILE '.local\bin\open-webui.exe'` | 1,2,3,5,10 | **BLOCKER** | Not portable as written; the POSIX equivalent is `Join-Path $env:HOME '.local/bin/open-webui'`. |
| `bin/hoot.ps1:409` | `$candidate = Join-Path $Root "eidolon\target\$p\eidolon.exe"` | 1,3,10 | **BLOCKER** | `Join-Path $Root "eidolon/target/$p/eidolon"` (no `.exe`) under a platform test. |
| `bin/hoot.ps1:427` | `$cfg = Join-Path $env:APPDATA 'eidolon\config.toml'` | 1,5,10 | **BLOCKER** | Use `Get-EidolonConfigDir` (already defined at `bin/hoot.ps1:130`, and already POSIX-aware) instead of `$env:APPDATA` directly. |
| `bin/hoot.ps1:708` | `"hoot uses   $Bin\llama-server.exe"` | 1,4,10 | **BLOCKER** | Interpolate the resolved binary name instead of hardcoding `\llama-server.exe`. |
| `bin/PATH-backup.txt:1` | `C:\Users\dxcen\.cargo\bin;C:\Users\dxcen\AppData\Local\Programs\Python\Python314\Scripts\;…` (whole file, 537 bytes, one line) | 1,10 | COSMETIC | Nothing in the surveyed tree references it; delete it or replace with a note — it hardcodes this user's home directory. |
| `jev/automation/graph.py:5` | `` `C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv`), so this is a `` | 1,2 | COSMETIC *(docstring — module header, never executed)* | Reword to "the jevlike venv" without the absolute path. |
| `jev/test_server.py:6` | `C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\test_server.py` | 1,2 | COSMETIC *(docstring)* | Replace with a POSIX/Windows pair of invocations, or a `<venv>/python` placeholder. |
| `jev/tests/test_schema.py:6` | `C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\tests\\test_schema.py` | 1,2 | COSMETIC *(docstring)* | Same as above. |
| `jev/tests/test_server_automation.py:10` | `C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\tests\\test_server_automation.py` | 1,2 | COSMETIC *(docstring)* | Same as above. |
| `jev/tests/test_guards_openjev.py:17` | `JEV_TEST_OPENJEV=1 <venv>/python.exe -m unittest tests.test_guards_openjev -v` | 1 | COSMETIC *(docstring — already uses a placeholder, only `.exe` is Windows)* | Drop the `.exe`. |
| `extensions/browser/test_server.py:7` | `C:\\Users\\dxcen\\Projects\\bonsai2\\extensions\\browser\\.venv\\Scripts\\python -m pip install -r requirements-test.txt` | 1 | COSMETIC *(docstring)* | Replace with the two-line POSIX/Windows pair `service.py:30-31` already uses. |
| `extensions/browser/test_server.py:8` | `C:\\Users\\dxcen\\Projects\\bonsai2\\extensions\\browser\\.venv\\Scripts\\python -m unittest test_server -v` | 1 | COSMETIC *(docstring)* | Same as above. |
| `extensions/browser/service.py:31` | `.venv\\Scripts\\pip install -r requirements.txt      # Windows` | 1,3 | COSMETIC *(docstring — the POSIX form is on the line above, `:30`)* | None needed; it documents the Windows case on purpose. |
| `extensions/browser/service.py:40` | ``cache (`~/.cache/ms-playwright` or `%LOCALAPPDATA%\\ms-playwright`), so the`` | 1,5 | COSMETIC *(docstring)* | None needed. |
| `jev/test_server.py:110` | `os.environ, {"LOCALAPPDATA": "C:\\fake\\local"}` | 1,5 | DEGRADED *(test — the class has no `skipUnless`, so this runs on Linux too)* | Harmless: both sides of the `assertEqual` at `:112` are `PosixPath` of the same string, so it passes on Linux. Skip the class on non-Windows if the intent is Windows-only. |
| `jev/test_server.py:112` | `self.assertEqual(server._cache_dir(), Path("C:\\fake\\local"))` | 1 | DEGRADED *(test)* | Same as above. |

**Checked in this category and clean:** `models/openjev/config.json` and
`models/openjev/tokenizer_config.json` contain no paths; no `.gguf` weight file
was opened.

---

## Category 2 — Paths pointing OUTSIDE this project into another project

These are the sharpest ones: a flake must declare every input, and a path into a
sibling checkout cannot be declared as a flake input at all.

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `jev/server.py:62` | `os.getenv("JEVLIKE_ROOT", r"C:\Users\dxcen\Projects\cms-agent\models\jevlike")` | 1,2 | **BLOCKER** | Delete the default; make `JEVLIKE_ROOT` a required, flake-declared input. Note the comment at `jev/server.py:59-60` ("Imported from the sibling checkout rather than vendored") — this is a deliberate design decision, not an oversight, so the porter needs a decision, not a tidy-up. |
| `extensions/jev/extension.rn:44` | `command: "'C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv/Scripts/python.exe' -u server.py",` | 1,2,7 | **BLOCKER** | Same decision, in a second place: the interpreter is the sibling checkout's venv. |
| `jev/automation/graph.py:5` | `` `C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv`), so this is a `` | 1,2 | COSMETIC *(docstring)* | Reword. |
| `jev/test_server.py:6` | `C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\test_server.py` | 1,2 | COSMETIC *(docstring)* | Reword — but note the *test command* it documents genuinely depends on the sibling venv. |
| `jev/tests/test_schema.py:6` | `C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\tests\\test_schema.py` | 1,2 | COSMETIC *(docstring)* | Same. |
| `jev/tests/test_server_automation.py:10` | `C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\tests\\test_server_automation.py` | 1,2 | COSMETIC *(docstring)* | Same. |
| `bin/hoot.ps1:125` | `$shim = Join-Path $env:USERPROFILE '.local\bin\open-webui.exe'` | 1,2,3,5,10 | **BLOCKER** | Outside the repo but not another *source checkout* — it is a `uv tool install` shim. Declare `open-webui` as a flake input instead of probing a user profile. |

**Also outside the project but not a source checkout** (called out separately
because a flake still cannot reproduce them):

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `bin/hoot.ps1:340` | `$pkg = Join-Path $env:APPDATA 'uv\tools\open-webui\Lib\site-packages\open_webui'` | 2,3,5,10 | **BLOCKER** | Patch the `open-webui` package as a flake derivation instead of reaching into a user-profile install at launch. |
| `bin/hoot.ps1:427` | `$cfg = Join-Path $env:APPDATA 'eidolon\config.toml'` | 1,5,10 | **BLOCKER** | Generate the config inside the project (`$Root`) rather than in the user's config dir. |

Two further lines that `bin/hoot.ps1` uses to reach **outside the four surveyed
directories while staying inside this repository** — `bin/hoot.ps1:655` and
`bin/hoot.ps1:671`, which invoke the repo-root `setup-bonsai2.ps1` — are *not*
category 2 (the target is inside this project). They are listed once, under
category 4, and flagged again under category 10.

---

## Category 3 — `os.path` / `pathlib` separator assumptions, hardcoded `"\\"`

**Finding of substance: none in the project's own Python.** All path building in
`jev/` and `extensions/` goes through `pathlib` (`/` operator, `Path.joinpath`),
and `models/openjev/modeling_openjev.py:184-194` uses `os.path.join`, which is
portable. There is **no** `os.path.join` called with backslash literals, no
`os.sep`/`os.altsep`, and no `"\\"`-joined path anywhere in `jev/` or
`extensions/`. Every backslash hit in those two trees is a regex escape, a
string escape, or a docstring (see the false-positive list at the end).

Everything below is in `bin/` (a Windows-only launcher — see category 10) or in
a docstring.

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `bin/hoot.ps1:125` | `$shim = Join-Path $env:USERPROFILE '.local\bin\open-webui.exe'` | 1,2,3,5,10 | **BLOCKER** | `'.local/bin/open-webui'`. |
| `bin/hoot.ps1:335` | `$skin = Join-Path $Root 'webui\custom.css'` | 3,10 | **BLOCKER** | `Join-Path $Root 'webui' 'custom.css'` — one component per `Join-Path` argument. |
| `bin/hoot.ps1:340` | `$pkg = Join-Path $env:APPDATA 'uv\tools\open-webui\Lib\site-packages\open_webui'` | 2,3,5,10 | **BLOCKER** | Same: pass each component separately. |
| `bin/hoot.ps1:343` | `$brand = Join-Path $Root 'webui\brand'` | 3,10 | **BLOCKER** | `Join-Path $Root 'webui' 'brand'`. |
| `bin/hoot.ps1:346` | `foreach ($d in @('static', 'frontend\static')) {` | 3,10 | **BLOCKER** | `@('static', ('frontend/static'))`, or nest two `Join-Path` calls. |
| `bin/hoot.ps1:356` | `$ldr = Join-Path $Root 'webui\loader.js'` | 3,10 | **BLOCKER** | `Join-Path $Root 'webui' 'loader.js'`. |
| `bin/hoot.ps1:370` | `$idx = Join-Path $pkg 'frontend\index.html'` | 3,10 | **BLOCKER** | `Join-Path $pkg 'frontend' 'index.html'`. |
| `bin/hoot.ps1:429` | `$want = $Extensions -replace '\\', '/'` | 3,10 | DEGRADED | The conversion proves the assumption; once `Join-Path` inputs are components this becomes a no-op on both platforms and can stay or go. |
| `bin/hoot.ps1:692` | `$out = & $eidolon ext dir ($Extensions -replace '\\', '/')` | 3,10 | DEGRADED | Same as above. |
| `extensions/browser/service.py:31` | `.venv\\Scripts\\pip install -r requirements.txt      # Windows` | 1,3 | COSMETIC *(docstring)* | None needed. |

---

## Category 4 — Windows-only APIs

`msvcrt` is imported **only** inside a correct `sys.platform == "win32"` guard
(`jev/_paths.py:72-73`) and `ctypes.windll` / `powershell.exe` / `taskkill` in
the tests are behind `@unittest.skipUnless(sys.platform == "win32", …)`. Those
are listed here for completeness with their execution status; they are **not**
blockers. The real blockers are all in `bin/`.

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `bin/hoot.cmd:2` | `powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0hoot.ps1" %*` | 4,7,10 | **BLOCKER** | Needs a POSIX launcher (`bin/hoot` `sh` script) — `%~dp0` and `-ExecutionPolicy` have no POSIX meaning. |
| `bin/hoot.ps1:97` | `Get-Process llama-server -ErrorAction SilentlyContinue \|` | 4,10 | **BLOCKER** | `Get-Process` is unavailable outside PowerShell; the whole process-management half needs a POSIX rewrite (`pgrep`/`ps`). |
| `bin/hoot.ps1:101` | `Get-CimInstance Win32_Process -ErrorAction SilentlyContinue \|` | 4,10 | **BLOCKER** | CIM/WMI has no POSIX counterpart; use `/proc` or `pgrep -f`. |
| `bin/hoot.ps1:194` | `$p = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue \|` | 4,10 | **BLOCKER** | Same. |
| `bin/hoot.ps1:438` | `if (-not (Test-Path (Join-Path $Bin 'llama-server.exe'))) { Die 'no binaries; run: hoot setup' }` | 4,10 | **BLOCKER** | Name the binary without `.exe` and build it through the flake rather than checking for a prebuilt one. |
| `bin/hoot.ps1:448` | `& (Join-Path $Bin 'llama-server.exe') \`` | 4,10 | **BLOCKER** | Same. |
| `bin/hoot.ps1:502` | `Start-Process -FilePath (Join-Path $Bin 'llama-server.exe') \`` | 4,10 | **BLOCKER** | `Start-Process` has no POSIX counterpart; needs `&` with `&>/log &` or a supervisor. |
| `bin/hoot.ps1:533` | `Start-Process -FilePath $eidolon \`` | 4,10 | **BLOCKER** | Same. |
| `bin/hoot.ps1:552` | `Start-Process -FilePath $owui \`` | 4,10 | **BLOCKER** | Same. |
| `bin/hoot.ps1:666` | `& (Join-Path $Bin 'llama-bench.exe') -m $gguf.FullName -ngl 0 -ngl 99 -fa 1 -t 8 -p 128 -n 32 -r 1 @Rest` | 4,10 | **BLOCKER** | Drop `.exe`. |
| `bin/hoot.ps1:702` | `'edit' { Start-Process notepad.exe $Ini }` | 4,10 | **BLOCKER** | Use `$env:EDITOR` (or `vi`) instead of `notepad.exe`. |
| `bin/hoot.ps1:706` | `$other = Get-Command llama-server.exe -ErrorAction SilentlyContinue` | 4,10 | **BLOCKER** | `command -v llama-server`. |
| `bin/hoot.ps1:655` | `& (Join-Path $Root 'setup-bonsai2.ps1') -QwenQuant $Rest[0] -SkipBonsai` | 4,10 | **BLOCKER** | Invokes a repo-root `.ps1` that is outside the surveyed directories and was not surveyed. |
| `bin/hoot.ps1:671` | `& (Join-Path $Root 'setup-bonsai2.ps1') @Rest` | 4,10 | **BLOCKER** | Same file. |
| `extensions/browser/test_server.py:2011` | `handle = ctypes.windll.kernel32.CreateFileW(` | 4 | COSMETIC *(test helper inside `WindowsProfileLockTests`, which is decorated `@unittest.skipUnless(sys.platform == "win32", …)` at `:2019` — never runs on Linux; the module still imports fine because `windll` is only touched inside the function body)* | No fix needed. If the skip is ever removed, this must be deleted. |
| `extensions/browser/test_server.py:2258` | `["powershell.exe", "-NoProfile", "-Command", ps],` | 4,9 | COSMETIC *(test helper, used only by the class skipped at `:2291`)* | No fix needed. |
| `extensions/browser/test_server.py:2285` | `["taskkill", "/PID", str(pid), "/T", "/F"],` | 4,9 | COSMETIC *(same class, skipped at `:2291`)* | No fix needed. |
| `extensions/browser/test_server.py:2254` | `"Get-CimInstance Win32_Process -Filter \"Name='chrome.exe'\" \| "` | 4,9 | COSMETIC *(test helper, skipped)* | No fix needed. |

**Checked in this category and correct, not blockers:**

* `jev/_paths.py:72-73` — `if sys.platform == "win32": import msvcrt` — correctly
  guarded; the `else` branch at `:83-84` imports `fcntl` (POSIX).
* `extensions/browser/service.py:1194` — `exe = "chrome.exe" if sys.platform == "win32" else "chrome"` — correct.
* `extensions/browser/service.py:60` — `os.environ.setdefault("PLAYWRIGHT_BROWSERS_PATH", "0")` keeps the browser inside the venv, which is *good* for a hermetic flake.
* No `winreg`, no `pywin32`, no `os.startfile`, no `shell32` anywhere in the surveyed tree.

---

## Category 5 — Windows-only environment variables used as if universal

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `bin/hoot.ps1:125` | `$shim = Join-Path $env:USERPROFILE '.local\bin\open-webui.exe'` | 1,2,3,5,10 | **BLOCKER** | `$env:USERPROFILE` is `$null` on Linux; use `$HOME`. |
| `bin/hoot.ps1:340` | `$pkg = Join-Path $env:APPDATA 'uv\tools\open-webui\Lib\site-packages\open_webui'` | 2,3,5,10 | **BLOCKER** | `$env:APPDATA` is `$null` on Linux and `Join-Path` on a null base throws; resolve the package via the flake instead. |
| `bin/hoot.ps1:427` | `$cfg = Join-Path $env:APPDATA 'eidolon\config.toml'` | 1,5,10 | **BLOCKER** | The POSIX-aware `Get-EidolonConfigDir` already exists at `:130` — use it. |
| `bin/hoot.ps1:136` | `if ($env:APPDATA) { return (Join-Path $env:APPDATA 'eidolon') }` | 5,10 | DEGRADED | Guarded and followed by an `XDG_CONFIG_HOME`/`$HOME` fallback at `:137-139`, so this specific line is tolerable — but it is still PowerShell-only. |
| `jev/test_server.py:110` | `os.environ, {"LOCALAPPDATA": "C:\\fake\\local"}` | 1,5 | DEGRADED *(test, runs on Linux)* | Passes on Linux by accident (see category 1). |
| `jev/test_server.py:116` | `os.environ, {"LOCALAPPDATA": ""}` | 5 | DEGRADED *(test, runs on Linux)* | Same. |
| `jev/server.py:259` | ``on Windows, where both happen to land under `%LOCALAPPDATA%`.`` | 5 | COSMETIC *(docstring)* | None. |
| `jev/server.py:274` | ``# `_cache_dir()` itself uses above for `LOCALAPPDATA` and `XDG_CACHE_HOME` --`` | 5 | COSMETIC *(comment)* | None. |
| `jev/_paths.py:31` | ``Windows where both happen to be `%LOCALAPPDATA%`.`` | 5 | COSMETIC *(docstring)* | None. |
| `jev/automation/run.py:46` | ``to `%LOCALAPPDATA%\eidolon\extensions\jev\runs\<run-id>.jsonl`, appended`` | 5 | COSMETIC *(docstring — the code at `run.py:812-815` is platform-correct)* | Reword to "the platform cache dir". |
| `extensions/browser/service.py:40` | ``cache (`~/.cache/ms-playwright` or `%LOCALAPPDATA%\\ms-playwright`), so the`` | 1,5 | COSMETIC *(docstring)* | None. |
| `extensions/browser/service.py:1214` | ``# `%TEMP%\playwright_chromiumdev_profile-pmBKLx` paired with`` | 5 | COSMETIC *(comment)* | None. |
| `extensions/browser/service.py:1521` | ``# `/health` or anywhere request-shaped, so a slow %TEMP% (a box with a`` | 5 | COSMETIC *(comment)* | None. |

**Checked in this category and correct:** `jev/_paths.py:44` —
`return Path(os.environ.get("LOCALAPPDATA") or Path.home())` sits inside the
`sys.platform == "win32"` branch at `:43` and falls back to `Path.home()`. No
`TEMP`, `TMP`, `HOMEDRIVE`, `HOMEPATH`, or `COMPUTERNAME` is read anywhere in
the surveyed tree; temp paths go through `tempfile.gettempdir()`
(`extensions/browser/service.py:1527`), which honours `TMPDIR` on Linux.
`HOMEDRIVE`/`HOMEPATH`/`COMPUTERNAME`: zero hits.

---

## Category 6 — Locking, atomic rename, file-permission behaviour

**No `os.rename`, no `os.replace`, no `chmod`, no `umask`, and no unguarded
`os.O_BINARY` exist anywhere in the surveyed tree.** The rows here are the
places where behaviour genuinely differs between the two platforms, plus two
guarded branches listed for completeness.

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `jev/_paths.py:87` | `fcntl.flock(fd, fcntl.LOCK_EX)  # blocks until held` | 6 | DEGRADED | The Windows half (`msvcrt.locking`, `:77`) is closer to mandatory locking than POSIX `flock`, which is advisory: on Linux a process that opens the log *without* `flock` can still interleave writes. Safe today because every writer goes through `append_locked`; if that invariant is ever broken, `O_APPEND` alone is not enough on either platform. No code change needed — record the invariant. |
| `jev/_paths.py:90` | `fcntl.flock(fd, fcntl.LOCK_UN)` | 6 | COSMETIC | Symmetry only — `flock` releases on `close()`. The comment at `:64-66` already says so. |
| `jev/_paths.py:106` | `fd = os.open(path, os.O_APPEND \| os.O_CREAT \| os.O_WRONLY \| O_BINARY, 0o600)` | 6 | COSMETIC | `O_BINARY` is `0` on POSIX via the `getattr` fallback at `:57`, and mode `0o600` is honoured (further restricted by umask, never widened). |
| `extensions/browser/service.py:1361` | `fd = os.open(str(lock_path), os.O_RDWR)` | 6 | COSMETIC *(Windows branch — reached only when `windows=True`)* | The comment at `:1341-1344` documents that this depends on Windows' mandatory sharing semantics, which POSIX does **not** reproduce. Do not port it; the `SingletonLock` branch below is the POSIX answer. |
| `extensions/browser/service.py:1370` | `target = os.readlink(lock_path)` | 6 | DEGRADED | Correct POSIX design and correctly branched, but **never executed anywhere** — the docstring at `:1346-1347` says "reasoned from Playwright's own source quoted above, not observed against a real launch -- this box is Windows." Verify on Linux before trusting the profile sweep. |
| `extensions/browser/service.py:1440` | `shutil.rmtree(entry, ignore_errors=True)` | 6 | COSMETIC | `ignore_errors=True` behaves the same on both platforms; the guard is the `_profile_locked` check above it, whose POSIX half is the unverified one. |

---

## Category 7 — `shell=True` with a Windows-syntax command string

**There is no `shell=True` anywhere in `jev/`, `extensions/`, or `bin/`**, and no
`os.system` / `os.popen`. The two rows below are the shell invocations that *do*
exist and carry Windows syntax.

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `extensions/jev/extension.rn:44` | `command: "'C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv/Scripts/python.exe' -u server.py",` | 1,2,7 | **BLOCKER** | This string is run through `bash -c` (stated at `extensions/browser/extension.rn:14-17` and demonstrated by its own `:31`), so on Linux bash will look for a file literally named `C:/Users/...` and fail. Use the `test -f … && … \|\| …` idiom from `extensions/browser/extension.rn:31`. |
| `bin/hoot.cmd:2` | `powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0hoot.ps1" %*` | 4,7,10 | **BLOCKER** | Needs a POSIX launcher; on Linux there is no `%~dp0` and no `-ExecutionPolicy`. |

For contrast, the one manifest that gets this right is
`extensions/browser/extension.rn:31` — a plain POSIX conditional selecting
`bin/python` or `Scripts/python.exe` at start time. That is the pattern to
replicate for jev.

---

## Category 8 — Line-ending / encoding assumptions

The project is in good shape here: every text-mode read and write in `jev/` and
`extensions/` passes `encoding="utf-8"` **except** the three rows below, and the
JSONL writers deliberately write bytes via `os.open`/`os.write` with `O_BINARY`
(`jev/_paths.py:106`) so no CRLF translation happens on either platform. There is
no `newline=` anywhere.

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `models/openjev/modeling_openjev.py:187` | `json.dump(self.cfg, open(os.path.join(path, "config.json"), "w"), indent=2)` | 8 | DEGRADED | `open(..., "w", encoding="utf-8")`. This is real, executed code: `jev/server.py:222` imports this module. `config.json` holds the `nli_template` string, so a non-UTF-8 locale would silently re-encode it. |
| `models/openjev/modeling_openjev.py:191` | `cfg = json.load(open(os.path.join(path, "config.json")))` | 8 | DEGRADED | `open(..., encoding="utf-8")`. |
| `extensions/browser/test_server.py:2112` | `f.write_text("x")` | 8 | COSMETIC *(test helper; content is ASCII so the default encoding cannot change the result)* | Add `encoding="utf-8"` if the file ever holds non-ASCII. |

**Checked and clean:** `jev/automation/run.py:930`
(`path.open("r", encoding="utf-8")`), `jev/automation/graph.py:196` and `:200`
(`read_text(encoding="utf-8")`), every `read_text`/`write_text` in `jev/tests/`
and `jev/test_server.py` (e.g. `:733`), and all six
`jev/tests/fixtures/wiki/*.txt` reads.

**Line endings — verified, not assumed:** `file` reports
`jev/tests/fixtures/wiki/cat_snapshot.txt` as "Unicode text, UTF-8 text",
`jev/server.py` and `bin/hoot.ps1` as "ASCII text" — i.e. **no CRLF terminators
and no BOM**. `od -c` on `cat_snapshot.txt`, `bin/hoot.cmd`, and
`models/openjev/config.json` confirms `\n` only. So no fixture will compare
differently on Linux, and no `.gitattributes` is needed for this reason.

---

## Category 9 — Process and signal handling

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `bin/hoot.ps1:568` | `$p \| Stop-Process -Force -ErrorAction SilentlyContinue` | 9,10 | **BLOCKER** | No POSIX counterpart to `Stop-Process -Force` (an uncatchable terminate); needs `kill` / `pkill` with a real `SIGTERM`-then-`SIGKILL` escalation. |
| `bin/hoot.ps1:573` | `foreach ($x in $w) { Stop-Process -Id $x.ProcessId -Force -ErrorAction SilentlyContinue }` | 9,10 | **BLOCKER** | Same. |
| `bin/hoot.ps1:579` | `foreach ($id in $s) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }` | 9,10 | **BLOCKER** | Same. |
| `extensions/browser/service.py:1313` | `os.kill(pid, 0)` | 9 | COSMETIC | This *is* the POSIX idiom and only runs when `windows=False`. The docstring at `:1301-1311` states it was "reasoned from Python's documented POSIX semantics, not observed" — verify on Linux, but the code is right. |
| `extensions/browser/service.py:1256` | `` `SIGKILL` on POSIX or runs `taskkill /T /F` on Windows -- both terminate`` | 9 | COSMETIC *(comment quoting eidolon's `kill_tree`, which is out of scope)* | None. |
| `extensions/browser/test_server.py:2285` | `["taskkill", "/PID", str(pid), "/T", "/F"],` | 4,9 | COSMETIC *(test, skipped on Linux at `:2291`)* | None. |
| `extensions/browser/test_server.py:2258` | `["powershell.exe", "-NoProfile", "-Command", ps],` | 4,9 | COSMETIC *(test, skipped)* | None. |

**Checked and clean:** no `signal` module usage anywhere; no `SIGBREAK`, no
`CTRL_BREAK_EVENT`, no `CREATE_NEW_PROCESS_GROUP`, no `CREATE_NO_WINDOW`, no
`DETACHED_PROCESS`, no `startupinfo`, no `creationflags`, no `preexec_fn`, no
`os.setsid`, no `killpg`, no `taskkill` in shipped (non-test) code. Both services
are started with `uvicorn.run(...)` (`jev/server.py:699`,
`extensions/browser/service.py:1535`), and uvicorn installs its own
`SIGINT`/`SIGTERM` handlers on POSIX.

---

## Category 10 — Anything in `bin/` with no POSIX counterpart

`bin/` is the single largest blocker: all three files are Windows-only, and
`bin/hoot.ps1` is 740 of the 742 lines.

| path:line | Offending text | Cats | Severity | Smallest fix |
|---|---|---|---|---|
| `bin/hoot.ps1` (whole file, 740 lines; line 1: `# hoot -- launcher and CLI control for Minerva`) | — | 10 | **BLOCKER** | No POSIX counterpart exists. Its internal blockers are enumerated above in categories 1, 3, 4, 5, and 9 (29 distinct lines); the file also assumes `[CmdletBinding()]`/`param()`/`switch` and the `Get-*`/`Start-Process`/`Stop-Process`/`Join-Path` command set. A `bin/hoot` POSIX script (or a `hoot` flake app) is needed. |
| `bin/hoot.cmd` (whole file, 2 lines; line 2 is the payload) | `powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0hoot.ps1" %*` | 4,7,10 | **BLOCKER** | Replace with a `bash`/`sh` entry point; the `.cmd` shim itself is unnecessary on Linux. |
| `bin/PATH-backup.txt` (whole file, 537 bytes, 1 line) | `C:\Users\dxcen\.cargo\bin;C:\Users\dxcen\AppData\Local\Programs\Python\Python314\Scripts\;…` | 1,10 | COSMETIC | Nothing references it; delete, or replace with a `PATH`-shaped note that does not embed this user's home directory. |

**Not inside `bin/` but called by it:** `setup-bonsai2.ps1` at the repo root is
invoked from `bin/hoot.ps1:655` and `:671`. It sits outside the four surveyed
directories and was **not surveyed**; a porter must cover it separately.

---

## UNCERTAIN

These are real observations where I could not settle whether the answer is a
blocker. Reasoning is given so the next person can decide rather than re-derive.

1. **The Playwright temp sweep runs against a shared `/tmp` on Linux.**
   `extensions/browser/service.py:1527` passes
   `tmp_dir=Path(tempfile.gettempdir())`, and `_sweep_orphaned_profiles`
   (`:1379`) removes any directory under it matching
   `playwright_chromiumdev_profile-` (`:1225`) or `playwright-artifacts-`
   (`:1226`) that is older than 60s (`:1236`, `:1380`) and unlocked
   (`:1440`, `shutil.rmtree(..., ignore_errors=True)`). On Windows `%TEMP%` is
   per-user; on Linux `TMPDIR` usually points at `/tmp`, which is mode 1777 and
   shared by every user. Two consequences that do not exist on the source
   platform: (a) an unrelated user's Playwright profile can satisfy the name
   test and be deleted by the `rmtree`; (b) `_profile_locked`'s POSIX half
   (`:1370`, `os.readlink`) is unverified, so the "is anything holding it" test
   that is supposed to protect a live browser has never run. Whether this is a
   BLOCKER or just a DEGRADED multi-user hazard depends on whether the Linux
   deployment is single-tenant. Check: run the sweep on a Linux box with two
   Playwright launches as different users.

2. **`CacheDirTests` has no `skipUnless`, so it executes on Linux with
   Windows-shaped data.** `jev/test_server.py:108-112` patches `sys.platform` to
   `"win32"` and `LOCALAPPDATA` to `"C:\\fake\\local"`, then asserts
   `server._cache_dir() == Path("C:\\fake\\local")`. On Linux both sides are
   `PosixPath` of the same string, so it passes — the assertion proves nothing
   about Windows path semantics there, but nothing fails either. Whether the
   class should be skipped or left as-is is a judgement call the file itself
   acknowledges at `:102-105`. Check: run the suite on Linux and confirm.

3. **`jev/tests/test_run.py:2069` declares its own class unobserved on Linux:**
   "everything in this class has been run and observed on win32 only." The
   module also records two POSIX branches as *reasoned*, not observed
   (`jev/_paths.py:40-41`, `extensions/browser/service.py:1346-1347`). These are
   declarations that the Linux runs have never happened — the port's first job
   is to run them, not to change them.

4. **`extensions/jev/extension.rn:45` is `cwd: "../../jev"`** — a relative
   `cwd`. Whether the loader resolves it against the manifest's directory before
   `exec` matters if a Nix wrapper replaces the command with a store path. The
   loader lives in `eidolon/` (`crates/rune/src/ext.rs`), which is out of scope
   here, so I could not confirm it. Check: `eidolon/docs/extensions.md`.

5. **There is no dependency manifest for the jev service.** `jev/` contains no
   `requirements.txt`, `setup.py`, or `pyproject.toml`; only
   `extensions/browser/requirements.txt` and `requirements-test.txt` exist. The
   jev service needs `fastapi`, `uvicorn`, `torch`, `transformers`, and
   (for jevlike) the sibling checkout — a flake cannot be built from this tree
   without pinning them. This is not one of the ten POSIX categories, but it is
   the first thing the porter will hit.

6. **`%TEMP%`-shaped wording in comments only.** `extensions/browser/service.py:1214`
   and `:1521` refer to `%TEMP%` in comments while the code uses
   `tempfile.gettempdir()`. Cosmetic, listed so nobody greps `%TEMP%` and
   reports a phantom blocker.

7. **`subprocess.run(..., text=True)` decodes with the locale encoding** —
   `jev/test_server.py:197`, `extensions/browser/test_server.py:2259` and `:2286`.
   The first is the only one that runs on Linux and only asserts on ASCII
   (`"IMPORT_OK"`), so a `C`/`POSIX` locale container is fine. Under a flake's
   build sandbox with `LC_ALL=C`, expect ASCII-only output to be safe but
   anything else to mojibake. Check: run `jev/test_server.py` with `LC_ALL=C`.

8. **`bin/hoot.ps1:3` warns that PowerShell 5.1 reads `.ps1` as ANSI without a
   BOM.** Irrelevant on Linux (no PowerShell 5.1), and the file is verified
   ASCII with LF endings, so there is no BOM/CRLF trap here. Listed only because
   the comment looks like a Windows-only assumption at first glance.

9. **`models/` holds no path configuration.** `models/openjev/config.json` and
   `models/openjev/tokenizer_config.json` contain no filesystem paths (only
   `nli_template`, token ids, and architecture keys), and no `.gguf` file was
   opened. `models.ini` — which *does* name which weights to serve — sits at the
   repo **root**, outside `models/`, and was not surveyed. Whether the flake
   should reference the weights by store path or by derivation is unresolved
   here.

10. **`.orig` backup files inside the surveyed tree are not executed.**
    `extensions/browser/service.py.orig`, `extensions/browser/test_server.py.orig`
    (its lines 7-8 repeat the same Windows docstring paths as the live file), and
    `extensions/browser/tools/snapshot.rn.orig`. Nothing imports, loads, or
    invokes a `.orig` file. They are not blockers; a porter may want to delete
    them so they do not get counted twice.

---

## Grep hits that are NOT findings (recorded so nobody re-derives them)

| path:line | Hit | Why it is not a path |
|---|---|---|
| `jev/automation/a11y.py:60`, `extensions/browser/service.py:421` | `r'(?:\s+"(?P<name>(?:[^"\\]|\\.)*)")?'` | Regex escapes inside a pattern; `\\` is an escaped backslash in the regex language, not a separator. |
| `jev/automation/a11y.py:57,61,62,64,81` | `r"^/([A-Za-z][A-Za-z0-9_-]*):\s*(.*)$"` and friends | HTTP/`/url:` syntax in the accessibility-tree snapshot format. |
| `jev/automation/graph.py:65,82` | `PATH_RE_SRC = r"^(context\|input\|event\|run)(\.[A-Za-z0-9_-]+)*$"` | A dotted *data* path into a graph's context, not a filesystem path. |
| `jev/automation/graph.py:1000,1011` | `` `https://host/path`, that is `https://host`, the origin. `` | URL slashes in a docstring. |
| `extensions/browser/service.py:1331` | `const lockFile = process.platform === "win32" ? "lockfile" : "SingletonLock";` | A quotation of Playwright's own JavaScript, inside this project's docstring — documentation, not this project's code. |
| `models/openjev/tokenizer.json:2216,5695,13135,79783,84580` | `".Windows": 1863`, `"ĠWindows": 5342`, `"AllWindows": 79430`, … | Tokenizer vocabulary entries. |
| `jev/tests/*`: `"/call"`, `"/health"`, `"/wiki/Felidae#lead"`, `"/url: https://…"` | route and URL literals in tests and wiki fixtures | HTTP. |
| `jev/tests/test_run.py:855` | `\`{"error": "[exit code 1]\\nps:` | An escaped newline inside an assertion about a `ps` failure message. |

---

## Totals

Counted two ways, because a single line can be an instance of more than one
category.

**Distinct findings: 68** (a finding = one `path:line` citation; 67 of them
carry a line number, and the 68th is `bin/hoot.ps1` cited by file, because the
thing with no POSIX counterpart is the file and there is no single line to
point at)

| Severity | Distinct findings | of which line-numbered |
|---|---|---|
| **BLOCKER** | **31** | 30 |
| DEGRADED | 10 | 10 |
| COSMETIC | 27 | 27 |
| **Total** | **68** | **67** |

**Category rows: 90** (each occurrence counted in every category it belongs to)

| Category | Rows | BLOCKER | DEGRADED | COSMETIC |
|---|---|---|---|---|
| 1. Absolute Windows paths | 19 | 7 | 2 | 10 |
| 2. Paths outside the project (7 into `cms-agent`, 2 into the user profile) | 9 | 5 | 0 | 4 |
| 3. Separator assumptions / hardcoded `\\` | 10 | 7 | 2 | 1 |
| 4. Windows-only APIs | 18 | 14 | 0 | 4 |
| 5. Windows-only env vars | 13 | 3 | 3 | 7 |
| 6. Locking / rename / permissions | 6 | 0 | 2 | 4 |
| 7. `shell=True` with Windows syntax | 2 | 2 | 0 | 0 |
| 8. Encoding / line endings | 3 | 0 | 2 | 1 |
| 9. Process / signal handling | 7 | 3 | 0 | 4 |
| 10. Files in `bin/` with no POSIX counterpart | 3 | 2 | 0 | 1 |
| **Sum** | **90** | **43** | **11** | **36** |

(The row-level BLOCKER sum of 43 exceeds the 31 distinct blockers because
`extensions/jev/extension.rn:44` is counted in categories 1, 2 **and** 7;
`jev/server.py:62` in 1 and 2; and each `bin/hoot.ps1` / `bin/hoot.cmd` line in
category 10 as well as its own.)

**Where the blockers actually are — the one-paragraph version:**

1. **`bin/` is a total loss: 28 of the 31 distinct blockers are here.**
   `bin/hoot.ps1` (740 lines, cited as a whole file), plus 26 of its individual
   lines, plus `bin/hoot.cmd:2`. `bin/PATH-backup.txt:1` is the only file in
   `bin/` that is merely cosmetic.
2. **Two hardcoded paths into the sibling checkout `cms-agent`**, one in Python
   (`jev/server.py:62`) and one in a Rune manifest
   (`extensions/jev/extension.rn:44`). These cannot be declared as flake inputs
   at all — they need a product decision, not a path fix.
3. **One hardcoded path inside this repo but absolute to the developer's machine**
   (`jev/server.py:66`, `OPENJEV_DIR`).
4. **Everything else is clean.** `jev/automation/` (except the comment at
   `graph.py:5`), `extensions/jev/graphs/`, the Rune tool scripts, and the whole
   `_paths.py` platform split are already written for both platforms, and
   `extensions/browser/extension.rn:31` already contains the correct
   platform-branching pattern for the manifest that jev gets wrong.

---

## Coverage statement

**Searched exhaustively** — every file under `jev/`, `extensions/`, `bin/`, and
`models/` (excluding `.venv/`, `__pycache__/`, `node_modules/`, `target/`,
`.git/`), matching these patterns case-sensitively and, where noted,
case-insensitively:

* drive letters and raw backslash paths: `[A-Za-z]:[\\/]`, `[A-Za-z]:\\\\`;
* `cms-agent`, `jevlike`, `Projects[\\/]` (case-insensitive);
* `os.path.`, `os.sep`, `os.altsep`, `os.pathsep`, `\\\\\\\\`;
* `msvcrt`, `winreg`, `win32`, `pywin32`, `os.startfile`, `cmd.exe`,
  `powershell`, `.exe`, `.ps1`, `.bat`, `.cmd`, `shell32`, `ctypes.windll`,
  `WINFUNCTYPE`;
* `APPDATA`, `LOCALAPPDATA`, `USERPROFILE`, `HOMEDRIVE`, `HOMEPATH`,
  `COMPUTERNAME`, `PROGRAMFILES`, `SystemRoot`, `WINDIR`, `PATHEXT`, `TEMP`,
  `TMP`, `TMPDIR`, `%TEMP%`, `gettempdir`;
* `os.rename`, `os.replace`, `O_BINARY`, `O_TEXT`, `chmod`, `umask`, `locking`,
  `flock`, `LOCK_EX`, `CreateFileW`, `GetLastError`;
* `shell=True`, `os.system`, `os.popen`, `/bin/sh`, `/bin/bash`, `Start-Process`,
  `Invoke-Expression`;
* `open(`, `read_text(`, `write_text(`, `\.readlines(`, `newline=`, `\r\n`,
  `codecs.open`, `io.open`, `TextIOWrapper`;
* `CREATE_NEW_`, `CREATE_NO_WINDOW`, `taskkill`, `SIGBREAK`, `SIGTERM`,
  `SIGKILL`, `os.kill`, `creationflags`, `startupinfo`, `DETACHED_PROCESS`,
  `os.setsid`, `preexec_fn`, `killpg`, `Stop-Process`, `Get-Process`,
  `Get-CimInstance`;
* `sys.platform`, `os.name`, `platform.system`, `sys.executable`;
* `Scripts`, `.venv`, `<venv>`, `bin/python`;
* `subprocess`, `Popen`, `shutil.which`, `Path(`, `os.getpid`, `os.readlink`,
  `os.symlink`, `os.utime`, `os.tmpdir`.

**Read line-by-line (not sampled):** `jev/_paths.py` (all 114 lines);
`jev/server.py` lines 1-370 plus every cited region in 370-699;
`extensions/browser/service.py` lines 1180-1535 plus every cited region;
`extensions/browser/extension.rn` (43); `extensions/jev/extension.rn` (62);
`extensions/jev/tools/run.rn` (88) and `resume.rn` (104); `bin/hoot.cmd` (2);
`bin/PATH-backup.txt` (1); `bin/hoot.ps1` lines 1-60, 90-215, 330-470, 495-600,
640-740; `models/openjev/modeling_openjev.py` lines 183-197.

**Covered by exhaustive pattern grep only — NOT read line-by-line:**

* `jev/automation/run.py` (2,139 lines): only the regions I cite
  (`:1-70`, `:780-860`, `:930`, `:815`, `:2069` context) were read.
* `jev/test_server.py` (787), `jev/tests/test_run.py` (2,688),
  `jev/tests/test_schema.py` (627), `jev/tests/test_server_automation.py` (621),
  `jev/tests/test_a11y.py` (468), `jev/tests/test_guards.py` (386),
  `jev/tests/test_options.py` (322), `jev/tests/test_warrant.py` (237),
  `jev/tests/test_decisions.py` (216), `jev/tests/test_guards_openjev.py` (132),
  `jev/tests/test_template.py` (117).
* `extensions/browser/test_server.py` (2,535): only the cited regions
  (`:1998-2060`, `:2090-2130`, `:2210-2300`) were read.
* `extensions/browser/service.py` lines 1-1180 other than the cited ones.

So: a blocker in those unread line ranges that is **not** a Windows path, a
Windows API, a Windows environment variable, an encoding omission, a
separator assumption, a shell invocation, a permission/locking call, or a
process/signal call would not have been caught by pattern grep. Every line I do
report in those files was read back individually before being written down.

**Deliberately not opened:** `models/**/*.gguf`, `models/**/*.safetensors`, and
`models/openjev/tokenizer.json` (a 600k-line vocabulary) — file names and the
non-weight JSON/Python beside them were inspected instead.

**In scope by directory but not surveyed, and why:** the repo-root files
`serve.ps1`, `setup-bonsai2.ps1`, `setup-bonsai2.ps1.orig`, `models.ini`, and
`opencode.json`, plus `webui/`, `docs/`, `harnox/`, `providers/`, `llama.cpp/`,
`logs/`, `.cache/`, and `.webui/` — none of these is one of the four named
directories. **This matters: `bin/hoot.ps1:655` and `:671` invoke the
repo-root `setup-bonsai2.ps1`, so that file is on the critical path to a POSIX
port and is not covered by this inventory.**

**Excluded by instruction:** `eidolon/` (Rust, a separate task), and all
`.venv/`, `__pycache__/`, `node_modules/`, `target/`, `.git/` directories.
`extensions/browser/.venv/` in particular contains its own drive-letter paths
(e.g. `pyvenv.cfg`, `Scripts/activate`) and `.exe` launchers; these are
generated artefacts of the current platform's `python -m venv` and are excluded
as instructed — a Linux venv regenerates them.
