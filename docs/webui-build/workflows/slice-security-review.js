export const meta = {
  name: 'slice-security-review',
  description: 'Security review of one web UI slice: three lenses, adversarial verify per lens, completeness critic',
  whenToUse: 'Before merging a security-flagged slice of the Minerva web UI build',
  phases: [
    { title: 'Review', detail: 'three independent lenses read the slice diff' },
    { title: 'Verify', detail: 'one skeptic per lens tries to refute each finding' },
    { title: 'Critic', detail: 'what was not checked, and a merge verdict' },
  ],
}

const A = args
const COMMON = `You are reviewing slice ${A.slice} of the Minerva web UI build before it is merged. This is a READ-ONLY security review.

Where things are:
- The slice's worktree (its code at the reviewed commit): ${A.worktree}  (Windows path; in WSL it is ${A.wsl})
- Branch ${A.branch}, commit ${A.commit}, base ${A.base}. The diff to review: \`git -C ${A.worktree} diff ${A.base} ${A.commit}\` (run git from Git Bash on Windows; WSL git cannot read a worktree's .git file).
- The slice spec: ${A.spec}. The whole plan: ${A.plan}. The operator's settled decisions are in ${A.brief} (read its "Operator decisions" and "Hard rules").
- The executor's own security inventory (model output, a claim to check, not a fact): ${A.inventory}

Rules for you:
- Do NOT edit, stage, commit, reset or checkout anything in the worktree or the main repo. To experiment, copy files into a temp dir under the scratchpad (${A.scratch}) or WSL /tmp is NOT allowed (it is RAM); use $HOME/.cache/minerva-review-${A.slice} for any WSL scratch and as CARGO_TARGET_DIR, offline only, PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin.
- Never touch the operator's perch on 127.0.0.1:47820, ~/Minerva, ~/.config/eidolon, ~/.aoide, the real installer prefix or any real token. No model calls, no network beyond loopback ports you start yourself (use ${A.port} only).
- Prefer reading code; run tests or probes only when a claim cannot be settled by reading.
- Report only what you can tie to a file and line with a concrete failure scenario (inputs or state -> wrong outcome). No style notes. Severity: high (a credential leak, an auth bypass, code execution, the operator's real perch or files changed), medium (a real hazard needing a specific but plausible trigger), low (defence in depth, a contract that later slices could break), info (worth knowing, no fix needed).`

const FINDINGS = {
  type: 'object',
  properties: {
    findings: { type: 'array', items: { type: 'object', properties: {
      id: { type: 'string' }, severity: { type: 'string', enum: ['high', 'medium', 'low', 'info'] },
      file: { type: 'string' }, line: { type: 'integer' }, title: { type: 'string' },
      failure_scenario: { type: 'string' }, fix: { type: 'string' },
    }, required: ['id', 'severity', 'file', 'title', 'failure_scenario', 'fix'] } },
    checked: { type: 'array', items: { type: 'string' }, description: 'what you checked and found clean' },
  },
  required: ['findings', 'checked'],
}
const VERDICTS = {
  type: 'object',
  properties: { verdicts: { type: 'array', items: { type: 'object', properties: {
    id: { type: 'string' }, real: { type: 'boolean' }, severity: { type: 'string', enum: ['high', 'medium', 'low', 'info'] },
    reasoning: { type: 'string' }, evidence: { type: 'string' },
  }, required: ['id', 'real', 'severity', 'reasoning'] } } },
  required: ['verdicts'],
}
const CRITIC = {
  type: 'object',
  properties: {
    gaps: { type: 'array', items: { type: 'object', properties: { area: { type: 'string' }, why: { type: 'string' }, check: { type: 'string' } }, required: ['area', 'why'] } },
    verdict: { type: 'string', enum: ['merge', 'merge-after-fixes', 'block'] },
    summary: { type: 'string' },
  },
  required: ['gaps', 'verdict', 'summary'],
}

const reviewed = await pipeline(
  A.lenses,
  (lens) => agent(`${COMMON}\n\nYOUR LENS: ${lens.key}\n${lens.focus}\n\nGive each finding an id starting with "${lens.key}-".`,
    { label: `review:${lens.key}`, phase: 'Review', schema: FINDINGS, model: A.model }),
  (r, lens) => {
    if (!r || !r.findings.length) return { lens: lens.key, review: r, verdicts: [] }
    return agent(`${COMMON}\n\nYou are a SKEPTIC. Another reviewer, using the lens "${lens.key}", reported the findings below. For EACH one, try to refute it: read the cited code and its callers, check whether the failure scenario can actually happen (inputs reachable? guarded elsewhere? already tested?), and whether the severity is right. Default to real=false when the scenario cannot be shown to happen. Keep the same ids.\n\nFINDINGS:\n${JSON.stringify(r.findings, null, 1)}`,
      { label: `verify:${lens.key}`, phase: 'Verify', schema: VERDICTS, model: A.model })
      .then((v) => ({ lens: lens.key, review: r, verdicts: v ? v.verdicts : [] }))
  },
)

const all = reviewed.filter(Boolean)
const confirmed = [], rejected = []
for (const x of all) {
  const byId = new Map((x.verdicts || []).map((v) => [v.id, v]))
  for (const f of (x.review ? x.review.findings : [])) {
    const v = byId.get(f.id)
    if (v && v.real) confirmed.push({ ...f, severity: v.severity, verify: v.reasoning, evidence: v.evidence })
    else rejected.push({ id: f.id, title: f.title, why: v ? v.reasoning : 'no verdict returned' })
  }
}
const checked = all.flatMap((x) => (x.review ? x.review.checked.map((c) => `[${x.lens}] ${c}`) : []))
log(`${confirmed.length} confirmed, ${rejected.length} rejected across ${all.length} lenses`)

phase('Critic')
const critic = await agent(`${COMMON}\n\nYou are the COMPLETENESS CRITIC. Three lenses reviewed this slice and skeptics verified their findings. Below is what they checked, what survived and what was rejected. Find what is MISSING: an input, path, secret, process or file the slice touches that nobody examined; a claim in the executor's inventory nobody verified; a test the slice should have and does not. Read the diff yourself to answer. Then give a merge verdict for this slice: merge, merge-after-fixes (list the fixes as gaps), or block.\n\nCHECKED:\n${checked.join('\n')}\n\nCONFIRMED:\n${JSON.stringify(confirmed, null, 1)}\n\nREJECTED:\n${JSON.stringify(rejected, null, 1)}`,
  { label: 'critic', phase: 'Critic', schema: CRITIC, model: A.model })

return { slice: A.slice, commit: A.commit, confirmed, rejected, checked, critic }
