# installer: install, update, uninstall

One Linux **bundle**, three ways in:

```text
  minerva-<ver>-linux-x86_64.tar.gz   <-- the one artifact (static musl binaries, web UI, Windows hoot files)
        |
        +-- install.sh                 any Linux, or inside WSL
        +-- minerva-install.exe        Windows: checks WSL, hands the SAME bundle to WSL, adds hoot/PATH/Start menu
        +-- minerva update             later, from GitHub Releases, signature-verified
                 all three end in:  minerva install --from <unpacked bundle>   (installer/src/install.rs, the ONE place)
```

| Program | Where | What |
|---|---|---|
| `minerva` | Linux, in the bundle, `~/.local/bin/minerva` | `install`, `update [--check] [--rollback]`, `uninstall`, `version`, `plugins`, `services`, `jev-setup`, `keys`, and the verbs `web`, `status`, `stop`, `init` |
| `minerva-install.exe` | Windows | the installer a club member double-clicks (carries the bundle, no network needed) |

Version: one source of truth, the repo-root **`VERSION`** file (`0.1.0`). The build appends the short git sha:
`0.1.0-gabc1234` (`.dirty` when tracked sources differ from HEAD; a real release refuses a dirty tree).
Only the numbers decide "newer", so a rebuild of the same version is never offered as an update.

## What gets installed where

```text
 WSL / Linux  (prefix = ~/.local, or --prefix DIR)
   ~/.local/opt/minerva/<ver>/{bin,webui,windows,VERSION}   one complete version
   ~/.local/opt/minerva/current  -> <ver>                   flipped atomically
   ~/.local/opt/minerva/previous -> <old ver>               kept for --rollback
   ~/.local/bin/minerva          -> ../opt/minerva/current/bin/minerva
   ~/.local/state/minerva/update-check                      the once-a-day notice cache
   ~/.local/state/minerva/plugins.json                      which plugins WE installed (and their hashes)
   ~/.local/opt/minerva/jev-venv/                           only if you said yes to the jev step
   ~/.config/eidolon/plugins/{browser,jev,subagent}         the plugins (eidolon's own plugins dir; see "Plugins")
   ~/.config/minerva/update-keys/*.pub                      only keys you or a signed rotation added (see "Key rotation")
 Windows  (%LOCALAPPDATA%\Minerva, or --win-root DIR)
   bin\hoot.ps1  bin\hoot.cmd  bin\install.json             hoot + {distro, wsl_prefix}
   minerva-install.exe                                      kept for uninstall
   HKCU\Environment Path                                    + <root>\bin
   HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Minerva
   Start menu\Programs\Minerva.lnk
```

Nothing else is created, changed or removed (the plugins, the browser token, the jev token and the keys dir above are the
only things outside `opt/minerva`, `bin/minerva`, `state/minerva`). Never `~/.local/opt/eidolon*` or any other directory
beside `minerva`, and **never `~/Minerva`** (chats, settings): install, update and uninstall all say where it is kept. No admin rights
(everything is per user); `wsl --install` is the one step that needs them, and it is only ever offered.

The bundle (about 139 MB) holds `bin/{minerva, perch, eidolon, eidolon-browser, chromium, minerva-setup, perch-open.sh,
minerva-setup.sh}`, `lib/chromium/` (the pinned headless Chromium plus the libraries it needs, 285 MB unpacked), `plugins/{browser,
jev,subagent}`, `jev/` (the optional Python service's files), `webui/term/`, `windows/{hoot.ps1, hoot.cmd}`, `VERSION`, `LICENSE`.

## Plugins

`browser`, `subagent` and `jev` (from `eidolon-plugins/`) are preinstalled and pre-trusted, so `eidolon tools` shows them
(browser 7, subagent 7, jev 15 tools) on a fresh install with no network and no paid turn.

```text
 where eidolon looks:  <config dir>/eidolon/plugins   (~/.config/eidolon/plugins). The only override is the config key
                       `plugins_dir`; the perch's generated config does not set it.
 how:                  stock `eidolon plugins install file://<git repo> <name>` from a temp git repo made from the bundle's
                       plugins/ (a plain copy when git is missing), then `eidolon plugins trust <name>`; for browser also the
                       six grants on file:~/.config/eidolon/browser.token and `plugins service approve browser`.
 who owns what:        ~/.local/state/minerva/plugins.json records each plugin WE installed with its hash.
                       - a plugin of that name that is not ours: left exactly as it is (it is the member's); on a terminal we ask
                         whether to replace it, in a script the answer is no.
                       - ours and unchanged: only re-trusted if the vouch went missing, and only when the INSTALLED copy still hashes
                         to what we recorded. If it was edited (the hash differs) it is left untrusted, and we say so; we never
                         vouch for bytes we did not install. The temp repo used to install lives in a `tempfile` directory.
                       - ours and outdated (after `minerva update` / install): replaced, then trusted again (a vouch is bound to
                         the script hash, so this is how trust "carries over").
 uninstall:            removes only plugins in that record (stops their service, untrusts, revokes the grants, uninstalls),
                       then the record. The member's own plugins, eidolon's config and tokens stay.
 when it runs:         `minerva install`, `minerva update`/rollback, and every `hoot`/`minerva web` start (quiet, only when
                       something differs: `minerva plugins sync --if-needed --quiet`, then `minerva services start`).
 by hand:              minerva plugins sync [--if-needed] [--quiet] | status         minerva services start|stop|status
```

### Chromium for the browser plugin

`lib/chromium` is Chrome for Testing's `chrome-headless-shell` (pin and sha-256 in `chromium/pin.env`) with the shared
libraries it needs from Ubuntu 22.04 (`chromium/libs.lock`: 48 packages, every deb sha-256 checked; the libc family stays the
host's) and DejaVu fonts (a machine with no fonts renders no text). `bin/chromium` is a wrapper that sets `LD_LIBRARY_PATH` and a
fontconfig file, so a fresh Ubuntu with no `apt install` and no sudo can run it. `chromium/fetch.sh DEST` assembles it (called by
`build.sh`; downloads are cached in `~/.cache/minerva-build`); `chromium/resolve-libs.py` regenerates `libs.lock` when the pin changes.
The libraries come from Ubuntu 22.04, so the browser plugin needs glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+). `minerva install`
reads `ldd --version`; on an older host it warns once and still installs everything else. To refresh `libs.lock` (when the pin in
`pin.env` changes, or a library gets a security update): run `chromium/resolve-libs.py`, review the diff, then rebuild.
Chromium's own sandbox needs unprivileged user namespaces: Ubuntu 24.04+ restricts them with AppArmor, and then the browser
service reports why; `EIDOLON_BROWSER_NO_SANDBOX=1` is the member's own explicit way out (it is never set for them).

### The jev step (optional)

Offered at install (Windows exe: a prompt, default no; `--jev yes|no` skips it; `--yes` never answers it). It builds a venv at
`~/.local/opt/minerva/jev-venv` with CPU torch and the pinned requirements (tested on Python 3.14; Python 3.10-3.11 uses the unhashed ranges in
`requirements.txt` and says so), about 1.2 GB and 35 seconds on a fast line. Saying no changes nothing.
On Python 3.12+ every file is pinned by sha-256 and installed with `pip install --require-hashes --no-deps --isolated`, in two
steps: `requirements.torch.txt` from the PyTorch CPU index only (`--index-url`), then `requirements.pinned.txt` from PyPI only.
Two installs instead of one with `--extra-index-url` because with two indexes in one resolve a name can be answered by either
(dependency confusion); with hashes AND separate indexes, neither can substitute anything. Regenerate with `installer/jev-hashes.sh`.
Later: `minerva jev-setup [--dry-run] [--yes]` / `hoot jev-setup`. It also makes `~/.config/eidolon/jev.token` (0600) and grants the
two scorer tools; `minerva services start` then runs the service on port 8091. The 9 GB `openjev` weights are not fetched
(`jev/get-openjev.sh` does that by hand). Needs `python3` with `venv` (on Ubuntu: `sudo apt install python3-venv`).

### Nix

`nix/minerva.sh` does the same through `minerva-core` (this crate's `minerva` binary built by `nix/minerva-core.nix`) with the
plugin tree from `nix/plugins.nix` and the Nix `eidolon-browser` (Chromium from nixpkgs): `minerva web` runs
`minerva-core plugins sync --if-needed --quiet --eidolon ... --src ... --bin-dir ... --prefix ~/.local/state/minerva-nix`. The
store is never written; the record of what is ours lives in that state dir. `minerva plugins` and `minerva services` are the same verbs.

## Windows flow

```text
 minerva-install.exe
   1 wsl.exe -l -v  --- none/unusable? explain, OFFER `wsl --install -d Ubuntu` (admin + reboot), STOP: "run me again"
   2 pick the default distro if Ubuntu/Debian-like on WSL 2, else ask (never docker-desktop); read /etc/os-release, uname -m
   3 stream the bundle into WSL (tar over stdin), run  minerva install --from <it>  ; remove the staging dir
   4 hoot + install.json to %LOCALAPPDATA%\Minerva\bin, PATH (HKCU, WM_SETTINGCHANGE broadcast), uninstall entry, shortcut
   5 setup wizard in WSL (when stdin is a terminal; asks before redoing an existing ~/Minerva/minerva.toml)
 reruns are idempotent; a newer exe = an upgrade (new version dir, `current` flipped, the old one kept as `previous`)
 --uninstall: Linux side (minerva uninstall), PATH entry, Installed-apps entry, shortcut, Windows folder
```

It asks for no password or key. Provider sign-in (eidolon credentials) is a later step: `docs/Getting-Started.md`.
Prompts use cliclack on a terminal and plain lines otherwise (`--plain` forces plain if the console misbehaves).
Test/safety options: `--win-root`, `--wsl-prefix`, `--start-menu`, `--registry-root Software\MinervaInstallTest`
(the PATH and uninstall keys go under that HKCU key, no broadcast), `--no-registry`, `--no-shortcut`, `--skip-wizard`,
`--distro`, `--fake-wsl-missing` (prints the offer, runs nothing).

## hoot: dev mode vs installed mode

`bin/hoot.ps1` is one script. **Installed mode is on when `install.json` sits next to it** (the installer writes it).
Then every WSL call is `wsl.exe -d <distro>` and the programs come from `<prefix>/opt/minerva/current`. Without that
file it is dev mode: the repo checkout, exactly as before. Installed mode has no llama-server verbs (`serve`, `up`,
`models`, `get`, `setup`, `edit`, `which`); it adds `hoot update [-Check] [-Rollback]`, `hoot keys ...`, `hoot jev-setup` and
`hoot version`. `hoot keys` and `hoot jev-setup` run the Linux command; a key file named by a Windows path is mapped with `wslpath`.

**Self-refresh.** On every launch an installed hoot compares the version in `install.json` with the installed bundle's `VERSION`.
If they differ (the Linux side was updated by `minerva update`, `install.sh` or the exe), hoot copies the bundle's `windows/hoot.ps1`
over itself (not `hoot.cmd`: it is a fixed one-liner, `... %* & exit /b`, so cmd never re-reads it while it runs and it does not
change between versions; an install made before that keeps its old hoot.cmd until the exe is run again), records the new version, says so, and asks you to run the command again (it does not re-exec: the
script that is running is the one that was just replaced). A checkout never does this.
In installed mode `install.json` is validated before use: `wsl_prefix` must be an absolute Linux path with no quote, `$`, backslash,
`..` or control character, and `distro` plain; otherwise hoot refuses and says to reinstall. `hoot.cmd` calls PowerShell by its
absolute path under `%SystemRoot%`.

`bin/perch-open.sh` and `bin/minerva-setup.sh` do the same by position: **when a `perch` binary sits beside the script
it is an install** (binaries, UI and `minerva-setup` from there, its `bin/` first on PATH so the doors are the bundled
eidolon); in a checkout there is none, so they use `~/perch-target` etc. `MINERVA_PERCH_BIN`, `MINERVA_UI_DIR`,
`MINERVA_SETUP_BIN`, `MINERVA_PERCH_PORT`, `MINERVA_HOME` win in both modes.

## Updating

```text
 hoot update  /  minerva update            newest signed release (GitHub Releases, latest/download)
 hoot update -Check  /  minerva update --check
 hoot update -Rollback  /  minerva update --rollback     flip to `previous`; again flips forward
```

```text
 minerva update
   refuse if the install is under /nix/store  -> prints `nix profile upgrade ...`
   GET latest.json + latest.json.minisig      verify against the key COMPILED INTO this binary  (fail = stop)
   newer by number?  no -> "up to date"
   GET <bundle> + <bundle>.minisig            verify signature AND sha-256/size pinned in the signed manifest
   unpack in .update-<pid>, check its VERSION equals the manifest, install_from_dir, flip current, prune to 2 versions
   say "restart the server" if a perch is running (never kills it)        [hoot then refreshes %LOCALAPPDATA%\Minerva\bin]
```

Which keys: the trusted set is the key compiled into the program plus the keys in `~/.config/minerva/update-keys/`, minus revoked
ones (see "Key rotation"). Any mismatch, a missing signature, a wrong key, a tampered bundle or a bundle swapped for another validly signed one:
refused, nothing changes. A checksum alone is never enough. Only `https://` (and loopback `http://`, `file://` for
tests) are accepted. `--base URL` points the updater at a test release folder or a loopback server.

**Notice only.** `hoot`/`minerva` (web, status) check at most once a day, 3 s timeout, silent on any failure, verified
the same way, cached in `~/.local/state/minerva/update-check`, and print one line:
`Minerva 0.2.0 available -- run: hoot update`. Opt out: `MINERVA_NO_UPDATE_CHECK=1`. They never install anything.
(The web page's notice is a follow-up: `webui/term` belongs to another change.)

## Key rotation

A leaked or retired signing key can be replaced without anyone reinstalling.

```text
 trusted set = compiled-in key  +  ~/.config/minerva/update-keys/<ID>.pub   -  revoked ids
 state       = ~/.config/minerva/update-keys/state.json   {serial, revoked:[{id,on,by}]}
 the dir     = mode 0700 and owned by you (else ignored, with a warning); files 0600
```

**Signed rotation.** A release may carry `keys.json` + `keys.json.minisig` next to `latest.json`. `minerva update` fetches them
first (absent is fine), verifies the signature with a key that is already trusted, applies the changes, and only then verifies
the manifest and the bundle:

```json
{ "schema": 1, "serial": 3, "expires": "2027-01-01",
  "add":    [ { "key": "RWQ...the second line of the new .pub...", "expires": "2028-01-01", "comment": "release key 2" } ],
  "revoke": [ "0F7A3A5C447EC7EF" ] }
```

Rules: `serial` must be higher than the last applied but at most 16 higher (a leaked key cannot burn the counter with a huge
serial); a serial that is not higher is "already applied" and is answered BEFORE the signature or expiry is looked at, so
re-fetching a file you already applied (the release still carries it) never blocks an update, even after its signer was revoked
or it expired. Every key records who added it (`by`); revoking a key also revokes every key it added, transitively (a leaked key
cannot leave a backdoor behind), except keys added by that very file. `expires` is the date after which the file
is no longer accepted; a key that was ever revoked is never accepted again, even if a later file adds it, **and that includes the
compiled-in key** (after revoking it, a release signed only by the old key is refused); a `keys.json` signed by an unknown or a
revoked key is refused and nothing changes; a key with its own `expires` stops being trusted on that date.

**By hand.** `minerva keys list [--json]`, `keys inspect <pubfile|-> [--json]`, `keys add <pubfile|-> [--confirm FP]`,
`keys remove <id> [--confirm FP]`. `add` and `remove` show the key's **fingerprint** (the first 16 hex characters of the sha-256
of the 32 key bytes, in 4 groups of 4, e.g. `0F7A 3A5C 447E C7EF`; 64 bits, enough to tell keys apart by eye, not a collision-proof
digest) and want it typed back (from the terminal, never a pipe; spaces and case do not matter) or passed with `--confirm FP` by a
program whose user already typed it. The key id alone does NOT pass: an id is only 8 bytes the file itself claims, a forged `.pub` can
carry any id. `add` refuses a key id that already exists with different key bytes. The compiled-in key cannot
be removed by hand; only a signed rotation revokes it. `hoot keys ...` passes through to WSL.

**Contract for the perch (to be called later, not built here).** A perch route that adds a key: run
`minerva keys inspect - --json` with the public key on stdin (answer `{"id","fingerprint","comment"}`), show the fingerprint to
the person, and when they have typed the FINGERPRINT back run `minerva keys add - --confirm <FP>` with the same public key on stdin.
A wrong confirm exits 1 with "nothing changed". Removal: `minerva keys remove <id> --confirm <FP>` (fingerprint from `keys list`). The perch never writes the keys
dir itself.

**Operator's steps to rotate** (the secret keys never enter the repo):

```sh
minisign -G -p new.pub -s ~/.minisign/minerva-2.key                  # 1. the new key pair
# 2. write keys.json: add new.pub's second line (serial = last + 1, an expiry date); put the old key's id in "revoke" if it leaked
minisign -S -s ~/.minisign/minerva.key -m keys.json                   # 3. sign it with a key members already trust (the OLD one)
minisign -S -s ~/.minisign/minerva-2.key -m latest.json minerva-*.tar.gz   # 4. the release itself, signed by the NEW key
#    release assets: latest.json, latest.json.minisig, the bundle, its .minisig, keys.json, keys.json.minisig
```

`state.json` is required once the keys dir exists: an unreadable or corrupt one is a hard error with a sentence, and a missing one
is a fresh start only when the keys dir does not exist either; if the dir exists but `state.json` is gone, update refuses and says to
restore it or remove the dir by hand (silently resetting would forget revocations). `update --check` and the notice never write any
state: they preview a rotation in memory with the same rules.

**Attach `keys.json` (re-signed before its `expires`) to every release from the rotation on**: a member who skipped versions has to
see it. The signed manifest carries `keys_serial`; when that is above a member's local serial, `keys.json` is REQUIRED and a
missing file is refused, so an old key cannot be kept alive by hiding the rotation.

Then build the next release with `-PubKey new.pub` so fresh installs compile the new key in; members who update from the old
version get the rotation first, so their old binary already trusts the new key. If the old key leaked, publish the revoking
`keys.json` (signed by the old key, the file is checked before the revocation applies) as soon as possible: members apply it at
their next update, and from then on nothing signed only by the leaked key installs.

## Building a release

One command, does not publish:

```powershell
installer\build.ps1                # -OutDir installer\dist  -Version X.Y.Z  -PubKey FILE  -AllowDirty
```

Needs WSL with rust + the musl target (`rustup target add x86_64-unknown-linux-musl`), and Windows cargo (MSVC).
`installer/build.sh --out DIR` does the Linux half alone (any Linux). Output in `dist/`:
`minerva-<ver>-linux-x86_64.tar.gz`, `install.sh`, `minerva-install.exe`, `SHA256SUMS`, `latest.json`.
Linux binaries build with `CARGO_TARGET_DIR=$HOME/installer-target` from this repo's vendored trees (eidolon is built
unmodified). Static musl, so any distro works (no glibc floor); `build.sh` fails if a binary is not static. A release build requires
`musl-gcc` (`apt install musl-tools`) so the stack protector and fortify stay on. Without it, `build.sh --dev` (never for a release)
lets gcc compile the C parts against glibc headers with fortify off; that links and runs, but is not shippable. `--test-overrides`
(dev only) compiles in the test-only environment overrides (`MINERVA_KEYS_DIR` and friends); a release build never has them.
The Windows exe is built with a statically linked C runtime (`installer/.cargo/config.toml`) so it does not need the VC++
redistributable and imports no `VCRUNTIME140`; it calls `wsl.exe`, `cmd.exe` and `powershell.exe` by absolute path under
`%SystemRoot%\System32` and restricts DLL search to System32. The dirty check counts untracked files in every shipped path
(installer, nix, flake.nix, ...).

### Checking a download by hand

`install.sh` and the exe only know the download is intact (`SHA256SUMS` comes from the same place as the bundle). To know it is the
maintainer's, check `SHA256SUMS.minisig` yourself before running anything:

```sh
minisign -Vm SHA256SUMS -P <the public key from installer/minisign.pub>      # then: sha256sum -c SHA256SUMS --ignore-missing
```

Release key fingerprint (sha-256 of the 32 key bytes, first 16 hex, grouped): (none yet: `installer/minisign.pub` is still the placeholder; the operator pastes the fingerprint here, from `minerva keys inspect installer/minisign.pub`, when the real key is committed). `install.sh` refuses when
`SHA256SUMS`, its line for the bundle, or `sha256sum` is missing.

### One-time signing setup (operator)

```sh
minisign -G -p installer/minisign.pub -s ~/.minisign/minerva.key    # choose a password; the .key never enters the repo
git add installer/minisign.pub && git commit                        # the PUBLIC key is safe to commit
```

The build refuses to compile while `installer/minisign.pub` is the placeholder or missing (`-PubKey FILE` /
`MINERVA_UPDATE_PUBKEY` points elsewhere). Rotating the key does not need a reinstall: see "Key rotation". Keep the secret
key safe.

### Releasing

```powershell
installer\build.ps1                                       # clean tree, VERSION bumped and committed
```
```sh
cd installer/dist
minisign -S -s ~/.minisign/minerva.key -m latest.json minerva-*-linux-x86_64.tar.gz     # writes the .minisig files
gh release create v$(cat ../../VERSION) --repo dxcently/Minerva --title "Minerva $(cat ../../VERSION)" \
  --notes "..." latest.json latest.json.minisig minerva-*-linux-x86_64.tar.gz minerva-*-linux-x86_64.tar.gz.minisig \
  install.sh minerva-install.exe SHA256SUMS
```

`latest.json` carries a signed `released` date, `expires` (= released + 30 days) and `keys_serial`. Past `expires`, `update` and the
once-a-day notice still work but print "release info is stale (signed <date>) -- the maintainer should re-sign" (a replayed old
manifest is the thing this exposes). **Re-sign at least every 30 days**, without rebuilding: `installer/release-files.sh resign DIR`
re-stamps `latest.json`, then `minisign -S -s KEY -m latest.json` and re-upload both files. Also re-sign `keys.json` before its own
`expires`.

The updater fetches `https://github.com/dxcently/Minerva/releases/latest/download/latest.json` (+ `.minisig`, then the
bundle named in it). The exe and `install.sh` are for people; only `latest.json`, the bundle and their signatures
matter to the updater.

## Tests

`cargo test` in `installer/` (in WSL for the Unix half; `MINERVA_ALLOW_PLACEHOLDER_KEY=1` because tests use throwaway
keys): good signature installs, tampered bundle refused, wrong key refused, missing signature refused, swapped bundle
refused, rollback and forward, Nix refused, notice once a day / silent on failure / bad signature gives no notice,
install/rerun/upgrade/prune/uninstall layout, WSL list parsing, PATH list edits, key rotation (good, unknown signer, revoked
signer, revoked compiled-in key, manual add/remove, expiry, replay, open keys dir), and the plugin lifecycle (install, trust, foreign
plugin kept, update keeps trust, uninstall removes only ours) when `MINERVA_TEST_EIDOLON` names an eidolon binary (otherwise that
one test says it was skipped). The Windows pieces (registry,
shortcut, WSL calls) are exercised by running the exe with the test options above, never against the real settings.

## Threat notes

```text
 trusts            the key compiled into the binary, plus keys it was told to add (signed rotation, or by hand with a typed fingerprint)
 does not trust    GitHub, the network, the bundle's own SHA256SUMS (integrity only), a manifest older than it says
 knows about       a replayed old manifest: past `expires` it is flagged stale, not refused (operator's ruling: warn only, 30 days)
 does not defend   a compromised current release key AND the maintainer's account; rotate with a signed keys.json from a key
                   members already trust. Run as the member's own user: anything that can write ~/.local or ~/.config as them wins.
 L9 (no change)    accepted limit, recorded here, no code change (the brief did not spell out what L9 covers)
```

