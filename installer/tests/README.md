# tests/ — the perch contract, end to end

Cargo's integration tests for what the Minerva server and `hoot` ask of the installer (`installer/README.md`, "The perch contract").
They run the real `minerva` binary and the real scripts (`bin/perch-open.sh`, `nix/minerva.sh`) in a **cleared environment**: a temp
`HOME`, a temp `XDG_RUNTIME_DIR`, and nothing of the operator's visible. Unix only. Run in WSL:

```text
cd installer && MINERVA_ALLOW_PLACEHOLDER_KEY=1 CARGO_TARGET_DIR=$HOME/.cache/minerva-slices-target cargo test --offline
```

```text
common/mod.rs         the fixture Env (temp HOME with DECOY tokens at every default place a script could read one), the stub HTTP
                      server on a loopback port the OS picks, and the curl shim
cli.rs                `status --json` over fixture prefixes; `keys add|remove --json` (every error kind, keys dir unchanged on a
                      refusal); `keys list|inspect --json` failing in the same shape; a revoked key under a fresh id; the
                      `web --keys-arm` checks that need no perch
perch_open.rs         the key arm of bin/perch-open.sh (the Linux arm: only the Linux script arms); the flags a newer perch gets and
                      an older one does not, in both scripts
nix_allow_web.rs      `nix/minerva.sh keys allow-web`: refused with the Nix sentence, nothing armed (no curl call, no runtime file)
release_files.rs      `installer/release-files.sh` on temp release dirs: a keys.json that revokes an id with no revoke_fp entry, names a
                      fingerprint that is not 16 hex digits, or has a revoke or revoke_fp list it cannot read is refused in both modes,
                      nothing written; the forms it must accept (mixed case, spread over lines, no keys.json) pass
fixtures/             `perch --help` as printed by the perch an operator has installed today (perch-help-old.txt) and by a build of
                      master (perch-help-new.txt); the start tests probe against these, not against a hand-written imitation
```

**Nothing here can reach the operator's perch.** The stub listens on a loopback port the OS picks for this process, never 47820. A `curl` shim is first
on `PATH` for every script: it logs each call's argv, the mode of every `-H @file` header file and what it held, and exits 7 without
making any call aimed at `:47820`. So a script that ignored `--port` would be seen (and stopped), and "the token is not in argv" is
checked against what curl was really given. The decoy tokens (`DECOY`) sit where the scripts would find a default; the test hands over
a different one (`GIVEN`), and the stub must receive exactly that.

Tests that use the stub port, or ask what is on it, hold a `flock` on `minerva-installer-stub.lock` in the temp dir (the host's, not the
target dir's), so they take turns across threads, across processes and across worktrees on this host.
The start tests run a stub perch that is a shell script: its `--help` is a fixture (coloured when `CLICOLOR_FORCE` or `FORCE_COLOR`
is in its environment, as clap's is), it records the argv it was started with, and it writes its own pid, which is the one process the
test kills afterwards (and only while that pid is still the stub).

The unit tests of the same slice are next to the code: `src/status/tests.rs` (the JSON, the cache, the plugin record, the jev states,
each held to the sentence the command itself would print) and `src/keys.rs` (`every_refusal_carries_its_kind_through_added_context`,
and the fingerprint, record and crash tests: `a_revoked_key_has_no_way_back_under_a_fresh_id`, `trusted_never_lists_a_key_whose_fingerprint_is_revoked`,
`a_rotation_that_revokes_a_key_this_machine_never_held_needs_its_fingerprint`, `a_revocation_listed_again_without_its_fingerprint_keeps_the_one_it_was_given`,
`a_key_a_revoked_key_added_keeps_its_fingerprint_only_while_its_bytes_are_held`,
`add_manual_refuses_a_key_already_in_the_list_and_never_rewrites_its_record`, `a_crash_while_the_keys_dir_is_made_leaves_nothing_that_reads_as_a_keys_dir`,
`a_fault_after_the_record_and_before_the_rename_leaves_no_keys_dir`).
The `compiled` kind is only reachable there: a test build compiles in the placeholder key, so the CLI has no compiled-in key to refuse.
