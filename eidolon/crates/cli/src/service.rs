//! The service half of an extension: what turns a manifest's `service:`
//! declaration into a process that is running, or an honest refusal.
//!
//! The rules the spec fixes, each one here because its opposite was the
//! bug some other system grew: `command` with a `/` is a path (and
//! `{dir}` may stand for the plugin directory); without one it is
//! resolved on `$PATH`, by us, before anything spawns — a program we
//! could not find is a refusal naming it, not a spawn error after the
//! fact. `args` is an argv list handed to `exec` as-is: `{dir}` inside an
//! element expands to the plugin directory, and since no shell ever sees
//! the line, a path with a space in it stays one argument. The child is
//! detached — its own session and process group — so it outlives the CLI
//! that started it, with stdout and stderr in
//! `<cache>/eidolon/services/<name>.log`, the only place its failure is
//! visible. Readiness is polled, not assumed; a daemon that never
//! answers is SIGTERMed and its log tail shown, because a service that
//! failed to start is a leak the operator would otherwise hunt by hand.
//!
//! `status` answers by probing, never by trusting a file: the port is
//! connected to, or the answer says nothing is on it. `stop` is the one
//! place a recorded file is used, and only after `/proc/<pid>/cmdline`
//! is matched against the recorded argv — a pid file can lie, and the
//! thing that lies hardest is the one pointing at a pid some other
//! process holds now. (vault, *Plugin Packaging (spec)*, §"Services v1".)
//!
//! Operator-only by construction: a CLI verb, never a tool, and no
//! script reaches any of it — the same line the trust event draws.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;

/// How often readiness is polled. Short enough that a start feels
/// immediate; long enough that a hundred probes are not a flood.
const POLL: Duration = Duration::from_millis(100);

/// Where a service's state lives: the log, the pid file. Under the
/// operator's cache, because this is reproducible state — removing it
/// costs nothing but a lost log.
pub fn services_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("eidolon")
        .join("services")
}

/// The program, resolved: a declaration with a `/` is a path (`{dir}` the
/// plugin directory, relative against that directory); without one it is
/// looked up on `$PATH` here, so the refusal can name what was not found
/// rather than reporting a spawn failure afterwards.
fn resolve(command: &str, plugin_dir: &Path) -> anyhow::Result<PathBuf> {
    let path = if command.contains('/') {
        let filled = command.replace("{dir}", &plugin_dir.display().to_string());
        let p = Path::new(&filled);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            plugin_dir.join(p)
        }
    } else {
        let path = std::env::var_os("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        anyhow::bail!("command `{command}` is not on $PATH");
    };
    if path.is_file() {
        Ok(path)
    } else {
        anyhow::bail!("command `{command}` resolves to {} which does not exist", path.display())
    }
}

/// The argv the child runs: the resolved program, then every argument
/// with `{dir}` expanded. Expansion is textual inside the element and
/// the element stays one element — exec is given this list verbatim, and
/// no shell ever re-splits it.
fn argv_of(command: &Path, args: &[String], plugin_dir: &Path) -> Vec<String> {
    let dir = plugin_dir.display().to_string();
    let mut argv = vec![command.display().to_string()];
    argv.extend(args.iter().map(|a| a.replace("{dir}", &dir)));
    argv
}

/// Spawn detached: own session (`setsid`) and process group, so the
/// service outlives the CLI and no terminal signal reaches it; stdio
/// into the service's log, appended, because a restart's output belongs
/// beside the last attempt's. Returns the pid.
fn spawn_detached(argv: &[String], cwd: &Path, log: &Path) -> anyhow::Result<u32> {
    use std::os::unix::process::CommandExt;
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let err = out
        .try_clone()
        .with_context(|| format!("sharing {} for stderr", log.display()))?;
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        // Detached means the child keeps no group of ours: a terminal's
        // signals reach the CLI, not the service it started. (setsid is
        // the stronger form and is refused by some sandboxes — EPERM —
        // where process_group(0) is allowed; the properties the spec
        // names, outliving the CLI and no terminal signal, both hold.)
        .process_group(0);
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", argv.join(" ")))?;
    Ok(child.id())
}

/// Can a TCP connection be made, and — when a health path is declared —
/// does it answer 2xx? The whole of the HTTP the probe speaks.
fn probe(port: u16, health: Option<&str>) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let Some(path) = health else {
        return true;
    };
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 256];
    let n = stream.read(&mut head).unwrap_or(0);
    let line = String::from_utf8_lossy(&head[..n]);
    line.split_whitespace().nth(1).is_some_and(|code| code.starts_with('2'))
}

/// Poll readiness until it arrives or the budget runs out — unknown is
/// not zero: a probe that never succeeded is a failure with the log to
/// read, never a quiet success.
fn await_ready(port: u16, health: Option<&str>, budget: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + budget;
    loop {
        if probe(port, health) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "readiness did not arrive within {}s: nothing {} on 127.0.0.1:{}{}",
                budget.as_secs(),
                if health.is_some() { "healthy" } else { "listening" },
                port,
                health
                    .map(|h| format!(" at {h}"))
                    .unwrap_or_default(),
            );
        }
        std::thread::sleep(POLL);
    }
}

/// The recorded state of one started service: enough to stop the right
/// process and nothing else.
#[derive(serde::Serialize, serde::Deserialize)]
struct PidFile {
    pid: u32,
    argv: Vec<String>,
}

pub(crate) fn pid_path(services: &Path, name: &str) -> PathBuf {
    services.join(format!("{name}.pid"))
}

fn log_path(services: &Path, name: &str) -> PathBuf {
    services.join(format!("{name}.log"))
}

/// What the pid file says, if it says anything readable.
fn recorded(services: &Path, name: &str) -> Option<PidFile> {
    let text = std::fs::read_to_string(pid_path(services, name)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Is this plugin's service running, by the same two questions `stop`
/// asks before it signals anything: is a process recorded, and is that
/// process still the one recorded? `None` means there is nothing to
/// protect — no record at all, a record whose process has exited, or a
/// pid that now runs something else. A caller refuses on `Some`; the pid
/// is named so the operator knows what to stop.
pub fn running(services: &Path, name: &str) -> Option<u32> {
    let state = recorded(services, name)?;
    match cmdline(state.pid) {
        Some(actual) if actual == state.argv => Some(state.pid),
        // A recycled pid or an exited process is not this plugin's
        // service, and a stale file is not a reason to refuse.
        _ => None,
    }
}

/// The process's argv, as the kernel holds it — NUL-separated, with the
/// trailing NUL trimmed. `None` for a pid that has exited, including a
/// zombie nobody has reaped yet and the window just before it becomes
/// one: a dead process is not a running one, and an empty argv is the
/// kernel saying so.
fn cmdline(pid: u32) -> Option<Vec<String>> {
    // The state letter comes from `stat`, after the command in parens —
    // `Z` is a zombie, exited and waiting on a parent that has not
    // reaped it. It holds a `/proc` entry and an empty argv, which is
    // not the same as running.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = stat.rfind(')')?;
    let state = stat[open + 1..].trim_start().chars().next()?;
    if state == 'Z' {
        return None;
    }
    let text = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()?;
    let argv: Vec<String> = text
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if argv.is_empty() {
        // Readable and empty. The kernel releases a process's argv before
        // it flips the state letter to `Z`, so there is a window in which
        // this is not a zombie and still names nothing — observed for
        // real, as a `sleep 30` killed under load whose `stop` refusal
        // then called the pid recycled when it had not been recycled at
        // all. An empty argv is the same answer the letter gives a moment
        // later: this is not a running process. Neither the `Z` check nor
        // this one is redundant; they cover the two ends of the exit.
        return None;
    }
    Some(argv)
}

fn alive(pid: u32) -> bool {
    cmdline(pid).is_some()
}

/// The last lines of the log — the only place a service's failure is
/// visible, so a refusal carries it rather than pointing at it.
fn log_tail(services: &Path, name: &str) -> String {
    match std::fs::read_to_string(log_path(services, name)) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            if lines.is_empty() {
                "the log is empty".into()
            } else {
                let start = lines.len().saturating_sub(20);
                format!("log tail:\n{}", lines[start..].join("\n"))
            }
        }
        Err(_) => "the log could not be read".into(),
    }
}

/// `plugins service approve`: record that this plugin's service
/// declaration is approved as written — the act `start`'s prompt offers,
/// and the same act from the CLI for a headless operator. Written
/// through the permission store's whole-rewrite and composed check: a
/// record that would break the gate stays unwritten.
pub fn approve(plugins_dir: &Path, policy_path: &Path, name: &str) -> anyhow::Result<String> {
    let (_dir, service) = locate(plugins_dir, name)?;
    let hash = eidolon_rune::permits::service_declaration_hash(&service);
    let permits = std::sync::Arc::new(eidolon_rune::permits::Permits::from_policy_path(policy_path));
    let host = eidolon_rune::Host::new(std::env::current_dir()?);
    let table = std::fs::read_to_string(policy_path)
        .unwrap_or_else(|_| eidolon_rune::policy::DEFAULT_POLICY.to_string());
    permits.attach_checker(std::sync::Arc::new(move |src| {
        eidolon_rune::permits::check_composed(src, &table, &host)
    }));
    let mut rows = permits.rows();
    rows.attest.retain(|row| row.plugin != name);
    rows.attest.push(eidolon_rune::permits::AttestRow {
        plugin: name.to_string(),
        hash: hash.clone(),
        granted_by: eidolon_rune::permits::granted_by(),
        granted_at: eidolon_rune::permits::granted_at(),
    });
    rows.attest.sort_by(|a, b| a.plugin.cmp(&b.plugin));
    permits.write(&rows)?;
    Ok(format!(
        "{name}: service approved — `start` will run it while the declaration stays exactly what was approved"
    ))
}

fn entry_service(entry: &eidolon_rune::plugins::Plugin) -> Option<eidolon_rune::plugins::PluginService> {
    match entry {
        eidolon_rune::plugins::Plugin::Installed { manifest, .. } => manifest.service.clone(),
        eidolon_rune::plugins::Plugin::Broken { .. } => None,
    }
}

fn entry_dir(entry: &eidolon_rune::plugins::Plugin) -> Option<PathBuf> {
    match entry {
        eidolon_rune::plugins::Plugin::Installed { dir, .. } => Some(dir.clone()),
        eidolon_rune::plugins::Plugin::Broken { .. } => None,
    }
}

/// Find the plugin's directory and declaration in one pass, with the
/// record's say-so: a plugin the operator disabled does not start, the
/// same rule a run's door keeps.
/// The plugin's directory and declaration, whatever the record says —
/// the locating half the verbs share; `start` adds the record's say-so,
/// `status` does not (probing is not adopting).
fn locate(
    plugins_dir: &Path,
    name: &str,
) -> anyhow::Result<(PathBuf, eidolon_rune::plugins::PluginService)> {
    let plugins = eidolon_rune::plugins::discover(plugins_dir)?;
    for entry in &plugins {
        if let eidolon_rune::plugins::Plugin::Installed { manifest, .. } = entry
            && manifest.name == name
        {
            let dir = entry_dir(entry).context("an installed plugin has no directory")?;
            let service = entry_service(entry).context("no service declared")?;
            return Ok((dir, service));
        }
    }
    anyhow::bail!("no installed plugin `{name}` under {}", plugins_dir.display())
}

/// `find` is `locate` plus the record: a plugin the operator disabled
/// does not start, the same rule a run's door keeps.
fn find(
    plugins_dir: &Path,
    name: &str,
) -> anyhow::Result<(PathBuf, eidolon_rune::plugins::PluginService)> {
    if !eidolon_rune::store::Store::is_enabled(plugins_dir, name) {
        anyhow::bail!(
            "`{name}` is disabled — `plugins enable {name}` first; nothing was run"
        );
    }
    locate(plugins_dir, name)
}

/// `plugins service start`: approve if asked, spawn detached, poll
/// readiness, and on a timeout kill the session leader and show its log —
/// a daemon that never answered is a leak, and the refusal is the only
/// place anyone learns it.
pub fn start(
    plugins_dir: &Path,
    policy_path: &Path,
    services: &Path,
    name: &str,
) -> anyhow::Result<String> {
    let (plugin_dir, service) = find(plugins_dir, name)?;
    let hash = eidolon_rune::permits::service_declaration_hash(&service);
    let permits = eidolon_rune::permits::Permits::from_policy_path(policy_path);
    if !permits.rows().attested(name, &hash) {
        let argv = {
            let command = resolve(&service.command, &plugin_dir)?;
            argv_of(&command, &service.args, &plugin_dir)
        };
        anyhow::bail!(
            "`{name}`'s service is not approved. It would run:\n  {}\nwith output in {}\nApprove once with `plugins service approve {name}` — an update that changes the command asks again.",
            argv.join(" "),
            log_path(services, name).display()
        );
    }
    if let Some(state) = recorded(services, name)
        && cmdline(state.pid).as_deref() == Some(state.argv.as_slice())
    {
        return Ok(format!(
            "{name}: already running as pid {}{}",
            state.pid,
            match &service.readiness {
                Some(r) => format!(", 127.0.0.1:{} answering", r.port),
                None => String::new(),
            }
        ));
    }
    let command = resolve(&service.command, &plugin_dir)?;
    let argv = argv_of(&command, &service.args, &plugin_dir);
    let log = log_path(services, name);
    let pid = spawn_detached(&argv, &plugin_dir, &log)?;
    let pid_file = PidFile {
        pid,
        argv: argv.clone(),
    };
    std::fs::create_dir_all(services).with_context(|| format!("creating {}", services.display()))?;
    std::fs::write(
        pid_path(services, name),
        serde_json::to_string_pretty(&pid_file).context("rendering the pid record")?,
    )
    .with_context(|| format!("writing {}", pid_path(services, name).display()))?;
    match &service.readiness {
        Some(ready) => {
            let budget = Duration::from_secs(service.ready_timeout_s);
            if let Err(e) = await_ready(ready.port, ready.health.as_deref(), budget) {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                let _ = std::fs::remove_file(pid_path(services, name));
                anyhow::bail!("{name}: {e:#}\n{}", log_tail(services, name));
            }
            Ok(format!(
                "{name}: running as pid {pid} — 127.0.0.1:{} answering{} (output in {})",
                ready.port,
                ready
                    .health
                    .as_deref()
                    .map(|h| format!(" at {h}"))
                    .unwrap_or_default(),
                log.display()
            ))
        }
        None => Ok(format!(
            "{name}: spawned as pid {pid} — no readiness declared, `status` reports the record (output in {})",
            log.display()
        )),
    }
}

/// `plugins service stop`: signal the recorded pid only after
/// `/proc/<pid>/cmdline` matches the recorded argv — a file can lie, so
/// the check goes against the process. On any disagreement the refusal
/// names what is there instead of signalling it.
pub fn stop(services: &Path, name: &str) -> anyhow::Result<String> {
    let Some(state) = recorded(services, name) else {
        anyhow::bail!("nothing recorded for `{name}` — never started here, or the record was removed");
    };
    let Some(actual) = cmdline(state.pid) else {
        let _ = std::fs::remove_file(pid_path(services, name));
        return Ok(format!(
            "{name}: pid {} is gone — clearing the stale record",
            state.pid
        ));
    };
    if actual != state.argv {
        anyhow::bail!(
            "refusing to signal pid {}: it now runs `{}`, not the recorded `{}` — the pid was recycled",
            state.pid,
            actual.join(" "),
            state.argv.join(" ")
        );
    }
    unsafe {
        libc::kill(state.pid as i32, libc::SIGTERM);
    }
    let _ = std::fs::remove_file(pid_path(services, name));
    Ok(format!("{name}: stopped (pid {} signalled with SIGTERM)", state.pid))
}

/// `plugins service status`: probe, never trust. The port is connected
/// to; the pid file is not consulted as evidence of anything.
pub fn status(plugins_dir: &Path, services: &Path, name: &str) -> anyhow::Result<String> {
    let (plugin_dir, service) = match locate(plugins_dir, name) {
        Ok(found) => found,
        Err(e) => {
            let text = format!("{e:#}");
            if text.contains("no service declared") {
                return Ok("no service declared".into());
            }
            return Err(e);
        }
    };
    let _ = plugin_dir;
    // A `$PATH` question only exists for a bare program name: a
    // declaration that carries a path says where its program is.
    if !service.command.contains('/') {
        let on_path = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(&service.command).is_file()))
            .unwrap_or(false);
        if !on_path {
            return Ok("command not on $PATH".into());
        }
    }
    match &service.readiness {
        Some(ready) => {
            if probe(ready.port, ready.health.as_deref()) {
                Ok(format!("running (127.0.0.1:{} answering)", ready.port))
            } else {
                Ok(format!("not answering (nothing on 127.0.0.1:{})", ready.port))
            }
        }
        None => {
            // No readiness to probe: the record is the only word there
            // is, and `status` says so rather than guessing — recorded
            // and alive, or nothing.
            match recorded(services, name) {
                Some(state) if alive(state.pid) => {
                    Ok(format!("running as pid {} (no readiness declared)", state.pid))
                }
                _ => Ok("not running (no readiness declared, nothing recorded)".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ephemeral range is a shared resource inside one test binary:
    /// the port tests hold this while they hold listeners, so two
    /// parallel tests cannot take each other's ports. Poison-tolerant on
    /// purpose — a panic in one test says nothing about the next, and a
    /// poisoned lock must not manufacture a second failure.
    fn port_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A scratch plugins directory: one plugin, the manifest the tests
    /// hand in. Never the operator's real home.
    fn plugin(
        root: &Path,
        name: &str,
        service: &str,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.rn"),
            format!("pub fn manifest() {{ #{{ name: \"{name}\", description: \"t\", service: {service} }} }}"),
        )
        .unwrap();
        let services = root.join("services");
        let policy = root.join("policy.rn");
        (dir, services, policy)
    }

    /// Readiness against a port the test itself opened: the probe
    /// answers as soon as the listener is there. The timeout half of the
    /// pair lives in its own test below, on a port nothing can take —
    /// the first writing of this used a just-freed ephemeral port, and
    /// "free" is a claim a parallel test binary on the same machine can
    /// falsify between the drop and the probe.
    #[test]
    fn readiness_passes_on_an_open_port() {
        let _lock = port_lock();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        await_ready(port, None, Duration::from_secs(2)).unwrap();
    }

    /// The timeout half, on port 1: privileged, so nothing this user's
    /// other test binaries bind will ever sit there, and a connect is
    /// refused at once — deterministic across processes, which a
    /// freed-ephemeral port cannot be.
    #[test]
    fn readiness_times_out_on_a_port_nothing_sits_on() {
        let _lock = port_lock();
        assert!(
            await_ready(1, None, Duration::from_millis(250)).is_err(),
            "something answered on 127.0.0.1:1, which was not supposed to be possible"
        );
    }

    /// A health path is probed with HTTP and wants 2xx: a raw listener
    /// that answers the status line passes; one that answers 500 does
    /// not.
    #[test]
    fn health_wants_two_ohh() {
        let _lock = port_lock();
        for code in ["200 OK", "500 Internal Server Error"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let answer = format!("HTTP/1.1 {code}\r\nContent-Length: 0\r\n\r\n");
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut s = stream.unwrap();
                    let mut buf = [0u8; 512];
                    let _ = s.read(&mut buf);
                    let _ = s.write_all(answer.as_bytes());
                }
            });
            let got = await_ready(port, Some("/healthz"), Duration::from_secs(2));
            if code.starts_with('2') {
                got.unwrap();
            } else {
                assert!(got.is_err());
            }
        }
    }

    /// A declaration with no readiness: `start` returns as soon as the
    /// child is spawned — here `/bin/sh -c exit 0`, a real process, no
    /// daemon — and the record is written.
    #[test]
    fn start_without_readiness_returns_once_spawned() {
        let dir = tempfile::tempdir().unwrap();
        let (plugin_dir, services, policy) = plugin(
            dir.path(),
            "shy",
            "#{ command: \"/bin/sh\", args: [\"-c\", \"exit 0\"], ready_timeout_s: 1 }",
        );
        let plugins = plugin_dir.parent().unwrap();
        approve(plugins, &policy, "shy").unwrap();
        let said = start(plugins, &policy, &services, "shy").unwrap();
        assert!(said.contains("spawned"), "{said}");
        assert!(recorded(&services, "shy").is_some());
    }

    /// Unapproved, `start` refuses before anything spawns, naming the
    /// argv and the approving verb; after `approve` it runs; a changed
    /// command stops matching and asks again — an update cannot inherit
    /// the right to run a process.
    #[test]
    fn an_unapproved_service_refuses_then_runs_once_approved() {
        let dir = tempfile::tempdir().unwrap();
        let (plugin_dir, services, policy) = plugin(
            dir.path(),
            "sleeper",
            "#{ command: \"sleep\", args: [\"30\"], ready_timeout_s: 2 }",
        );
        let plugins = plugin_dir.parent().unwrap();
        let err = start(plugins, &policy, &services, "sleeper").unwrap_err().to_string();
        assert!(err.contains("not approved"), "{err}");
        assert!(err.contains("plugins service approve"), "{err}");
        assert!(!recorded(&services, "sleeper").is_some(), "nothing spawned");

        approve(plugins, &policy, "sleeper").unwrap();
        let said = start(plugins, &policy, &services, "sleeper").unwrap();
        assert!(said.contains("spawned"), "{said}");

        // The attestation is by hash: the declaration as the manifest
        // states it is approved, a changed command is not — an update
        // cannot inherit the right to run a process.
        let as_declared = eidolon_rune::plugins::PluginService {
            command: "sleep".into(),
            args: vec!["30".into()],
            readiness: None,
            ready_timeout_s: 2,
        };
        let mut changed = as_declared.clone();
        changed.args[0] = "31".into();
        let permits = eidolon_rune::permits::Permits::from_policy_path(&policy);
        let rows = permits.rows();
        assert!(rows.attested("sleeper", &eidolon_rune::permits::service_declaration_hash(&as_declared)));
        assert!(!rows.attested("sleeper", &eidolon_rune::permits::service_declaration_hash(&changed)));
        stop(&services, "sleeper").unwrap();
    }

    /// A child nobody has reaped holds a `/proc` entry, an empty argv and
    /// the state letter `Z`: all three are read, and the answer is that
    /// this is not a running service. The window this guards — argv
    /// released, letter not yet `Z` — is too narrow to reach from a test;
    /// `cmdline`'s comment says where the sighting came from.
    #[test]
    fn a_dead_child_is_not_a_running_one() {
        let mut child = std::process::Command::new("sleep").arg("0.01").spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(300));
        assert!(cmdline(pid).is_none(), "a zombie is not a running service");
        assert!(!alive(pid));
        let _ = child.wait();
    }

    /// `stop` refuses to signal a pid whose cmdline does not match the
    /// record — here the test's own process, alive and pointed at by a
    /// lying record, which must survive the call untouched.
    #[test]
    fn stop_refuses_a_pid_whose_cmdline_disagrees() {
        let dir = tempfile::tempdir().unwrap();
        let services = dir.path().join("services");
        std::fs::create_dir_all(&services).unwrap();
        let mine = std::process::id();
        std::fs::write(
            pid_path(&services, "liar"),
            serde_json::to_string(&PidFile {
                pid: mine,
                argv: vec!["/definitely/not/me".into()],
            })
            .unwrap(),
        )
        .unwrap();
        let err = stop(&services, "liar").unwrap_err().to_string();
        assert!(err.contains("refusing"), "{err}");
        assert!(err.contains("recycled"), "{err}");
        // And the truth of it: this process is still here to assert.
        assert!(alive(mine));
    }

    /// The whole round trip against a real child: spawn a sleeper the
    /// test starts through `start`, watch `stop` end it, and find the
    /// record cleared. The command is `/bin/sleep` directly — a shell
    /// would `exec` its single command and change the argv out from
    /// under the record, which is exactly what `stop`'s guard exists to
    /// refuse.
    #[test]
    fn start_then_stop_ends_the_recorded_process() {
        let dir = tempfile::tempdir().unwrap();
        let (plugin_dir, services, policy) = plugin(
            dir.path(),
            "sleeper",
            "#{ command: \"sleep\", args: [\"30\"], ready_timeout_s: 2 }",
        );
        let plugins = plugin_dir.parent().unwrap();
        approve(plugins, &policy, "sleeper").unwrap();
        start(plugins, &policy, &services, "sleeper").unwrap();
        let state = recorded(&services, "sleeper").unwrap();
        // `spawn` returns before the child has exec'd, and until it does the
        // kernel gives the new pid no argv at all: `cmdline` reads None, so
        // `alive` answers false and `stop` would take the fresh record for a
        // stale one — clearing it and signalling nothing while the sleeper is
        // still on its way up. Wait for the state the rest of this test acts
        // on — the recorded pid running the recorded argv, the same predicate
        // `stop` itself checks — rather than assuming it; under full-suite
        // load the window is wide enough to lose.
        let deadline = Instant::now() + Duration::from_secs(5);
        while cmdline(state.pid).as_deref() != Some(state.argv.as_slice()) {
            assert!(
                Instant::now() < deadline,
                "pid {} never came up as `{}` — the pre-exec window this test guards",
                state.pid,
                state.argv.join(" ")
            );
            std::thread::sleep(POLL);
        }
        assert!(alive(state.pid));
        stop(&services, "sleeper").unwrap();
        // SIGTERM takes a beat to reap; poll rather than sleep a fixed
        // guess, the way the readiness probe does.
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(state.pid) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        assert!(!alive(state.pid));
        assert!(recorded(&services, "sleeper").is_none());
    }
}
