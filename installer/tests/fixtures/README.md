# tests/fixtures/ — `perch --help`, captured

```text
perch-help-old.txt   `perch --help` of the perch the operator has installed (~/perch-target/release/perch in WSL), built before
                     --plugins-dir, --aoide and the agent token files: it lists --home and --eidolon-config, so perch-open.sh starts
                     it, but it has none of the newer flags
perch-help-new.txt   `perch --help` of a debug build of master (the slices' shared target dir): every flag the start passes when it
                     is listed
```

`perch_open.rs` starts a stub perch whose `--help` is one of these, to prove the scripts pass a flag only when the binary lists it.
Captured with the binary's own `--help` (clap, one line per option, `--flag <VALUE>` at the start of the line); regenerate
`perch-help-new.txt` the same way when the perch gains a flag that perch-open.sh should pass, and add the flag to
`the_fixtures_are_what_they_claim`.
