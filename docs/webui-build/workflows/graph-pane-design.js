export const meta = {
  name: 'graph-pane-design',
  description: 'Read the code paths the graph sub-tabs and graph pane touch, draft the RT (perch run tag) and GP (page) slice specs, critique adversarially, revise',
  phases: [
    { title: 'Read', detail: 'four readers: perch runs, graph tab, panes and inspector, in-chat jev runs' },
    { title: 'Draft', detail: 'one drafter writes RT.json and GP.json' },
    { title: 'Critique', detail: 'edge-case and security critics try to break the drafts' },
    { title: 'Revise', detail: 'the drafter closes what the critics found' },
  ],
}

const SCR = '<old scratchpad>'
const SL = SCR + '\\slices'
const REPO = 'C:\\Users\\dxcen\\Projects\\Minerva'
const W2B2 = REPO + '\\.claude\\worktrees\\wf_c49456c8-919-1'

const DESIGN = `THE USER'S DESIGN (decided 2026-10-08, do not reopen):
- The inspector's graph tab stays in the inspector (right rail). Inside it, a strip of sub-tabs: [turns] (the chat's turn graph, W3's TurnGraph) plus one sub-tab per run, then [ new run ]. One graph drawn at a time in the tab. The tab follows the chat the inspector shows (the focused chat, else the last one that had focus).
- "That chat's runs" = BOTH kinds: (a) jev runs the agent made inside the chat (in the chat's own session journal, W1 draws them as rows today), and (b) perch runs (/hub/runs, detached) started from beside that chat. (b) needs the perch to record the chat's id on a run when it starts; today a run has no chat link.
- With no chat open, the sub-tabs are ALL perch runs (today's RunsPane list).
- [ ⇱ pane ] (and aux-click / middle-click a sub-tab) opens that one graph as a PANE in the dwindle tiling beside the chat, PINNED to that chat: it never follows focus (clicking a non-chat pane focuses it, see notes/pane.js:141 and app.js:103). One graph per pane; split for more graph panes.
- TUI look, no real buttons (the K component's [ label ] text keys), mouse-first. Every string from a model, tool, page or perch goes through clean and is drawn as a text node. Periodic reads only through core/lane.js (hubPoll); a person's actions through hubPost/hubGet.`

const ctx = `Repo: ${REPO} (master b08a32e). W2b-2 (graph steps, park, jev's view) is NOT merged yet: its worktree is ${W2B2} (read its graph/ files there too). W3 (turns.js, the turn graph) is being built now on b08a32e; it keeps export TurnGraph({ s, sid }) and reads its chat only from props. Slice rules: ${SL}\\BRIEF.md and ${SL}\\WAVE3.md. Example slice specs: ${SL}\\W2b-1.json, ${SL}\\W2b-2.json, ${SL}\\W3.json, ${SL}\\R1a.json (if present). READ-ONLY: do not edit, stage or commit anything in the repo or worktrees. Cite file:line for every claim.`

const READERS = [
  { key: 'perch-runs', prompt: "Map the perch's /hub/runs code for adding a chat tag to a run. Read perch/src/runs/* whole (mod.rs, proc.rs, state.rs, log.rs, graphs.rs, health.rs, README.md), the POST /hub/runs body parsing and its refusals, the run state file and what GET /hub/runs and GET /hub/runs/<rid> return, perch/tests/runs*.rs, and serve.rs's seam pins for /hub/runs. Then find how the perch names a chat elsewhere: the session id (sid) shape and its validation in the sessions routes (perch/src/ sessions or door code), and whether the perch can tell a sid is real. Answer: where a 'chat' field would be parsed, validated, stored and returned; what validation is right for a label that must never become a path; whether filtering belongs in the perch (?chat=) or the page; what existing tests must change; the security risks (injection into state files, path use, cross-chat leakage between members if the perch is multi-user or not)." },
  { key: 'graph-tab', prompt: "Map the page's graph tab for turning it into a sub-tab strip. Read webui/term/graph/ whole on master (index.js RunsPane, runs.js signals picked/runList/graphList/form, view.js RunList/NewRun/RunView, jevrun.js, project.js, draw.js, README.md), and the W2b-2 worktree's graph/steps.js, park.js, context.js. Answer: how picked/runList work and where a sub-tab strip replaces RunList; how [ new run ] posts (runs.js startFromForm) and where the chat id would be added; how the tab gets { s, sid } and what it shows with no chat; which parts are pure and tested (tests/runs.test.mjs, graph tests) and must keep passing; what one-graph-at-a-time means for watch/unwatch polling." },
  { key: 'panes', prompt: "Map the pane system for adding a 'graph' pane kind pinned to a chat. Read webui/term/app.js (paneKinds, openPane, focusPane, beside(), the dwindle layout, xpanes, inspected/lastChat around :61, :102-121, :168, :197, :1207, :1245, :1377-1430, :1504-1506, :1731), core/registry.js whole (paneKinds, rightTabs, leftTabs, commands, keys), and the notes pane precedent (webui/term/notes/index.js, pane.js, store.js: how a note pane is opened, persisted across reload, focused, closed). Answer: the exact registry seam a graph pane kind uses (no app.js edit if possible: say if app.js must change and why); how a pane carries its own data (pinned sid + which graph); how panes persist across reload; how aux-click 'beside' works and how a sub-tab could call it; what happens to a pinned pane when its chat is deleted/trashed or is a mirror; and the tests that cover notes panes (tests/*.test.mjs) a graph pane should copy." },
  { key: 'chat-jev', prompt: `Map how jev runs made INSIDE a chat appear today, to list and draw them as graph sub-tabs. Read webui/term/renderers/ (jev.js and whatever draws workflow rows and 'saved runs'), the session model the inspector tabs get as s (find it from app.js INSPECTOR and core/), graph/jevrun.js (newRun/feed/view/reduce, drawing + marks), and the W1 notes in ${SL}\\TRACK.md about W1. Answer: what a chat's session rows hold for a jev workflow run (run id, records, the jev_graph pages), whether jevrun.js can reduce them as they are into a drawing (or what adapter is needed), how the page can list a chat's in-journal runs without a new perch route, and whether the session already holds those records in the page or they need a fetch (and through which existing route). Flag anything that would need eidolon changes (forbidden) or a new perch route.` },
]

phase('Read')
const reads = await parallel(READERS.map((r) => () => agent(`${ctx}\n\n${DESIGN}\n\nYOUR PART (${r.key}): ${r.prompt}\n\nReturn a dense map: findings with file:line, then 'constraints for the spec', then 'unknowns'.`, { label: `read:${r.key}`, phase: 'Read' })))
const READ = READERS.map((r, i) => `=== ${r.key} ===\n${reads[i] || '(reader returned nothing)'}`).join('\n\n')

phase('Draft')
const DRAFT_OUT = { type: 'object', properties: {
  files: { type: 'array', items: { type: 'string' } },
  summary: { type: 'string', description: 'per slice: what, owns_files, depends_on, size, security_review, and the edge-case decisions taken' },
  questions: { type: 'array', items: { type: 'string' }, description: "only decisions that are genuinely the user's, each with a recommendation" },
}, required: ['files', 'summary', 'questions'] }

const draftPrompt = `${ctx}\n\n${DESIGN}\n\nThe readers' maps:\n${READ}\n\nWrite the slice specs for this design, in the same JSON shape as W3.json/W2b-2.json (id, title, what, owns_files, depends_on, tests, live_check, size, risk, security_review, and anything else those files carry), with the Write tool:\n- ${SL}\\RT.json: the perch records a run's chat. POST /hub/runs takes an optional chat id, validated as a label (never a path), stored with the run, returned by the list and the detail; filtering where the readers say it belongs. security_review true (a perch input into a state file). Port: 48142.\n- ${SL}\\GP.json: the page. The sub-tab strip in the inspector's graph tab ([turns], the chat's runs of both kinds, [ new run ] carrying the chat id), all perch runs with no chat, [ ⇱ pane ] and aux-click opening a graph pane pinned to the chat (one graph per pane) through the registry's pane-kind seam, persistence across reload like notes panes, and the edge cases (chat deleted or trashed, a run from another chat, door mode with no perch runs, a run that ends while shown, two panes on one graph). Port: 48143. If GP is bigger than M, split it into GP-a (sub-tabs + chat scoping) and GP-b (the pane) with a dependency, each its own file.\nEach spec: depends_on names W2b-2, W3 and RT where true; owns_files exact; tests named concretely (each would fail if the behaviour broke); live_check concrete on its port with master's binaries and the F0 fixtures; no eidolon edits, no app.js edit unless a reader proved the registry lacks the seam (then say exactly which lines and why). Return the file paths, a summary, and only genuine user decisions as questions with your recommendation.`
const draft = await agent(draftPrompt, { label: 'draft', phase: 'Draft', schema: DRAFT_OUT })

phase('Critique')
const CRIT = { type: 'object', properties: { findings: { type: 'array', items: { type: 'object', properties: {
  file: { type: 'string' }, problem: { type: 'string' }, evidence: { type: 'string' }, fix: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
}, required: ['file', 'problem', 'evidence', 'fix', 'severity'] } } }, required: ['findings'] }
const LENSES = [
  { key: 'edges', prompt: 'Edge cases and contradictions: does each spec match the user design exactly (sub-tabs in the inspector, both kinds of runs, pinned panes, all runs with no chat)? Is anything assumed that the code does not support (cite file:line)? Missing edge cases (chat deleted, mirrors, reload, run ending while shown, door mode, many runs overflowing the strip, the same run open twice)? Tests that would not fail if the behaviour broke? Grants that collide with W2b-2, W3 or another in-flight slice?' },
  { key: 'security', prompt: "Security and the hard rules: can the chat id reach a path, argv, a log line unescaped, or another member's view? Is validation fail-closed? Does any perch or model string reach the DOM without clean()? Any eidolon edit, app.js edit without proof, network beyond loopback, token in argv/env/URL/log? Is the live check safe (never the operator perch on 47820, never ~/.aoide)?" },
]
const crits = await parallel(LENSES.map((l) => () => agent(`${ctx}\n\n${DESIGN}\n\nThe drafted specs are at: ${(draft && draft.files || []).join(', ')}. Read them whole, and the code they cite. Readers' maps for reference:\n${READ}\n\nYou are an adversarial critic (${l.key}). ${l.prompt}\nReturn only real findings with evidence; none if the drafts hold.`, { label: `critic:${l.key}`, phase: 'Critique', schema: CRIT })))
const findings = crits.filter(Boolean).flatMap((c) => c.findings)
log(`${findings.length} critic findings (${findings.filter((f) => f.severity !== 'minor').length} blocker/major)`)

phase('Revise')
const REV_OUT = { type: 'object', properties: { ...DRAFT_OUT.properties, rejected: { type: 'array', items: { type: 'string' } } }, required: ['files', 'summary', 'questions'] }
const revised = findings.length ? await agent(`${ctx}\n\n${DESIGN}\n\nYou drafted ${(draft && draft.files || []).join(', ')}. The critics found:\n${JSON.stringify(findings, null, 1)}\n\nFix every blocker and major in the files (Write/Edit), and each minor where cheap. A finding you reject: say why with evidence. Return the final file list, the summary, and the user questions (only genuine user decisions, each with a recommendation).`, { label: 'revise', phase: 'Revise', schema: REV_OUT }) : draft

return { draft, findings, final: revised }