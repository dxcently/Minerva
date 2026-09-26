# Judgement

What catches a decision that is well-formed, authorized, in-bounds,
confidently made, and wrong.

This is the ruling task **S47** was asked for. It amends `automation.md` in
three places — the floor (§4), the `choose` block (§1, §2) and the decision
row (§5) — and it stands on the three rulings it was told not to re-litigate:
A5's *an observation the node cannot vouch for is never scored*, A6's
*authority comes from a written envelope and never from confidence*, and the
floor itself, which was observed working at hop 3 and at S52's two parks. It
extends four rulings in `Decisions.md`: *a measurement can refute the
justification and leave the ruling standing*; *a protection is only as
reachable as the layer it inspects*; *a filter that is too narrow produces
exactly what a filter that is working produces*; and *report the number you
measured, not the number you were given*. It writes no code. The
specification at the end is what an executor lands.

Every number below was measured on this box, today, in this session, by the
procedure named beside it, unless it is attributed to a Build-Log entry.
Nothing was reasoned from the model's architecture that could be observed
from its output. The probe scripts are in this session's scratch under `s47/`
and are not part of the tree; the spec turns the ones worth keeping into a
tool with tests.

## The model in one page

A chooser's confidence is a claim about the **options, relative to the rows
it was trained on**. Inside that distribution a calibrated 0.99 means "right
99 times in 100" and the floor is a sound instrument. Outside it the number
is a property of byte coincidences, and the floor reads it as permission. So
the question "is the chooser calibrated?" has no answer without saying *on
what*, and the answer today is:

| eval set | rows | options | top-1 | chance | ECE | at p ≥ 0.9: share, accuracy |
|---|---:|---:|---:|---:|---:|---|
| synthetic test (the training distribution) | 400 | 2–8 | **99.75%** | ~22% | **0.004** | — |
| synthetic test, shuffled context (jevlike's own control) | 400 | 2–8 | 22.75% | ~22% | **0.625** | — |
| Wikispeedia test (human next-clicks toward a target) | 4,373 | 43.2 mean | **4.48%** | 3.62% | **0.443** | 9.3% of rows, **7.1%** right |
| the same architecture after 12 minutes on 41,291 Wikispeedia rows, same test | 4,373 | 43.2 mean | **28.4%** | 3.62% | **0.039** | 0.5% of rows, 81.8% right |

The shipped checkpoint is exactly calibrated on the one thing it was trained
on, and on the task it is deployed for its confidence carries no information
at all: at every confidence level from 0.1 to 1.0 its accuracy is 2–7%, which
is chance. wiki-hop's own gate — floor 0.35, margin 0.05 — passes **61.7%**
of Wikispeedia steps through at **4.9%** accuracy and parks the rest at 3.9%.
The floor separates nothing from nothing. Every number that depends on it —
the escalation rate, the park count the unattended design is costed against,
`escalations: 30` — is decoration on this graph until the checkpoint has rows
from this graph's distribution and a measured curve over them.

The fourth row is the same 41,280 parameters after twelve minutes on rows of
the task: calibrated to 0.039, and under wiki-hop's own gate 22.1% of steps
pass at 65.4% accuracy while the 77.9% that park are right 17.9% of the
time — the floor separating something from something. The instrument was
never the problem; what it was pointed at was. It also says what a calibrated
tiny chooser costs: four hops in five park.

That is the finding, and it is the first ruling: **confidence is evidence
only inside a distribution somebody measured it on, and a run must know
whether it is inside one.** Everything else follows from it:

1. A checkpoint carries a **calibration record** — per eval set, per
   confidence bin, how often it was right — keyed by its content hash. A
   graph names the set its floor was set against. A run whose checkpoint
   has no record for that set is **advisory**: the chooser still scores and
   still logs, and every choice parks with its ranking as a suggestion. That
   is the bootstrap mode `automation.md` §1 already describes in prose
   ("the first runs are a person clicking with jev watching"), now enforced
   by a mechanism instead of hoped for — because the hope was measured false
   today: an out-of-distribution byte model is *confidently* random, not
   *unsurely* random, and J2 saw it at two hops out of three.
2. The decision that is wrong at hop 1 is one a **rule** can make: the goal
   was in the menu. `choose.prefer` is a list of guards evaluated per option
   before the chooser is consulted; the first option that passes is taken,
   `source: "rule"`, with the chooser's scores logged beside it. On 4,373
   human next-clicks the goal was in the menu 27.6% of the time and the
   human took it 90.1% of the time it was; the chooser took it 6.1%.
3. The runner hands the chooser the **menu shape the record was measured
   on**: duplicate labels collapse before scoring. Without that, the margin
   check compares a label with its own copy, and the trained checkpoint's
   right answer at hop 1 — `Felidae`, three times in the menu — parks as a
   tie with itself.
4. The five other directions offered are each answered with a measurement
   where one exists, and four are rejected on it. The one kept is the one
   already built: the park *is* the second opinion, and an advisory run
   asks it every time.
5. The landmark predicate is not what hides "See also". Document order and
   `max: 64` are — "See also" is 34 links at lines 3,561–3,675 of a
   10,795-line tree, behind eleven sections holding 763 named links — so no
   predicate reaches it and a `section` scope that names it does. The
   nearest-landmark rule stays.

What a check can see decides where it lives, and the table is in §2.4.

## 1. Is the chooser calibrated?

**Measurement.** Three instruments, all cheap, all run today.

*The training distribution.* `python -m jevlike.eval runs/synthetic.pt
data/synthetic/test.jsonl --device cpu` in the jevlike venv, 3.3 s wall:
top-1 0.9975, top-3 1.0, ECE 0.0041; shuffled-context control top-1 0.2275,
ECE 0.6250. The control is the number nobody read: it pairs every menu with
the wrong context, and the model's mean confidence stays near 0.85 while its
accuracy falls to chance. jevlike's README says a useful model should beat
the control; it does not say what the control's *calibration* means, and it
means this: **when the context stops carrying the answer, this model's
confidence does not fall.** That was measurable on 2026-09-18 and would have
predicted J2's hop 1.

*The task distribution.* Wikispeedia — West and Leskovec, WWW 2012 — is
51,284 human next-clicks toward a named target article, built by jevlike's
own `build_wikispeedia` into the exact context shape wiki-hop renders
(`Target article: …\nCurrent article: …\n<body>`), 64-option menus, split by
target so no target leaks across train and test. `scripts/get_wikispeedia.sh`
fetched 46 MB and built 41,291 / 5,620 / 4,373 rows in 2 m 05 s. Over the
test split the shipped `synthetic.pt` (id `827bed5a49f4`) scores:

| confidence bin | rows | accuracy | mean confidence | gap |
|---|---:|---:|---:|---:|
| [0.1, 0.2) | 433 | 0.023 | 0.164 | +0.14 |
| [0.2, 0.3) | 825 | 0.039 | 0.249 | +0.21 |
| [0.3, 0.4) | 680 | 0.049 | 0.349 | +0.30 |
| [0.4, 0.5) | 623 | 0.030 | 0.449 | +0.42 |
| [0.5, 0.6) | 466 | 0.041 | 0.547 | +0.51 |
| [0.6, 0.7) | 339 | 0.056 | 0.649 | +0.59 |
| [0.7, 0.8) | 316 | 0.057 | 0.745 | +0.69 |
| [0.8, 0.9) | 278 | 0.061 | 0.849 | +0.79 |
| [0.9, 1.0] | 408 | **0.071** | 0.963 | **+0.89** |

ECE 0.443; shuffled-context ECE 0.449 — the real context and a wrong one
calibrate identically, which is the same fact as the synthetic control seen
from the other side. What the gates let through:

| gate | passes | accuracy of what passed | accuracy of what parked |
|---|---:|---:|---:|
| wiki-hop: floor 0.35, margin 0.05 | 61.7% | 0.049 | 0.039 |
| schema default: 0.50 / 0.10 | 41.1% | 0.057 | 0.036 |
| triage `judge`: 0.60 / 0.15 | 30.7% | 0.062 | 0.037 |
| 0.90 / 0.10 | 9.3% | 0.071 | 0.042 |

*The incident itself.* E5's live capture of the Cat article (`within: main`,
`roles: [link]`, 4,954 chars) parsed with the real `a11y.parse_refs` and the
real `refs` filter gives 87 `main`-nearest named links, capped to 64 — 60
distinct, because `Felidae` appears three times and `Carnivora` and `[1]`
twice; the automation path does not deduplicate, only `jev_choose` does. The
shipped checkpoint over that menu, under nine contexts:

| context | top pick | p | `Felidae` p (rank) |
|---|---|---:|---:|
| A. J2's shape: `Target article: Felidae` / `Current article:` blank / tree | crepuscular | **0.9845** | 0.00007 (29) |
| B. after S52: `Current article: Cat` / tree | crepuscular | **0.1196** | 0.0065 (23) |
| C. goal swapped to Dog | Gmelin | 0.069 | 0.0035 (31) |
| D. goal swapped to Photosynthesis | meowing | 0.090 | 0.0059 (28) |
| E. tree only, no goal line | meowing | 0.710 | 0.00004 (49) |
| F. goal lines only, **no tree at all** | crepuscular | **0.9994** | 0.000000 (5) |
| G. a single space | frequency | 0.247 | 0.0226 (8) |
| H. the synthetic format: `Choose the exact badge Felidae. Badge: Felidae.` | grunting | 0.401 | 0.000007 (49) |

A reproduces J2 (0.9977 live on a page 200 chars different). F is the one to
read twice: the 0.99 for "crepuscular" is produced by the goal line alone —
the page contributed nothing — and B shows the same pick fall from 0.98 to
0.12 when three bytes are added to a line that has nothing to do with the
choice. S52's live rows confirm it the other way round: the same checkpoint,
the same page, goal "Wolf", with `Current article: Cat` present, parked at
0.13 and 0.12 and the driving model picked "Carnivora" at p = 0.004. **J2's
click and S52's parks are the same instrument reading the same page; what
differed was whether the word "Cat" was on the second line.** And H says the
checkpoint cannot even do its own trained task over this vocabulary: 64
mixed-case, bracketed, Unicode labels are not eight colour-animal pairs.

**Ruling 1.** The floor is a sound instrument against a checkpoint whose
calibration has been measured on rows of the graph's own distribution, and
against nothing else. On wiki-hop today it measures nothing, and the
escalation rate the unattended design is costed against is a property of
byte coincidences in a 192-byte window. `automation.md` §1's sentence —
*"against the synthetic checkpoint almost every step will escalate, which is
the point"* — is refuted by J2 and by the 61.7% row above, and is struck by
the spec; the reason it was wrong is the reason the ruling is general: **an
untrained scorer does not know it is untrained.** Confidence measures
agreement among the options; it cannot measure distance from the training
set, and the floor was asked to read the second off the first.

**What it changes.** Everything in this document that costs anything is
worth building only as (a) the calibration gate itself and (b) rules, which
are free. The detectors in §2.3 are answered with the same data and mostly
rejected, because a check that reads the chooser's numbers inherits the
chooser's ignorance. The design's own retraining loop (`automation.md` §5)
is not optional polish; it is the only path by which the floor ever becomes
an instrument on a graph, and the record that says whether it has is the
thing this ruling builds.

### The trained checkpoint, same instruments

A checkpoint trained on the 41,291 Wikispeedia rows with jevlike's defaults
(192-byte context, 32-byte options, 4 epochs, best validation NLL kept;
12.5 minutes on 8 threads with three other agents on the box) over the same 4,373 test rows:

| confidence bin | rows | accuracy | mean confidence | gap |
|---|---:|---:|---:|---:|
| [0.0, 0.1) | 704 | 0.057 | 0.079 | +0.02 |
| [0.1, 0.2) | 1,549 | 0.150 | 0.146 | −0.00 |
| [0.2, 0.3) | 880 | 0.272 | 0.246 | −0.03 |
| [0.3, 0.4) | 461 | 0.438 | 0.344 | −0.09 |
| [0.4, 0.5) | 265 | **0.570** | 0.444 | −0.13 |
| [0.5, 0.6) | 164 | 0.622 | 0.539 | −0.08 |
| [0.6, 0.7) | 125 | 0.808 | 0.657 | −0.15 |
| [0.7, 0.8) | 113 | 0.796 | 0.744 | −0.05 |
| [0.8, 0.9) | 90 | 0.756 | 0.847 | +0.09 |
| [0.9, 1.0] | 22 | 0.818 | 0.940 | +0.12 |

Top-1 **28.4%** (7.9× chance), top-3 41.7%, ECE **0.039**; `jevlike.eval`
and the probe agree to four places. Shuffled-context top-1 15.2%, ECE 0.061:
the model has also learned which labels people click regardless of the goal,
and that prior is about half of its skill. The gates:

| gate | passes | accuracy of what passed | accuracy of what parked |
|---|---:|---:|---:|
| wiki-hop: floor 0.35, margin 0.05 | 22.1% | **0.654** | 0.179 |
| schema default: 0.50 / 0.10 | 11.8% | 0.737 | 0.224 |
| triage `judge`: 0.60 / 0.15 | 8.0% | 0.791 | 0.240 |
| 0.80 / 0.10 | 2.6% | 0.768 | 0.272 |
| 0.90 / 0.10 | 0.5% | 0.818 | 0.282 |

Goal in the menu: the trained chooser takes it 75.8% of the time (the
shipped one 6.1%; people 90.1%).

Twelve minutes of training on rows of the task turned the same 41,280
parameters from an instrument that reads nothing into one that reads
exactly what its record says: at wiki-hop's own gate one hop in five
auto-clears and two of every three of those are right, and the parked hops
are right one time in six. Both halves of the ruling are measured, not
argued. By the rule stated in the next section — written before these
numbers were read — this checkpoint lands in the table's third row: the
first bin with at least 100 rows and at least 0.5 accuracy is [0.4, 0.5),
so the floor is 0.4, and the escalation rate the unattended design should be
costed against is the share of rows below it, **82%** (78% at the shipped
0.35 / 0.05). That is the honest number. It is not the number
`automation.md` was written against, and it says that on a 41K-parameter
chooser a research run is a run of parks with an occasional click, whether
or not the chooser is calibrated — the calibration decides only whether the
clicks are worth anything.

The hop that started this, on the trained checkpoint, same nine contexts:
A (J2's shape) ranks `Felidae` first at 0.243 and `crepuscular` tenth at
0.007; B (with `Cat` on the second line) 0.221 — the three-byte change that
moved the shipped checkpoint by 0.86 moves this one by 0.02; F (no page at
all) 0.254, the same pick at the same confidence, which is the page-ablation
figure in §2.3 seen from the front. And the reconstruction found a fourth
thing, a corollary of this ruling with its own paragraph in §2.1: `Felidae`
appears **three times** in the 64-option menu, each copy scores 0.243, and
wiki-hop's margin check — `top1 − top2 ≥ 0.05` — reads 0.243 − 0.243 = 0
and **parks the right answer as a tie with itself**. With duplicate labels
collapsed before scoring (five of the 87 candidates are copies; the
collapsed 64 admit four more distinct links) `Felidae` scores 0.468 against
`Felis` 0.131 and the gate clicks it, margin 0.34 at shape A and 0.25 at
shape B. Row C is the other lesson: with goal `Dog` the trained checkpoint
clicks `C` — one of the single-letter geologic-period links in the taxobox's
timescale strip — at 0.53, a label shape Wikispeedia's titles never had. A
calibrated checkpoint is calibrated on its set's menus; that is C3 in §6,
with a face.

### The experiment, as an executor runs it

No judgement calls; every command is literal, every result has a meaning
stated before it is read. Run from `C:\Users\dxcen\Projects\cms-agent\models\jevlike`
with its own venv (`.venv\Scripts\python.exe`), `PYTHONUTF8=1` set (the
labels contain U+A792), and a root directory outside both repositories.

1. `sh scripts/get_wikispeedia.sh <root>` with `.venv\Scripts` on `PATH` so
   `jevlike-data` resolves. Expected: `{"test": 4373, "train": 41291,
   "validation": 5620}`. Any other count is a changed upstream archive —
   record it and continue; the split is by target hash and stable.
2. `python -m jevlike.eval runs/synthetic.pt <root>/jsonl/test.jsonl --device cpu`
   — the shipped checkpoint on the task. Record `model.top1`, `model.ece`,
   `shuffled_context.top1`.
3. `python -m jevlike.train <root>/jsonl/train.jsonl --validation <root>/jsonl/validation.jsonl --output <root>/wikispeedia.pt --device cpu --epochs 8`
   (about 20 minutes; `OMP_NUM_THREADS=8`), then step 2 against
   `<root>/wikispeedia.pt`.
4. Once spec step 1 has landed: `python jev/calibrate.py <root>/jsonl/test.jsonl --set wikispeedia`
   under `JEVLIKE_CKPT=<root>/wikispeedia.pt`, which writes the calibration
   record and prints the bin table and the gate table. Until it lands, the
   bin table is produced by the scratch probe (`s47/calibrate.py`), whose
   formula is jevlike's own ten-bin ECE with the per-bin accuracy printed
   beside it.

What the numbers mean, decided now:

| result | meaning | what to build |
|---|---|---|
| trained top-1 under twice chance (< 7%) | the byte encoder cannot learn this task at this window | wiki-hop stays advisory indefinitely; the next experiment is `--encoder hf` with the frozen Qwen2.5-0.5B the README reports at 26%, and `--context-tokens 512` |
| trained top-1 ≥ 15% but no bin with ≥ 100 rows reaches 0.5 accuracy | the chooser ranks but cannot be trusted alone at any floor | advisory stays; report top-3 hit rate, because the ranking in the park payload is what the second tier reads |
| some bin with ≥ 100 rows reaches ≥ 0.5 accuracy | the floor has a place to stand | set wiki-hop's floor at that bin's lower edge, declare `calibration: wikispeedia`, and the share of rows below that edge is the escalation rate the unattended design should be costed with |
| the shipped checkpoint's shuffled-context top-1 equals its real top-1 (it does: 3.3% vs 4.5%) | confirmed: the context contributes nothing today | nothing further; it is why advisory is the default |

The four-epoch run above is in the third row. Validation NLL was still
falling at epoch four (3.38, 3.17, 3.06, 3.02), so the eight-epoch run is
expected to land there too, somewhat higher; the executor reads which row
applied from its own table, not from this one.

The Wikispeedia proxy has two stated gaps against wiki-hop's own rows: its
labels are article titles, wiki-hop's are link text (`[1]`, `PreꞒ`, `S`);
and its body is prose, wiki-hop's is a link tree. The calibration record
names the set so the substitution is on the record, and the rows wiki-hop
logs — every `rule`-verified row in particular (§2.2) — are how the graph
earns a curve of its own.

## 2. What catches a confident mistake

### 2.1 The calibration record, and the advisory run

**Choice.** The service keeps, per checkpoint content hash, a record of the
checkpoint's measured calibration on named eval sets: for each set, the row
count, top-1, top-3, ECE, the ten bins with their counts and accuracies, the
gate table at the graph defaults, and when and from which file it was
measured. `jev/calibrate.py` writes it; `/health` lists it; `automation.start`
reads it. A graph names the set its floor was set against in
`meta.jev.calibration`. A run then has exactly three states:

- **calibrated** — the graph names a set and the checkpoint has a record for
  it: the floor and margin apply as today, and the report and the status
  carry `calibration: {set, ckpt, floor, floor_bin: {lo, hi, n, accuracy}}`,
  so an operator reading a report sees "floor 0.35 sits in a bin measured at
  0.049 on 680 rows" and not a number that looks like a probability;
- **advisory** — the graph names no set: the chooser scores every menu as
  today, the row is logged with its probabilities as today, and every choice
  **parks**, `why: "advisory: checkpoint <id> has no calibration for this
  graph"`, with the options ranked by the chooser's scores. The second tier
  picks; the row says who;
- **refused** — the graph names a set the checkpoint has no record for:
  `automation.start` fails before the first action, naming the set and the
  checkpoint id, with both remedies in the message. The same shape as a
  `requires` the box cannot meet.

**Rejected.** A per-run switch that trusts an uncalibrated checkpoint
(`trust_uncalibrated: true`, or a `--yolo` for confidence). A6 refused a
yolo for authority because nothing looks at the call; this would be a yolo
for correctness where nothing looks at the number, and the number was
measured to be noise. Also rejected: making advisory opt-in, so that an
undeclared graph runs the floor as today with a warning. That is the status
quo with a label, and the status quo auto-clicked 62% of the time at chance.
Also rejected: raising the default floor to 1.0. `_check_number(…, 0, 1)`
admits it, and a softmax over 64 options can emit 0.9994 (row F) — floor
1.0 is a park most of the time and a click sometimes, which is the wrong
kind of almost. Also rejected: putting the record in the checkpoint file.
The checkpoint is a `torch.save` payload in another project's tree
(`cms-agent`, reached by `JEVLIKE_ROOT`), and the calibration is jev's fact
about it, measured with jev's context format; it lives under jev's own
state directory, keyed by the hash `_ckpt_id()` already computes, so a
retrained file with a stale record is refused by construction.

**Why the chooser still scores an advisory step.** Because the row is the
product. A parked step whose row carries the chooser's probabilities and the
second tier's label is a training row *and* a calibration row: once a graph
has a few hundred of them, `jev/calibrate.py` can be run against the graph's
own exported rows and the graph can declare its own set. A run that skipped
the chooser when advisory would never earn its way out of advisory.

**The menu the record was measured on.** A calibration record is a fact
about menus of a particular shape, and the runner must hand the chooser that
shape. Wikispeedia's menus are unique titles; wiki-hop's live menus are not —
the Cat capture carries `Felidae` three times and `Carnivora` and `[1]`
twice — and a softmax over a menu with copies splits one decision's mass
across them. Two things follow, both measured in §1: the margin check reads
a label against its own copy and parks a right answer as a tie (0.243
against 0.243 on the trained checkpoint), and the probability the row logs
for that label is a third of what the record was measured for, so every such
row misreports the calibration it is meant to feed. `jev_choose` already
collapses duplicate labels; the automation path does not, and will:
`build_options` collapses options with identical labels to the first
occurrence, before `max` is applied, and the number collapsed is counted on
the run. The first occurrence's `ref` is what is clicked. Identical text
pointing at two destinations is the page's ambiguity, not the runner's; the
`url` bound is where an author resolves it.

**Where it lives.** The record: the service, beside the decision log, because
the service is the process that knows which checkpoint it loaded. The gate:
`run.py`'s `_choose_phase`, the one place a probability is compared to a
floor. The refusal: `run.py`'s `start`, before any action is requested, where
`_check_input` already refuses. Nothing in the driver, the gate layer, or
the browser sees a probability and none of them should.

### 2.2 A rule before the chooser: `choose.prefer`

**Choice.** `choose` gains `prefer`, an ordered list of guards from the
closed grammar, each evaluated per option with the option in scope as
`option.*` (`label`, `event`, and the source's own data — `ref`, `name`,
`url` for `refs`; `command` for a `menu` item). Rules are tried in order and
options in menu order; the first pair that passes takes that option with
`source: "rule"`, `verified: "rule"`, and no floor is consulted. The chooser
is still called (when there are two or more options) and its probabilities
are logged on the row — see above for why. `equals` gains `fold: true`
(casefold and strip both sides when both are strings), because a link's text
is `cat` and a goal is `Cat`. wiki-hop declares one rule:

```json
"prefer": [
  { "type": "equals", "params": { "path": "option.label", "value": "{{context.goal}}", "fold": true } }
]
```

At hop 1 that rule fires on option 0, `Felidae`, and the run reaches
`arrived` on the next snapshot through the guard that already exists. It is
the whole of what would have caught J2's first hop, it costs nothing, and it
is the design's own thesis — *the graph shapes the menu* — applied one step
further: the graph can also say when the menu has already answered.

**Rejected.** Making the rule a runner built-in ("if an option equals the
goal, take it"). The runner has no `goal`; wiki-hop has one because its
author put it in `context`. A built-in would be a second, hidden graph
feature keyed on a context name, and the next graph's goal is called
`target`. Also rejected: an NLI rule by default (`entails` per option:
"this link leads to {{goal}}"). It is admitted by the grammar, the author
pays for it, and the lint says what it costs — one openjev pair per option,
64 pairs at hop 1 — but it is not what wiki-hop needs and it is not
entailment: a link text does not *entail* a destination. Also rejected:
letting a rule pick among several passing options by score. A rule that
needs a tie-break is two rules; write the second.

**Where it lives.** `options.py`, beside `build_options`, evaluated in
`_choose_phase` between building the menu and consulting the chooser, using
`guards.evaluate_guard` unchanged with an extended scope. The lint refuses an
`option.*` path anywhere but inside `prefer`.

### 2.3 The five directions, answered

**A second opinion.** Rejected as a standing second scorer. Two uncalibrated
scorers that disagree are two noises, and their agreement is not evidence:
on the Wikispeedia rows the shipped checkpoint agrees with *itself under a
shuffled context* to within a point of accuracy. A calibrated scorer does
not need one. And the second opinion this system already has — the park,
answered by the driving model or a person with the whole menu in front of
them — is the only one that can be *right* about hop 1, because it is the
only one that reads the labels as words. The advisory run asks it every time;
the calibrated run asks it when the curve says to. What the second opinion
must be given is the trajectory (§5): today's payload shows the goal and the
current page and not the picks that led there.

**Scoring against the goal, not only the context.** Rejected. The chooser's
context already begins with the goal, and rows C, D and F show it is read —
and read as bytes, not as a destination. openjev cannot substitute: the
guard `entails("crepuscular" → "leads toward Felidae")` is a question about
the world, not about the premise text, and `automation.md` §3 already rules
that such a hypothesis scores neutral and neutral fails. It would also cost
one 4B-parameter forward pass per option — 43 to 64 per hop against a warm
call measured at 4–6 s for one pair (§3) — on a box with 17.7 GB free while
openjev holds ~16 GB in float32. The measured way to score options against
the goal is to train the chooser on rows that have one, which Wikispeedia
rows do.

**Post-hoc verification.** Kept as graph authoring, refused as a runner
mechanism, and named for what it can and cannot see. After the click the
runner has the page the click produced and the label it clicked; an `always`
guard can compare them (`equals context.obs.h1` with the last pick, or an
`entails` over the title), and the graph that wants to is one transition
from having it. That check catches a link that lied and a stale ref that
resolved to the wrong element — A6's adversary, A5's `stale`. It cannot see
hop 1, because "crepuscular" led exactly where it said. What would see hop 1
post hoc is *progress*, next.

**Progress as a first-class signal.** Rejected as a runner mechanism, kept as
graph authoring, for the reason the brief anticipated: it requires a distance
that is meaningful, and none exists for a page in general. What exists is
weaker and honest: the goal in the menu (the rule above, one hop of
lookahead, 27.6% of human steps); the goal string on the page (`contains`
over `context.obs.text`, a guard the graph can write today); and a bound on
wandering, which `visits` already is — wiki-hop's `reading` allows 30, which
is a run that can wander thirty hops at 0.99 each. A runner-invented distance
would be the chooser's problem in a second costume. Two things the runner
*can* do and does not: show the trajectory in the payload (§5), and count
the hops since the last rule or guard fired, `steps_since_signal`, which is
a number a graph's `count` guard can bound — added to the run's scope as
`run.since_signal`, cost zero, and the graph decides what to do with it.

**Accepting it, with backtracking.** Partly accepted. A graph that takes a
wrong turn and later notices is better than one that agonises per hop — the
ruling above puts the agonising at the calibration boundary, not at every
hop of a calibrated run. But "later notices" needs a signal and a way back,
and the run today has neither: it cannot notice (no signal; §2.3 above) and
it cannot go back on a chooser pick (`browser_back` is reachable from `EMPTY`
only). The way back is graph authoring and one line: `also: [{"label": "go
back", "event": "BACK"}]` with `BACK` → `browser_back` and re-entry, the
structural escape `automation.md` §4 already prescribes so "the chooser can
learn it too". Under an advisory run the second tier can take it; under a
calibrated one the chooser has rows for it. It is recommended for wiki-hop
and not required, because it is a graph's decision and because the page
after `browser_back` is a page whose `h1` is in `visited`, and the graph's
`exclude` needs to know that; that interaction is the author's.

**Self-consistency checks — the sixth direction, tried and rejected on
data.** Two forward passes per hop (about 5 ms) could ask whether the top
pick survives removing the goal line, or removing the page. On the shipped
checkpoint over 4,373 rows the top pick survived page removal on 56% of
right picks and 49% of wrong ones, and goal removal on 14% and 9.5%: no
discriminative power. At p ≥ 0.9 the pick survived page removal 84% of the
time — row F's signature — but since those rows are 7% right, "the page
contributed nothing" is the same information as "no calibration", already
known. On the trained, calibrated checkpoint the same two checks survive on
9.5% of right and 12.0% of wrong picks (goal removed) and on 97.0% and 90.3%
(page removed): still nothing to separate on. The second pair says
something else worth having: the trained model's pick barely depends on the
page, because after the two header lines the 192-byte window holds about
140 bytes of the link tree, which on wiki-hop is `- main [ref=…]:` and three
nameless `- link [ref=…] [cursor=pointer]:` lines — no label reaches the
window at all. On this graph, at this width, the chooser reads the goal, the
h1 and the option labels, and `{{context.obs.text}}` in the context template
is dead weight (C5 in §6). A check that reads the chooser's numbers
inherits the chooser's ignorance; this one is the cleanest demonstration.

### 2.4 Where each check lives

A bound belongs at the layer that can see it; a check in the wrong layer is
a check that cannot see what it needs.

| check | what it must see | layer | per-hop cost | catches hop 1? |
|---|---|---|---|---|
| calibration gate (§2.1) | the checkpoint's id and its measured curve; the graph's declared set | the service: `start` refuses, `_choose_phase` parks | 0 when calibrated; one park per choice when advisory | yes, by parking it |
| `prefer` rule (§2.2) | the option list with each option's data, and the run's context | the service: `options.py` in `_choose_phase` | microseconds | yes, exactly |
| the floor | the chooser's scores | the service: `_choose_phase` | 0 | on the shipped checkpoint no, by construction; on the trained one with the menu collapsed, yes (`Felidae` 0.47 over 0.13) |
| post-hoc arrival / link-honesty | the page after the click and the pick before it | the graph: an `always` guard | 0, or one openjev call if NLI | no |
| progress | a distance that does not exist; or a signal the graph names | the graph: guards and `visits`; the runner supplies `run.since_signal` | 0 | no, but bounds the damage |
| the second opinion | the whole menu, the goal, the trajectory | the driving model or a person, through the park | one model turn per park | yes, when asked |
| `expect` (A5) | the observation before it is stored | the service: `_run_tool_action` | 0 | no — the observation was valid |
| the warrant (A6) | the call, the count, the clock, the origin | the gate layer and the browser | ~0 | no — the click was in bounds |

Nothing in this table is in the gate layer or the browser, and that is
right: neither can see a probability, and a probability is not authority.
The one thing every row shares is that it is on the record: a rule pick
says `rule`, an advisory park says `advisory`, a calibrated click carries the
bin it cleared, and a reconstruction can tell them apart.

## 3. What it costs

J2's 89.62 s for three hops is not 30 s per hop. The session journal is
binary, but every nested call's id carries its dispatch time in milliseconds
(`script_browser_snapshot_1789829420864_2`), and against the run's own
`start_wall` of 1789829418.471 the breakdown is:

| moment | at | interval | what it is |
|---|---:|---:|---|
| `browser_open` dispatched | +0.0 s | | |
| snapshot 1 dispatched | +2.4 s | 2.4 s | open and navigation |
| click 1 dispatched | +76.9 s | **74.5 s** | snapshot (~0.4 s, E5), the two arrival guards — the second is openjev, **cold: ~24 s load on an idle box per `/health`'s own docstring, more under load** — then the chooser |
| snapshot 2 dispatched | +77.7 s | 0.8 s | click 1 |
| click 2 dispatched | +84.4 s | **6.7 s** | snapshot, warm openjev, chooser |
| snapshot 3 dispatched | +84.6 s | 0.3 s | click 2 |
| park | +89.6 s | **5.0 s** | snapshot, warm openjev, chooser |

So a warm hop on this graph is **5–7 s**, of which the snapshot is ~0.4 s,
the click 0.3–0.8 s, and the rest — 4–6 s — is one openjev pair over a
40-token premise. The chooser is not visible at this resolution: the probe
scored 17,492 menus of 43 options in 44.6 s, 2.5 ms each batched. S52's run
of the same graph measured 33 s for hop 1 and 42 s for hop 2 in a different
session on a loaded box, which is the cross-session variance the record
already warns about; the 5–7 s figure is J2's and is stated as such.

Against that, each mechanism:

| mechanism | per hop | model loads | tokens |
|---|---|---|---|
| calibration gate, calibrated run | one dict lookup | none | none |
| calibration gate, advisory run | **one park**: a model turn per choice. J1 measured 20–23 s per escalated step against 3–5 s auto-cleared with `ollama:deepseek-v4.1-flash`; `automation.md` §4 puts a local 27B at 30–60 s for a 1,000-token payload, and this box has measured 3.3 tok/s under memory pressure | none new | ~1,000 in, ~20 out per park |
| `prefer` rule | microseconds; the chooser call it does not save is 2.5 ms | none | none |
| chooser scoring on a rule step | 2.5 ms | none | none |
| `evidence.picks`, `run.since_signal` | 0 | none | a few tokens in the payload |
| `section` scope (§4) | the same 0.4 s projection pass E5 measured "indistinguishable" | none | none |
| the rejected per-option openjev | 43–64 pairs at ~5 s each warm | none, but 16 GB resident stays resident | none |
| the rejected second scorer | 2.5 ms if jevlike-class; a second openjev-class model is 16 GB this box does not have twice | one | none |

**The trade, stated.** An advisory run is the honest cost of running an
uncalibrated chooser: N hops cost N model turns, which on this box with a
local 27B means a 5–7 s hop becomes a 35–70 s hop, five to ten times. That is
not a regression against today — today's 5–7 s hop was right 5% of the time
— it is the price of the second opinion being the only opinion, and it ends
the day the graph has rows and a curve. The `escalations` budget bounds it
(wiki-hop's is 30, equal to `steps`, which the design set for exactly this
mode). A calibrated run costs nothing more than today — but its record says how
often it parks, and for the tiny checkpoint at wiki-hop's gate that is 78%
of hops: on this graph the unattended design's cost is dominated by parks
whether or not the chooser is calibrated, until a bigger encoder (C6) or the
graph's own rows move the curve. `prefer` takes the 27.6% of hops whose menu
holds the goal for free, so parks are bounded by 72% of hops with the rule
and 78% without; the exact figure is the executor's, from step 0's table.

## 4. The landmark predicate, and "See also"

**Measurement.** In the unfiltered `within: main` capture of Cat
(`e5work/evidence/cat_within_main_unfiltered.txt`, 10,795 lines), the H2
regions and their named links, by document order:

| region | lines | named links | in a nested `navigation` | citation-style `[n]` |
|---|---:|---:|---:|---:|
| Etymology and naming | 397–553 | 39 | 0 | 17 |
| Taxonomy | 554–627 | 21 | 0 | 11 |
| Evolution | 628–1,236 | 109 | 0 | 33 |
| Characteristics | 1,237–1,545 | 76 | 0 | 24 |
| Senses | 1,546–1,815 | 69 | 0 | 27 |
| Behavior | 1,816–2,571 | 184 | 0 | 97 |
| Lifespan and health | 2,572–2,698 | 34 | 0 | 18 |
| Health issues | 2,699–2,820 | 35 | 0 | 9 |
| Ecology | 2,821–3,016 | 49 | 0 | 22 |
| Interaction with humans | 3,017–3,246 | 63 | 0 | 33 |
| History and mythology | 3,247–3,560 | 84 | 0 | 21 |
| **See also** | **3,561–3,675** | **34** | **3** (the Portals bar) | 0 |
| Notes | 3,676–3,686 | 2 | 0 | 0 |
| References | 3,687–9,512 | **1,414** | 0 | 0 |
| External links | 9,513–10,795 | 286 | 271 | 0 |

The lead and infobox — what the nearest-landmark rule admits — hold the 87.
"See also" holds 31 real links and sits behind 763 named links in document
order.

**Ruling.** The nearest-landmark predicate stays, and the "descendant of
`main`" alternative is rejected on the count above: it would raise the
admitted set from 87 to 2,586, of which 1,414 are references and 271 are
the external-links bar, and it would change wiki-hop's menu **not at all**,
because `max: 64` keeps the first 64 in document order and the first 64
descendants of `main` are the same lead and infobox links the nearest rule
already keeps. A predicate is not what hides "See also"; **order is**, and
the only order the runner has is the page's. Changing the order to a
relevance order is the chooser's job by another name.

What research needs is to *name the section*. `browser_snapshot` gains
`section: "<heading text>"`: the landmark whose first heading child has that
name becomes the scope for `roles`, kept links are those whose nearest
landmark is that section (so the Portals `navigation` inside "See also" is
excluded, 3 of 34), the head gains `section:`, and a page with no such
heading or more than one is refused by name, the shape `within` already has.
It is a projection over the tree Playwright already built, in the same pass
E5 measured as free, in the process that owns the tree. A research graph
then asks `{"within": "main", "section": "See also", "roles": ["link"]}` and
gets 31 options, none of them `[1]`; the article-namespace `url` bound from
A6 drops the three portal links if the graph wants them gone. A graph that
wants both the lead and "See also" takes two snapshots into two `into`
names, or one node per section; one scope per snapshot is A5's rule and it
holds.

**Rejected.** Region names. Vector 2022's regions are unnamed (`- region
[ref=…]`); the heading is the only handle. Also rejected: a `depth` for
sections — E3 already showed depth buys characters without buying options.
Also rejected: doing it in `a11y.py`. The consumer never holds the tree it
would filter (A5, ruling 2); the projection is where `_filter_roles` is.

**Confidence in the fix, separately:** the diagnosis is a count and is
certain for this article; the heading-under-region structure is Vector
2022's and was observed on one article; other sites will differ, and the
refusal-by-name is what makes that a clean failure rather than an empty
menu that looks like a working filter.

## 5. What this means for research

The user's goal is not Wikipedia. It is *"navigate a website or automation
of work and research"*, and research is where a confident wrong turn
compounds silently, because there is no "Felidae" to notice you missed. What
this ruling says for it, plainly:

1. **A research graph runs advisory until it has earned a curve.** The
   chooser ranks; the driving model or a person chooses; every step is on
   the record with both opinions. That is what `automation.md` said the
   first hundred runs would be, and it is now what the runner does rather
   than what the prose hoped. The cost is §3's: a model turn per hop — and
   the day the graph has a curve, a 41K-parameter chooser's curve says four
   hops in five still park. That is the price of the encoder, not of the
   gate; C6 is what would lower it.
2. **"Found it" is a rule the author writes, not a feeling the chooser
   has.** `prefer` is where a research graph states what the thing it is
   looking for looks like when it is in the menu — a label, a URL pattern,
   a heading — and the rule fires before any number is read.
3. **The trajectory is evidence.** The escalation payload gains
   `evidence.picks` — the labels chosen so far in this run, in order — so the
   second opinion sees `goal: Felidae, picks: [crepuscular, dusk]` and can
   say "go back" if the menu offers it. Today it sees the goal and the
   current page and has to infer the drift.
4. **Wandering is bounded by numbers the graph declares** — `visits`,
   `steps`, and a `count` guard over `run.since_signal` — never by a
   distance the runner invents.
5. **The log is still the product.** A `rule`-verified row is a labelled
   row at zero cost; an advisory row is a labelled row at one model turn;
   an `outcome`-promoted row is the weakest tier, as A6 already ruled. The
   day a research graph has a few hundred of them, `jev/calibrate.py` over
   its own export is the experiment of §1 again, on the right distribution,
   and the floor is set from a table rather than a guess.

And one thing it does not do: it does not make the chooser understand
language. A 41,280-parameter byte model that scores 4.5% on human clicks
after seeing none of them may score 29% after seeing forty thousand (the
README's number, not this box's until the experiment says so); it will not
score 90%. The floor's job on a research graph is to route the hard hops to
someone who can read, and the calibration record is what tells it which hops
those are.

## 6. Measurements not yet taken

Each names the fallback that holds until it is taken.

- **C1 — the warm openjev call, alone.** J2's 4–6 s per warm hop is
  snapshot + entail + chooser, separated by subtraction from E5's snapshot
  figure. Spec step 4 instruments `entail_calls` and per-call seconds into
  the report, which is also S50's open item. Fallback: the figures in §3 are
  J2's intervals and are labelled so.
- **C2 — an advisory park's cost with a local model on this box.** J1's
  20–23 s was against `ollama:deepseek-v4.1-flash`; the 30–60 s figure is
  `automation.md`'s estimate for a local 27B. Fallback: both quoted, neither
  promoted.
- **C3 — the calibration record's transfer from Wikispeedia to wiki-hop's
  own rows.** Titles versus link text, prose versus a link tree. Fallback:
  the record names the set; wiki-hop's first few hundred rows are the
  measurement, and until then wiki-hop's floor is a Wikispeedia floor by
  declaration. The gap has a face already: with goal `Dog` the trained
  checkpoint clicks the taxobox's `C` at 0.53, a label Wikispeedia never
  showed it; the record's [0.5, 0.6) bin says 62% right, and nothing measured
  today says whether that holds for single-letter labels.
- **C4 — `section` on articles other than Cat**, and on a site that is not
  Wikipedia. Fallback: refusal by name.
- **C5 — the 512-byte context variant** (`--context-tokens 512`), which
  `automation.md` §5 already names as the retraining width and which
  changes `CONTEXT_TOKENS` in `server.py`. Not run today; the 192-byte
  window is what the service serves, and §2.3 measured that on wiki-hop no
  option label reaches it — the page is invisible to the chooser at this
  width. The cheaper experiment is the context template: put
  `{{context.obs.text}}` through a projection that lists the option labels
  first, and measure whether the page-ablation figure moves.
- **C6 — the frozen-encoder path** (`--encoder hf`, README: 26% on
  target-disjoint Wikispeedia) — a 0.5B-parameter model resident per
  chooser call, whose cost on this box is unmeasured and whose benefit the
  trained-tiny result in §1 decides is worth measuring or not.
- **C7 — `jev_choose`**, the ad-hoc tool, has no floor and no calibration
  gate and is out of this ruling's scope; a model calling it directly gets
  a number this document says is noise off-distribution. Noted, not ruled.

**Assumed rather than measured:** that E5's scratch capture stands in for
J2's page (they differ by 200 characters and one option; the reconstruction
reproduced J2's pick and its magnitude); that the four-epoch trained
checkpoint stands in for an eight-epoch one (the experiment says eight; the
validation curve was still falling, so eight is expected to be at least as
good and the floor to sit no lower);
that this box's 8 threads at 12:19 with three other agents live is a
representative training and probing environment (it is not, for timing;
the accuracies do not depend on it).

## 7. Confidence, per ruling

Stated separately from the rulings, as the day's rule requires.

| ruling | confidence | rests on |
|---|---|---|
| 1. The shipped chooser's confidence is uninformative on the task; the floor on wiki-hop measures nothing today | **high** | four independent measurements: the synthetic shuffled control (ECE 0.625), the Wikispeedia table (chance at every bin), the nine-context reconstruction (F: 0.9994 with no page), and S52's live parks on the same page under a three-byte context change |
| 1a. `automation.md`'s "almost every step will escalate" is refuted | **high** | J2's two clicks at 0.9977 and 0.69; 61.7% gate pass-through on 4,373 rows |
| 2.1 Confidence is evidence only inside a measured distribution; a run must be calibrated, advisory or refused | **high** on the principle — it is what calibration means; **medium** on the mechanism's shape (the record's keys, the set-name declaration), which is one design among several that would satisfy the principle |
| 2.2 `prefer` rules before the chooser, with the chooser's scores logged | **high** that it is right and free and would have caught hop 1; **medium** on how much of research it covers — that is the author's rule to write |
| 2.3 second scorer, per-option NLI, runner-side progress, self-consistency: rejected | **high** for the first two (cost and semantics are measured and ruled elsewhere); **high** for self-consistency on both checkpoints (measured: no signal on the shipped one, none on the calibrated one); **medium** for runner-side progress, which is rejected on the absence of a distance rather than on a measurement |
| 2.3 post-hoc verification as graph authoring only | **medium-high**: it cannot see hop 1 by construction; whether graphs will write it is not this document's to know |
| 2.3 backtracking as a menu escape | **medium**: recommended, not required; its interaction with `exclude` is the author's |
| 3. costs | **high** for the J2 breakdown (dispatch ids, summing to 89.6); **medium** for the warm openjev share (by subtraction, C1); **low-medium** for the advisory park cost on a local model (C2) |
| 4. nearest-landmark stays; `section` is the fix | **high** on the diagnosis (a count on this article); **medium-high** on the fix (one skin, one article) |
| 5. research posture | **high** that advisory-until-calibrated is the honest posture; it is also the design's own stated intent |
| the trained-checkpoint numbers in §1 | **high** on Wikispeedia itself (two independent computations agree to four places over 4,373 rows); **medium** that they transfer to wiki-hop's own menus (C3: row C's `C` at 0.53 is a label shape the set never had), which is why the record names the set and the graph's first few hundred rows are the measurement that matters |
| 2.1 duplicate labels collapse before scoring | **high**: measured — the right answer parked as a tie with its own copy at 0.243; the fix is what `jev_choose` already does |

---

## Implementation specification

Eight steps, two agents, in this order. Steps 1–4, 2b and 6 are jev
(Python, `jev/` and `extensions/jev/`); step 5 is the browser extension;
step 0 is a procedure with no tree changes and is run first because its
result decides how long step 2's advisory state lasts, and step 2b is where
it ends for wiki-hop. Each step leaves its suite green.
Test names are the names to use; for every change the test beside it is the
one that fails if the change is reverted, and a change without one is
documentation. Nothing here touches `cms-agent`; jevlike is reached exactly
as `server.py` reaches it today.

**Isolation, first.** jev's file paths are gated by **four** variables once
step 1 lands: `JEV_DECISIONS_LOG`, `JEV_RUNS_DIR`, `JEV_DECISIONS_DIR`, and
the new `JEV_CALIBRATION_DIR`. Every live test names all four, and proves
the redirection with a forced write before trusting it — the rule J2 wrote
by breaking it. Production `decisions.jsonl` hash is verified before and
after any live step.

| step | tree | agent | depends on |
|---|---|---|---|
| 0 | none — the experiment, into the Build-Log | executor | — |
| 1 | `jev/automation/calibration.py`, `jev/calibrate.py`, `jev/server.py` | jev | — |
| 2 | `jev/automation/run.py`, `graph.py`, `schema.json`, both graphs, tests | jev | 1 |
| 3 | `jev/automation/options.py`, `guards.py`, `graph.py`, `schema.json`, `decisions.py`, `wiki-hop.json`, a fixture | jev | — (lands after 2 for one coherent `_choose_phase`) |
| 2b | `extensions/jev/graphs/wiki-hop.json` — the declaration, from step 0's table | jev | 0, 1, 2, 3 |
| 4 | `jev/automation/run.py` (timings, `entail_calls`, `picks`, `since_signal`) | jev | 2 |
| 5 | `extensions/browser/service.py`, `tools/snapshot.rn`, `jev/automation/a11y.py` (head key only) | browser | — |
| 6 | `docs/design/automation.md`, `docs/design/unattended.md` | jev | 1–5 |

### Step 0 — the experiment

Exactly §1's "as an executor runs it", with `--epochs 8`, recorded in the
Build-Log as observed numbers: the three `jevlike.eval` outputs (shipped on
synthetic, shipped on Wikispeedia, trained on Wikispeedia), and the bin and
gate tables from `jev/calibrate.py` once step 1 exists — until then from the
same formula run by hand, and said so. The root directory is outside both
repositories and is named in the entry. The interpretation is the table in
§1; the executor writes which row applied and does not choose a floor —
that is the operator's, from the table.

### Step 1 — the calibration record

**`jev/automation/calibration.py`** — new.

1. `def calibration_dir() -> Path` — `JEV_CALIBRATION_DIR` when set, else
   `_paths.cache_dir() / "eidolon" / "extensions" / "jev" / "calibration"`,
   the same shape as `decisions.decisions_dir()` and documented as the
   fourth isolation variable.
2. `def record_path(ckpt_id: str) -> Path` — `calibration_dir() / f"{ckpt_id}.json"`.
3. `def bins(records: list[tuple[float, bool]]) -> list[dict]` — ten
   equal-width bins over `[0, 1]`, the last closed at 1.0, each
   `{"lo", "hi", "n", "accuracy", "confidence"}` (`accuracy`/`confidence`
   `None` when `n == 0`); `def ece(bins, total) -> float` — jevlike's own
   formula, `sum(n/total * |accuracy - confidence|)` over non-empty bins.
4. `def gate_table(records: list[tuple[float, float, bool]], gates: list[tuple[float, float]]) -> list[dict]`
   — per `(floor, margin)`: `passed` share, `accuracy_passed`,
   `accuracy_parked`, from `(top1, margin, correct)` triples.
5. `def summarize(scored: list[dict]) -> dict` — from per-row dicts
   `{p, margin, rank_correct, n_options, goal_in_menu | None}` to
   `{rows, top1, top3, ece, bins, gates, goal_in_menu: {share, chooser_accuracy} | None}`;
   `gates` are the five in §1 plus the loaded graph defaults when given.
6. `def write_record(ckpt_id: str, set_name: str, summary: dict, *, source: str) -> Path`
   — reads any existing record, sets `sets[set_name] = {**summary, "source": source, "measured": <iso>}`,
   writes `{"ckpt": ckpt_id, "sets": {...}}` atomically (write to a sibling
   temp file, `os.replace`), 0o600.
7. `def read_record(ckpt_id: str | None) -> dict | None` — `None` when the id
   is `None`, the file is absent, or its `ckpt` disagrees with the id.

Tests, **`jev/tests/test_calibration.py`** (new): `bins_partition_every_row_exactly_once`
(1,000 synthetic confidences; the counts sum to 1,000 and 1.0 lands in the
last bin); `ece_matches_jevlike_evals_formula_on_a_pinned_fixture` (a
20-row fixture with a hand-computed ECE literal); `gate_table_reports_passed_share_and_both_accuracies`;
`write_then_read_round_trips_and_merging_keeps_other_sets`;
`a_record_for_a_different_checkpoint_is_not_read` (file says `ckpt: "aaaa…"`,
asked for `"bbbb…"` → `None`); `calibration_dir_honours_the_override`.

**`jev/calibrate.py`** — new CLI, run with the jevlike venv's python from
`jev/`: `python calibrate.py <jsonl> --set <name> [--batch-size 64]`. Sets
`EIDOLON_SERVICE_PORT`/`EIDOLON_SERVICE_TOKEN` to placeholders if unset (the
way every test file does), imports `server`, takes `server._jevlike()` and
`server._ckpt_id()`, reads rows with `jevlike.data.validate` (imported the
way `server` imports jevlike, through `JEVLIKE_ROOT`), scores in batches
through the same closure the service uses (`server._jevlike()(context,
options)` per row is acceptable; batching is an optimisation the executor
may take through `jevlike.model` directly, stated in the docstring either
way), computes `goal_in_menu` only when every context's first line starts
with `Target article: `, prints the summary as JSON and the bin and gate
tables as text, and writes the record. Refuses a set name that is not
`^[a-z][a-z0-9_-]{0,31}$`.
Test, `jev/tests/test_calibrate_cli.py`: `the_cli_writes_a_record_for_the_loaded_checkpoint`
— a 40-row synthetic JSONL, a fake `server._jevlike` returning a fixed
ranking (patched), `JEV_CALIBRATION_DIR` in a temp dir; the record exists,
names the set, and its `rows` is 40. Marked slow and skipped unless
`JEV_TEST_JEVLIKE=1`: `the_shipped_checkpoint_scores_its_own_synthetic_test_at_its_pinned_numbers`
— top-1 0.9975 and ECE under 0.01 on `data/synthetic/test.jsonl`, which
pins §1's first row.

**`jev/server.py`**

1. `health()` gains `"ckpt": _ckpt_id()` and
   `"calibration": sorted(record["sets"]) if record else []` where
   `record = calibration.read_record(_ckpt_id())` — a stat and a small
   file read, never a model load. Test, `jev/test_server.py` `HealthTests`:
   `health_names_the_checkpoint_and_its_calibration_sets_without_loading_a_model`
   (the existing never-loads assertion extended; with `JEV_CALIBRATION_DIR`
   pointed at a temp dir holding a record for the test ckpt id, the list is
   `["wikispeedia"]`; without one, `[]`).
2. `_automation_start` passes `calibration=calibration.read_record(_ckpt_id())`
   into `automation_run.start`; `_automation_step`/`_status`/`_answer`
   pass the same into their reconstruction kwargs, beside `chooser`/`entail`.

### Step 2 — the advisory run

**`jev/automation/graph.py`**

1. `_JEV_KEYS` gains `"calibration"`; `_validate_jev` checks it is a string
   matching `^[a-z][a-z0-9_-]{0,31}$` when present. `Graph` gains
   `calibration: str | None`; `_build_graph` sets it.
   Tests (`test_schema.py`, `NegativeLintTests`): `a_calibration_that_is_not_a_set_name_is_rejected`;
   `WorkedExampleTests` stays green with both graphs undeclared.

**`jev/automation/run.py`**

2. `_RunState.__init__` gains `calibration: dict | None = None` (the
   record) after `graph_ref`; stores `self.calibration`, and
   `self.calibration_set: str | None = graph.calibration`. `Run.__init__`,
   `_restore_run_state` and `start()` thread it (`start(..., calibration=None)`;
   the reconstruction path re-reads from the kwarg the service passes, as
   `chooser` is).
3. `start()`, after `_check_input`: if `graph.calibration is not None` and
   (`calibration is None` or `graph.calibration not in calibration["sets"]`):
   `raise ValueError(f"graph {ref} declares calibration {set!r} but checkpoint {ckpt_id or 'none'} has no calibration record for it -- run jev/calibrate.py <rows> --set {set}, or remove meta.jev.calibration to run advisory")`.
   Nothing is requested; no run id is minted.
4. `_choose_phase`, in the `else` branch after the chooser call and the
   counters: `if rt.calibration_set is None:` build
   `_floor_escalation(...)` with `why` replaced by
   `f"advisory: checkpoint {rt.ckpt_id or 'none'} has no calibration for this graph"`
   and `"advisory": True` added to the payload; park; `source = verified = answer["by"]`.
   Else the existing floor/margin branch, unchanged. `_floor_escalation`
   takes an optional `why` override rather than a second builder.
5. `_final_report` and `_status_dict` gain `"calibration": _calibration_field(rt)`
   — `None` when advisory, else
   `{"set", "ckpt", "floor": <the state's or default floor>, "floor_bin": <the bin of the record's set whose [lo, hi) contains the floor: lo, hi, n, accuracy>}`
   computed against the graph's default floor (per-state floors are in the
   escalation payload already; the report carries the default).
6. `_persist_park` writes `"calibration_set": rt.calibration_set`;
   `_restore_run_state` restores it from the reloaded graph, not the
   snapshot — the graph is the owner — and the snapshot value is a
   cross-check that warns on mismatch.
   Tests (`test_run.py`, new class `AdvisoryRunTests(_IsolatedDecisionsDir)`):
   - `an_undeclared_graph_parks_every_choice_with_why_advisory` — wiki-hop
     as shipped (undeclared), `constant_chooser(top=0.9)`; the first choose
     yields `kind: escalate`, `advisory: True`, `why` starts with
     `advisory:`, `options[0].p == 0.9`, `chooser_calls == 1`; answered by
     index, the run continues to `arrived`; the row has `source: "model"`
     and the chooser's probs.
   - `a_declared_set_missing_from_the_record_is_refused_at_start_naming_both`
     — an inline graph with `calibration: "wikispeedia"`, `calibration=None`;
     `ValueError` names `wikispeedia` and the ckpt id; the driver saw no call.
   - `a_declared_set_present_runs_the_floor_as_before` — the same graph
     with a record fixture `{"ckpt": "test-ckpt", "sets": {"wikispeedia": {...bins...}}}`;
     `constant_chooser(top=0.9)` clears the floor with no park; the report's
     `calibration.set == "wikispeedia"` and `floor_bin.accuracy` is the
     fixture's value for the bin containing 0.35.
   - `a_calibrated_run_still_parks_under_the_floor` — same record, a chooser
     at 0.2; `why` is the floor's text, `advisory` absent.
   - `a_park_snapshot_carries_the_calibration_set_and_a_restart_keeps_it`
     — in `PersistenceTests`, via `_simulate_restart`.
   - Every existing test that asserts a chooser pick clears the floor
     (`ConfidenceFloorTests`, `WikiHopEndToEndTests`, `TriageLinuxEndToEndTests`,
     `WikiHopRealCaptureTests`, the HTTP tests in `test_server_automation.py`)
     is updated to run calibrated: `_IsolatedDecisionsDir` gains a module
     fixture `CALIBRATED = {"ckpt": "test-ckpt", "sets": {"test": …}}`, and
     the tests pass `calibration=CALIBRATED` with graphs that declare
     `calibration: "test"` — the two shipped graphs are loaded through a
     helper that injects the declaration for tests only, so the shipped
     files stay undeclared until step 0's table says what to declare. The
     count of tests that had to change is reported in the Build-Log entry:
     it is the measure of how much of the suite assumed an uncalibrated
     0.9 was a decision.

**`jev/automation/schema.json`** — `$defs.jev.properties` gains
`"calibration": { "type": "string", "pattern": "^[a-z][a-z0-9_-]{0,31}$" }`
after `warrant`. The copy in `automation.md` §1 is updated in step 6.

**`extensions/jev/graphs/wiki-hop.json`, `triage-linux.json`** — unchanged
in this step: both run advisory until step 0's table gives one of them a set
to declare. The Build-Log entry says so in those words. Step 2b, below, is
where wiki-hop's declaration lands.

### Step 2b — wiki-hop declares its set

Lands after 0, 1, 2 and 3, because it needs the record (1), the gate (2),
the collapsed menu (3 — without it the record's probabilities do not
describe the live rows), and the eight-epoch table (0). The executor:
places the trained checkpoint where the operator says (its path is not
this document's; the record is keyed by hash, not path) and names it in the
service's environment as `JEVLIKE_CKPT`, the way the synthetic one is
today; runs `python jev/calibrate.py <root>/jsonl/test.jsonl --set wikispeedia`
under that environment; sets `meta.jev.calibration: "wikispeedia"` on
wiki-hop with `floor` at the lower edge of the first bin that has at least
100 rows and at least 0.5 accuracy in that table (0.4 on the four-epoch
table) and `margin` unchanged; and records in the Build-Log the gate row
for that floor — passed share, accuracy passed, accuracy parked — as the
graph's expected escalation rate and auto-click accuracy. `triage-linux`
stays advisory: no set exists for it, and the run that produces its rows
is the advisory run. Test: `WorkedExampleTests` loads the declared graph;
`WikiHopEndToEndTests` runs it against the calibration fixture (which
gains a `wikispeedia` set for the test). A fresh checkout on a box without
the trained checkpoint gets the refusal from step 2, naming `wikispeedia`
and the synthetic checkpoint's id, with both remedies in the message. That
refusal is the intended behaviour and
`a_declared_set_missing_from_the_record_is_refused_at_start_naming_both`
is its test.

### Step 3 — the menu: duplicates, then `choose.prefer`

**`jev/automation/guards.py`**

1. `equals` reads `params.get("fold", False)`; when true and both sides are
   strings, compares `left.casefold().strip() == right.casefold().strip()`.
   `GUARD_KINDS` unchanged.
   Test (`test_guards.py`, `EqualsTests`): `fold_matches_case_and_surrounding_whitespace_insensitively`;
   `fold_does_not_apply_to_non_strings`.

**`jev/automation/graph.py`**

2. `_CHOOSE_KEYS` gains `"prefer"`; `_validate_choose` validates it as a
   non-empty array of guards through `_validate_guard(..., roots=PATH_ROOTS | {"option"})`
   — `_validate_guard` gains a keyword `roots` defaulting to the four, and
   `PATH_RE` is built per call from it (or two precompiled patterns; the
   executor's choice, stated). `equals` admits `fold` (boolean) in its
   `_unknown_keys` set. An `option.*` path in any guard outside `prefer`
   is refused with `{where}.path: 'option' is only in scope inside choose.prefer`.
   `_lint_choose` adds, for each NLI kind found inside `prefer`, no error —
   the grammar admits it — but the loaded `Graph` records
   `prefer_nli_states: list[str]` and `start()` appends a warning naming
   the state and "one entail pair per option per visit" to `rt.warnings`.
   Tests (`test_schema.py`): `a_prefer_rule_may_read_option_paths`;
   `an_option_path_outside_prefer_is_rejected_naming_the_site`;
   `a_prefer_that_is_not_a_guard_list_is_rejected`;
   `an_nli_prefer_loads_and_is_named_in_the_graphs_warning_list`.

**`jev/automation/schema.json`** — `$defs.choose.properties` gains
`"prefer": { "type": "array", "minItems": 1, "items": { "$ref": "#/$defs/guard" } }`;
`$defs.path`'s pattern gains `option` as a fifth root with a `description`
saying it is valid only under `choose.prefer` and the loader enforces that;
the `equals` branch's `params.properties` gains `"fold": { "type": "boolean", "default": false }`.

**`jev/automation/options.py`**

0. `build_options` collapses options whose `label` is identical (exact
   string; no folding — `Cat` and `cat` are two links) to the first
   occurrence, **before** `max` is applied, and returns the number collapsed
   beside the list (a second return value, or a field on the result; the
   executor chooses and states which). `_choose_phase` adds it to
   `rt.counts["duplicates_collapsed"]`, reported with the other counts. The
   `menu` and `also` sources go through the same pass, so an author who
   lists a label twice gets one option and a count of one.
   Tests (`test_options.py`): `duplicate_labels_collapse_to_the_first_occurrence_before_max_is_applied`
   (a refs body with `Felidae` at positions 1 and 4 and `max: 3` — the
   options are the first three distinct labels and the `Felidae` option's
   `ref` is position 1's); `collapsing_is_exact_and_does_not_fold_case`.
   (`test_run.py`, `WikiHopRealCaptureTests`):
   `the_real_cat_menu_reaches_the_chooser_with_unique_labels` — a recording
   chooser; every label it was handed is distinct; `duplicates_collapsed >= 1`
   with the observed count (5 on E5's capture) in the Build-Log; and
   `a_pair_of_identical_labels_no_longer_parks_as_a_margin_tie` — an inline
   graph run calibrated (the fixture record) whose menu has `X` twice and
   a fake chooser that gives every `X` it is handed 0.25 and the rest the
   remainder: the chooser is handed one `X`, and by the fake's rule it is
   0.5 against nothing above 0.1, so the gate clicks it and there is no
   park.

3. `def first_preferred(prefer: list[dict], opts: list[Option], scope: dict, warnings: list[str], *, entail, default_threshold) -> int | None`
   — for each rule, for each option index, `guards.evaluate_guard(rule, {**scope, "option": {**o.data, "label": o.label, "event": o.event}}, warnings, entail=entail, default_threshold=default_threshold)`;
   first true returns the index. NLI leaves go one call per option; a
   `first_passing`-style batch is not built (the lint's warning is the
   cost's notice).
   Tests (`test_options.py`, new `PreferTests`): `the_first_rule_and_the_first_option_win_in_that_order`;
   `a_rule_sees_the_options_own_data_not_only_its_label` (a `refs` option
   matched on `option.url`); `no_rule_passing_returns_none`;
   `a_rule_value_may_be_a_template_over_context` (`{{context.goal}}`).

**`jev/automation/run.py`**

4. `_choose_phase`, after `build_options` and the `EMPTY` check and before
   the forced-pick branch: `preferred = options.first_preferred(choose_cfg.get("prefer") or [], opts, scope, rt.warnings, entail=rt.entail, default_threshold=rt.default_threshold())`.
   When not `None`: `probs = list(rt.chooser(...))` and `chooser_calls += 1`
   if `len(opts) >= 2`, else `[1.0]`; `index = preferred`; `source = verified = "rule"`;
   fall through to `_finish_choice`. The floor and the advisory park are
   both skipped: a rule is a decision, not a score. `counts` gains
   `"rule_picks"`, reported.
   Tests (`test_run.py`, new `PreferRuleTests(_IsolatedDecisionsDir)`):
   - `a_prefer_rule_takes_the_matching_option_and_skips_the_floor` — an
     inline advisory graph whose rule matches option 3; `constant_chooser(prefer_index=0, top=0.9)`;
     no park; the row has `label == 3`, `source == "rule"`, `verified == "rule"`,
     `probs[0] == 0.9` (the chooser's, logged); `chooser_calls == 1`;
     `rule_picks == 1`.
   - `a_prefer_rule_that_matches_nothing_falls_through_to_the_chooser_and_the_advisory_park`.
   - `a_rule_pick_on_a_single_option_menu_logs_forced_probs` (`[1.0]`,
     no chooser call).
   - `WikiHopRealCaptureTests.the_real_cat_page_with_goal_felidae_is_taken_by_rule` —
     against a new fixture `jev/tests/fixtures/wiki/cat_main_links.txt`, a
     live `{"within": "main", "roles": ["link"]}` capture of the Cat article
     taken by the executor and recorded in the Build-Log with its `chars`
     (E5 measured 4,954 and J2 4,756 an hour apart); `refusing_chooser` is
     **not** used — the chooser is still called — so use `constant_chooser`
     and assert the click's `ref` is the `Felidae` option's ref and the row
     says `rule`. This is J2's first hop, caught.

**`jev/automation/decisions.py`** — the docstring's `source` list gains
`"rule"` and `verified` gains `"rule"`; no code change. The exporter (still
unbuilt) is ruled here to treat `verified: "rule"` as the strongest tier
after `human`: deterministic, and the label is the rule's, not the chooser's.

**`extensions/jev/graphs/wiki-hop.json`** — `reading.meta.choose` gains the
`prefer` block quoted in §2.2, and — recommended, applied — `also: [{"label": "go back", "event": "BACK"}]`
with `on.BACK: {"target": "reading", "reenter": true, "actions": [{"type": "tool", "params": {"name": "browser_back"}}]}`.
`WorkedExampleTests` stays green; `WikiHopEndToEndTests.test_two_hop_walk_reaches_the_goal`'s
row assertions gain the extra option (`["Felis", "Mammal", "go back"]`) and
its `source` — with goal `Felis` and option `Felis` present, the rule fires
and `source == "rule"`; the test's docstring says this is now the rule's
path and the chooser's path is `a_prefer_rule_that_matches_nothing_…`.

### Step 4 — what the report did not carry

**`jev/automation/run.py`**

1. `rt.counts` gains `"entail_calls"` and `"rule_picks"`; `rt.timings = {"entail_s": 0.0, "chooser_s": 0.0, "tool_s": 0.0}`.
   The `entail` and `chooser` closures are wrapped once in `_RunState.__init__`
   with `time.monotonic()` accounting; `_run_tool_action` adds the interval
   between yielding the request and receiving the result to `tool_s`.
   `_final_report` and `_status_dict` gain `"entail_calls"`, `"rule_picks"`,
   `"timings"` (rounded to ms). This closes S50's `entail_calls` item and
   is the instrument C1 needs.
2. `rt.picks: list[str]` — appended in `_finish_choice` with `chosen.label`;
   persisted in the park snapshot as `"picks"`; restored; `_evidence` gains
   `"picks": list(rt.picks)`.
3. `rt.since_signal: int` — reset to 0 when a rule fires or any `always`
   guard passes, incremented once per choose phase otherwise; exposed in
   `scope()` as `run.since_signal`; persisted and restored.
   Tests (`test_run.py`): `the_report_counts_entail_calls_and_carries_timings`
   (a fake `entail` called twice; `entail_calls == 2`; every timing key
   present and non-negative); `evidence_carries_the_picks_so_far` (two
   hops then a park; `evidence.picks == [first, second]`);
   `picks_survive_a_restart` (in `PersistenceTests`);
   `since_signal_resets_on_a_rule_and_on_a_passing_always_guard_and_counts_otherwise`
   (a `count` guard over `run.since_signal` with `gte: 2` reaches its
   target on the third un-signalled choose).

### Step 5 — `section`

**`extensions/browser/service.py`**

1. `_filter_roles(tree, roles, within, max_n, section=None)`: during the
   existing walk, when `section` is given, a landmark line becomes the
   **section root** if the first element line nested directly under it is
   `heading` with `name == section`; the walk records every such root;
   after the walk (or as it goes) a line is kept only when its nearest
   landmark is a section root — never `within` itself. `0` roots →
   `ValueError(f"section {section!r}: no landmark whose first heading is that on {url}")`;
   more than one → `ValueError(f"section {section!r}: {n} landmarks whose first heading is that on {url}; a section names exactly one")`.
   Survivors are re-parented under the scope line exactly as today, so
   `a11y.py`'s nearest landmark still reads `within` and `options.py`'s
   consumer-side check is unchanged. `section` requires `roles` (the
   projection is where it is applied) and `within` (a section is inside a
   landmark; unscoped section scoping is refused as `within` + `max` are
   today, with a sentence). `_snapshot` validates it before any Playwright
   call, as `roles` is; `_head` gains `section=` between `roles` and `chars`.
   Tests (`RoleProjectionUnitTests`): `a_section_keeps_only_links_whose_nearest_landmark_is_that_section`
   (a hand-written tree with two regions, each headed); `links_inside_a_navigation_nested_in_the_section_are_excluded`
   (the Portals shape); `a_section_with_no_such_heading_is_refused_by_name`;
   `two_sections_with_the_same_heading_are_refused_with_the_count`;
   `the_head_carries_section_between_roles_and_chars`; `section_requires_roles_and_within`.
   (`RoleFilteredSnapshotTests`, real Chromium, a `data:` page):
   `a_section_scoped_snapshot_of_a_real_page_yields_that_sections_refs_and_they_click`.
   Live, recorded not asserted: `{"within": "main", "section": "See also", "roles": ["link"]}`
   on `https://en.wikipedia.org/wiki/Cat` — the option count (31 expected
   from §4's table, stated as expected and replaced by the observed number)
   and the wall seconds beside an unsectioned call in the same session.

**`extensions/browser/tools/snapshot.rn`** — `section` added to
`input_schema.properties` ("Restrict a roles-filtered snapshot to the one
section whose first heading has this exact text — `See also`, `References`.
Requires within and roles. A page with no such heading, or more than one, is
refused by name.") and forwarded by the same `insert` idiom.

**`jev/automation/a11y.py`** — no parser change: `parse_head` already lifts
any `key: value` line, so `obs.section` arrives for free; `build_obs` adds
`"section": head.get("section") if head else None` beside `scope`, with the
test `section_is_lifted_when_present` in `HeadBlockTests`.

### Step 6 — the documents

**`docs/design/automation.md`** — five edits, no others: the schema block
(the three additions above); §1's wiki-hop graph and the sentence *"against
the synthetic checkpoint almost every step will escalate, which is the
point"* replaced by: the synthetic checkpoint is uncalibrated on this graph
and the run is therefore advisory — every step parks — until the graph
declares a calibration set the loaded checkpoint has a record for
(`docs/design/judgement.md` §2.1); §4 "The floor" — *"`jevlike-eval` prints
expected calibration error and per-bin accuracy"* is false (`eval.py` prints
ECE and no bins) and becomes: `jev/calibrate.py` prints the bins and the
gate table and writes the record the run reads; §5 "From log to checkpoint",
step 3, cites `jev/calibrate.py` and the declaration; §2 "The four sources"
gains one paragraph on `prefer` and the `option.*` scope. **`docs/design/unattended.md`**
§7 gains one bullet: under an advisory run the count of parks is the count
of hops, and the gate's one question is unchanged by it — a park is the
graph's question, not the gate's, as that document's last paragraph already
says.

### Order, and what each step proves on its own

0 first, because it is the only step that decides whether advisory is a
phase or a state for wiki-hop, and it touches no tree. 1 next: the record
is the fact everything else reads, and `/health` naming it is the first
place an operator sees which sets a checkpoint has. 2 after 1: the run
knows which of three states it is in, and says so in every report, park and
row; the count of tests that had to change is the honest measure of the
suite's prior assumption. 3 is the free half — rules, and the fixture that
proves J2's first hop would have been caught by one line of graph — and
lands after 2 so `_choose_phase` is edited once in its final shape, and it
carries the collapse of duplicate labels, without which the record's
probabilities do not describe the live rows. 2b after 3: one line of graph
and a number from a table, which is the moment wiki-hop stops being
advisory and the Build-Log gets the expected park rate. 4 is the
instrumentation §3 and §6 ask for and S50 already filed. 5 in parallel with
all of it, in the browser tree, and proves the "See also" ruling on a real
page. 6 last, and it is the step that strikes the two sentences this
document measured false.

After all of it, on this box with the trained checkpoint named and its
record written: wiki-hop with goal `Felidae` from `Cat` is expected to reach
`arrived` in one hop with `rule_picks: 1` — and, were the rule struck, to
reach it anyway on the collapsed menu, `Felidae` at 0.47 over `Felis` at
0.13. wiki-hop with goal `Wolf` from `Cat` is expected to park about four
hops in five, which is the record's number and not a hope, with every park
carrying the ranking and the picks so far. The hop counts and the parked
share it actually shows are stated in the Build-Log as observed, not here.
