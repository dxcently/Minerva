#!/usr/bin/env python3
"""Write the thin per-graph workflows from the table below.

Each graph has a workflow in workflows/<id>.rn; they differ in the graph id, the
header, the params, the approval and the defaults. The pin line is left alone if
the file exists (tests/jev/pin.sh owns it).

    python3 tests/jev/gen_workflows.py          write the files
    python3 tests/jev/gen_workflows.py --check  exit 1 if one differs (ignoring the pin)
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
WF = os.path.join(HERE, "..", "..", "jev", "workflows")
ZERO = "sha256:" + "0" * 64

TAIL = """
// PIN is the sha256 of graphs/%(id)s.json; tests/jev/pin.sh rewrites it.
const GRAPH = "%(id)s";
const PIN = "%(pin)s";

pub fn workflow() {
    #{
        name: "%(id)s",
        description: "%(desc)s",
        approval: "%(approval)s",
        params: #{
            "type": "object",
            "properties": #{
%(props)s
            },%(required)s
        },
    }
}

pub async fn run(args) {
    run_pinned(GRAPH, PIN, %(input)s).await
}
"""

TRIAGE_PROPS = [
    ("distro", "string", "Run the commands inside this WSL distribution."),
    ("ssh", "string", "Run the commands on this host over ssh (user@host or host; key-based)."),
    ("ssh_port", "integer", "The ssh port, with ssh. Default 22."),
]

WORKFLOWS = [
    dict(
        id="wiki-hop",
        approval="mutating",
        desc="Reach a target Wikipedia article from a start article by following links in the browser plugin, a chooser scoring the links on each page. Parks on a person when the chooser is unsure. Needs the browser plugin and the jev service.",
        props=[
            ("start", "string", "The article to start from, as it appears in the URL after /wiki/ (Cat, Ancient_Egypt)."),
            ("goal", "string", "The title of the article to reach."),
        ],
        required=["start", "goal"],
        input="args",
        head="""// `wiki-hop`: follow Wikipedia links from `start` to `goal` through the browser plugin.
// A hop costs about five steps of the host's 200-step cap, so a run is bounded at
// roughly thirty hops.
""",
    ),
    dict(
        id="ctf-juice-recon",
        approval="read_only",
        desc="Read-only recon of a local OWASP Juice Shop lab at 127.0.0.1:3001: its HTTP headers and robots.txt. Two fixed curl commands; optionally run inside a WSL distro.",
        props=[("distro", "string", "Run the curls inside this WSL distribution. Omit when the lab listens on this machine.")],
        required=[],
        input="with_defaults(args, #{})",
        head="""// `ctf-juice-recon`: the headers and robots.txt of a local Juice Shop lab; no chooser, no model.
""",
    ),
    dict(
        id="triage-linux",
        approval="read_only",
        desc="Triage an unfamiliar Linux box with a fixed menu of read-only commands, a chooser picking each next one: what it is, what it exposes, what is running, what looks out of place. Here by default; distro or ssh to run elsewhere. Needs the jev service.",
        props=TRIAGE_PROPS,
        required=[],
        input="with_defaults(args, #{})",
        head="""// `triage-linux`: a chooser walks a fixed menu of read-only recon commands.
""",
    ),
    dict(
        id="triage-wsl",
        approval="read_only",
        desc="Triage a WSL distribution (Ubuntu by default) with a fixed menu of read-only commands run through wsl.exe, a chooser picking each next one. Needs a Windows host and the jev service.",
        props=[("distro", "string", "The WSL distribution. Default Ubuntu.")],
        required=[],
        input='with_defaults(args, #{ distro: "Ubuntu" })',
        head="""// `triage-wsl`: triage-linux inside a WSL distribution through wsl.exe.
""",
    ),
    dict(
        id="triage-botforge",
        approval="read_only",
        desc="Read-only recon of the BotForge range guest over ssh with a fixed menu of commands, the chooser's pick always taken (no confidence floor, no NLI). ssh defaults to mford@127.0.0.1 port 2222. Needs the jev service for jev_choose.",
        props=[
            ("ssh", "string", "The host to triage over ssh. Default mford@127.0.0.1."),
            ("ssh_port", "integer", "The ssh port. Default 2222."),
        ],
        required=[],
        input='with_defaults(args, #{ ssh: "mford@127.0.0.1", ssh_port: 2222 })',
        head="""// `triage-botforge`: triage-linux for unattended runs. No NLI guards and floor and
// margin 0, so the chooser's pick is always taken and a run never parks.
""",
    ),
]


def render(w, pin):
    props = ",\n".join(
        '                "%s": #{ "type": "%s", "description": "%s" }' % (n, t, d.replace('"', '\\"'))
        for n, t, d in w["props"]
    )
    props += ","
    required = ""
    if w["required"]:
        required = '\n            "required": [%s],' % ", ".join('"%s"' % r for r in w["required"])
    return w["head"] + TAIL % dict(
        id=w["id"], pin=pin, desc=w["desc"], approval=w["approval"],
        props=props, required=required, input=w["input"],
    )


def main():
    check = "--check" in sys.argv
    bad = False
    for w in WORKFLOWS:
        path = os.path.join(WF, w["id"] + ".rn")
        have = open(path, encoding="utf-8").read() if os.path.exists(path) else None
        pin = ZERO
        if have:
            m = re.search(r'^const PIN = "(sha256:[0-9a-f]+)";$', have, re.M)
            if m:
                pin = m.group(1)
        want = render(w, pin)
        if have == want:
            print("ok     " + w["id"])
        elif check:
            print("STALE  " + w["id"], file=sys.stderr)
            bad = True
        else:
            with open(path, "w", encoding="utf-8", newline="\n") as f:
                f.write(want)
            print("wrote  " + w["id"])
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
