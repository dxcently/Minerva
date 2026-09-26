# Eidolon system files (binary-managed)

Everything in this directory was unpacked from the running `eidolon` binary
at launch and is kept in lockstep with it: what the harness ships, exactly
as it ships it. It is here to be read — every script in it is one that
runs, and `RUNE.md` is the guide to writing the next one.

**Do not edit here.** The directory is wiped and rewritten whenever the
binary's copy changes (`.stamp` is the version), so an edit lasts until the
next upgrade. Your copies live one level up:

- `../tools/*.rn`, `../policy.rn` and `../ui.rn` were seeded from here on
  first launch and are yours from then on: edit them in place, and the
  harness runs what you wrote. On an upgrade, a file you have not touched
  (still byte-identical to what the previous release shipped) is carried
  forward to the new version; a file you have edited is left exactly as it
  is, said on stderr at launch, and the newer version is here to diff
  against. Nothing of yours is ever deleted, and a seed you delete comes
  back on the next launch. A tool named like a built-in *is* that built-in:
  it is loaded in the built-in's place and on the built-in's terms (the
  `mneme_*` tools need a vault configured and `peers`/`send` a session with
  peers, whoever wrote the file).
- `../providers/` is **not** seeded. A script there loads *after* the
  `[[providers]]` patches in `config.toml` and replaces the built-in whole,
  so a seeded copy would discard the patch naming your key — and the
  directory may be generated for you, as `config.toml` is. `providers/`
  here is the reference: a `[[providers]]` table naming `zai` or
  `deepseek` changes a field of the built-in, and a `providers/zai.rn` of
  your own replaces it.
- `prelude.rn` is compiled in front of every `ui.rn` and cannot be
  replaced; it is here so the builders `ui.rn` calls are readable.
- `examples/` are starting points, not shipped behaviour.
