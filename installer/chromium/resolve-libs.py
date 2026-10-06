#!/usr/bin/env python3
"""Regenerate libs.lock: the Ubuntu 22.04 (jammy) packages whose shared libraries chrome-headless-shell needs on a
machine that has only the essential packages. Developer tool, needs network; the build itself only reads libs.lock.

usage: resolve-libs.py WORKDIR > libs.lock
jammy (glibc 2.35) on purpose: a library built against an older glibc runs on every newer Ubuntu/Debian.
Lines: <package> <version> <pool path> <sha256> <size>
"""
import gzip, re, sys, urllib.request, os

MIRROR = "https://archive.ubuntu.com/ubuntu/"
SUITES = ["jammy", "jammy-updates", "jammy-security"]
ROOTS = """libglib2.0-0 libnspr4 libnss3 libatk1.0-0 libatk-bridge2.0-0 libatspi2.0-0 libdbus-1-3 libx11-6 libxcomposite1
libxdamage1 libxext6 libxfixes3 libxrandr2 libgbm1 libexpat1 libxcb1 libxkbcommon0 libudev1 libasound2 libxi6 libxrender1
libxres1 libdrm2 fonts-dejavu-core""".split()
# every Debian-family system has these (libc family, compiler runtime, zlib) and a newer one may have dropped others, e.g.
# libpcre3: so everything else the libraries need is bundled
SKIP = {"libc6", "libgcc-s1", "gcc-12-base", "libstdc++6", "zlib1g", "debconf", "dpkg", "perl-base", "libcrypt1", "libc-bin"}

def load(work):
    pk = {}
    for s in SUITES:
        f = os.path.join(work, s + ".gz")
        if not os.path.exists(f):
            urllib.request.urlretrieve(MIRROR + f"dists/{s}/main/binary-amd64/Packages.gz", f)
        for stanza in gzip.open(f, "rt", encoding="utf-8").read().split("\n\n"):
            d = {}
            key = None
            for line in stanza.split("\n"):
                if line[:1] in " \t" and key:
                    d[key] += "\n" + line
                elif ":" in line:
                    key, v = line.split(":", 1)
                    d[key] = v.strip()
            if "Package" not in d:
                continue
            old = pk.get(d["Package"])
            if old is None or vkey(d["Version"]) > vkey(old["Version"]):
                pk[d["Package"]] = d
    return pk

def vkey(v):
    return [int(x) if x.isdigit() else x for x in re.split(r"(\d+)", v) if x]

def main():
    work = sys.argv[1]
    os.makedirs(work, exist_ok=True)
    pk = load(work)
    seen, todo = {}, list(ROOTS)
    while todo:
        n = todo.pop()
        if n in seen or n in SKIP:
            continue
        d = pk.get(n)
        if d is None:
            sys.stderr.write(f"unresolved (skipped): {n}\n")
            continue
        seen[n] = d
        for alt in (d.get("Depends", "") + "," + d.get("Pre-Depends", "")).replace("\n", " ").split(","):
            alt = alt.strip()
            if alt:
                todo.append(re.split(r"[ (|]", alt)[0])
    for n in sorted(seen):
        d = seen[n]
        print(n, d["Version"], d["Filename"], d["SHA256"], d["Size"])

main()
