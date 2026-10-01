#!/usr/bin/env bash
# wiki-hop.sh: run wiki-hop end to end with no Chromium and no model: installs jev and the
# fake `browser` into the temp config XDG_CONFIG_HOME names (guard.sh refuses otherwise),
# starts stub_service.py and runs `Cat` -> `Ancient Egypt`.
#
#   t=$(mktemp -d); HOME=$t XDG_CONFIG_HOME=$t/cfg XDG_STATE_HOME=$t/state \
#     XDG_DATA_HOME=$t/data bash tests/jev/wiki-hop.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
. "$here/guard.sh"
require_temp_config || exit 2
: "${XDG_STATE_HOME:?set XDG_STATE_HOME to a temp dir}" "${XDG_DATA_HOME:?set XDG_DATA_HOME to a temp dir}"
plug=$XDG_CONFIG_HOME/eidolon/plugins
mkdir -p "$plug"
rm -rf "$plug/jev" "$plug/browser"
cp -r "$repo/jev" "$plug/jev"
cp -r "$here/fake-browser" "$plug/browser"
eidolon plugins trust jev >/dev/null
eidolon plugins trust browser >/dev/null
umask 077
tok=$HOME/.config/eidolon/jev.token; mkdir -p "$(dirname "$tok")"
head -c 24 /dev/urandom | base64 > "$tok"
for v in jev_choose jev_entail; do eidolon plugins grant $v "file:~/.config/eidolon/jev.token" >/dev/null; done
python3 "$here/stub_service.py" --port 8091 --token-file "$tok" >/dev/null 2>&1 &
svc=$!
trap 'kill $svc 2>/dev/null || true' EXIT
sleep 1
export FAKE_BROWSER="$here/fake-browser" FB_STATE=$(mktemp -d)
eidolon workflow run "$plug/jev" wiki-hop --args '{"start":"Cat","goal":"Ancient Egypt"}' --provider mock | grep -o 'final_state[^,]*'
