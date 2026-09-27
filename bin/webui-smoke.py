#!/usr/bin/env python3
"""Frame reader and predicate-waiter for bin/webui-smoke.sh.

`GET /api/events` is SSE: the door writes one `data: <json>` line per frame
(stream.rs:35-46). This reads the raw bytes the smoke's `curl` appends, keeps
only payloads that parse to an object with a string `type` (sse.js:31 — "no
type, no frame"), and answers the four questions the shell asks: wait for a
frame, count frames, list their types, read one field out of a frame.

Every offset is a byte offset into the raw file, so a step's window is
`--from $(wc -c < raw)` taken before its POST: no step can match an earlier
step's frames, and a frame the door sent before the mark is invisible.

Subcommands
  wait  --file RAW --from N [--to N] --timeout S [--type T]
        [--where PATH=REGEX]... [--last N]
        Prints the matching frame as one JSON line; exit 3 on timeout with
        the window's last N frames on stderr (never stdout).
  count --file RAW --from N [--to N] [--type T] [--where PATH=REGEX]...
        Prints how many frames in the window match. Exit 0 always.
  types --file RAW --from N [--to N]
        Prints the matching frames' types, space-separated, one line.
  tail  --file RAW --from N [--to N] [--last N]
        Prints the window's last N frames as JSON lines.
  field --path PATH   (JSON on stdin)
        Prints that field; exit 4 when it is absent. `--raw` prints the JSON
        encoding of a non-string value instead of its `str()`.

`PATH` is dotted: `kind`, `ask_id`, `input.command`. A `--where` value is a
Python regex searched in the field's text, so `--where 'input.command=^echo '`
anchors where it matters and `=` is a substring check by default.
"""

import argparse
import json
import re
import sys
import time

MISSING = object()


def frames(raw, start=0, end=None):
    """Parse every complete frame in raw[start:end]. A half-written last
    line, or one without a type, is dropped rather than guessed at."""
    try:
        with open(raw, "rb") as f:
            f.seek(start)
            data = f.read() if end is None else f.read(max(0, end - start))
    except (FileNotFoundError, ValueError):
        return []
    out = []
    for line in data.decode("utf-8", "replace").split("\n"):
        if not line.startswith("data:"):
            continue
        payload = line[5:].lstrip(" ")
        if not payload:
            continue
        try:
            frame = json.loads(payload)
        except ValueError:
            continue
        if isinstance(frame, dict) and isinstance(frame.get("type"), str):
            out.append(frame)
    return out


def dig(frame, path):
    cur = frame
    for part in path.split("."):
        if isinstance(cur, dict) and part in cur:
            cur = cur[part]
        else:
            return MISSING
    return cur


def text(value):
    return value if isinstance(value, str) else json.dumps(value)


def where_arg(spec):
    if "=" not in spec:
        raise argparse.ArgumentTypeError("--where takes PATH=REGEX, got %r" % spec)
    path, rx = spec.split("=", 1)
    try:
        return (path, re.compile(rx))
    except re.error as e:
        raise argparse.ArgumentTypeError("--where %s: bad regex: %s" % (path, e))


def matches(frame, type_, wheres):
    if type_ is not None and frame.get("type") != type_:
        return False
    for path, rx in wheres:
        value = dig(frame, path)
        if value is MISSING or not rx.search(text(value)):
            return False
    return True


def window(args):
    return frames(args.file, args.start, args.end)


def cmd_wait(args):
    deadline = time.monotonic() + args.timeout
    while True:
        for frame in window(args):
            if matches(frame, args.type, args.wheres):
                print(json.dumps(frame, ensure_ascii=False))
                return 0
        if time.monotonic() >= deadline:
            break
        time.sleep(0.1)
    seen = [f for f in window(args) if matches(f, args.type, []) or args.type is None]
    if args.last:
        for frame in (seen or window(args))[-args.last:]:
            sys.stderr.write("  frame: %s\n" % json.dumps(frame, ensure_ascii=False))
    sys.stderr.write(
        "  no %s frame within %.1fs (from byte %d)\n"
        % (args.type or "...", args.timeout, args.start)
    )
    return 3


def cmd_count(args):
    print(sum(1 for f in window(args) if matches(f, args.type, args.wheres)))
    return 0


def cmd_types(args):
    print(" ".join(f["type"] for f in window(args)))
    return 0


def cmd_tail(args):
    for frame in window(args)[-args.last:]:
        print(json.dumps(frame, ensure_ascii=False))
    return 0


def cmd_field(args):
    raw = sys.stdin.read()
    try:
        frame = json.loads(raw)
    except ValueError as e:
        sys.stderr.write("field: stdin is not JSON: %s\n" % e)
        return 2
    value = dig(frame, args.path)
    if value is MISSING:
        sys.stderr.write("field: no %s in %s\n" % (args.path, text(frame)[:200]))
        return 4
    print(value if args.raw else text(value))
    return 0


def main(argv):
    ap = argparse.ArgumentParser(prog="webui-smoke.py", description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    def shared(p, need_file=True):
        if need_file:
            p.add_argument("--file", required=True)
            p.add_argument("--from", dest="start", type=int, default=0)
            p.add_argument("--to", dest="end", type=int, default=None)
        p.add_argument("--type", default=None)
        p.add_argument("--where", dest="wheres", action="append", type=where_arg, default=[])

    w = sub.add_parser("wait")
    shared(w)
    w.add_argument("--timeout", type=float, required=True)
    w.add_argument("--last", type=int, default=0, help="frames to show on timeout")
    w.set_defaults(fn=cmd_wait)

    c = sub.add_parser("count")
    shared(c)
    c.set_defaults(fn=cmd_count)

    t = sub.add_parser("types")
    shared(t, need_file=True)
    t.set_defaults(fn=cmd_types)

    tl = sub.add_parser("tail")
    shared(tl, need_file=True)
    tl.add_argument("--last", type=int, default=12)
    tl.set_defaults(fn=cmd_tail)

    f = sub.add_parser("field")
    f.add_argument("--path", required=True)
    f.add_argument("--raw", action="store_true")
    f.set_defaults(fn=cmd_field)

    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
