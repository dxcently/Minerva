//! `minerva`: the Linux command. Installed at `~/.local/bin/minerva` (-> `current`). It installs bundles, updates and
//! rolls back (signed), uninstalls, manages the keys it trusts, keeps the bundled plugins current, and gives Linux the
//! same verbs `hoot` gives Windows.

#[cfg(not(unix))]
fn main() {
    eprintln!("minerva: this is the Linux command; on Windows use minerva-install.exe and hoot");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() {
    std::process::exit(app::run());
}

#[cfg(unix)]
mod app {
    use std::io::{BufRead, IsTerminal, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use anyhow::{Context, Result, bail};
    use minerva_installer::install::{self, Layout};
    use minerva_installer::keys::{self, Source, Trust};
    use minerva_installer::plugins::{self, PluginEnv, SyncOpts};
    use minerva_installer::{DEFAULT_BASE, FULL_VERSION, jev, notice, update, verify};

    const HELP: &str = "minerva: the Minerva command

usage: minerva <command> [options]
  web [--no-open]        start (or reuse) the Minerva server and open it signed in (prints the link when it cannot open one)
  status                 is the server up, and its chats
  stop                   stop the server (chats are kept)
  init                   the setup wizard (home, default model, project roots)
  update                 install the newest signed release (the previous one is kept)
  update --check         only say whether a newer release exists
  update --rollback      flip back to the previous version (again: forward)
  version                the installed version
  uninstall              remove the installed program and the plugins it installed (never ~/Minerva: your chats and settings)
  install --from DIR     lay an unpacked bundle down (what install.sh and minerva-install.exe call)
  plugins sync|status    install/refresh the bundled plugins (browser, subagent, jev) and trust them
  services start|stop|status   the browser service (and the jev service, once set up)
  jev-setup [--dry-run]  the optional jev graph chooser: a Python venv with CPU torch (about 1.2 GB); asks first
  keys list|inspect|add|remove  who may sign releases (see: minerva keys help)
options for install/uninstall/update: --prefix DIR (default ~/.local)
for tests: update --base URL (https://, http://127.0.0.1:..., file:///dir); MINERVA_NO_UPDATE_CHECK=1 silences the daily notice
";

    const KEYS_HELP: &str = "minerva keys: who may sign releases

  keys list [--json]               every key: id, fingerprint, state (trusted/revoked/expired), where it came from
  keys inspect <pubfile|-> [--json]   show the id and fingerprint of a public key file (- = stdin); changes nothing
  keys add <pubfile|-> [--confirm FP]   trust one more key. Shows the fingerprint; you type it back to confirm
  keys remove <id> [--confirm FP]       stop trusting a key you added (the compiled-in key cannot be removed)

The trusted set is the key compiled into this program plus the .pub files in ~/.config/minerva/update-keys (0700/0600),
minus revoked keys. A release may carry a signed keys.json that adds and revokes keys; `minerva update` applies it.
--confirm FP is for programs: the person typed the fingerprint (16 hex digits, spaces optional), the program passes it on.
";

    pub fn run() -> i32 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match dispatch(&args) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("minerva: {e:#}");
                1
            }
        }
    }

    /// `--name value` out of `args` (removed); `--flag` presence via `take_flag`.
    fn take_opt(args: &mut Vec<String>, name: &str) -> Result<Option<String>> {
        if let Some(i) = args.iter().position(|a| a == name) {
            if i + 1 >= args.len() {
                bail!("{name} needs a value");
            }
            let v = args.remove(i + 1);
            args.remove(i);
            return Ok(Some(v));
        }
        Ok(None)
    }

    fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
        match args.iter().position(|a| a == name) {
            Some(i) => {
                args.remove(i);
                true
            }
            None => false,
        }
    }

    fn no_more(rest: &[String]) -> Result<()> {
        if !rest.is_empty() {
            bail!("unexpected argument {:?} (try: minerva help)", rest[0]);
        }
        Ok(())
    }

    fn now_secs() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }

    fn trust() -> Result<Trust> {
        Ok(Trust::new(verify::PUBKEY, Trust::default_dir()?))
    }

    fn dispatch(argv: &[String]) -> Result<i32> {
        let Some((cmd, rest)) = argv.split_first() else {
            print!("{HELP}");
            return Ok(0);
        };
        let mut rest = rest.to_vec();
        match cmd.as_str() {
            "-h" | "--help" | "help" => {
                print!("{HELP}");
                Ok(0)
            }
            "version" | "--version" => {
                println!("{}", installed_version());
                Ok(0)
            }
            "install" => cmd_install(&mut rest),
            "uninstall" => cmd_uninstall(&mut rest),
            "update" => cmd_update(&mut rest),
            "check-update" => {
                // Quiet, at most once a day, notice only. `--as hoot` says which command to run.
                let who = take_opt(&mut rest, "--as")?.unwrap_or_else(|| "minerva".into());
                let base = take_opt(&mut rest, "--base")?.unwrap_or_else(|| DEFAULT_BASE.to_string());
                if opted_out() {
                    return Ok(0);
                }
                if let Some(line) = notice_line(&base, &who) {
                    println!("{line}");
                }
                Ok(0)
            }
            "plugins" => cmd_plugins(&mut rest),
            "services" => cmd_services(&mut rest),
            "jev-setup" => cmd_jev_setup(&mut rest),
            "keys" => cmd_keys(&mut rest),
            "web" | "status" | "stop" | "init" => verb(cmd, &rest),
            other => bail!("unknown command {other:?} (try: minerva help)"),
        }
    }

    // ---- install / uninstall / update ---------------------------------------------------------------------------------

    fn prefix_arg(rest: &mut Vec<String>) -> Result<PathBuf> {
        match take_opt(rest, "--prefix")? {
            Some(p) => Ok(PathBuf::from(p)),
            None => match std::env::current_exe().ok().and_then(|e| Layout::from_exe(&e).ok()) {
                Some(l) => Ok(l.prefix),
                None => Layout::default_prefix(),
            },
        }
    }

    fn plugin_env(layout: &Layout) -> PluginEnv {
        let cur = layout.current();
        PluginEnv { eidolon: cur.join("bin/eidolon"), bin_dir: cur.join("bin"), src: cur.join("plugins"), layout: layout.clone() }
    }

    fn ask_yes_no(question: &str) -> bool {
        if !std::io::stdin().is_terminal() {
            return false;
        }
        eprint!("{question} [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).is_ok() && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    /// Sync the bundled plugins from `layout`'s current version. Never fails the caller: says what went wrong.
    fn sync_plugins(layout: &Layout, version: &str, if_needed: bool, quiet: bool, interactive: bool) {
        sync_plugins_env(&plugin_env(layout), version, if_needed, quiet, interactive)
    }

    fn sync_plugins_env(env: &PluginEnv, version: &str, if_needed: bool, quiet: bool, interactive: bool) {
        if !env.src.is_dir() {
            return;
        }
        let mut ask = |name: &str| ask_yes_no(&format!("You already have a plugin called {name} of your own. Replace it with Minerva's?"));
        let ask_ref: Option<&mut dyn FnMut(&str) -> bool> = if interactive { Some(&mut ask) } else { None };
        match plugins::sync(env, SyncOpts { version, if_needed, ask_replace: ask_ref }) {
            Ok(lines) => {
                if !quiet {
                    for l in lines {
                        println!("{l}");
                    }
                }
            }
            Err(e) => eprintln!("minerva: plugin setup did not finish: {e:#} (run: minerva plugins sync)"),
        }
    }

    fn cmd_install(rest: &mut Vec<String>) -> Result<i32> {
        let from = take_opt(rest, "--from")?.context("install needs --from DIR (an unpacked bundle)")?;
        let prefix = prefix_arg(rest)?;
        let jev_choice = take_opt(rest, "--jev")?.unwrap_or_else(|| "ask".into());
        no_more(rest)?;
        if !matches!(jev_choice.as_str(), "ask" | "yes" | "no") {
            bail!("--jev takes ask, yes or no");
        }
        let layout = Layout::new(prefix);
        if install::is_nix_managed(&layout.prefix) {
            bail!("{} is inside the Nix store", layout.prefix.display());
        }
        let was = layout.current_version();
        let done = install::install_from_dir(Path::new(&from), &layout)?;
        match (&was, done.fresh) {
            (_, false) => println!("already installed: {} (nothing to unpack; `current` points at it)", done.full),
            (None, true) => println!("installed {} in {}", done.full, layout.version_dir(&done.full).display()),
            (Some(w), true) => println!("upgraded {w} -> {} (previous kept: `minerva update --rollback`)", done.full),
        }
        println!("command: {} (put {} on your PATH if it is not)", layout.bin_link().display(), layout.prefix.join("bin").display());
        println!("your chats and settings live in ~/Minerva and are never touched by install, update or uninstall");
        if let Some(w) = install::glibc_warning(&host_glibc_text()) {
            eprintln!("warning: {w}");
        }
        sync_plugins(&layout, &done.full, false, false, std::io::stdin().is_terminal());
        offer_jev(&layout, &jev_choice);
        Ok(0)
    }

    /// What `ldd --version` says (the host's C library; minerva itself is static and does not care).
    fn host_glibc_text() -> String {
        Command::new("ldd").arg("--version").output().map(|o| format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))).unwrap_or_default()
    }

    fn offer_jev(layout: &Layout, choice: &str) {
        if jev::is_set_up(layout) {
            println!("jev: already set up (minerva jev-setup refreshes it)");
            return;
        }
        let yes = match choice {
            "yes" => true,
            "no" => false,
            _ => {
                if !std::io::stdin().is_terminal() {
                    return;
                }
                eprintln!("{}", jev::OFFER);
                ask_yes_no("Set it up now?")
            }
        };
        if yes {
            if let Err(e) = run_jev_setup(layout, false) {
                eprintln!("minerva: jev setup did not finish: {e:#}");
            }
        } else if choice == "no" {
            println!("jev graph chooser: skipped (minerva jev-setup, later)");
        }
    }

    fn run_jev_setup(layout: &Layout, dry_run: bool) -> Result<()> {
        let env = plugin_env(layout);
        let lines = jev::setup(&env, &layout.current().join("jev"), &jev::SetupOpts { python: "python3", dry_run })?;
        for l in lines {
            println!("{l}");
        }
        Ok(())
    }

    fn cmd_uninstall(rest: &mut Vec<String>) -> Result<i32> {
        let prefix = prefix_arg(rest)?;
        no_more(rest)?;
        let layout = Layout::new(prefix);
        // What we installed into the member's eidolon config goes first, while the eidolon that can remove it is still here.
        if layout.current().join("bin/eidolon").exists() {
            let _ = jev::stop(&layout);
            match plugins::remove_ours(&plugin_env(&layout)) {
                Ok(lines) => lines.iter().for_each(|l| println!("plugin {l}")),
                Err(e) => eprintln!("minerva: could not remove the plugins: {e:#}"),
            }
        }
        let gone = install::uninstall(&layout)?;
        if gone.is_empty() {
            println!("nothing of Minerva was installed there");
        }
        for g in gone {
            println!("removed {}", g.display());
        }
        let home = std::env::var("MINERVA_HOME").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "~/Minerva".to_string());
        println!("kept {home} (your chats and settings); delete it yourself if you want it gone");
        println!("kept ~/.config/minerva/update-keys and ~/.config/eidolon (yours); plugins of your own were never touched");
        Ok(0)
    }

    fn cmd_update(rest: &mut Vec<String>) -> Result<i32> {
        let base = take_opt(rest, "--base")?.unwrap_or_else(|| DEFAULT_BASE.to_string());
        let rollback = take_flag(rest, "--rollback");
        let check_only = take_flag(rest, "--check");
        // For the tests only (cargo test, or a build with --features test-overrides): pretend to be this path, to see the
        // Nix refusal. A release build has no such flag.
        #[cfg(any(test, feature = "test-overrides"))]
        let fake_self = take_opt(rest, "--self-path")?;
        #[cfg(not(any(test, feature = "test-overrides")))]
        let fake_self: Option<String> = None;
        let exe = match fake_self {
            Some(p) => PathBuf::from(p),
            None => std::env::current_exe().context("finding myself")?,
        };
        no_more(rest)?;
        let cur = installed_version_of(&exe);
        let tr = trust()?;
        let mut out = std::io::stdout();
        match update::run(&update::Opts { base: &base, trust: &tr, now: now_secs(), current: &cur, exe: &exe, rollback, check_only }, &mut out) {
            Ok(outcome) => {
                if let update::Outcome::Updated { .. } | update::Outcome::RolledBack { .. } = outcome {
                    if let Ok(layout) = Layout::from_exe(&exe) {
                        let v = layout.current_version().unwrap_or_default();
                        sync_plugins(&layout, &v, false, false, false);
                        if jev::is_set_up(&layout) {
                            println!("jev is set up; its pinned requirements may have changed: minerva jev-setup refreshes it");
                        }
                    }
                }
                Ok(0)
            }
            Err(e) => {
                eprintln!("minerva: update refused: {e:#}");
                Ok(1)
            }
        }
    }

    fn installed_layout() -> Result<Layout> {
        Layout::from_exe(&std::env::current_exe()?)
    }

    /// Where the plugins come from and where the record is kept. An installed copy knows its own layout; a Nix install
    /// (nix/minerva.sh) has no `opt/minerva/current`, so it names the pieces: `--eidolon BIN --src DIR --bin-dir DIR
    /// --prefix DIR [--version V]` (prefix = where `state/minerva/plugins.json` lives).
    fn plugin_target(rest: &mut Vec<String>) -> Result<(PluginEnv, String)> {
        match take_opt(rest, "--eidolon")? {
            Some(eid) => {
                let src = take_opt(rest, "--src")?.context("--eidolon needs --src DIR (the plugins)")?;
                let prefix = take_opt(rest, "--prefix")?.context("--eidolon needs --prefix DIR (where the record is kept)")?;
                let bin_dir = take_opt(rest, "--bin-dir")?.unwrap_or_default();
                let version = take_opt(rest, "--version")?.unwrap_or_else(|| FULL_VERSION.to_string());
                let env = PluginEnv { eidolon: PathBuf::from(eid), bin_dir: PathBuf::from(bin_dir), src: PathBuf::from(src), layout: Layout::new(PathBuf::from(prefix)) };
                Ok((env, version))
            }
            None => {
                let layout = installed_layout()?;
                let v = layout.current_version().context("nothing is installed")?;
                Ok((plugin_env(&layout), v))
            }
        }
    }

    fn cmd_plugins(rest: &mut Vec<String>) -> Result<i32> {
        let sub = if rest.is_empty() { "status".to_string() } else { rest.remove(0) };
        match sub.as_str() {
            "sync" => {
                let if_needed = take_flag(rest, "--if-needed");
                let quiet = take_flag(rest, "--quiet");
                let (env, v) = plugin_target(rest)?;
                no_more(rest)?;
                sync_plugins_env(&env, &v, if_needed, quiet, !quiet && std::io::stdin().is_terminal());
                Ok(0)
            }
            "status" => {
                let (env, _) = plugin_target(rest)?;
                let layout = env.layout;
                no_more(rest)?;
                let st = plugins::load_state(&layout);
                println!("plugins dir: {}", plugins::plugins_dir()?.display());
                if st.plugins.is_empty() {
                    println!("none installed by Minerva (minerva plugins sync)");
                }
                for (n, o) in &st.plugins {
                    println!("{n} {} ({}), installed by Minerva", o.version, o.how);
                }
                Ok(0)
            }
            _ => bail!("usage: minerva plugins sync [--if-needed] [--quiet] | status"),
        }
    }

    fn cmd_services(rest: &mut Vec<String>) -> Result<i32> {
        let op = if rest.is_empty() { "status".to_string() } else { rest.remove(0) };
        let quiet = take_flag(rest, "--quiet");
        let nix_style = rest.iter().any(|a| a == "--eidolon");
        let (env, _) = plugin_target(rest)?;
        no_more(rest)?;
        if !matches!(op.as_str(), "start" | "stop" | "status") {
            bail!("usage: minerva services start|stop|status");
        }
        let layout = env.layout.clone();
        let mut code = 0;
        match plugins::service(&env, &op) {
            Ok(l) => {
                if !quiet {
                    println!("{l}")
                }
            }
            Err(e) => {
                if !quiet {
                    eprintln!("minerva: {e:#}");
                }
                code = 1;
            }
        }
        if nix_style {
            return Ok(code); // a Nix install has no jev venv; its jev service is a separate slice
        }
        let jl = match op.as_str() {
            "start" => jev::start(&layout, &layout.current().join("jev")),
            "stop" => jev::stop(&layout),
            _ => Ok(jev::status(&layout)),
        };
        match jl {
            Ok(l) => {
                if !quiet {
                    println!("{l}")
                }
            }
            Err(e) => {
                if !quiet {
                    eprintln!("minerva: {e:#}")
                }
            }
        }
        Ok(code)
    }

    fn cmd_jev_setup(rest: &mut Vec<String>) -> Result<i32> {
        let dry = take_flag(rest, "--dry-run");
        let yes = take_flag(rest, "--yes");
        no_more(rest)?;
        let layout = installed_layout()?;
        if !dry && !yes {
            if !std::io::stdin().is_terminal() {
                bail!("jev-setup asks first and there is no terminal; pass --yes to go ahead, or --dry-run to see the plan");
            }
            eprintln!("{}", jev::OFFER);
            if !ask_yes_no("Set it up now?") {
                println!("nothing changed");
                return Ok(0);
            }
        }
        run_jev_setup(&layout, dry)?;
        Ok(0)
    }

    // ---- keys ---------------------------------------------------------------------------------------------------------

    fn read_pub_arg(arg: &str) -> Result<String> {
        if arg == "-" {
            let mut s = String::new();
            std::io::stdin().take(64 * 1024).read_to_string(&mut s).context("reading the key from stdin")?;
            Ok(s)
        } else {
            let meta = std::fs::metadata(arg).with_context(|| format!("reading {arg}"))?;
            if meta.len() > 64 * 1024 {
                bail!("{arg} is not a public key file (too large)");
            }
            std::fs::read_to_string(arg).with_context(|| format!("reading {arg}"))
        }
    }

    /// The fingerprint as typed: lower case, no spaces.
    fn norm_fp(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase()
    }

    /// The typed confirmation. It is the key's FINGERPRINT (16 hex digits of the sha-256 of the key bytes), not its id: the
    /// id is a label anyone can copy onto a different key. `--confirm FP` comes from a program whose person typed it.
    fn confirm_fp(expected: &str, given: Option<String>, what: &str) -> Result<()> {
        if let Some(g) = given {
            if norm_fp(&g) == norm_fp(expected) {
                return Ok(());
            }
            bail!("the confirmation does not match the key's fingerprint; nothing changed");
        }
        let tty = std::fs::File::open("/dev/tty").ok();
        let Some(tty) = tty else { bail!("{what} needs a typed confirmation and there is no terminal: pass --confirm \"{expected}\" (after the person has checked the fingerprint)") };
        eprint!("Type the fingerprint ({expected}) to confirm: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::BufReader::new(tty).read_line(&mut line)?;
        if norm_fp(&line) == norm_fp(expected) {
            Ok(())
        } else {
            bail!("that is not the fingerprint; nothing changed")
        }
    }

    fn json_str(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
    }

    fn cmd_keys(rest: &mut Vec<String>) -> Result<i32> {
        let sub = if rest.is_empty() { "list".to_string() } else { rest.remove(0) };
        let json = take_flag(rest, "--json");
        let confirm = take_opt(rest, "--confirm")?;
        let tr = trust()?;
        let today = keys::today();
        match sub.as_str() {
            "help" | "-h" | "--help" => {
                print!("{KEYS_HELP}");
                Ok(0)
            }
            "list" => {
                no_more(rest)?;
                let trusted: Vec<String> = tr.trusted(&today)?.into_iter().map(|k| k.id).collect();
                let st = tr.state()?;
                let mut rows = Vec::new();
                for k in tr.all() {
                    let state = if st.revoked.iter().any(|r| r.id.eq_ignore_ascii_case(&k.id)) {
                        "revoked"
                    } else if trusted.contains(&k.id) {
                        "trusted"
                    } else {
                        "expired"
                    };
                    rows.push((k, state));
                }
                if json {
                    let items: Vec<String> = rows
                        .iter()
                        .map(|(k, s)| {
                            format!(
                                r#"{{"id":{},"fingerprint":{},"state":{},"source":{},"expires":{}}}"#,
                                json_str(&k.id),
                                json_str(&k.fingerprint),
                                json_str(s),
                                json_str(k.source.label()),
                                k.expires.as_deref().map(json_str).unwrap_or_else(|| "null".into())
                            )
                        })
                        .collect();
                    println!("[{}]", items.join(","));
                } else {
                    for (k, s) in &rows {
                        println!("{}  {}  {:8}  {}{}", k.id, k.fingerprint, s, k.source.label(), k.expires.as_ref().map(|e| format!(" (expires {e})")).unwrap_or_default());
                    }
                    for r in st.revoked.iter().filter(|r| !rows.iter().any(|(k, _)| k.id == r.id)) {
                        println!("{}  {:39}  revoked   revoked on {}", r.id, "", r.on);
                    }
                    if rows.is_empty() {
                        println!("no keys (this build has the placeholder key)");
                    }
                }
                Ok(0)
            }
            "inspect" | "add" => {
                let arg = rest.first().cloned().context("name a public key file, or - for stdin")?;
                rest.remove(0);
                no_more(rest)?;
                let text = read_pub_arg(&arg)?;
                let k = keys::parse_pub(&text, Source::Manual)?;
                if sub == "inspect" {
                    if json {
                        println!(r#"{{"id":{},"fingerprint":{},"comment":{}}}"#, json_str(&k.id), json_str(&k.fingerprint), json_str(&k.comment));
                    } else {
                        println!("key id       {}\nfingerprint  {}\ncomment      {}", k.id, k.fingerprint, k.comment);
                    }
                    return Ok(0);
                }
                eprintln!("Trusting a key lets whoever holds its secret half install software on this machine through `update`.\nCompare the fingerprint with the one the maintainer published, not just the id.");
                eprintln!("key id       {}\nfingerprint  {}\ncomment      {}", k.id, k.fingerprint, k.comment);
                confirm_fp(&k.fingerprint, confirm, "adding a key")?;
                tr.add_manual(&text)?;
                println!("added {}", k.id);
                Ok(0)
            }
            "remove" => {
                let id = rest.first().cloned().context("name a key id (minerva keys list)")?;
                rest.remove(0);
                no_more(rest)?;
                let k = tr.all().into_iter().find(|k| k.id.eq_ignore_ascii_case(&id)).with_context(|| format!("no key {id} (minerva keys list)"))?;
                eprintln!("key id       {}\nfingerprint  {}\nsource       {}", k.id, k.fingerprint, k.source.label());
                confirm_fp(&k.fingerprint, confirm, "removing a key")?;
                tr.remove(&k.id)?;
                println!("removed {}", k.id);
                Ok(0)
            }
            _ => bail!("usage: minerva keys list|inspect|add|remove (minerva keys help)"),
        }
    }

    // ---- versions, notice, the verbs ----------------------------------------------------------------------------------

    fn version_dir_of(exe: &Path) -> Option<PathBuf> {
        exe.canonicalize().ok()?.parent()?.parent().map(Path::to_path_buf)
    }

    fn installed_version_of(exe: &Path) -> String {
        version_dir_of(exe)
            .and_then(|d| std::fs::read_to_string(d.join("VERSION")).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| FULL_VERSION.to_string())
    }

    fn installed_version() -> String {
        std::env::current_exe().map(|e| installed_version_of(&e)).unwrap_or_else(|_| FULL_VERSION.to_string())
    }

    fn opted_out() -> bool {
        std::env::var("MINERVA_NO_UPDATE_CHECK").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
    }

    fn notice_line(base: &str, who: &str) -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let layout = Layout::from_exe(&exe).ok()?; // not an installed copy (cargo run): no check
        if install::is_nix_managed(&exe) {
            return None;
        }
        let tr = trust().ok()?;
        notice::check(&layout, base, &tr, &installed_version_of(&exe), who, now_secs())
    }

    /// web / status / stop / init: the same scripts hoot runs, from this version's own bin/.
    fn verb(cmd: &str, rest: &[String]) -> Result<i32> {
        let exe = std::env::current_exe()?.canonicalize()?;
        let bin = exe.parent().context("no bin dir")?.to_path_buf();
        let in_wsl = std::fs::read_to_string("/proc/version").map(|s| s.to_ascii_lowercase().contains("microsoft")).unwrap_or(false);
        let perch_open = |flags: &[&str]| -> Result<std::process::ExitStatus> {
            Command::new("bash").arg(bin.join("perch-open.sh")).args(flags).status().context("running perch-open.sh")
        };
        // The bundled programs first on PATH for the scripts and the doors they start.
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        // set before any other thread exists
        unsafe { std::env::set_var("PATH", path) };
        let code = match cmd {
            "status" => perch_open(&["--status"])?.code().unwrap_or(1),
            "stop" => perch_open(&["--stop"])?.code().unwrap_or(1),
            "init" => Command::new(bin.join("minerva-setup")).status().context("running minerva-setup")?.code().unwrap_or(1),
            _ => {
                let home = std::env::var("MINERVA_HOME")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{}/Minerva", std::env::var("HOME").unwrap_or_default()));
                if !Path::new(&home).join("minerva.toml").exists() {
                    eprintln!("first run: a few questions first (minerva init asks them again any time)");
                    let st = Command::new(bin.join("minerva-setup")).status().context("running minerva-setup")?;
                    if !st.success() {
                        bail!("the setup wizard did not finish; run: minerva init");
                    }
                }
                let no_open = rest.iter().any(|a| a == "--no-open");
                if in_wsl && !no_open {
                    perch_open(&[])?.code().unwrap_or(1)
                } else {
                    let o = Command::new("bash").arg(bin.join("perch-open.sh")).arg("--code").output().context("running perch-open.sh")?;
                    eprint!("{}", String::from_utf8_lossy(&o.stderr));
                    if !o.status.success() {
                        return Ok(o.status.code().unwrap_or(1));
                    }
                    let link = String::from_utf8_lossy(&o.stdout).lines().last().unwrap_or("").trim().to_string();
                    if !no_open && Command::new("xdg-open").arg(&link).status().map(|s| s.success()).unwrap_or(false) {
                        println!("opened Minerva in your browser");
                    } else {
                        println!("{link}");
                    }
                    0
                }
            }
        };
        if matches!(cmd, "web" | "status") && !opted_out() {
            if let Some(line) = notice_line(DEFAULT_BASE, "minerva") {
                eprintln!("{line}");
            }
        }
        Ok(code)
    }
}
