# plugins/ — eidolon plugins that Minerva owns

Plugins whose other half is the Minerva perch. They are MIT like the rest of Minerva and live here, not in `eidolon-plugins/`
(which is the curated upstream collection, vendored and never hand-edited), because they change with the perch: a tool here is a
thin client of a route in `perch/src/`, and the two ship in the same bundle.

```text
term/     the agent's hands on the web terminals: term_list, term_open, term_read, term_send, term_close     (slice T3)
mail/     mail_send, the agent's Aoide mail, opt in per chat                                                  (slice M3, not yet)
```

A folder here is a plugin when it has a `plugin.rn`. `installer/build.sh` and `nix/plugins.nix` copy every such folder next to
`browser`, `jev` and `subagent` (without its `tests/`), and `minerva plugins sync` installs them into the member's eidolon the same
way. `build.sh` ships only the files git tracks, and it refuses a symlink or a folder named like one of the other plugins; a file
that should ship must be committed, and a `.env`, a key or a cache is never shipped. The trust each one gets is written out in
`installer/src/plugins.rs` (`trust_one`), not left to a generic rule, and a folder there with no rule is installed and vouched for
nothing:

```text
term   each pass that reaches it (an install, an update, a full sync; not a settled --if-needed pass): untrust (every vouch term has),
       then list, read, open, close vouched; term_send NEVER vouched (it asks in auto and in manual, runs in yolo); all five granted
       file:~/.config/eidolon/minerva-term.token
mail   each pass that reaches it: untrust, then no vouch (mail_send asks in auto and in manual); granted file:~/.config/eidolon/minerva-mail.token
```

The token files, and the url file the tools read to find the perch, are written by the perch itself at every start
(`perch/src/agent/README.md`). A plugin here reads them at call time and holds nothing between calls. A url file that names no
loopback perch makes the tools refuse the call and send nothing: there is no default port to fall back to.
