export const meta = {
  name: 'agent-runs-design',
  description: 'Design option C (agents start perch runs tagged by their own chat) as two slice specs, trim GP-c to part A, critique adversarially, revise',
  phases: [
    { title: 'Read', detail: 'four readers: agent gate, plugins/term precedent and trust, runs create path after RT, bash workflow runs today' },
    { title: 'Draft', detail: 'one drafter writes RA.json, PR.json and trims GP-c.json' },
    { title: 'Critique', detail: 'edge-case and security critics try to break the drafts' },
    { title: 'Revise', detail: 'the drafter closes what the critics found' },
  ],
}

const SCR = '<old scratchpad>'
const SL = SCR + '\\slices'
const REPO = 'C:\\Users\\dxcen\\Projects\\Minerva'

const DESIGN = `THE USER'S DECISIONS (2026-10-08, do not reopen):
- The inspector's graph tab becomes a strip of sub-tabs: [turns] plus the inspected chat's runs, [ new run ]; with no chat, all perch runs; any graph opens as a pane pinned to its chat (slices RT, GP-a, GP-b: specs at ${SL}\\RT.json, GP-a.json, GP-b.json; RT is being built now).
- In this eidolon a workflow run is always its own session ("A run is a session", eidolon/crates/cli/src/workflow.rs:3), so runs an agent starts never land in the chat's own journal. The user chose A + C:
  - C: agents start runs THROUGH THE PERCH. A new agent-scope route on the perch's agent gate (today it knows only Term and Mail, perch/src/agent/mod.rs:49-70) creates an ordinary perch run whose chat tag comes from the caller's own scoped token (agent::Caller.chat), never from a body, query or model text. The run then shows in that chat's strip with no extra page code, under the perch's one-browser cap and its mock-only provider, and the person answers its parks in the page. Plus a plugin tool in Minerva's own plugins/ (like plugins/term, slice T3) that the chat's agent calls.
  - A: GP-c part A only (a run's own session opened as a chat draws its graph, read-only). GP-c part B (reading run lines out of bash output) is DROPPED.
- Hard rules: never edit, fork or decode eidolon (reading its source is fine). Client-side before upstream. Tokens never in argv, env, URL, log or report. GPL boundary for Aoide does not apply here. No model calls in tests; mock only.`

const ctx = `Repo: ${REPO} (master b761462). Slice rules: ${SL}\\BRIEF.md and ${SL}\\WAVE3.md. Example specs: ${SL}\\RT.json (perch, just written, read it whole: RA builds on it), ${SL}\\T3.json (plugins/term), ${SL}\\AG.json (the agent gate) if present. READ-ONLY: do not edit, stage or commit anything in the repo or worktrees. Cite file:line for every claim.`

const READERS = [
  { key: 'agent-gate', prompt: "Map the perch's agent gate for adding a Runs scope. Read perch/src/agent/ whole (agent_scope, Scope, Caller and its chat, verb(), authenticate, how a scoped token is minted and attributed to a chat's door), how term and mail dispatch their agent verbs (perch/src/term/ agent routes, perch/src/mail/ agent routes), perch/tests/agent.rs, term_agent.rs, mail_agent.rs, and serve.rs's seam pins for agent paths (every_seam..., the 401->404 flips AG did). Answer: the exact path shape a runs agent route takes (e.g. /hub/a/runs or what the gate's grammar allows), which verbs it needs (start; list and read the caller's own chat's runs; NOT answer, cancel or abandon unless you argue otherwise), how Caller.chat reaches RT's create (RT.json: Store::begin(home, graph, args, chat)), what the gate does with a token whose chat is gone, which files a slice must own, and the pins that flip." },
  { key: 'plugin', prompt: "Map plugins/term as the precedent for a new plugin tool the chat's agent calls to start and watch perch runs. Read plugins/ whole (README, plugins/term: manifest, tools/*.rn, how a tool finds the perch URL and its scoped token without argv/env/URL, its tests), the installer's trust arms (installer/: trust_one, the T3 rule that term trust is re-applied only on unsettled passes, the vouch form `plugins trust <plugin> <verb>`), the memory note on eidolon's workflow vouch gate (eidolon/crates/cli/src/workflow.rs:465-512: `workflow run` needs EVERY tools/*.rn verb of the plugin vouched) and eidolon/crates/*/policy.rs:641-658 (a vouch answers only the table's fallthrough). Answer: the plugin's name and layout, its tools (verbs) and their inputs/outputs, how it gets its token and URL, how the installer ships and trusts it, whether a per-verb vouch hurts anything (does the plugin have workflows of its own?), and the tests (tests/plugins or installer tests) it copies." },
  { key: 'runs-create', prompt: `Map the runs create path RA will call. Read ${SL}\\RT.json whole, perch/src/runs/state.rs (create, begin, the holder 409, the health gate, refuse/problem), perch/src/runs/proc.rs (the child's argv, the generated config with the mock model, swarm off), perch/src/runs/README.md. Answer: how an agent-scope start reuses create without duplicating its checks (graph allow-list --run-graphs, args cap, the holder, health), what the agent's answer should carry (rid, state; never paths), how the agent reads its own chat's runs (RT's ?chat= filter, applied with Caller.chat, never a query value), and what must differ for an agent caller (e.g. no answer/cancel; rate limits; a cap on concurrent runs per chat).` },
  { key: 'bash-runs', prompt: "Map what happens TODAY when a chat's agent runs `eidolon workflow run ...` through bash. Read the generated policies and guard rows (perch/src/guard/, perch/src/mode/ and their README; G1's rows), the member's policy table and how eidolon judges a bash call that runs eidolon itself, eidolon/crates/cli/src/workflow.rs (the door: provider, sessions dir, config), and the vendored jev plugin's README. Answer: does any row or guard flag or refuse it today; which provider and config such a run uses (the member's real model, not mock?); whether it bypasses the perch's one-browser cap; and what a slice could add client-side (a guard row that asks, with the sentence pointing at the new tool) without editing eidolon. Recommend whether the guard row belongs in RA, PR or its own slice." },
]

phase('Read')
const reads = await parallel(READERS.map((r) => () => agent(`${ctx}\n\n${DESIGN}\n\nYOUR PART (${r.key}): ${r.prompt}\n\nReturn a dense map: findings with file:line, then 'constraints for the spec', then 'unknowns'.`, { label: `read:${r.key}`, phase: 'Read' })))
const READ = READERS.map((r, i) => `=== ${r.key} ===\n${reads[i] || '(reader returned nothing)'}`).join('\n\n')

phase('Draft')
const DRAFT_OUT = { type: 'object', properties: {
  files: { type: 'array', items: { type: 'string' } },
  summary: { type: 'string', description: 'per slice: what, owns_files, depends_on, size, security_review, and the decisions taken' },
  questions: { type: 'array', items: { type: 'string' }, description: "only decisions that are genuinely the user's, each with a recommendation" },
}, required: ['files', 'summary', 'questions'] }

const draftPrompt = `${ctx}\n\n${DESIGN}\n\nThe readers' maps:\n${READ}\n\nWrite, in the same JSON shape as RT.json (id, title, operator_decision, why, what, owns_files, depends_on, tests, live_check, size, risk, security_review, port, notes), with the Write tool:\n- ${SL}\\RA.json: the perch's agent route for runs (Runs scope on the agent gate; start with chat = Caller.chat; list and read the caller's own chat's runs only; the verbs the readers justify). security_review true. Port 48146. Depends on RT.\n- ${SL}\\PR.json: the plugin tool(s) in Minerva's plugins/ the chat's agent calls (name it; shipped and trusted by the installer like plugins/term), plus the guard row for bash-started workflow runs if the bash-runs reader recommends it here (else name its own slice in notes). Port 48147. Depends on RA (and T3's installer arms).\n- Edit ${SL}\\GP-c.json: part A only; delete part B and every conditional block for it; keep its readonly threading through W2b-2's Steps and ParkAsk (W2b-2 is MERGED on master b761462 now: cite master's lines).\nEach spec: owns_files exact; tests named concretely (each would fail if the behaviour broke); live_check concrete on its port with the slice's own binaries and the F0 fixtures; never edit eidolon; a token or login code never in argv, env, URL, any file or log. Return the file paths, a summary, and only genuine user decisions as questions with your recommendation.`
const draft = await agent(draftPrompt, { label: 'draft', phase: 'Draft', schema: DRAFT_OUT })

phase('Critique')
const CRIT = { type: 'object', properties: { findings: { type: 'array', items: { type: 'object', properties: {
  file: { type: 'string' }, problem: { type: 'string' }, evidence: { type: 'string' }, fix: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
}, required: ['file', 'problem', 'evidence', 'fix', 'severity'] } } }, required: ['findings'] }
const LENSES = [
  { key: 'edges', prompt: 'Edge cases and contradictions: do the specs match the user decisions exactly (C: tag from the token, not the body; A only in GP-c)? Anything assumed that the code does not support (cite file:line)? Missing cases: the chat closed or trashed while its run goes, two chats racing for the one browser, an agent starting runs in a loop, the person answering a park an agent started, the plugin used under a door with no perch, an old perch without RA. Tests that would not fail if the behaviour broke? Grants colliding with RT (in flight), GP-a, GP-b, SA3 (subagent vouching, in flight), I2 (installer, in flight)?' },
  { key: 'security', prompt: "Security and the hard rules: can an agent tag a run with ANOTHER chat (body, query, header, model text)? Can it read or steer another chat's runs? Can it answer, cancel or abandon a run, and should it? Is the token kept out of argv, env, URL, files and logs in the plugin? Does the plugin's trust (per-verb vouch) weaken any gate or break a workflow (the vouch_check gate)? Can the agent escape mock-only or the one-browser cap? Is the bash guard row fail-closed and honest? Is the live check safe (never the operator perch on 47820, never ~/.aoide, never the operator's eidolon config)?" },
]
const crits = await parallel(LENSES.map((l) => () => agent(`${ctx}\n\n${DESIGN}\n\nThe drafted specs are at: ${(draft && draft.files || []).join(', ')}. Read them whole, and the code they cite. Readers' maps for reference:\n${READ}\n\nYou are an adversarial critic (${l.key}). ${l.prompt}\nReturn only real findings with evidence; none if the drafts hold.`, { label: `critic:${l.key}`, phase: 'Critique', schema: CRIT })))
const findings = crits.filter(Boolean).flatMap((c) => c.findings)
log(`${findings.length} critic findings (${findings.filter((f) => f.severity !== 'minor').length} blocker/major)`)

phase('Revise')
const REV_OUT = { type: 'object', properties: { ...DRAFT_OUT.properties, rejected: { type: 'array', items: { type: 'string' } } }, required: ['files', 'summary', 'questions'] }
const revised = findings.length ? await agent(`${ctx}\n\n${DESIGN}\n\nYou drafted ${(draft && draft.files || []).join(', ')}. The critics found:\n${JSON.stringify(findings, null, 1)}\n\nFix every blocker and major in the files (Write/Edit), and each minor where cheap. A finding you reject: say why with evidence. Return the final file list, the summary, and the user questions (only genuine user decisions, each with a recommendation).`, { label: 'revise', phase: 'Revise', schema: REV_OUT }) : draft

return { draft, findings, final: revised }