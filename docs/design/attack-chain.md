# The ATT&CK chain, and how triage walks it

MITRE ATT&CK Enterprise describes an intrusion as tactics (the attacker's goal
at a step, `TA####`) made of techniques (how, `T####`, sub-techniques
`T####.###`). MITRE D3FEND is the defensive side: countermeasures grouped as
Model, Harden, Detect, Isolate, Deceive, Evict and Restore, mapped onto the
same ATT&CK techniques. Triage tags every finding with ATT&CK and every action
with D3FEND (`crates/attack` holds both, from MITRE's published data).

This page is generic on purpose. It is in the repo, and the repo is readable by
the agent under test, so nothing here comes from a benchmark's answer key.
A benchmark's own chain lives with its scorer, outside the agent's reach
(see `triage-toolkit.md`, answer hygiene).

Tactics are ATT&CK Enterprise v19.2 (15 tactics; v19 split the old Defense
Evasion into Stealth, which keeps TA0005, and Defense Impairment, TA0112); `crates/attack` checks
every ID against the vendored data, which wins if they ever differ.

## The chain

```
 before the box        getting in           staying & rising              moving & taking             the goal
 ─────────────────    ─────────────────    ─────────────────────────    ─────────────────────────   ──────────
 Reconnaissance   ─▶  Initial Access   ─▶  Persistence              ─▶  Credential Access       ─▶  Collection
 Resource Dev.        Execution            Privilege Escalation         Discovery                    Command & Control
                                           Stealth                      Lateral Movement             Exfiltration
                                           Defense Impairment                                        Impact
```

The order is a story, not a rule: real intrusions loop back (discovery before
escalation, persistence added late).

| # | tactic | ID | the attacker wants to… | on a Linux host it looks like |
|---|---|---|---|---|
| 1 | Reconnaissance | TA0043 | learn about the target before touching it | little on the host; scans in logs from outside |
| 2 | Resource Development | TA0042 | get infrastructure, accounts, tools ready | attacker IPs, domains, registries that show up later |
| 3 | Initial Access | TA0001 | get a foothold | exposed services, weak or reused credentials, brute force in auth logs |
| 4 | Execution | TA0002 | run code | shells, interpreters, scheduled or service-started commands |
| 5 | Persistence | TA0003 | survive reboots and cleanup | cron entries, systemd units, extra accounts, `authorized_keys`, login scripts |
| 6 | Privilege Escalation | TA0004 | get root | UID 0 accounts, sudoers rules, SUID binaries, container escapes, polkit rules |
| 7 | Stealth | TA0005 | not be seen | hidden files and users, masquerading names, library preload tricks |
| 8 | Defense Impairment | TA0112 | weaken the defenses | disabled or cleared logging, loosened firewall, tampered security tools |
| 9 | Credential Access | TA0006 | collect secrets | readable shadow, secrets in files and history, cracking tools |
| 10 | Discovery | TA0007 | map the host and network | scanners and enumeration tools left behind |
| 11 | Lateral Movement | TA0008 | reach other machines | remote services (ssh, database, remote shells) open to the world |
| 12 | Collection | TA0009 | gather data of interest | staged archives, dumps |
| 13 | Command and Control | TA0011 | talk to the attacker | reverse shells, odd listeners, beacons to fixed IPs |
| 14 | Exfiltration | TA0010 | get data out | transfer services, outbound uploads |
| 15 | Impact | TA0040 | damage or disrupt | deleted or encrypted data, stopped services, forwarding abuse |

## How triage uses it

### Hunt order: backwards from what hurts most

A defender arrives after the fact, so triage walks the chain from what is still
live and dangerous back to how it started:

```
 1. Persistence + Privilege Escalation   what lets them come back as root      SEV1
 2. Command and Control                  what is talking to them right now     SEV1
 3. Stealth + Defense Impairment         what hides the rest, what blinds you  SEV1-2
 4. Credential Access                    what they can reuse                   SEV2
 5. Initial Access + Lateral Movement    the doors: exposed services, weak auth SEV2
 6. Execution, Discovery, Collection     tools and staging left behind         SEV2-3
 7. Impact, Exfiltration                 what was done: evidence, forensics    SEV2-3
 8. Hardening beyond the intrusion       baseline hygiene                      SEV3
```

Severity orders the work queue; the escalation ladder (T0–T3 in
`triage-toolkit.md`) decides who may act on each item.

### Finding → countermeasure

Each finding carries its ATT&CK technique, and D3FEND proposes counters for
it. The ladder's risk check then picks the tier:

| D3FEND tactic | typical action on a host | risk | usual tier |
|---|---|---|---|
| Model | inventory users, services, listeners, packages, baselines | read-only | T0 |
| Detect | read logs, compare to the baseline, find hidden or odd things | read-only | T0 |
| Harden | tighten configs, permissions, service settings | reversible | T1 |
| Isolate | firewall rules, bind services to localhost, drop group memberships | access-sensitive | T2–T3 |
| Deceive | not used in triage | – | – |
| Evict | remove accounts, keys, jobs, units, binaries, packages | often irreversible | T2–T3 |
| Restore | fix broken sudo/PAM, restart required services, recover files | access-sensitive | T3 |

Anything touching the agent's own access (ssh, sudo, PAM, firewall) is T3 and
runs with the dead-man rollback armed, whatever its D3FEND tactic.

### Tagging

- A finding gets one primary technique (the most specific sub-technique that
  fits) and optional secondary ones.
- An action gets its D3FEND technique and the finding it answers.
- IDs are validated against the vendored data; an unknown ID is a failed tag,
  not a guess.
- In a benchmark, the scorer compares tags to its own key to report tagging
  accuracy per tactic.

## Build paths

What exists to build on (fetched 2026-09-25): ATT&CK publishes STIX 2.1 JSON
collections per release (`enterprise-attack.json`, plus pinned
`enterprise-attack-<version>.json` and an `index.json` of releases) under its
terms of use; D3FEND 1.6.0 publishes its ontology as OWL, TTL and JSON-LD, and
its inferred relationships (ATT&CK mappings among them) as CSV and JSON, approved for public release.
Minerva's rule is a Rust core with Rune for scripts, so the paths below are
judged against that.

### Getting the data in

| path | how | gains | losses |
|---|---|---|---|
| A. full STIX at runtime | commit the release bundle, parse it with serde at start | complete, nothing to derive | tens of MB in the repo, slow start, most of it unused |
| **B. compact tables** (pick) | `minerva attack update <attack-ver> <d3fend-ver>` fetches the pinned releases once, extracts TSVs into `crates/attack/data/` with a `VERSIONS` file (url, version, sha256); `include_str!` builds them into the binary | offline, small, diffs show what a release changed, one reviewed update step | the extractor is ours to keep working when MITRE changes the schema |
| C. live lookup | query MITRE's TAXII server or the website | always current | network at runtime breaks the loopback-only rule and offline club use |
| D. Python library | MITRE's `mitreattack-python` | ready-made | against the Rust core rule; JEV would own it |

The compact tables, per release:

```
attack/techniques.tsv   id  name  tactics  parent  revoked_by  url
attack/tactics.tsv      id  name  order
d3fend/techniques.tsv   id  name  tactic   artifacts
d3fend/counters.tsv     attack_id  d3fend_id  relation
```

Revoked and deprecated IDs keep a row pointing at their replacement, so an old
tag (or an old report, such as one written before the TA0005 split) still
resolves.

### Putting tags on things

| path | who tags | gains | losses |
|---|---|---|---|
| 1. static | each JEV graph node declares the techniques it hunts and the D3FEND action it takes | deterministic, free, reviewable in the graph | only as good as the graph; can't tag something unexpected |
| 2. chosen | the Jev chooser picks from candidates (the node's tags, plus techniques D3FEND links to the artifact found: a cron file, a unit, an account) | calibrated, cheap, stays inside known IDs | needs good candidates |
| 3. free | DeepSeek names a technique for a novel finding | handles the unexpected | slowest, can be confidently wrong |

Pick: all three as a ladder, the same shape as the escalation ladder: static
when the node knows, chosen when there are candidates, free only for what
neither covers. Every tag, whichever path made it, is validated against the
tables; an unknown or revoked-without-replacement ID is recorded as a failed
tag.

### Where it shows up

```
crates/attack ──┬──▶ Rune binding   attack::technique("T1053.003"), attack::counters(id)
                │                   so extensions and new JEV graphs tag without Rust
                ├──▶ hub timeline   every finding and action carries its tags
                ├──▶ bench report   score and tagging accuracy grouped by tactic
                └──▶ web UI         dim chips (T1053.003 · Cron), name on hover, filter by tactic
```

Each run records the ATT&CK and D3FEND versions it tagged against, so reports
from different releases are never compared as if they were the same.

### Order of work

1. `crates/attack`: the extractor, the tables, validation, the Rune binding, a
   README with MITRE's required notices.
2. Static tags on the generic triage graphs; tags in the hub timeline and the
   bench report.
3. Chosen tags through the Jev chooser; tagging accuracy in the report.
4. Chips and the tactic filter in the web UI.

## Sources

- MITRE ATT&CK Enterprise tactics: https://attack.mitre.org/tactics/enterprise/
- MITRE ATT&CK data (STIX): https://github.com/mitre-attack/attack-stix-data
- MITRE D3FEND: https://d3fend.mitre.org/
- D3FEND ontology downloads (1.6.0): https://d3fend.mitre.org/resources/ontology/
- ATT&CK terms of use (attribution): https://attack.mitre.org/resources/legal-and-branding/terms-of-use/
