# Supervised Eidolon agents

Run from PowerShell 7 (the process argument and process-tree termination APIs
require modern .NET):

```powershell
pwsh -File bin/watch-agent.ps1 -TaskFile C:/path/task.txt -RunName implementation-001
pwsh -File bin/watch-agent.ps1 -TaskFile C:/path/review.txt -RunName review-001
```

The runner pins `ollama:deepseek-v4.1-flash`, disables fallback, and supplies
Git Bash in the child PATH when installed at its standard Windows location.
It does not edit the live Eidolon configuration. Use separate implementation
and read-only review prompts, claim file ownership through peers/send, and
inspect changed content after each run.

Every ten seconds, the supervisor updates PID, elapsed time and status under
`logs/agents/`. Stdout and stderr are captured separately and remain readable
while running. At 480 seconds (override with `-TimeoutSeconds`), it kills only
the launched process tree and returns 124. Other failures retain their exit
code. A unique RunName prevents overwriting evidence. `exited` means the
process ended with zero, not that the task passed; inspect both logs for tool
failures, denied actions, and the agent's final report. A quiet interval does
not prove a stall; the hard deadline bounds silent hangs.

The supervisor must remain running. This is a per-run watchdog, not a scheduled
service or a promise to monitor agents after the parent process is closed.
Existing service processes are not stopped or rebuilt by the runner.
Workspace catalog integration:
- `webui/minerva_catalog.py` merges runtime tools/Rune skills into WebUI's existing list responses; the separate inventory panel has been removed.
- Workspace Model presets uses WebUI's native saved-preset list and editor. Raw base models belong in model selection, not in the Workspace preset list or its count.
- Counts, search, sorting and pagination use those merged responses. Existing saved items win on matching IDs. Loaded/configured tools with the same name collapse to one runtime entry.
- Native create/update/delete handlers remain authoritative. Compiled/runtime tools are read-only; this patch does not turn them into Python tools or create a second executor.
- Runtime inventory retains the existing operator/admin boundary. Other users keep their normal permission-filtered native records. Club-wide per-user runtime grants are not implemented yet.
- Native Python tests and a native-frontend fixture verified merged lists and search. An empty Knowledge/Prompts/Skills catalog still correctly counts as zero.
- The bidirectional bridge for Eidolon to read/create/use WebUI notes, knowledge, prompts, skills and tools remains unfinished. Catalog visibility does not imply that bridge is complete.
