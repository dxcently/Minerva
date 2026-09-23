"""Fetch each chosen dinkie icon, measure its real bbox against the declared
viewBox, and emit CSS mask rules with a viewBox that actually fits.

The mapping agents disagreed about whether the `-small` variants are clipped.
Rather than trust either side, measure: pull the path coords out of the body,
compare with the width/height Iconify declares, and widen the viewBox when the
art overflows it. Also renders a contact sheet so the result can be eyeballed.
"""
import json, re, subprocess, urllib.parse, urllib.request, os

S = os.path.dirname(os.path.abspath(__file__))
API = "https://api.iconify.design/dinkie-icons.json?icons="

# One icon per selector. Where the workflow returned two candidates for the
# same button, the non-"-small" form wins unless measurement says otherwise --
# decided below, after the bbox is known.
CHOSEN = [
    ("#integration-menu-button",      "jigsaw-puzzle-piece-small",      "tools / integrations"),
    ("#voice-input-button",           "mic-small",                      "dictate"),
    ("#model-valves-button",          "adjustments-filled",             "per-model valves"),
    ("#continue-response-button",     "right-black-triangle",           "continue response"),
    ('[id^="speak-button-"]',         "speaker-with-three-sound-waves", "read aloud"),
    ('[id^="info-"]',                 "circled-information-source",     "generation info"),
    (".copy-response-button",         "clipboard",                      "copy response"),
    ("#chat-copy-button",             "clipboard",                      "copy chat"),
    (".edit-user-message-button",     "pencil",                         "edit message"),
    ("#sidebar-search-button",        "left-magnifying-glass-small",    "search chats"),
    ("#sidebar-toggle-button",        "menu",                           "toggle sidebar"),
    ("#generate-title-button",        "sparkles",                       "generate title"),
    ('button[aria-label="Controls"]', "adjustments",                    "controls panel"),
    ("#save-temporary-chat-button",   "floppy-disk",                    "save temporary chat"),
    ("#chat-artifacts-button",        "cube",                           "artifacts"),
    # Added after the audit: it rejected these buttons only because the
    # "-small" icon first proposed was malformed or clipped. The full-size
    # forms measure clean, and the selectors were verified present.
    ("#input-menu-button",            "paperclip",                      "attach"),
    ("#sidebar-new-chat-button",      "memo",                           "new chat (sidebar)"),
    ("#new-chat-button",              "memo",                           "new chat"),
    (".regenerate-response-button",   "repeat-arrow",                   "regenerate"),
]

names = sorted({n for _, n, _ in CHOSEN})
# Fetched with curl -- the Iconify API 403s urllib's default User-Agent.
raw = os.path.join(S, "dinkie-raw.json")
if not os.path.exists(raw):
    subprocess.run(["curl.exe", "-s", API + urllib.parse.quote(",".join(names)), "-o", raw], check=True)
data = json.load(open(raw, encoding="utf-8"))

set_w, set_h = data.get("width", 16), data.get("height", 16)
icons = data["icons"]
missing = data.get("not_found", [])
if missing:
    print("NOT FOUND:", missing)

NUM = re.compile(r"-?\d+(?:\.\d+)?")


def bbox(body):
    """Crude but sufficient: dinkie bodies are axis-aligned rect/path steps,
    so every coordinate literal is a real x or y on the grid."""
    xs, ys = [], []
    for seg in re.findall(r"[MmLlHhVv][^A-Za-z]*", body):
        nums = [float(n) for n in NUM.findall(seg)]
        cmd = seg[0]
        if cmd in "Hh":
            xs += nums
        elif cmd in "Vv":
            ys += nums
        else:
            xs += nums[0::2]
            ys += nums[1::2]
    for r in re.findall(r'<rect[^>]*>', body):
        g = lambda a: float(re.search(a + r'="(-?[\d.]+)"', r).group(1)) if re.search(a + r'="(-?[\d.]+)"', r) else 0.0
        xs += [g("x"), g("x") + g("width")]
        ys += [g("y"), g("y") + g("height")]
    if not xs or not ys:
        return None
    return min(xs), min(ys), max(xs), max(ys)


rows = []
for sel, name, meaning in CHOSEN:
    ic = icons.get(name)
    if not ic:
        print(f"  !! {name} missing from reply")
        continue
    body = ic["body"]
    w, h = ic.get("width", set_w), ic.get("height", set_h)
    bb = bbox(body)
    note = ""
    if bb:
        x0, y0, x1, y1 = bb
        # Expand the viewBox if the art runs past it -- this is the clipping
        # the agents argued about, fixed rather than avoided.
        vw, vh = max(w, x1), max(h, y1)
        if vw > w or vh > h:
            note = f"CLIPPED {w}x{h} -> {vw:g}x{vh:g}"
        w, h = vw, vh
    rows.append((sel, name, meaning, body, w, h, note))
    print(f"  {name:34s} {w:g}x{h:g} {note}")

# ---- CSS -------------------------------------------------------------------
out = []
out.append("/* ---- dinkie-icons swaps (generated; see webui/README-icons.md) ---- */")
sels = ",\n".join(f".dark {s} svg" for s, *_ in rows)
out.append(sels + " {\n  display: none;\n}")
base = ",\n".join(f".dark {s}::before" for s, *_ in rows)
out.append(base + """ {
  content: "";
  display: block;
  width: 1.15rem;
  height: 1.15rem;
  background-color: currentColor;
  -webkit-mask-repeat: no-repeat; mask-repeat: no-repeat;
  -webkit-mask-position: center;  mask-position: center;
  -webkit-mask-size: contain;     mask-size: contain;
  image-rendering: pixelated;
}""")
for sel, name, meaning, body, w, h, note in rows:
    svg = (f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w:g} {h:g}'>{body}</svg>")
    uri = urllib.parse.quote(svg, safe="")
    out.append(
        f"/* {meaning} -- dinkie-icons:{name}{' | ' + note if note else ''} */\n"
        f".dark {sel}::before {{\n"
        f"  -webkit-mask-image: url(\"data:image/svg+xml,{uri}\");\n"
        f"  mask-image: url(\"data:image/svg+xml,{uri}\");\n}}"
    )
open(os.path.join(S, "icons.css"), "w", encoding="utf-8").write("\n".join(out) + "\n")
print("\nwrote icons.css")

# ---- contact sheet, so this can be eyeballed rather than trusted ----------
# Build the sheet from the SAME data URIs the CSS ships, via a stylesheet --
# inline style attributes turned the quoting into a minefield and produced a
# malformed URI that silently rendered nothing.
sheet_rules, cells = [], []
for i, (sel, name, meaning, body, w, h, note) in enumerate(rows):
    svg = f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 {w:g} {h:g}'>{body}</svg>"
    uri = urllib.parse.quote(svg, safe="")
    sheet_rules.append(
        f".i{i}{{-webkit-mask-image:url(\"data:image/svg+xml,{uri}\");"
        f"mask-image:url(\"data:image/svg+xml,{uri}\")}}"
    )
    flag = " <s>clip-fixed</s>" if note else ""
    cells.append(f"<figure><div class='i i{i}'></div><figcaption>{meaning}<br><b>{name}</b>{flag}</figcaption></figure>")

sheet = ("<!doctype html><meta charset=utf-8><style>"
    "body{background:#0b1410;color:#39ff6a;font:12px 'Cascadia Mono',monospace;margin:0;padding:16px;"
    "display:grid;grid-template-columns:repeat(5,1fr);gap:16px}"
    "figure{margin:0;text-align:center}"
    ".i{width:40px;height:40px;margin:0 auto 6px;background:#39ff6a;"
    "-webkit-mask-repeat:no-repeat;mask-repeat:no-repeat;"
    "-webkit-mask-position:center;mask-position:center;"
    "-webkit-mask-size:contain;mask-size:contain;image-rendering:pixelated}"
    "figcaption{font-size:10px;line-height:1.35;color:#8fd0a0}"
    "b{color:#39ff6a;font-weight:400}s{color:#ffa23a;text-decoration:none}"
    + "".join(sheet_rules) + "</style>" + "".join(cells))

open(os.path.join(S, "sheet.html"), "w", encoding="utf-8").write(sheet)
subprocess.run([
    "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
    "--headless", "--disable-gpu", "--screenshot=" + os.path.join(S, "sheet.png"),
    "--window-size=900,560", "file:///" + os.path.join(S, "sheet.html").replace("\\", "/"),
], check=False)
print("wrote sheet.png")
