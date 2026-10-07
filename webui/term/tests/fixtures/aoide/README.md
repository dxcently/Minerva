# tests/fixtures/aoide: what the mesh tab is tested against

Input for `tests/mesh.test.mjs` (the mesh tab, `mesh/view.js` and `mesh/index.js`). The files are LF on every checkout (this folder's `.gitattributes`). Nothing here is a copy of anything real: the state is made up, and `snapshot.json` and `events.json` are what the perch's mesh routes (`perch/src/mesh/`, slice R2b) answered in front of `fake_aoide`.

| file | what it is |
|---|---|
| `mesh.json`, `outbox.json`, `version.txt` | byte-identical copies of `perch/tests/fixtures/aoide/` (slice R2a): `aoide mesh --json`, `aoide mail outbox --json` (the envelope aoide prints; a snapshot's `mesh` and `outbox` are their `data`) and `aoide --version`. The test checks the snapshot carries exactly this `data` |
| `home/state/` | an Aoide home's state, as the perch reads it: `nodes.json` (osaka with the flat `allows`, yomi with the words per mesh, bridge with neither; and in them the fields that must never reach the page: `pubkey`, `tokenFile`, `bearerSecret`, a credential in a url), `node-cache/osaka.json` (a roster with markup and an ANSI escape in it) and `yomi.json`, `stage/graph.json` (with `say`, `activity` and `socket` fields the perch must drop) |
| `events.jsonl` | the feed the perch tails: both record shapes (the feed aoided keeps, and the audit log), the classes the page must never see (`secret`, `notification`), and a hostile payload |
| `snapshot.json` | `GET /hub/mesh` for that home with `fake_aoide` replaying `mesh.json` and `outbox.json`, as the perch answered it, in the documented key order |
| `snapshot-none.json` | `GET /hub/mesh` on a perch with no aoide program and no state folder: `available: false` and the one sentence |
| `events.json` | `GET /hub/mesh/events?after=0` over all of `events.jsonl`: the four audit and gate events, `next` the file's size |

`snapshot.json` was captured from a perch started by `perch/tests/live/fake-perch.sh` (a temp home, a python stand-in for `aoided` that only answers `ping`, `--aoide`, `--aoide-home`, `--aoide-daemon-socket` and `--aoide-events` pointing at that home) and four values were set by hand, because they depend on the machine: `aoide.path` is `~/.local/bin/aoide`, the daemon is `/run/user/1000/aoide/aoided.sock` answering in 3 ms, and the two roster ages are 180 s and 172800 s. The fake's log held exactly `--version`, `mesh --json` and `mail outbox --json`, and the stand-in daemon was sent exactly `{"v":0,"op":"ping"}`.

The shapes are the perch's, and the perch's per-mesh `grants` form is modelled from P-CHARTER's announcement, not observed (`perch/src/mesh/README.md`). `X0` is where a real aoide's output is compared; a fixture that disagrees with it is a fixture to fix.
