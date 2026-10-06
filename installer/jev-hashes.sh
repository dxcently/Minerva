#!/usr/bin/env bash
# jev-hashes.sh: regenerate jev/requirements.torch.txt and jev/requirements.pinned.txt (hash-pinned; needs network and python3 -m venv).
# Run it when a pin changes. The versions below are the ones tested together; bump them here, rerun, test jev-setup.
set -eu
J=$(cd "$(dirname "${BASH_SOURCE[0]}")/../jev" && pwd)
W=$(mktemp -d /tmp/hashreq.XXXXXX)
cd "$W"
python3 -m venv v && ./v/bin/pip install -q pip-tools >/dev/null
cat > torch.in <<'EOF'
torch==2.14.1+cpu
EOF
cat > pypi.in <<'EOF'
annotated-doc==0.0.5
annotated-types==0.8.0
anyio==4.15.1
certifi==2026.7.22
click==8.5.0
fastapi==0.142.2
filelock==4.0.12
fsspec==2026.9.0
h11==0.16.0
hf-xet==1.6.0
httpcore==1.0.9
httpx==0.28.1
huggingface_hub==1.33.0
idna==3.20
Jinja2==3.1.6
markdown-it-py==4.2.0
MarkupSafe==3.0.4
mdurl==0.1.2
mpmath==1.3.0
networkx==3.7
numpy==2.5.3
opentelemetry-api==1.45.0
packaging==26.3
pydantic==2.13.5
pydantic_core==2.46.5
Pygments==2.21.0
PyYAML==6.0.3
regex==2026.9.29
rich==15.0.0
safetensors==0.8.0
setuptools==84.0.0
shellingham==1.5.4
starlette==1.7.0
sympy==1.14.0
tokenizers==0.23.2
tqdm==4.70.1
transformers==5.18.0
typer==0.27.2
typing-inspection==0.4.4
typing_extensions==4.16.0
uvicorn==0.54.0
EOF
python3 - <<'PY' > torch.txt
import re, urllib.request, html
page = urllib.request.urlopen("https://download.pytorch.org/whl/cpu/torch/").read().decode()
hs = set()
for m in re.finditer(r'href="([^"]+)"', page):
    href = html.unescape(m.group(1))
    name = href.split("#")[0].rsplit("/", 1)[-1]
    if name.startswith("torch-2.14.1%2Bcpu-") and "linux" in name and "x86_64" in name and name.endswith(".whl") and "#sha256=" in href:
        hs.add(href.split("#sha256=")[1])
assert hs, "no torch 2.14.1+cpu linux wheels found on the index"
print("torch==2.14.1+cpu \\")
hs = sorted(hs)
for i, h in enumerate(hs):
    print("    --hash=sha256:" + h + (" \\" if i < len(hs) - 1 else ""))
PY
./v/bin/pip-compile --quiet --generate-hashes --no-annotate --no-header --no-emit-index-url --allow-unsafe pypi.in -o pypi.txt 2>&1 | tail -5
wc -l torch.txt pypi.txt
{
  echo "# jev's torch, from the PyTorch CPU index only: pip install --require-hashes --no-deps --index-url https://download.pytorch.org/whl/cpu -r requirements.torch.txt"
  echo "# (the CPU build has no CUDA libraries, ~1.2 GB less than the default Linux wheel). Every file is pinned by sha-256, so the"
  echo "# index cannot substitute anything; installed first, with --no-deps, because the rest comes from PyPI (requirements.pinned.txt)."
  echo "# Regenerate: installer/jev-hashes.sh (pip-compile --generate-hashes). Tested on Python 3.14."
  cat torch.txt
} > "$J/requirements.torch.txt"
{
  echo "# jev's everything-else, from PyPI only: pip install --require-hashes --no-deps -r requirements.pinned.txt (after requirements.torch.txt)."
  echo "# Every file is pinned by sha-256; --no-deps because this list is the whole closure. Regenerate: installer/jev-hashes.sh."
  echo "# Resolved and tested on Python 3.14 (2026-10-06); hashes cover every published wheel of each version, so 3.12/3.13 work too."
  cat pypi.txt
} > "$J/requirements.pinned.txt"
rm -rf "$W"
head -8 "$J/requirements.torch.txt" | cut -c1-150
grep -c -- "--hash" "$J/requirements.pinned.txt"
