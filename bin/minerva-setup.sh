#!/usr/bin/env bash
# minerva-setup.sh: run the Minerva first-run wizard (setup/), with a clear message when it is not built.
# `hoot init` and a first `hoot` call this through wsl.exe; every argument is passed on
# (`--print-roots` is what perch-open.sh uses). Exit 3 means "not built"; the wizard itself exits 2 (no terminal) or 130 (cancelled).
#   MINERVA_SETUP_BIN  default $HOME/setup-target/release/minerva-setup. Build it in WSL with:
#                        cd setup && CARGO_TARGET_DIR=$HOME/setup-target cargo build --release
set -euo pipefail
BIN=${MINERVA_SETUP_BIN:-$HOME/setup-target/release/minerva-setup}
if [ ! -x "$BIN" ]; then
  here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
  echo "minerva-setup: the setup wizard is not built: $BIN" >&2
  echo "build it:  cd $(cd "$here/.." && pwd)/setup && CARGO_TARGET_DIR=\$HOME/setup-target cargo build --release" >&2
  exit 3
fi
exec "$BIN" "$@"
