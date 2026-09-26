#!/bin/sh
# get-openjev.sh -- download the openjev NLI weights jev's `entail` needs (~9 GB)
#
#   jev/get-openjev.sh [DEST]     DEST defaults to <repo>/models/openjev
#
# Weights only: the model code is vendored at jev/openjev/modeling_openjev.py,
# because upstream's copy has moved on since this checkpoint. The revision is
# pinned to the one whose files were hash-checked against a working box
# (model.safetensors sha256 e6d4a4fa...c01d3b, 2026-09-26).
set -eu
REPO=AlexWortega/openjev
REV=058a6c24911b46d908fbe23541390f8af3df3e4d
SUB=qwen3.5-4b-nli
root=$(cd "$(dirname "$0")/.." && pwd)
dest=${1:-$root/models/openjev}
hf=${HF:-$root/jev/.venv/bin/hf}
[ -x "$hf" ] || { echo "no hf CLI at $hf; install jev/requirements.txt first" >&2; exit 1; }
tmp=$(mktemp -d "$root/models/.openjev-dl.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
"$hf" download "$REPO" --revision "$REV" --include "$SUB/*" --local-dir "$tmp"
mkdir -p "$dest"
mv "$tmp/$SUB"/* "$dest"/
echo "openjev weights in $dest"
