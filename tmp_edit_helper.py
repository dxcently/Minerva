import re


def sub(path, old, new):
    with open(path, "rb") as f:
        data = f.read().decode("utf-8")
    pattern = re.compile("\r?\n".join(re.escape(l) for l in old.split("\n")))
    m = pattern.search(data)
    if m is None:
        raise SystemExit("NOT FOUND:\n" + old[:300])
    text = data[m.start():m.end()]
    if "\r\n" in text:
        new = new.replace("\n", "\r\n")
    with open(path, "wb") as f:
        f.write((data[: m.start()] + new + data[m.end():]).encode("utf-8"))
    print("ok: replaced %d chars with %d" % (len(text), len(new)))
