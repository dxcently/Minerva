# nix: packaging for the root flake

The root `flake.nix` builds everything Minerva runs from the vendored trees, so one Minerva commit
pins the lot. One input only: nixpkgs, at the revision `eidolon/flake.lock` pins (one toolchain).
`eidolon/`, `harnox/` and `eidolon-plugins/` are subpaths of this repo, not inputs. eidolon itself is
built by its own unmodified `eidolon/nix/eidolon.nix`. Systems: x86_64-linux, aarch64-linux.

The version of Minerva comes from the root `VERSION` file (read by the flake and by the installer;
there is no second copy).

## Packages

| attr | what | file |
|---|---|---|
| `.#eidolon` | the agent core (runs its test suite) | `eidolon/nix/eidolon.nix` |
| `.#perch` | the Minerva server in front of the eidolon doors | `perch.nix` |
| `.#minerva-setup` | the first-run wizard | `minerva-setup.nix` |
| `.#webui-term` | the static web UI, at `share/minerva/webui-term` | `webui-term.nix` |
| `.#minerva` (= `.#default`) | the desktop set: the `minerva` command + `eidolon` on PATH | `minerva.nix`, `minerva.sh` |
| `.#minerva-eidolon` | server wrapper: `bin/minerva-eidolon` execs eidolon, no config baked in, nothing named `eidolon` | `minerva-eidolon.nix` |
| `.#eidolon-browser` | the browser plugin service (chromium from nixpkgs) | `eidolon-plugins/browser/package.nix` |
| `.#minerva-core` | the installer crate's `minerva` as `bin/minerva-core` (plugin sync) | `minerva-core.nix` |
| `.#minerva-plugins` | the bundled plugins at `share/minerva/plugins` | `plugins.nix` |

`checks.<system>.perch-tests` runs perch's tests (`--features test-bins`). Not built: `jev-service`
(torch; needs its own slice).

## The `minerva` command

`minerva web | status | stop | init | plugins | services | update | version`, same verbs as the Linux installer.
`perch`, `minerva-setup`, the UI and `eidolon` are referenced by store path. `minerva update`
never updates itself: it prints `installed with Nix; update with: nix profile upgrade minerva`.

## Plugins preinstalled

`minerva-core.nix` builds the installer crate's `minerva` binary as `bin/minerva-core`, and `plugins.nix` lays the bundled
plugins (browser, jev, subagent; without the browser's Rust source) at `share/minerva/plugins`. `minerva web` first runs
`minerva-core plugins sync --if-needed --quiet` and `services start --quiet`: the plugins go into the member's own
`~/.config/eidolon/plugins` through eidolon's stock `plugins install` and are trusted (the browser also gets its token grants);
a plugin of the same name that is not ours is never overwritten; a newer Minerva replaces ours and keeps them trusted. What is
ours is recorded in `~/.local/state/minerva-nix/state/minerva/plugins.json`, never in the store. The browser service is
`.#eidolon-browser` (Chromium from nixpkgs). `MINERVA_NO_PLUGIN_SYNC=1` skips all this. The compiled-in release key of
`minerva-core` is the placeholder on purpose: a Nix install never updates itself. jev's Python service stays a separate slice
(the jev plugin is installed and trusted, its service is not started).

## Install and update

```sh
nix profile install github:dxcently/Minerva      # installs .#default (the `minerva` entry)
nix profile upgrade minerva                      # update
```

NixOS, as a flake input (packages only; there is no module yet):

```nix
{
  inputs.minerva.url = "github:dxcently/Minerva";
  # then, in a module:
  #   environment.systemPackages = [ inputs.minerva.packages.${pkgs.system}.default ];
  # update with: nix flake update minerva
}
```

## Building and testing

A flake in a git checkout only sees tracked files. To build uncommitted work, build from a scratch
copy (`git archive HEAD`, overlay the new files, `git init && git add -A` there) and keep `result`
links out of the repo: `nix build .#minerva -o /tmp/minerva-result`. Never `path:./eidolon` (the
`target/` directories).
