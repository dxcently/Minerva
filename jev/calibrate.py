"""Calibration + gate + ablation probe for a jevlike checkpoint over a JSONL eval set.
Usage: python calibrate.py <ckpt> <jsonl> [floor margin ...]
"""
import os, sys, json, math, time
from pathlib import Path

# jevlike is vendored at jev/jevlike, beside this file -- the same resolution
# server.py uses for the service (`JEVLIKE_ROOT`, defaulting to this
# directory). It used to be imported from a sibling checkout on one dev box,
# which ModuleNotFoundErrors on any other machine. The environment variable
# still overrides it, so an experiment against a modified jevlike checkout
# needs no edit here.
_JEV_DIR = Path(__file__).resolve().parent
JEVLIKE_ROOT = Path(os.getenv("JEVLIKE_ROOT", str(_JEV_DIR)))
if not JEVLIKE_ROOT.is_dir():
    raise SystemExit(
        f"JEVLIKE_ROOT is not a directory: {JEVLIKE_ROOT} -- set the "
        f"JEVLIKE_ROOT environment variable to a jevlike checkout"
    )
if str(JEVLIKE_ROOT) not in sys.path:
    sys.path.insert(0, str(JEVLIKE_ROOT))

import torch
from jevlike.model import load_checkpoint
from jevlike.data import ChoiceExample, validate
from jevlike.train import move

ckpt, path = sys.argv[1], sys.argv[2]
gates = [(0.35, 0.05), (0.5, 0.1), (0.6, 0.15), (0.8, 0.1), (0.9, 0.1)]
torch.set_num_threads(4)
model, collator, cfg = load_checkpoint(ckpt, torch.device('cpu'))
model.eval()
rows = [validate(json.loads(l)) for l in open(path, encoding='utf-8') if l.strip()]
print(f'ckpt={ckpt.split(chr(92))[-1].split("/")[-1]} rows={len(rows)} cfg={cfg}')

def score_batch(examples):
    batch = move(collator(examples), torch.device('cpu'))
    with torch.no_grad():
        p = model(batch).softmax(-1)
    return [p[i, :len(e.options)].tolist() for i, e in enumerate(examples)]

def variants(e):
    lines = e.context.split('\n')
    goal_line = lines[0] if lines and lines[0].startswith('Target article:') else ''
    rest = '\n'.join(lines[1:]) if goal_line else e.context
    no_goal = rest
    header_only = '\n'.join(lines[:2]) + '\n' if goal_line else ''
    return no_goal, header_only

t0 = time.time()
recs = []
B = 64
prev_ctx = None
for start in range(0, len(rows), B):
    chunk = rows[start:start + B]
    full = score_batch(chunk)
    ng = score_batch([ChoiceExample(variants(e)[0] or ' ', e.options, e.label) for e in chunk])
    ho = score_batch([ChoiceExample(variants(e)[1] or ' ', e.options, e.label) for e in chunk])
    # shuffled-context control: pair each row with the previous row's context (eval.py rolls by 1 within a batch)
    sh = score_batch([ChoiceExample(chunk[i - 1].context, chunk[i].options, chunk[i].label) for i in range(len(chunk))])
    for e, pf, png, pho, psh in zip(chunk, full, ng, ho, sh):
        top = max(range(len(pf)), key=lambda i: pf[i])
        srt = sorted(pf, reverse=True)
        goal = e.context.split('\n')[0].replace('Target article: ', '')
        recs.append({
            'correct': top == e.label, 'p': pf[top], 'margin': pf[top] - (srt[1] if len(srt) > 1 else 0.0),
            'rank_correct': sorted(range(len(pf)), key=lambda i: -pf[i]).index(e.label) + 1,
            'n_opts': len(pf),
            'goal_in_menu': goal in e.options, 'goal_is_label': e.options[e.label] == goal,
            'surv_no_goal': max(range(len(png)), key=lambda i: png[i]) == top,
            'surv_header_only': max(range(len(pho)), key=lambda i: pho[i]) == top,
            'p_header_only': max(pho), 'p_no_goal': max(png),
            'sh_correct': max(range(len(psh)), key=lambda i: psh[i]) == e.label, 'sh_p': max(psh),
        })
elapsed = time.time() - t0
n = len(recs)
acc = sum(r['correct'] for r in recs) / n
top3 = sum(r['rank_correct'] <= 3 for r in recs) / n
mean_opts = sum(r['n_opts'] for r in recs) / n
print(f'scored {n} rows x 4 variants in {elapsed:.1f}s ({elapsed / n * 1000:.2f} ms/row/variant/4)')
print(f'top1={acc:.4f} top3={top3:.4f} mean_options={mean_opts:.1f} random_top1~={sum(1 / r["n_opts"] for r in recs) / n:.4f}')
sh_acc = sum(r['sh_correct'] for r in recs) / n
print(f'shuffled-context control: top1={sh_acc:.4f} mean max-p={sum(r["sh_p"] for r in recs) / n:.4f}')

def ece_and_bins(key_p='p', key_c='correct'):
    ece = 0.0; bins = []
    for b in range(10):
        lo, hi = b / 10, (b + 1) / 10
        sel = [r for r in recs if lo <= r[key_p] < hi or (b == 9 and r[key_p] == 1.0)]
        if not sel: bins.append((lo, hi, 0, None, None)); continue
        a = sum(r[key_c] for r in sel) / len(sel); c = sum(r[key_p] for r in sel) / len(sel)
        ece += len(sel) / n * abs(a - c)
        bins.append((lo, hi, len(sel), a, c))
    return ece, bins

ece, bins = ece_and_bins()
print(f'ECE={ece:.4f}   per-bin (lo-hi: n, accuracy, mean confidence):')
for lo, hi, k, a, c in bins:
    print(f'  [{lo:.1f},{hi:.1f}): n={k:5d}' + (f'  acc={a:.3f}  conf={c:.3f}  gap={c - a:+.3f}' if k else ''))
sh_ece, _ = ece_and_bins('sh_p', 'sh_correct')
print(f'shuffled-context ECE={sh_ece:.4f}')
print('gate pass-through (top1>=floor and margin>=margin): share of rows passed, accuracy among passed, accuracy among parked')
for floor, margin in gates:
    passed = [r for r in recs if r['p'] >= floor and r['margin'] >= margin]
    parked = [r for r in recs if not (r['p'] >= floor and r['margin'] >= margin)]
    pa = sum(r['correct'] for r in passed) / len(passed) if passed else float('nan')
    ka = sum(r['correct'] for r in parked) / len(parked) if parked else float('nan')
    print(f'  floor={floor:.2f} margin={margin:.2f}: passed {len(passed) / n:6.1%}  acc(passed)={pa:.3f}  acc(parked)={ka:.3f}')
gim = [r for r in recs if r['goal_in_menu']]
print(f'goal-in-menu rows: {len(gim) / n:.1%} of rows; among them label==goal {sum(r["goal_is_label"] for r in gim) / max(1, len(gim)):.1%}; chooser correct {sum(r["correct"] for r in gim) / max(1, len(gim)):.3f}')
for name, key in (('goal ablation (top pick survives removing the Target line)', 'surv_no_goal'), ('page ablation (top pick survives removing the body)', 'surv_header_only')):
    right = [r for r in recs if r['correct']]; wrong = [r for r in recs if not r['correct']]
    print(f'{name}: survives on {sum(r[key] for r in right) / max(1, len(right)):.3f} of right picks, {sum(r[key] for r in wrong) / max(1, len(wrong)):.3f} of wrong picks')
hi_conf = [r for r in recs if r['p'] >= 0.9]
if hi_conf:
    print(f'rows with p>=0.9: {len(hi_conf)} ({len(hi_conf) / n:.1%}); accuracy {sum(r["correct"] for r in hi_conf) / len(hi_conf):.3f}; page-ablation survival {sum(r["surv_header_only"] for r in hi_conf) / len(hi_conf):.3f}; goal-ablation survival {sum(r["surv_no_goal"] for r in hi_conf) / len(hi_conf):.3f}')
