# Aoide core on Minerva

Scope: `aoide` and `aoided` from `Aoide/pkgs/aoide`. Lyra, the desktop/rice components, and AoideOS are not part of this integration.

## Current support

- WSL/Linux: both core binaries build. The protocol suite passes (154 tests), and the two storage lock regressions pass.
- Native Windows: the protocol suite passes (152 tests), including an actual child process exit-code/audit check, replacement-file detection, and private-file ACL creation. The complete core binaries do not build yet.
- Native storage check still has 10 errors: Unix IPC in `attest`, symlink creation, hostname, and Linux-only no-replace rename. Further server/client/process-identity/secrets dependencies still need a native port once storage compiles. This is not a working native release.

Windows changes currently cover executable discovery, file identity, private file creation, and cross-platform locking. Private files receive owner/SYSTEM access at creation, before bytes are written. Unix feed permissions remain unchanged; Windows feeds do not infer group grants from Unix mode bits. No credential gate was replaced by a successful no-op.

## Reproduce

From `Aoide/pkgs/aoide` on Windows:

```powershell
cargo test -p aoide-protocol --lib
cargo check -p aoide-cli --bin aoide --bin aoided
```

In Ubuntu/WSL:

```bash
cd /mnt/c/Users/dxcen/Projects/bonsai2/Aoide/pkgs/aoide
CARGO_TARGET_DIR="$HOME/.cache/minerva/aoide-target" cargo build -p aoide-cli --bin aoide --bin aoided
```

No resident Aoide service has been installed. `aoided` currently does not handle `--help`; passing it starts the daemon. The temporary daemon from the build check was stopped explicitly.
