#!/usr/bin/env python3
"""A stand-in for the jev service, for tests: rules, not a chooser and not an NLI model.
It answers the real contract (POST /call {method, args} with a bearer token, {ok, result}
back) deterministically, so it proves the plumbing and says nothing about judgement.

  choose   MODE=first (default)  0.9 on the first option, the rest share 0.1
           MODE=flat             every option equal (so a floor parks the run)
           MODE=last             0.9 on the last option
           MODE=none             answers an error, like a service that is up but broken
           An option equal to the context's first "Target article: X" gets 0.95.
  entail   entailment 0.9 when most of the hypothesis's longer words appear in the
           premise, else neutral 0.8. MODE=none errors here too.

    python3 tests/jev/stub_service.py --port 8091 --token-file /tmp/jev.token [--mode first]
"""
import argparse
import json
import re
from http.server import BaseHTTPRequestHandler, HTTPServer

STATE = {"mode": "first", "token": "", "calls": []}


def choose(args):
    options = args.get("options") or []
    context = args.get("context") or ""
    if len(options) < 2 or len(set(options)) != len(options):
        raise ValueError("choose needs a context and at least two distinct options")
    mode = STATE["mode"]
    n = len(options)
    if mode == "flat":
        probs = [1.0 / n] * n
    else:
        hot = n - 1 if mode == "last" else 0
        probs = [0.1 / (n - 1)] * n
        probs[hot] = 0.9
    m = re.search(r"Target article: (.*)", context)
    if m:
        for i, o in enumerate(options):
            if o.strip().lower() == m.group(1).strip().lower():
                probs = [0.05 / (n - 1)] * n
                probs[i] = 0.95
    scored = {o: p for o, p in zip(options, probs)}
    best = max(scored, key=scored.get)
    return {"probs": scored, "best": best}


def words(s):
    return [w for w in re.findall(r"[a-z0-9]+", s.lower()) if len(w) >= 5]


def entail(args):
    premise = (args.get("premise") or "").lower()
    out = []
    for h in args.get("hypotheses") or []:
        ws = words(h)
        hit = sum(1 for w in ws if w in premise)
        if ws and hit * 2 > len(ws):
            scores = {"contradiction": 0.02, "entailment": 0.9, "neutral": 0.08}
            label = "entailment"
        else:
            scores = {"contradiction": 0.1, "entailment": 0.1, "neutral": 0.8}
            label = "neutral"
        out.append({"hypothesis": h, "label": label, "scores": scores})
    return {"results": out}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path == "/health":
            return self.reply(200, {"ok": True, "stub": True})
        if self.path == "/calls":
            return self.reply(200, STATE["calls"])
        self.reply(404, {"ok": False, "error": "not found"})

    def do_POST(self):
        if self.path != "/call":
            return self.reply(404, {"ok": False, "error": "not found"})
        if self.headers.get("Authorization") != "Bearer " + STATE["token"]:
            return self.reply(401, {"ok": False, "error": "bad token"})
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}")
        method, args = body.get("method"), body.get("args") or {}
        STATE["calls"].append(method)
        if STATE["mode"] == "none":
            return self.reply(200, {"ok": False, "error": "stub: the model is not loaded"})
        try:
            if method == "choose":
                return self.reply(200, {"ok": True, "result": choose(args)})
            if method == "entail":
                return self.reply(200, {"ok": True, "result": entail(args)})
            return self.reply(200, {"ok": False, "error": "unknown method " + str(method)})
        except Exception as e:
            return self.reply(200, {"ok": False, "error": str(e)})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8091)
    ap.add_argument("--token-file", required=True)
    ap.add_argument("--mode", default="first", choices=["first", "flat", "last", "none"])
    a = ap.parse_args()
    STATE["mode"] = a.mode
    STATE["token"] = open(a.token_file).read().strip()
    HTTPServer(("127.0.0.1", a.port), H).serve_forever()


if __name__ == "__main__":
    main()
