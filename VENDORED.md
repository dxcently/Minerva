# Vendored code

Minerva carries these projects as **plain snapshots**: copies of one upstream commit, with no history and no submodule. A fresh clone therefore builds with nothing else to fetch.

- **Local edits:** none, unless a row says otherwise.
- **Updating:** re-copy a newer upstream commit over the folder and change its row here. Never hand-edit a snapshot. Fix it upstream and re-copy.

| Folder | Upstream | Revision | License | Notes |
|---|---|---|---|---|
| `eidolon/` | github.com/noah427/eidolon | `master` @ `c725183` (2026-09-26); upstream has no release tags | MIT (`Cargo.toml`, `[workspace.package]`) | The agent harness. Unix-only by upstream policy, so it runs in WSL. Its `[patch]` builds against `../harnox`, the folder below. Published here with Noah's OK (2026-09-26). |
| `harnox/` | github.com/noah427/harnox | tag `v0.3.8` (`3c1fe7d`), the tag eidolon `c725183` pins | MIT (`Cargo.toml`) | eidolon's shared Rust + LLM foundation. Published with Noah's OK (2026-09-26). |
| `aoide-core/` | github.com/dxcently/aoide-core | `main` @ `c81e2c05` | **GPL-3.0** (`aoide-core/LICENSE`) | Aoide's mesh node (`aoide`, `aoided`): `pkgs/aoide` of dxcently/Aoide, split out with its history, minus the lyra/song/screen paint half. It is a separate program: nothing MIT in Minerva links it, and the two only talk over its CLI and sockets. The GPL covers this folder and not the rest of the repo. |
| `jev/jevlike/` | jevlike 0.1.0 (Minimal Labs) | 0.1.0 | MIT (`jev/jevlike/LICENSE`) | Only `data.py`, `model.py` and `train.py`, which `jev/server.py` imports. **Local edit:** `__init__.py` no longer re-exports the vision scorers that were left out. The checkpoint is `jev/runs/synthetic.pt` (169 KB). |
| `jev/openjev/modeling_openjev.py` | huggingface.co/AlexWortega/openjev | the copy that shipped alongside the weights below | MIT (model card) | Model code only. The ~9 GB weights are downloaded by `jev/get-openjev.sh`, pinned to revision `058a6c24911b46d908fbe23541390f8af3df3e4d`, subfolder `qwen3.5-4b-nli`. `model.safetensors` sha256 `e6d4a4fa…c01d3b`, checked 2026-09-26. Upstream's `modeling_openjev.py` has since changed, which is why the matching copy is kept here. |

Neither eidolon nor harnox ships a LICENSE file upstream. Both declare MIT in `Cargo.toml` only. That declaration is the whole license statement quoted above, until upstream adds a file.
