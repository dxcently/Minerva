# FB: fresh-box fixes (written 2026-10-15 from the fresh-box audit wf_68ce7483-735; docs and config only)

Base: master 4831852, in a fresh worktree. This PC is about to be wiped. These trivial fixes let a fresh Windows or Linux box follow
the repo's own instructions. No Rust or JS code changes. Each item names its evidence: check it before you change anything, and
skip (and report) any item whose evidence does not hold.

1. .gitattributes: force LF for installer/chromium/pin.env, installer/chromium/libs.lock, installer/minisign.pub and providers/*.rn
   (`text eol=lf`). In a Git-for-Windows clone (core.autocrlf=true) pin.env gets CRLF, and fetch.sh then reads CFT_VERSION with a
   trailing \r. Keep the existing lines (.gitattributes:1-5). Show it works: clone master into a scratch dir with -c core.autocrlf=true
   and check those files with `git ls-files --eol` or `file`.
2. git rm tmp_edit_helper.py and tmp_fix_tasklist.py (repo root; scratch scripts tracked by mistake). First grep that nothing refers
   to them.
3. bench/bench.config.example:22-24 and bin/peer-bench.sh:11 hard-code /mnt/c/Users/dxcen/... and /tmp/venvtest. Make them
   repo-relative, or an env var with a placeholder default, in each file's own style.
4. docs/Getting-Started.md: the WSL clone target (:83, and the providers cp at :185-186) is ~/Minerva, which is also the perch's
   home (bin/perch-open.sh:92; HANDOFF.md says to clone elsewhere). Change the WSL clone path to ~/src/Minerva and fix the cp
   source to match. The Windows clone path ($HOME\Minerva, :141) is on another filesystem and stays.
5. Fresh-WSL prerequisites in docs/Getting-Started.md (and one line in perch/README.md where it lists what the perch needs):
   /etc/wsl.conf `[boot] systemd=true` (it gives XDG_RUNTIME_DIR, which the perch refuses to start without: bin/perch-open.sh:111-114);
   %UserProfile%\.wslconfig `vmIdleTimeout=-1` (already at Getting-Started.md:347; make it a listed prerequisite); `apt install
   musl-tools` (installer/README.md:392 needs it for a release bundle); and for building minerva-install.exe on Windows, Visual Studio
   Build Tools with "Desktop development with C++" (installer/README.md:387). In bin/perch-open.sh, ONLY the XDG_RUNTIME_DIR refusal
   message changes: it says to enable systemd in /etc/wsl.conf and run `wsl --shutdown`.
6. The Rust minimum. The crates use edition 2024 and let-chains (perch/src/guard/mod.rs:175-177), which need Rust 1.88 or newer. grep
   the docs for every stated Rust minimum (e.g. "1.85") and correct it to 1.88. Confirm with `rustc --version` in WSL, and
   say in your report which toolchain built this tree. Do not add rust-toolchain.toml and do not change Cargo.toml.
7. installer/README.md and installer/release-files.sh: the release procedure must sign SHA256SUMS too (`minisign -S` on it) and
   upload SHA256SUMS.minisig with `gh release create` (members are told to check it at :406). Fix the sentence at :429 claiming
   build.ps1 bumps and commits VERSION (build.ps1 only reads it). Name where minisign comes from (its GitHub releases, or `apt install
   minisign`).
8. A short "Tested on" line in docs/Getting-Started.md: the stack was built and tested on WSL Ubuntu 26.04 with Python 3.14, and
   models.ini's numbers were tuned on this PC's GPU and the Prism llama.cpp build. On other hardware they are a starting point to
   re-tune. Read the distro and Python version from WSL (`cat /etc/os-release`, `python3 --version`); don't guess.

## Grants

.gitattributes, tmp_edit_helper.py and tmp_fix_tasklist.py (delete), bench/bench.config.example, bin/peer-bench.sh, docs/Getting-Started.md,
perch/README.md (prerequisite lines only), bin/perch-open.sh (the XDG_RUNTIME_DIR refusal message only), installer/README.md,
installer/release-files.sh. Never edit HANDOFF.md, docs/webui-build/, Cargo.*, any .rs/.js/.mjs, or vendored trees. Keep each
file's line endings as they are (many docs have mixed endings: change only the lines you touch). One commit, `docs: ...`, no
attribution. Run `bash -n bin/perch-open.sh bin/peer-bench.sh installer/release-files.sh` after editing them.
