//! `minerva-install.exe`: the Windows installer. It carries the same Linux bundle `install.sh` and `minerva update`
//! use, hands it to WSL, and adds the Windows side (hoot, PATH, Start menu, an uninstall entry).
//!
//! No WSL: it explains, OFFERS `wsl --install` (admin + a reboot), and never runs it without a yes.
//! It asks for no secret and never touches the Minerva home (`~/Minerva`).

#[cfg(not(windows))]
fn main() {
    eprintln!("minerva-install: this is the Windows installer. On Linux or inside WSL use install.sh from the release.");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    std::process::exit(app::run());
}

#[cfg(windows)]
mod app {
    use std::collections::HashMap;
    use std::io::{BufRead, IsTerminal, Read, Write};
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use anyhow::{Context, Result, bail};
    use minerva_installer::wslparse::{self, Distro};
    use minerva_installer::{FULL_VERSION, pathlist};
    use winreg::RegKey;
    use winreg::RegValue;
    use winreg::enums::*;

    static BUNDLE: &[u8] = include_bytes!(env!("MINERVA_EMBED_BUNDLE"));

    const HELP: &str = "minerva-install: install Minerva (club tool) on this PC

usage: minerva-install [options]
  (none)         install or upgrade: needs WSL (it offers to set that up), then everything else
  --uninstall    remove what this installed (the Minerva home ~/Minerva with your chats is kept)
  --distro NAME  use this WSL distribution instead of asking
  --yes          answer yes to the questions that are safe to default (never `wsl --install`)
  --plain        plain text prompts (if the fancy ones misbehave in this console)
options for testing (they keep the installer away from the real PC settings):
  --win-root DIR       Windows install folder (default %LOCALAPPDATA%\\Minerva)
  --wsl-prefix DIR     Linux prefix inside WSL (default ~/.local; the program goes in DIR/opt/minerva)
  --start-menu DIR     where the Start menu shortcut goes (default the user's Programs folder)
  --registry-root KEY  HKCU key to use instead of the real Environment and Uninstall keys, e.g. Software\\MinervaInstallTest
  --no-registry        skip the PATH and uninstall-list entries
  --no-shortcut        skip the Start menu shortcut
  --skip-wizard        do not run the setup wizard
  --jev yes|no         the optional jev graph chooser (about 1.2 GB): default is to ask, and the answer is no without a terminal;
                       --yes never says yes to it
  --fake-wsl-missing   pretend WSL is not installed (never runs anything)
";

    #[derive(Default)]
    struct Args {
        uninstall: bool,
        distro: Option<String>,
        yes: bool,
        plain: bool,
        win_root: Option<PathBuf>,
        wsl_prefix: Option<String>,
        start_menu: Option<PathBuf>,
        registry_root: Option<String>,
        no_registry: bool,
        no_shortcut: bool,
        skip_wizard: bool,
        jev: Option<String>,
        fake_wsl_missing: bool,
    }

    fn parse_args() -> Result<Option<Args>> {
        let mut a = Args::default();
        let mut it = std::env::args().skip(1);
        while let Some(x) = it.next() {
            let mut val = |name: &str| it.next().with_context(|| format!("{name} needs a value"));
            match x.as_str() {
                "-h" | "--help" => {
                    print!("{HELP}");
                    return Ok(None);
                }
                "--uninstall" => a.uninstall = true,
                "--yes" => a.yes = true,
                "--plain" => a.plain = true,
                "--no-registry" => a.no_registry = true,
                "--no-shortcut" => a.no_shortcut = true,
                "--skip-wizard" => a.skip_wizard = true,
                "--fake-wsl-missing" => a.fake_wsl_missing = true,
                "--jev" => {
                    let v = val("--jev")?;
                    if v != "yes" && v != "no" {
                        bail!("--jev takes yes or no");
                    }
                    a.jev = Some(v);
                }
                "--distro" => a.distro = Some(val("--distro")?),
                "--win-root" => a.win_root = Some(val("--win-root")?.into()),
                "--wsl-prefix" => a.wsl_prefix = Some(val("--wsl-prefix")?),
                "--start-menu" => a.start_menu = Some(val("--start-menu")?.into()),
                "--registry-root" => a.registry_root = Some(val("--registry-root")?),
                other => bail!("unknown option {other:?} (try --help)"),
            }
        }
        Ok(Some(a))
    }

    pub fn run() -> i32 {
        harden_dll_search();
        match parse_args().and_then(|a| match a {
            None => Ok(0),
            Some(a) if a.uninstall => uninstall(&a),
            Some(a) => install(&a),
        }) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("\n  xx minerva-install: {e:#}");
                1
            }
        }
    }

    // ---- prompts -----------------------------------------------------------------------------------------------

    fn fancy(a: &Args) -> bool {
        !a.plain && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    fn confirm(a: &Args, q: &str, default: bool) -> Result<bool> {
        if fancy(a) {
            return cliclack::confirm(q).initial_value(default).interact().context("the prompt was cancelled");
        }
        print!("  ? {q} [{}] ", if default { "Y/n" } else { "y/N" });
        std::io::stdout().flush()?;
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line)? == 0 {
            println!();
            return Ok(false); // no input means no
        }
        Ok(match line.trim().to_ascii_lowercase().as_str() {
            "" => default,
            "y" | "yes" => true,
            _ => false,
        })
    }

    fn select(a: &Args, q: &str, items: &[String]) -> Result<usize> {
        if fancy(a) {
            let mut s = cliclack::select(q);
            for (i, it) in items.iter().enumerate() {
                s = s.item(i, it, "");
            }
            return s.interact().context("the prompt was cancelled");
        }
        println!("  ? {q}");
        for (i, it) in items.iter().enumerate() {
            println!("      {}) {it}", i + 1);
        }
        print!("  number: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let n: usize = line.trim().parse().context("not a number")?;
        if n == 0 || n > items.len() {
            bail!("pick 1..{}", items.len());
        }
        Ok(n - 1)
    }

    fn say(m: &str) {
        println!("  >> {m}");
    }
    fn ok(m: &str) {
        println!("  ok {m}");
    }

    // ---- WSL ---------------------------------------------------------------------------------------------------

    /// A program that ships with Windows, by absolute path under `%SystemRoot%` (fallback `C:\Windows`). A bare name would be
    /// looked up in the current directory and on PATH first: a planted `wsl.exe` next to a downloaded installer would run.
    fn system_exe(rel: &str) -> PathBuf {
        let root = ["SystemRoot", "windir"]
            .iter()
            .filter_map(|n| std::env::var_os(n))
            .map(PathBuf::from)
            .find(|p| p.is_absolute() && p.is_dir())
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        root.join(rel)
    }

    fn wsl_exe() -> PathBuf {
        system_exe(r"System32\wsl.exe")
    }

    fn cmd_exe() -> PathBuf {
        system_exe(r"System32\cmd.exe")
    }

    fn powershell_exe() -> PathBuf {
        system_exe(r"System32\WindowsPowerShell\v1.0\powershell.exe")
    }

    /// Only the system directory for any DLL loaded from now on (a no-op on failure: older Windows lacks the call).
    fn harden_dll_search() {
        use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
        unsafe {
            SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
        }
    }

    fn wsl(distro: Option<&str>) -> Command {
        let mut c = Command::new(wsl_exe());
        if let Some(d) = distro {
            c.args(["-d", d]);
        }
        c
    }

    fn wsl_out(distro: &str, argv: &[&str]) -> Result<String> {
        let o = wsl(Some(distro)).arg("-e").args(argv).output().context("running wsl.exe")?;
        if !o.status.success() {
            bail!("wsl {} failed: {}", argv.join(" "), String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    enum Probe {
        Missing(String),
        Distros(Vec<Distro>),
    }

    fn probe(a: &Args) -> Probe {
        if a.fake_wsl_missing {
            return Probe::Missing("(simulated for a test)".into());
        }
        match wsl(None).args(["-l", "-v"]).output() {
            Err(e) => Probe::Missing(format!("wsl.exe could not run: {e}")),
            Ok(o) => {
                let ds = wslparse::parse_list_verbose(&wslparse::decode(&o.stdout));
                if ds.is_empty() {
                    Probe::Missing("WSL answers, but no Linux distribution is installed".into())
                } else {
                    Probe::Distros(ds)
                }
            }
        }
    }

    /// Explain, and offer to run `wsl --install -d Ubuntu`. Returns the process exit code.
    fn offer_wsl(a: &Args, why: &str) -> Result<i32> {
        println!();
        println!("  Minerva runs its programs inside WSL (Windows Subsystem for Linux), and I cannot find a usable one:");
        println!("    {why}");
        println!();
        println!("  `wsl --install -d Ubuntu` sets it up. What you should know first:");
        println!("    - it needs administrator rights (Windows will ask), it downloads Ubuntu (a few hundred MB),");
        println!("    - Windows will ask for a RESTART, and Ubuntu asks you to make a username and password on first start,");
        println!("    - after that, run this installer again; nothing is lost by stopping now.");
        println!();
        if !confirm(a, "Run `wsl --install -d Ubuntu` now?", false)? {
            println!("  Nothing was changed. When you are ready: open an administrator PowerShell, run");
            println!("      wsl --install -d Ubuntu");
            println!("  restart, finish the Ubuntu first-start questions, then run minerva-install.exe again.");
            return Ok(1);
        }
        if a.fake_wsl_missing {
            println!("  (test mode: I would run `wsl.exe --install -d Ubuntu` here; I did not)");
            return Ok(1);
        }
        let st = Command::new(wsl_exe()).args(["--install", "-d", "Ubuntu"]).status().context("running wsl --install")?;
        println!();
        if st.success() {
            println!("  Done. Restart Windows if it asks, open Ubuntu once to create your user, then run this installer again.");
        } else {
            println!("  wsl --install did not finish (exit {}). Try it in an administrator PowerShell, then run this again.", st.code().unwrap_or(-1));
        }
        Ok(1)
    }

    fn pick_distro(a: &Args, ds: &[Distro]) -> Result<Option<String>> {
        if let Some(want) = &a.distro {
            return match ds.iter().find(|d| &d.name == want) {
                Some(d) if d.version == 2 => Ok(Some(d.name.clone())),
                Some(_) => bail!("{want} is WSL 1; Minerva needs WSL 2 (wsl --set-version {want} 2)"),
                None => bail!("no WSL distribution called {want}"),
            };
        }
        let cands: Vec<&Distro> = ds.iter().filter(|d| wslparse::is_candidate(d)).collect();
        if let Some(d) = cands.iter().find(|d| d.is_default) {
            ok(&format!("using your default WSL distribution: {}", d.name));
            return Ok(Some(d.name.clone()));
        }
        if cands.is_empty() {
            return Ok(None);
        }
        let names: Vec<String> = cands.iter().map(|d| d.name.clone()).collect();
        let i = select(a, "Which WSL distribution should Minerva go into?", &names)?;
        Ok(Some(names[i].clone()))
    }

    fn check_distro(distro: &str) -> Result<String> {
        let rel = wsl_out(distro, &["cat", "/etc/os-release"]).context("reading /etc/os-release in the distribution")?;
        let low = rel.to_ascii_lowercase();
        let line = |k: &str| low.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim_matches('"').to_string())).unwrap_or_default();
        let (id, like) = (line("id="), line("id_like="));
        if !(id == "ubuntu" || id == "debian" || like.contains("debian") || like.contains("ubuntu")) {
            bail!("{distro} is {id:?}; Minerva's installer supports Ubuntu and Debian-like distributions");
        }
        let arch = wsl_out(distro, &["uname", "-m"])?;
        if arch.trim() != "x86_64" {
            bail!("{distro} is {} ; the bundle is x86_64 only", arch.trim());
        }
        wsl_out(distro, &["sh", "-c", "printf %s \"$HOME\""])
    }

    // ---- the bundle --------------------------------------------------------------------------------------------

    struct Bundle {
        top: String,
        full: String,
        windows: HashMap<String, Vec<u8>>,
    }

    fn read_bundle() -> Result<Bundle> {
        if BUNDLE.is_empty() {
            bail!("this minerva-install.exe was built without a bundle (installer/build.ps1 makes the real one)");
        }
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(BUNDLE));
        let (mut top, mut full) = (String::new(), String::new());
        let mut windows = HashMap::new();
        for e in ar.entries()? {
            let mut e = e?;
            let path = e.path()?.to_string_lossy().replace('\\', "/");
            let Some((t, rel)) = path.split_once('/') else { continue };
            top = t.to_string();
            if rel == "VERSION" {
                let mut s = String::new();
                e.read_to_string(&mut s)?;
                full = s.trim().to_string();
            } else if let Some(f) = rel.strip_prefix("windows/").filter(|f| !f.is_empty() && !f.contains('/')) {
                let mut b = Vec::new();
                e.read_to_end(&mut b)?;
                windows.insert(f.to_string(), b);
            }
        }
        if top.is_empty() || full.is_empty() || !windows.contains_key("hoot.ps1") || !windows.contains_key("hoot.cmd") {
            bail!("the embedded bundle is incomplete (no VERSION or no windows/hoot.ps1)");
        }
        minerva_installer::version::check_dir_name(&full)?;
        Ok(Bundle { top, full, windows })
    }

    /// Unpack the bundle into a fresh dir in WSL, run `minerva install` from it (the one install logic), clean up.
    fn install_in_wsl(distro: &str, home: &str, prefix: &str, b: &Bundle, jev: &str) -> Result<()> {
        let stage = format!("{home}/.cache/minerva-install/{}-{}", b.full, std::process::id());
        let mut child = wsl(Some(distro))
            .args(["-e", "sh", "-c", "set -e; rm -rf -- \"$1\"; mkdir -p -- \"$1\"; tar -xzf - -C \"$1\"", "sh", &stage])
            .stdin(Stdio::piped())
            .spawn()
            .context("starting wsl.exe")?;
        {
            let mut si = child.stdin.take().context("no stdin")?;
            si.write_all(BUNDLE).context("sending the bundle into WSL")?;
        }
        if !child.wait()?.success() {
            bail!("could not unpack the bundle inside WSL (is there disk space in the distribution?)");
        }
        let top = format!("{stage}/{}", b.top);
        let r = wsl(Some(distro)).args(["-e", &format!("{top}/bin/minerva"), "install", "--from", &top, "--prefix", prefix, "--jev", jev]).status();
        let _ = wsl(Some(distro)).args(["-e", "rm", "-rf", "--", &stage]).status();
        if !r.context("running the bundled installer")?.success() {
            bail!("the install inside WSL failed (the message above says why)");
        }
        Ok(())
    }

    // ---- the Windows side --------------------------------------------------------------------------------------

    fn win_root(a: &Args) -> Result<PathBuf> {
        match &a.win_root {
            Some(p) => Ok(p.clone()),
            None => Ok(Path::new(&std::env::var("LOCALAPPDATA").context("LOCALAPPDATA is not set")?).join("Minerva")),
        }
    }

    fn env_subkey(a: &Args) -> String {
        match &a.registry_root {
            Some(r) => format!("{}\\Environment", r.trim_end_matches('\\')),
            None => "Environment".to_string(),
        }
    }

    fn uninstall_subkey(a: &Args) -> String {
        match &a.registry_root {
            Some(r) => format!("{}\\Uninstall\\Minerva", r.trim_end_matches('\\')),
            None => "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Minerva".to_string(),
        }
    }

    fn shortcut_path(a: &Args) -> Result<PathBuf> {
        let dir = match &a.start_menu {
            Some(d) => d.clone(),
            None => Path::new(&std::env::var("APPDATA").context("APPDATA is not set")?).join("Microsoft\\Windows\\Start Menu\\Programs"),
        };
        Ok(dir.join("Minerva.lnk"))
    }

    fn change_user_path(a: &Args, f: impl Fn(&str) -> Option<String>) -> Result<bool> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey_with_flags(env_subkey(a), KEY_READ | KEY_WRITE)?;
        // Only "there is no Path value" means empty. Any other failure (access, a value of the wrong type) stops here:
        // writing "our entry only" over a PATH we could not read would wipe the person's PATH.
        let cur: String = match key.get_value::<String, _>("Path") {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).context("reading the user PATH failed; nothing was written"),
        };
        let Some(new) = f(&cur) else { return Ok(false) };
        let bytes: Vec<u8> = new.encode_utf16().chain(std::iter::once(0)).flat_map(|u| u.to_le_bytes()).collect();
        key.set_raw_value("Path", &RegValue { bytes, vtype: REG_EXPAND_SZ })?;
        if a.registry_root.is_none() {
            broadcast_environment_change();
        }
        Ok(true)
    }

    fn broadcast_environment_change() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
        let env: Vec<u16> = "Environment".encode_utf16().chain(std::iter::once(0)).collect();
        let mut res = 0usize;
        unsafe {
            SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, env.as_ptr() as isize, SMTO_ABORTIFHUNG, 5000, &mut res);
        }
    }

    fn write_uninstall_entry(a: &Args, root: &Path, exe: &Path) -> Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(uninstall_subkey(a))?;
        key.set_value("DisplayName", &"Minerva")?;
        key.set_value("DisplayVersion", &FULL_VERSION)?;
        key.set_value("Publisher", &"Minerva club")?;
        key.set_value("InstallLocation", &root.display().to_string())?;
        key.set_value("UninstallString", &format!("\"{}\" --uninstall", exe.display()))?;
        key.set_value("NoModify", &1u32)?;
        key.set_value("NoRepair", &1u32)?;
        Ok(())
    }

    fn make_shortcut(lnk: &Path, hoot_cmd: &Path, workdir: &Path) -> Result<()> {
        if let Some(p) = lnk.parent() {
            std::fs::create_dir_all(p)?;
        }
        let ps1 = std::env::temp_dir().join(format!("minerva-lnk-{}.ps1", std::process::id()));
        std::fs::write(
            &ps1,
            "param([string]$Lnk,[string]$Target,[string]$Arguments,[string]$Dir)\r\n\
             $s = (New-Object -ComObject WScript.Shell).CreateShortcut($Lnk)\r\n\
             $s.TargetPath = $Target; $s.Arguments = $Arguments; $s.WorkingDirectory = $Dir\r\n\
             $s.Description = 'Open Minerva'; $s.Save()\r\n",
        )?;
        let args = format!("/d /c \"\"{}\" || pause\"", hoot_cmd.display());
        let st = Command::new(powershell_exe())
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&ps1)
            .args(["-Lnk"])
            .arg(lnk)
            .args(["-Target"])
            .arg(cmd_exe())
            .args(["-Arguments", &args, "-Dir"])
            .arg(workdir)
            .status();
        let _ = std::fs::remove_file(&ps1);
        if !st.context("running powershell for the shortcut")?.success() {
            bail!("could not create the shortcut {}", lnk.display());
        }
        Ok(())
    }

    // ---- install -----------------------------------------------------------------------------------------------

    fn install(a: &Args) -> Result<i32> {
        println!("\n  Minerva installer {FULL_VERSION}\n");
        let bundle = read_bundle()?;
        say("1/6  checking WSL");
        let ds = match probe(a) {
            Probe::Missing(why) => return offer_wsl(a, &why),
            Probe::Distros(ds) => ds,
        };
        let Some(distro) = pick_distro(a, &ds)? else {
            return offer_wsl(a, "WSL is there, but it has no Ubuntu or Debian distribution on WSL 2");
        };
        let home = check_distro(&distro)?;
        let prefix = a.wsl_prefix.clone().unwrap_or_else(|| format!("{home}/.local"));
        if !prefix.starts_with('/') {
            bail!("--wsl-prefix must be an absolute Linux path");
        }
        ok(&format!("{distro} (WSL 2); installing under {prefix}/opt/minerva"));

        say(&format!("2/6  unpacking Minerva {} inside WSL", bundle.full));
        // The optional jev step: a person decides, never a default. `--yes` does not count as a yes to a 1.2 GB download.
        let jev = match &a.jev {
            Some(v) => v.clone(),
            None if std::io::stdin().is_terminal() => {
                println!("\n  {}\n", minerva_installer::JEV_OFFER.replace('\n', "\n  "));
                if confirm(a, "Set up jev now?", false)? { "yes".to_string() } else { "no".to_string() }
            }
            None => "no".to_string(),
        };
        install_in_wsl(&distro, &home, &prefix, &bundle, &jev)?;

        say("3/6  Windows side: hoot");
        let root = win_root(a)?;
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).with_context(|| format!("creating {}", bin.display()))?;
        for (name, bytes) in &bundle.windows {
            std::fs::write(bin.join(name), bytes).with_context(|| format!("writing {name}"))?;
        }
        let marker = serde_json::json!({ "distro": distro, "wsl_prefix": prefix, "version": bundle.full });
        std::fs::write(bin.join("install.json"), serde_json::to_vec_pretty(&marker)?)?;
        let me = std::env::current_exe()?;
        let my_copy = root.join("minerva-install.exe");
        if me.canonicalize().ok() != my_copy.canonicalize().ok() {
            std::fs::copy(&me, &my_copy).context("keeping a copy of the installer for uninstall")?;
        }
        ok(&format!("hoot in {}", bin.display()));

        if a.no_registry {
            say("4/6  PATH and uninstall entry: skipped (--no-registry)");
        } else {
            say("4/6  adding hoot to your user PATH, and Minerva to Installed apps");
            let bin_s = bin.display().to_string();
            let added = change_user_path(a, |cur| pathlist::add(cur, &bin_s))?;
            write_uninstall_entry(a, &root, &my_copy)?;
            ok(if added { "PATH updated (open a NEW terminal to use `hoot`)" } else { "PATH already had it" });
        }
        if a.no_shortcut {
            say("5/6  Start menu: skipped (--no-shortcut)");
        } else {
            say("5/6  Start menu shortcut");
            make_shortcut(&shortcut_path(a)?, &bin.join("hoot.cmd"), &root)?;
            ok("Minerva is in the Start menu");
        }

        say("6/6  first-run setup");
        let toml_there = wsl(Some(&distro))
            .args(["-e", "sh", "-c", "test -f \"${MINERVA_HOME:-$HOME/Minerva}/minerva.toml\""])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let interactive = std::io::stdin().is_terminal();
        if a.skip_wizard {
            ok("skipped (--skip-wizard); `hoot init` runs it any time");
        } else if !interactive {
            ok("not a terminal here; `hoot` runs the wizard on first start");
        } else if !toml_there || confirm(a, "Minerva is already set up. Run the setup questions again?", false)? {
            let st = wsl(Some(&distro)).args(["-e", &format!("{prefix}/opt/minerva/current/bin/minerva-setup")]).status();
            match st {
                Ok(s) if s.success() => ok("setup saved"),
                _ => println!("  !! the setup wizard did not finish; run: hoot init"),
            }
        } else {
            ok("kept your existing setup");
        }

        println!();
        println!("  Minerva {} is installed.", bundle.full);
        println!("    open it:   hoot   (new terminal)  or the Start menu entry \"Minerva\"");
        println!("    sign in to a model provider (eidolon): see docs/Getting-Started.md in the Minerva repository,");
        println!("      part \"providers\"; this installer asked for no password or key and stored none.");
        println!("    update:    hoot update      remove: minerva-install.exe --uninstall (or Installed apps)");
        println!("    your chats and settings: ~/Minerva inside {distro}; installing, updating and removing never delete them.");
        Ok(0)
    }

    // ---- uninstall ---------------------------------------------------------------------------------------------

    fn uninstall(a: &Args) -> Result<i32> {
        println!("\n  Minerva uninstall\n");
        let root = win_root(a)?;
        let bin = root.join("bin");
        let marker: Option<serde_json::Value> =
            std::fs::read(bin.join("install.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let distro = a.distro.clone().or_else(|| marker.as_ref().and_then(|m| m["distro"].as_str().map(String::from)));
        let prefix = a.wsl_prefix.clone().or_else(|| marker.as_ref().and_then(|m| m["wsl_prefix"].as_str().map(String::from)));
        let mut kept = String::from("~/Minerva");

        match (&distro, &prefix) {
            (Some(d), Some(p)) if !a.fake_wsl_missing => {
                say(&format!("removing the Linux side in {d} ({p}/opt/minerva)"));
                let mut c = wsl(Some(d));
                let st = c.args(["-e", &format!("{p}/opt/minerva/current/bin/minerva"), "uninstall", "--prefix", p]).status();
                match st {
                    Ok(s) if s.success() => kept = "~/Minerva".into(),
                    _ => println!("  !! could not run the Linux uninstall (is the distribution gone?); remove {p}/opt/minerva by hand"),
                }
            }
            _ => say("no WSL install recorded here; only the Windows side is removed"),
        }

        say("removing the Windows side");
        if !a.no_registry {
            let bin_s = bin.display().to_string();
            let _ = change_user_path(a, |cur| pathlist::remove(cur, &bin_s));
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(uninstall_subkey(a));
        }
        if let Ok(lnk) = shortcut_path(a) {
            let _ = std::fs::remove_file(lnk);
        }
        let _ = std::fs::remove_dir_all(&bin);
        // The running exe cannot delete itself: a detached cmd removes it (and the folder if empty) a moment later.
        let me = root.join("minerva-install.exe");
        let line = format!("/d /c ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"", me.display(), root.display());
        let _ = Command::new(cmd_exe()).raw_arg(line).creation_flags(0x0800_0000).spawn();
        println!();
        println!("  Minerva is removed. Kept on purpose: your Minerva home {kept} (chats and settings) inside WSL.");
        println!("  Delete it yourself if you want it gone: wsl rm -rf ~/Minerva");
        Ok(0)
    }
}
