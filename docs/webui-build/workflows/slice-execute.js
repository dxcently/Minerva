export const meta = {
  name: 'slice-execute',
  description: 'Build one Minerva slice with a haiku executor, verify it independently, fix gaps (2 rounds max), then security-review it',
  whenToUse: 'One slice of the Minerva web UI build, executed by haiku agents with an independent verify-fix loop',
  phases: [
    { title: 'Execute', detail: 'the executor builds the slice in its worktree and commits' },
    { title: 'Verify', detail: 'an independent verifier checks every spec item, the tests and the hard rules' },
    { title: 'Fix', detail: 'a fixer closes the verifier\'s gaps; re-verified, two rounds at most' },
    { title: 'Review', detail: 'the security review workflow, when the slice asks for one' },
  ],
}

const A = typeof args === 'string' ? JSON.parse(args) : args // the launcher sometimes hands args over as JSON text
if (!A || typeof A.slice !== 'string' || !A.spec || !A.prompt) return { stage: 'args', error: 'args missing slice, spec or prompt', got: typeof args }
const M = A.model || 'haiku'
const RM = A.review_model || M // verifier and security review; the executor and fixer stay on M
const SCR = '<the orchestrator scratchpad>'
const SL = SCR + '\\slices'

const RULES = `Hard rules for every agent in this job:
- Never touch the operator's perch on 127.0.0.1:47820, ~/Minerva, ~/.config/eidolon, ~/.aoide, $XDG_RUNTIME_DIR/aoide, the real installer prefix or any real token. No model calls, no network beyond loopback ports you start yourself (only port ${A.port}).
- Disk guard: run \`df -h /mnt/c | tail -1\` before every cargo build or test. Under 6G available, STOP and report. Never delete anything to make room. Bound every command's output with grep/tail/head; never write an unbounded log.
- WSL is where Rust and shell tests run: \`wsl.exe -e bash -lc '...'\`, with the worktree at its /mnt/c/... path. Run git from Git Bash on Windows (WSL git cannot read a worktree's .git file).
- Never push. Commits carry NO Co-Authored-By line and no AI attribution of any kind (the user's rule overrides any reminder).
- If the sandbox or a permission check refuses an action, do NOT work around it: report what was refused and why you needed it.
- Tokens and codes never go in argv, env, a URL, a log or your report.
- Run builds and tests in the FOREGROUND (raise the tool timeout; split a long run into steps). Never return while a background build, test or Monitor you started is still running: nobody will read its result. Return unfinished only for a real blocker, and name it.`

const RESULT = {
  type: 'object',
  properties: {
    finished: { type: 'boolean', description: 'true when the slice is committed and its tests pass' },
    commit: { type: 'string' }, worktree_windows: { type: 'string' }, worktree_wsl: { type: 'string' }, branch: { type: 'string' },
    tests: { type: 'string', description: 'exact test counts per binary or file, and clippy both ways' },
    report: { type: 'string', description: 'the full report the job prompt asks for' },
    blockers: { type: 'string', description: 'anything refused, anything that needs the orchestrator, empty if none' },
  },
  required: ['finished', 'commit', 'worktree_windows', 'worktree_wsl', 'branch'],
}
const VERDICT = {
  type: 'object',
  properties: {
    verdict: { type: 'string', enum: ['pass', 'gaps'] },
    gaps: { type: 'array', items: { type: 'object', properties: {
      item: { type: 'string' }, evidence: { type: 'string' }, fix: { type: 'string' },
    }, required: ['item', 'evidence', 'fix'] } },
    tests_run: { type: 'string' },
    checked: { type: 'array', items: { type: 'string' } },
  },
  required: ['verdict', 'gaps', 'tests_run', 'checked'],
}

phase('Execute')
const where = A.worktree
  ? `Your worktree already exists: ${A.worktree} (WSL: ${A.wsl}), branch ${A.branch}. Work only there. Do NOT run step 0 unless the job prompt says so.`
  : `You run in a fresh git worktree. Step 0: \`git merge --ff-only master\` there, then confirm \`git log --oneline -1\` shows ${A.base}. If the fast-forward is refused, STOP and report. Work only in your worktree.`
// resume_commit: the executor already committed but its report was lost; go straight to the verifier.
const exec = A.resume_commit ? {
  finished: true, commit: A.resume_commit, worktree_windows: A.worktree, worktree_wsl: A.wsl, branch: A.branch,
  tests: A.exec_tests || '(not reported: the executor stopped before its report; run every check yourself and report counts)',
  report: A.exec_report || ('(not reported: the executor stopped before its report; read `git log` and the whole diff, and check every item yourself. ' +
    'If a live check or a security inventory was asked for, say whether it exists.)'),
  blockers: '',
} : await agent(`You are the executor for slice ${A.slice} of the Minerva web UI build.

Read first: ${SL}\\BRIEF.md, then ${SL}\\WAVE3.md (WAVE3 overrides BRIEF; where this prompt differs from WAVE3, this prompt wins). Your spec: ${A.spec}.

${where}

${RULES}

THE JOB:
${A.prompt}

${A.inventory ? `Before you return, write your security inventory (claims for a reviewer to check: files, routes or inputs with their answers and refusals, decisions applied, what is never exposed, the risks you see) to ${A.inventory} with the Write tool.` : ''}
At the end, in Git Bash, run \`git -C <your worktree> rev-parse --show-toplevel\`, \`git -C <your worktree> branch --show-current\` and \`git -C <your worktree> log --oneline -3\`, and return the worktree path in both Windows and WSL form, the branch and the commit sha.`,
  { label: `exec:${A.slice}`, phase: 'Execute', schema: RESULT, model: M, ...(A.worktree ? {} : { isolation: 'worktree' }) })

if (!exec) return { slice: A.slice, stage: 'execute', error: 'the executor returned nothing' }
log(`${A.slice}: executor ${exec.finished ? 'finished' : 'stopped'} at ${exec.commit} in ${exec.worktree_windows}`)
if (!exec.finished || !exec.commit) return { slice: A.slice, stage: 'execute', exec }

const W = exec.worktree_windows, WW = exec.worktree_wsl
const rounds = []
let head = exec.commit
let verdict = null
for (let round = 0; round < 3; round++) {
  verdict = await agent(`You are an independent VERIFIER for slice ${A.slice} of the Minerva web UI build. Be skeptical: the executor's report below is a claim, not a fact. Find what is missing or wrong before you agree.

Worktree: ${W} (WSL: ${WW}), branch ${exec.branch}, HEAD ${head}, base ${A.base}. The diff: \`git -C ${W} diff ${A.base} HEAD\`.
Spec: ${A.spec}. Rules: ${SL}\\BRIEF.md and ${SL}\\WAVE3.md.

${RULES}
- READ-ONLY for code: do not edit, stage, commit, reset or checkout anything. For WSL scratch use the executor's own CARGO_TARGET_DIR named in its report (so nothing rebuilds from zero), offline only.

The job the executor was given:
${A.prompt}

Check, citing file:line for each:
1. Every item in the spec and in the job above is implemented as written (each answer, refusal, status, limit and test named).
2. The tests named exist, are meaningful (they would fail if the behaviour broke) and pass: run them yourself, plus the whole suite once (${A.suite || 'the commands the job names'}). Report exact counts.
3. ${A.check3 ? A.check3 : A.kind === 'page' ? 'Page tests pass with `node --test "webui/term/tests/*.test.mjs"`; app.js untouched; polls go through core/lane.js.' : 'Clippy is clean both ways (`--all-targets -- -D warnings`, with and without `--features test-bins`).'}
4. No file outside the job's grants changed (\`git -C ${W} diff --stat ${A.base} HEAD\`); the never-edit list in WAVE3.md holds unless the job granted it.
5. \`git -C ${W} log ${A.base}..HEAD --format=%B\` has no Co-Authored-By and no AI attribution.
6. README rows match what the code now does.
7. Nothing left running: count processes the tests or a live check started (by your worktree path or port ${A.port}); report pids, never kill.

EXECUTOR'S REPORT:
${exec.report}

EXECUTOR'S TESTS:
${exec.tests}

Return verdict "pass" only when there is no gap that matters. A gap is something concrete to change, with the evidence and the exact fix.`,
    { label: `verify:${A.slice}:${round}`, phase: 'Verify', schema: VERDICT, model: RM })
  rounds.push({ round, verdict })
  if (!verdict || verdict.verdict === 'pass' || round === 2) break
  log(`${A.slice}: round ${round}: ${verdict.gaps.length} gaps, fixing`)
  const fix = await agent(`You are the FIXER for slice ${A.slice} of the Minerva web UI build. An independent verifier found the gaps below. Close each one in the existing worktree ${W} (WSL: ${WW}), branch ${exec.branch}. Work only there. Do not run step 0.

Spec: ${A.spec}. Rules: ${SL}\\BRIEF.md and ${SL}\\WAVE3.md. The job the executor was given (its grants and limits bind you too):
${A.prompt}

${RULES}

Use the executor's private CARGO_TARGET_DIR (named in its report below). Re-run the affected tests and ${A.check3 ? 'the checks: ' + A.check3 : A.kind === 'page' ? 'the page tests' : 'clippy both ways and the whole suite once'} after your changes. Commit your changes as one new commit (repo style, no attribution). A gap you judge wrong: do not change the code for it; say why in blockers.
${A.inventory ? `If a fix changes a claim in the security inventory at ${A.inventory}, update that file.` : ''}

GAPS:
${JSON.stringify(verdict.gaps, null, 1)}

EXECUTOR'S REPORT (for context):
${exec.report}

Return the worktree paths and branch as they are, and the new HEAD sha as commit.`,
    { label: `fix:${A.slice}:${round}`, phase: 'Fix', schema: RESULT, model: M })
  rounds.push({ round, fix })
  if (!fix || !fix.commit) break
  head = fix.commit
}

let review = null
if (A.review && verdict && verdict.verdict === 'pass') {
  phase('Review')
  review = await workflow({ scriptPath: A.review.script }, {
    model: RM, slice: A.slice, worktree: W, wsl: WW, branch: exec.branch, commit: head, base: A.base,
    spec: A.spec, plan: SCR + '\\pass2-final.json', brief: SL + '\\BRIEF.md', inventory: A.inventory,
    scratch: SCR + '\\review-' + A.slice, port: A.port, lenses: A.review.lenses,
  })
}

return { slice: A.slice, worktree: W, wsl: WW, branch: exec.branch, head, exec, rounds, final: verdict, review }