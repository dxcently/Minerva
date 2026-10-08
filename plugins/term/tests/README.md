# tests/ — plugins/term against a stub perch, through a real eidolon

No model is called and no perch is needed. Run in WSL or Linux with `eidolon` and `python3` on `PATH` (or `EIDOLON=/path`):

```text
bash plugins/term/tests/shape.sh        the tools: listing, trust, request shapes, errors, the url file      (port 48116)
bash plugins/term/tests/bundle.sh       installer/build.sh --dry-run lists plugins/term and the notices      (no port)
```

```text
shape.sh        the runner: a fresh temp HOME and XDG_* per case, eidolon under `env -i` with the mock provider and no credentials
stub.py         127.0.0.1:48116, answers /hub/term/a/<verb> and writes one line per request (method, path, auth state, type, body)
check.py        the assertions of each case
probe/          a plugin only the tests use: one workflow that calls the five tools through the gate, as an agent's turn would
bundle.sh       the build.sh dry run
```

**Isolation.** `eidolon` runs under `env -i` with `HOME` and every `XDG_*` dir in a temp dir; the operator's `~/.config/eidolon` is never
read. The stub is the only listener a tool can reach, it refuses to start on 47820, 8090 or 8091, and it logs only whether the
Authorization header matched the test token (`ok`, `wrong`, `none` or `other`), never the header. The plugin's tools name no port at
all (there is no default to fall back to), so a case reaches the stub only through the url file the case writes; `plugins/term` itself
is not touched, and each case runs a **copy** of it.

| case | what it proves |
|---|---|
| tails | the five tool files carry the same shared tail, byte for byte, and a single token file, scope path and timeout; no file names port 47820 |
| listing | `eidolon tools` lists the five `term_*`; `list`/`read` are read_only, `open`/`send`/`close` mutating; the descriptions say what the model must be told (untrusted text and titles, ssh via open + send, `held`) |
| ungranted | the verbs are vouched but the token file is not granted and nobody can be asked: refused, and the stub saw nothing |
| notoken | granted, but no token file: refused naming the file, nothing sent |
| vouched | the state the installer leaves (four vouches, five grants), nobody at the door: list, open, read, close run; **term_send is declined and never reaches the perch** |
| yolo | every verb's exact request (method, path, Bearer, content type, body), extra fields a model adds dropped, and the answers (a note line, then the perch's JSON) |
| errors | 423, 403 (the guard's sentence is passed on), 404, 401 and an unreachable perch become one sentence each; a send with nothing to type and a call with no id send nothing |
| note | a read's answer leads with the untrusted note line, and the JSON after it is the perch's, `text` left exactly as given |
| list-note | a list's answer leads with the note line, and the titles come through as the perch gave them |
| urlfile | the url file is used when it names `http://127.0.0.1:<port>`; any other host, scheme, userinfo, path, port, `localhost`, `[::2]`, garbage, or no file at all is **refused by name, and the stub sees nothing** (no fallback port). `base_url` also accepts `http://[::1]:<port>`, but no case here exercises that branch: the stub listens on 127.0.0.1 only |

`T3_SHOW=1` prints what each run reported.

**Mutation check (run once by hand, not kept in the suite).** Two breaks were put into the five tools one at a time, and `shape.sh`
failed on each: the note dropped from `read.rn` (the answer no longer leads with the note; the `note` case caught it), and
`localhost` accepted in the url file of all five tools (the `urlfile` refusal caught it, and the stub saw the request). Each file was
restored from a copy taken before the change, and compared byte for byte. Other breaks named in earlier drafts of this README were not run.
