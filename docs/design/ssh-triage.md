# SSH as the triage door into other systems

Design only, no code, no commit. Written against the eidolon-door checkout
at `C:\Users\dxcen\Projects\eidolon-door\eidolon`, commit `a618ef1` and
`master` beyond it. Labels: **observed** (I read the file and cite it),
**reasoned** (follows from cited lines), **hypothesis** (a belief with no
measurement behind it, named as such). Style and label conventions follow
`docs/design/triage.md`, `linux.md`, `observation.md`, `permissions.md`,
`judgement.md`.

## 0. Goal and non-goals

**Goal.** Minerva's triage agent, running as eidolon in WSL2, needs to
inspect processes, services, logs, disk and network on machines other than
the one it runs on — starting with the Windows host it already lives
underneath (loopback today; sshd makes it a second box for this purpose)
and extending to other club machines. "See whatever process it needs to,
including being able to ssh into other systems (with passcode) and triage
there" — the operator's own words. The chosen transport is uniform: ssh
everywhere, because the Windows host already runs sshd and WSL2 already has
an ssh client, so triage does not need two remote-access mechanisms for two
kinds of target.

**Non-goals, stated so scope does not creep during implementation.**

| out of scope here | why |
|---|---|
| Any write action over ssh (`systemctl restart`, `kill`, editing a remote file) | triage is read-mostly inspection; a write-capable remote tool is a separate `.rn` file with its own policy rule and its own warrant clause, not a mode of this one |
| Deploying eidolon or any agent on the remote box | the remote side is a plain sshd target, not a peer session; nothing here talks to eidolon's swarm primitives (`peers`/`send`, `crates/rune/src/lib.rs:129-136`, **observed**) |
| Credential management UI, secret rotation | out of scope; this reuses whatever `eidolon secret set` already does |
| Non-ssh remote access (WinRM, RDP, a custom agent) | ssh was the operator's chosen path; not evaluated here |

## 1. The tool's shape

One `.rn` file, `~/.config/eidolon/tools/ssh.rn`, on the same contract every
script tool uses:

```rune
pub fn manifest() { #{ name: "ssh", description: "…", approval: "…", input_schema: #{…} } }
pub async fn call(input) { … }
```

**observed** — this is the whole contract; `manifest()` runs once at load
and is deserialized into a `ToolManifest`, `call(input)` runs per dispatch
with the model's JSON input as a Rune object, and a file dropped in the
tools directory is picked up by `register_dir` without a rebuild
(`crates/rune/src/lib.rs:30-40`, `:179-231`).

### Why `exec`, not `shell`, and why argv-only

The host exposes two primitives a `.rn` tool can call: `shell(command,
timeout_s)`, which spawns `bash -c <command>` (**observed**,
`crates/tools/src/shell.rs:90-92`, wired at `crates/rune/src/host.rs:386-399`),
and `exec(program, args, timeout_s)`, which spawns the program directly with
argv as a list of strings and **no shell in between** (**observed**,
`crates/tools/src/shell.rs:107-113`'s doc comment: "no pipes, no redirection,
no globbing … nothing a `;`, a `$(…)` or a pipe in an argument can do — the
child sees each argument as one word because nothing ever parsed a command
line"; wired at `crates/rune/src/host.rs:405-437`). Both were added together
in commit `a618ef1` (**observed**, `git show a618ef1 --stat`: `crates/rune/src/host.rs`
+125, `crates/tools/src/shell.rs` +111).

`ssh.rn` calls `exec("ssh", [flags…, host, remote_command_argv…], timeout_s)`.
**Reasoned**: the tool's own argument is the remote command, which will
itself often contain characters (`|`, `;`, spaces in paths) that would need
shell quoting if built into one string; building it as an argv list handed
to `exec` means the *local* side never parses a command line, so a model
cannot smuggle a local shell metacharacter into the ssh invocation. What
still gets a shell is the **remote** end — ssh always hands its trailing
arguments to the remote shell by SSH protocol convention unless
`-T`/`RequestTTY=no` and a no-shell remote command form is used; that is
outside eidolon's control and is a fact about ssh, not about eidolon
(**reasoned**, not eidolon source — this is the standard OpenSSH client
contract, not something this repo implements or tests).

### The argv, concretely

```
ssh -o BatchMode=yes -o ConnectTimeout=<n> -o StrictHostKeyChecking=yes
    -F <minerva-managed known_hosts / config> <user>@<host> -- <remote argv…>
```

| piece | value | reasoning |
|---|---|---|
| `-o BatchMode=yes` | always | refuses any interactive password prompt; if key auth fails, ssh exits nonzero instead of hanging waiting for a tty that isn't there (this is ssh's own documented behaviour, not eidolon's — **reasoned**, not verifiable from this repo) |
| `-o ConnectTimeout=<n>` | small, e.g. 5–10s | bounds the "host unreachable" failure mode so triage does not hang the turn; independent of eidolon's own `timeout_s` on `exec`, which bounds the whole call including remote command execution (**observed**, `crates/tools/src/shell.rs:107-113`, same timeout/cancel plumbing as `shell`) |
| `-o StrictHostKeyChecking=yes` (never `no`) | always | see §6, "host key changed" — accepting an unknown or changed host key silently is exactly the MITM window ssh exists to prevent; **this document rules it out categorically**, not something eidolon enforces — the tool author must not pass `no` |
| host allow-list | from Minerva config, not model input | the model supplies which host *by name from a fixed list*, never a free-form hostname string, so the tool cannot be walked to an arbitrary network address by a crafted prompt |
| remote argv | a fixed per-technique command, per §5's matrix | never a free-form shell string — same reasoning as `exec` itself: structure, not escaping, is what makes injection impossible |

## 2. Authentication — the key question

"With passcode" has two honest readings and one dishonest one.

| option | shape | verdict |
|---|---|---|
| (a) key-based, passphrase held by `ssh-agent` | operator runs `ssh-add` once, interactively, outside eidolon; the agent socket (`SSH_AUTH_SOCK`) is inherited by the WSL2 process eidolon runs as; every `ssh` call authenticates silently against the agent | **recommended** |
| (b) password auth via an `SSH_ASKPASS` helper reading eidolon's secret store | ssh calls the helper program named by `SSH_ASKPASS` when it needs a passphrase or password and `DISPLAY`/`SSH_ASKPASS_REQUIRE=force` is set; the helper reads the secret from eidolon's store and prints it to stdout for ssh to consume — never argv, never an env var holding the value itself, never a log line | possible, **more moving parts, only if (a) is unavailable** |
| (c) `sshpass -p <password> ssh …` or `SSHPASS=<password> sshpass ssh …` | the password sits in a process's argv (`-p`) or its environment, both of which are visible to anything that can read `/proc/<pid>/cmdline` or `/proc/<pid>/environ` on the same box, and argv additionally appears in shell history and process listings | **rejected** — this is precisely the shape triage itself would use to find a secret on someone else's box (see `docs/design/triage.md:88`, "`cat ~/.ssh/id_rsa`… disclosure is not mutation" — the same triage agent auditing *other* systems must not carry this pattern itself) |

### Can a Rune tool read a secret at all?

**No — by design, and this matters for (b).** eidolon's secret store is
reachable from a `.rn` script only through `api_request`'s `token_secret`
field, and even there the value never becomes a string a script can see:

> "`token_secret` names an entry in the custodied store (`eidolon secret
> set NAME`)… resolved by `resolve` at call time, and put into a header by
> `eidolon_tools::api::request` without ever being a string a script, a tool
> result or a log can reach." (**observed**, `crates/rune/src/endpoint.rs:30-36`)

The resolution path is Rust-only: `resolve()` in `endpoint.rs:69-94`
(**observed**) reads `store.external_value(name)` and wraps it in
`Zeroizing<String>`, consumed inside `eidolon_tools::api::request` — never
returned to the calling script. Confirmed by grepping `host.rs` for every
registered Rune primitive (`cwd`, `fs_write`, `fs_edit`, `shell`, `exec`,
`shell_background`, `grep`, `web_search`, `mneme_call`/`mneme_rpc`,
`swarm_call`, `peers`, `peer_send`, `ask_user`, `choices_user` —
**observed**, `crates/rune/src/host.rs:334-763`): there is no
`secret_get`/`secret_read` primitive in that list. `api_request` itself is
declared in `crates/rune/src/lib.rs`'s module doc (**observed**, line 66-73)
but its wiring was not found registered in `host.rs` at the read revision —
either it is added elsewhere in the `a618ef1` diff (`crates/rune/src/script.rs`,
+27 lines, not read in full here) or it is exposed through a different seam;
**not fully verified — see the open item at the end of this document.**
What *is* verified: no Rune host function anywhere in `host.rs` hands a
secret's plaintext back to a script.

**Consequence for (b).** An `SSH_ASKPASS` helper that reads eidolon's secret
store cannot be *the Rune tool itself* doing the reading — it must be a
small separate executable (outside the `.rn` sandbox, invoked by `ssh` as a
subprocess, not by eidolon) that has its own access to wherever the secret
actually lives on disk (e.g. the same custodied store's file, or a
`token_file`-style path). This is new surface this design introduces and
does not find precedent for in the codebase — **hypothesis**, flagged as an
open question in §7.

### Where the passphrase is entered

- **Recommended (a):** once, by the operator, interactively, via `ssh-add`,
  outside any eidolon session. eidolon never sees it.
- **Never via `ask_user` or `choices_user`.** Both route through the
  ordinary dispatcher path (**observed**, `crates/rune/src/host.rs:735-762`:
  `ask_user`/`choices_user` call `host.dispatch(...)`), and every dispatched
  call's result is journaled into the session log — this is the same
  mechanism `docs/design/observation.md` and `docs/design/permissions.md`
  describe in detail for every tool call (**observed** pattern, cited there:
  `crates/core/src/dispatch.rs`'s `dispatch_with` journals a `ToolResult`
  record for every call regardless of origin, per `docs/design/observation.md`
  lines 99-115 and 502-525 of this session's reading). A `choices_user`
  answer or an `ask_user` answer is tool-result content like any other, and
  tool-result content is written to the `.eid` log. **A password must never
  be solicited through either primitive.** This is a hard rule, not a
  preference: the log is plaintext-readable by anything that can read the
  session file, and `docs/design/permissions.md` already treats "disclosure
  is not mutation" as a real failure mode for read-only tool output
  (`docs/design/triage.md:88`, cited above) — an operator-entered secret
  flowing through `ask_user` would be worse, because it is disclosure of a
  *credential*, not just of a config value.

## 3. Policy: how this tool is classified today

**Observed, and this is the load-bearing finding for the whole design.**
Classification happens once per tool call, before dispatch, in
`ScriptPolicy::classify` (`crates/rune/src/policy.rs:278-294`). It asks one
question first: does `policy.rn`'s `shell_arg(tool_name)` name a field that
holds a shell command string?

```rune
pub fn shell_arg(tool) {
    match tool {
        "bash" => "command",
        "Bash" => "command",
        _ => "",
    }
}
```
(**observed**, `crates/rune/policy.rn:82-91`)

For any tool name other than `bash`/`Bash`, this returns `""`, and
`classify()` falls through to `self.tool_verdict(name, manifest.approval,
&call.input)` — **the tool is classified opaquely, as one call, by its
declared `approval` and whatever `policy.rn`'s `classify_tool` match arm
says about its name** (**observed**, `crates/rune/src/policy.rs:284-294`).
The harnox shell-decomposition algebra — pipes, `&&`, redirects, the
read-only composition rule `docs/design/triage.md` describes at length — is
**never invoked** for a tool named `ssh`, because that algebra only runs
over the string found in the `shell_arg`-named field, and `ssh` has no such
field mapped.

Concretely: an `ssh` tool call's **inner remote command is invisible to the
policy engine.** The engine never sees `ls /var/log` versus `rm -rf /`; it
sees only "the `ssh` tool was called," once, and classifies that.

`classify_tool`'s fallback for a name it does not recognize:

```rune
_ => {
    if tool.starts_with("melete_") { return classify_melete(tool, approval); }
    v(FLAG, `unclassified tool (declared ${approval})`, false)
}
```
(**observed**, `crates/rune/policy.rn:188-198`)

`policy.rn` already has a line for the raw `ssh` **program**, reached only
when a *shell string* is decomposed and `ssh` appears as a leaf program
inside it: `v(FLAG, "reaches another host", false)` (**observed**,
`crates/rune/policy.rn:382-384`). That line is dead code for this design —
it fires only if some other tool's shell-string field contained a literal
`ssh …` invocation, not for a dedicated `ssh` tool called via `exec`.

### The proposed rule

Two additions to `policy.rn`, neither implemented here (design only):

1. A `classify_tool` arm for `"ssh"` that does **not** blanket-allow or
   blanket-flag, but inspects `input` — the call's own arguments — the way
   `classify_send` already does for `send` (**observed** precedent,
   `crates/rune/policy.rn:219-228`, matching on `input.get("to")`). The
   `ssh` tool's `input` carries `host` and the remote technique name (see
   §5's fixed matrix); the policy arm checks whether that technique is on a
   **read-only allow-list per remote OS**, and returns `ALLOW,
   read_only: true` only for those; anything else is `FLAG`.
2. Because the remote argv is fixed per technique (never freeform, per §1),
   the allow-list can be exact-string matching, the same shape a `commands`
   warrant clause already uses (`docs/design/triage.md:44-46`, `commands` is
   a `BTreeSet<String>` checked by string equality) — no second shell parser
   is needed, because there is no shell string to parse on the local side.

**What this does not give for free:** `docs/design/triage.md`'s proposed
`reads` warrant clause (§2.3 there) is defined in terms of `Ruling.read_only`
composed by the harnox algebra over a decomposed shell command
(`docs/design/triage.md:66-77`, `docs/design/permissions.md:776-822`
tracing the same field). An `ssh` tool call is never decomposed, so `reads`
as specified there does not automatically cover it — the policy arm above
would need to set `read_only: true` itself, by hand, per allow-listed
technique, which is a second, parallel judgment about what counts as a read,
made by whoever edits this tool's `policy.rn` arm rather than by the
composed algebra. **Reasoned**, flagged as an open question in §7 (should a
`reads` warrant cover this tool, and if so, on whose word).

### Audit

Every call is journaled regardless of tier — `Allow`, `Flag`→asked, or
`Deny` all reach `Dispatcher::adjudicate`
(`docs/design/permissions.md:47-52`, `crates/core/src/dispatch.rs:380-434`,
**observed** per that document's own citation) and every dispatched call's
result is journaled (`docs/design/observation.md`'s dispatch.rs citations,
above). So **host + full argv (including the remote command) land in the
session log for every call**, whatever tier the call was classified at —
this is a property of the dispatcher, not something this tool needs to add.

## 4. Remote OS matrix

| target | reached via | shell on connect | representative read-only techniques |
|---|---|---|---|
| Linux | ssh, OpenSSH server | remote user's default shell (bash/sh), or none with a no-shell remote command | `ps -eo pid,ppid,user,etime,cmd`, `ss -ltnp` / `netstat -tulpn`, `systemctl status <unit>` / `journalctl -u <unit> -n 50`, `cat /proc/<pid>/cmdline`, `df -h`, `tail -n 100 <logfile>` — same technique labels `docs/design/triage.md` §6.1 already uses for local Linux triage |
| Windows (the local host, via its own sshd, reached from WSL2) | ssh, Windows OpenSSH server (`sshd` as a Windows service) | **cmd.exe by default**, or PowerShell if the target user's default shell was configured to `powershell.exe`/`pwsh.exe` in the Windows OpenSSH registry setting — this is a documented fact about Windows' OpenSSH port, not something in the eidolon repo, so it is **documentation-grade, not observed here** | `tasklist`, `netstat -ano`, `Get-CimInstance Win32_Process` via `powershell -NoProfile -Command "…"`, `Get-WinEvent` for logs — the same commands `docs/design/triage.md` §6.1 already lists as the *local* Windows triage set, now run remotely instead of via local `bash -c` |
| macOS | ssh, macOS's built-in OpenSSH (usually disabled by default, "Remote Login" must be turned on) | the user's shell, typically `zsh` since Catalina | `ps -eo pid,ppid,user,etime,comm`, `lsof -i`, `launchctl list` (macOS's service manager, no direct Linux equivalent), `log show --last 1h` (unified logging, replaces `journalctl`), `df -h` |

**Reasoned, not eidolon source:** the Windows-shell and macOS-launchctl
facts above are standard platform documentation, not something this
codebase implements or tests; they are stated because the remote argv in
§1/§3 must be built per target OS and this matrix is what that build reads.

## 5. Failure modes and returns

`exec`'s own return shape, unchanged for `ssh`: `Exec` rendered to a string
via `.render()` on success, or an `Err` on spawn failure
(**observed**, `crates/tools/src/shell.rs:107-113` signature,
`Exec::render()` referenced at `crates/tools/src/shell.rs:47` and
`docs/design/observation.md:361-370`'s description of the same struct for
`bash`). `bash.rn`'s wrapper pattern — `is_error` set from a nonzero exit —
applies identically once `ssh` is wired the same way `docs/design/observation.md`
§4 specifies for `bash` (**observed** precedent,
`crates/rune/builtin/bash.rn`; the `is_error`-on-nonzero-exit rule is
`docs/design/observation.md`'s "Step 5" for `crates/rune`).

| failure | ssh's own exit behaviour | what the tool should surface |
|---|---|---|
| host unreachable / connection refused / `ConnectTimeout` expired | ssh exits 255 with a message on stderr naming the host | `is_error: true`, content names the host and "unreachable" — never retried automatically |
| auth refused (no working key, agent empty, or password path fails) | ssh exits 255, "Permission denied (publickey)" or similar | `is_error: true` — **must not** fall back to a weaker auth method silently; `BatchMode=yes` already prevents ssh from hanging on an interactive prompt it cannot show |
| **host key unknown or changed** | ssh refuses the connection outright under `StrictHostKeyChecking=yes` and exits nonzero with `REMOTE HOST IDENTIFICATION HAS CHANGED!` on a mismatch | `is_error: true`, and this is the one failure mode this design insists **must stop, not be auto-accepted** — there is no flag anywhere in this tool that ever sets `StrictHostKeyChecking=no` or `UserKnownHostsFile=/dev/null`. A changed host key is exactly the signal a MITM or a re-imaged box produces, and the fix is a human updating `known_hosts` deliberately, not the tool doing it silently |
| remote command itself fails (nonzero exit on the far end) | ssh forwards the remote exit code as its own exit code | `is_error: true` if nonzero, same as any local command — this is not a transport failure, just an unsuccessful triage step, and the graph-level `expect`/recovery machinery `docs/design/observation.md` and `docs/design/triage.md` already specify for local commands applies unchanged |
| `exec`'s own `timeout_s` expires | eidolon kills the process group (**observed**, `crates/tools/src/shell.rs:169-174`, `kill_group`) | same as any local timed-out command |

## 6. Open questions for the operator

1. **Which remote hosts and OSes, concretely?** This document assumes "the
   Windows host via its own sshd" is the first target and names Linux/macOS
   as the others the operator mentioned; the actual allow-list (§1) needs
   real hostnames/IPs before this ships.
2. **Keys or passwords?** §2 recommends (a), `ssh-agent` with key
   passphrases, and treats (b) (`SSH_ASKPASS` against the secret store) as
   a fallback that introduces a new component (a small non-Rune helper
   binary) this repo has no precedent for. Confirm which is acceptable, or
   whether passwords are needed at all for the first targets.
3. **Who manages `known_hosts`, and how?** §6 rules out auto-accepting a
   changed key; someone has to populate and maintain the known-hosts file
   this tool points `-F`/`UserKnownHostsFile` at. Is that a file checked
   into Minerva's config (fingerprints are not secrets), or operator-local
   state outside the repo?
4. **Do write actions ever go over ssh, and if so, when?** §0 excludes them
   from this design entirely. If the answer is "yes, eventually," that is a
   second tool file with its own policy arm and very likely its own warrant
   clause (`commands`/`reads` per `docs/design/triage.md` §2), not a flag on
   this one.
5. **Should this tool's read-only techniques count toward a `reads`
   warrant clause?** §3 notes that `docs/design/triage.md`'s `reads` clause
   is defined over the harnox-composed `Ruling.read_only`, which this tool
   never produces automatically — someone has to hand-declare which `ssh`
   techniques are reads, in `policy.rn`, by inspecting `input`. Confirm
   that is acceptable, or that `ssh` calls should always just ask (`FLAG`)
   regardless of technique, at least until this is settled.

## 7. Test plan

| test | setup | what it proves |
|---|---|---|
| loopback smoke test | WSL2's ssh client against the Windows host's own sshd, on `localhost` or the host's LAN-visible name, using a throwaway key added just for this test | the whole path — key auth, `BatchMode`, `ConnectTimeout`, argv-only `exec`, a fixed read-only Windows technique (`tasklist`) — works end to end before any other box is involved |
| host-key-change refusal | connect once to record the fingerprint, then replace the target's host key (a fresh sshd instance, or a throwaway container) and connect again | confirms the tool surfaces `is_error` and stops, rather than silently reconnecting — this is the one behaviour §6 says must never be soft |
| unreachable host | point the allow-list at a host/port nothing is listening on | confirms `ConnectTimeout` bounds the failure and the tool does not hang the turn |
| auth-refused | a real reachable sshd, but no working key/agent entry for it | confirms `BatchMode=yes` produces a fast, clean `is_error` rather than a hang |
| throwaway container sshd, Linux target | a disposable `linuxserver/openssh-server`-style container (or any minimal sshd image) reachable from WSL2, seeded with the test key | exercises the full Linux technique matrix (§4) — `ps`, `ss`/`netstat`, `journalctl` — against a target that is not the operator's own machine, safe to discard |
| policy classification check | `eidolon policy` (or the equivalent direct call to `ScriptPolicy::explain`/`classify`) against an `ssh` call with a read-only technique versus a non-allow-listed one | confirms the proposed `classify_tool` arm (§3) actually produces `ALLOW, read_only: true` for the allow-listed set and `FLAG` for everything else, and that the remote argv shows up verbatim in the policy explanation the way `docs/design/triage.md`'s M1/M11 measurements did for local commands |

---

**What could not be verified from this repo alone:** whether `api_request`
(named in `crates/rune/src/lib.rs`'s module doc, §2 above) is actually wired
as a registered Rune primitive somewhere this reading did not reach — the
`a618ef1` diff also touches `crates/rune/src/script.rs` (+27 lines) and
`crates/cli/src/main.rs` (+11 lines), neither read in full here. This does
not change any conclusion in this document (no path from a Rune script to a
raw secret value was found regardless of where `api_request` is wired,
because `endpoint.rs:69-94`'s `resolve()` is what keeps the value out of
script reach, and that function was read in full), but it is named rather
than silently assumed complete.
