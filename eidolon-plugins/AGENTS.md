# For agents

## Installing a plugin

1. Read `<name>/README.md` in full before running anything.
2. Run its sections in order: **Prerequisites**, **Install**, **Verify**.
   Where a section offers a choice (Nix or Cargo), use Nix if `nix` is on
   PATH, otherwise the other route.
3. Stop at the first command that fails. Report the command and its output.
   Don't improvise a workaround.
4. **Verify** passing is the definition of installed. Report its output.

What you may touch: `~/.config/eidolon/plugins/` and its `plugins.toml`
(which `eidolon plugins install` writes), the plugin's own token file, and a
Nix profile or `~/.cargo/bin` for its service. Two commands in the Install
section change permissions — `eidolon plugins trust` vouches the verbs (and
writes `policy.permits.rn`), `eidolon plugins grant` records the credential.
Run them as written and repeat them in your report: they are the part of an
install a person should be able to see afterwards. Nothing else under
`~/.config/eidolon/` — not `config.toml`, not `policy.rn`.

A tool file can name where it sends requests and which secret it spends.
Read every `tools/*.rn` you're about to install; the endpoint is at the
bottom of each file.

## Adding a plugin

- A folder per plugin, laid out as in the README's **Layout**. The folder
  *is* the plugin directory: `plugin.rn` at the root — `manifest()` returning
  `name` (equal to the folder's name), `version`, `description`, `claims` —
  and the payload directories beside it.
- Tool files are named for the bare verb (`open.rn`, `spawn.rn`), and the
  tool they declare carries the namespaced name (`name: "browser_open"`,
  `name: "subagent_spawn"`): eidolon registers `<folder>_<stem>`, so the stem
  is the verb and the declaration is what a session, a log line and a vouch
  row all say. A contribution that would join to a built-in's name is
  refused, not applied — plugins never shadow anything.
- Installing is `eidolon plugins install <owner>/<repo> <folder>`, then
  `eidolon plugins trust <plugin>` (installing does not trust); the listing is
  `eidolon plugins --dir <dir>`, and uninstalling is
  `eidolon plugins uninstall <plugin>`.
- Every tool file is self-contained: `manifest()`, `call()`, and its endpoint.
  Repeat shared helpers in each file rather than sharing them.
- Anything that has to keep running and that the plugin owns (a browser) is a
  service in `service/`, in Rust, built by `package.nix` and added to
  `flake.nix`, and declared in `plugin.rn`'s `service:` block so
  `eidolon plugins service` can start it.
  - The one exception is a model server the operator runs separately
    (`jev`'s Python and torch chooser and NLI scorer). It is not shipped and has
    no `service:` block. The plugin's README documents how to start it, its
    port and its token file, and its tools fail or park with a clear reason
    when it is down.
- The README has the four sections, as plain commands an agent can run.
- `cargo clippy --all-targets` is clean and `cargo test` passes.
