//! `minerva-setup`: the first-run wizard. `hoot` runs it when `<home>/minerva.toml` is missing, and
//! `hoot init` runs it again on purpose. `--print-roots` is the non-interactive half: it prints the
//! saved project roots, one per line, for `bin/perch-open.sh`.

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use minerva_setup::{
    DEFAULT_MODEL, Settings, check_home, check_root, default_home, eidolon_models, expand, mode_of, model_choices, parse_models,
    prepare_home, settings_path, usable_roots, windows_profile,
};

const HELP: &str = "minerva-setup: the Minerva first-run wizard

usage: minerva-setup [--print-roots]
  (none)         ask for the default model and project roots; write <home>/minerva.toml
  --print-roots  print the saved roots, one existing folder per line (for bin/perch-open.sh); asks nothing

environment:
  MINERVA_HOME   the Minerva home, for tests and second servers (default $HOME/Minerva); never asked
";

/// Which kind of machine this is. Only the desktop is built.
///
/// EXTENSION POINT (server role step, not approved yet): add `Mgmt`, `Range` and `Club` here, ask for the role
/// in `main` before `ask_desktop`, and give each role its own `ask_*` function that returns what it needs
/// to write. Secrets must never be typed into a cliclack `input` or kept in `Answers`: use
/// `cliclack::password` and pipe the value to `eidolon secret set <name>` over the child's stdin, so it is
/// never in argv, env or a file this crate writes. See logs/agents/deploy-PLAN.md (roles and secrets).
#[derive(Debug, Clone, Copy)]
enum Role {
    Desktop,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => {}
        ["--print-roots"] => return report(print_roots()),
        ["-h"] | ["--help"] => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("minerva-setup: unknown argument (try --help)");
            return ExitCode::from(2);
        }
    }
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        eprintln!("minerva-setup: this asks questions; run it in a terminal (in Windows: hoot init)");
        return ExitCode::from(2);
    }
    match wizard(Role::Desktop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if is_cancel(&e) => ExitCode::from(130),
        Err(e) => report(Err(e)),
    }
}

fn report(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("minerva-setup: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// cliclack prints its own "Operation cancelled" and returns `Interrupted` on Esc or Ctrl-C.
fn is_cancel(e: &anyhow::Error) -> bool {
    e.downcast_ref::<io::Error>().is_some_and(|io| io.kind() == io::ErrorKind::Interrupted)
}

fn home_env() -> Option<std::ffi::OsString> {
    std::env::var_os("HOME")
}

fn print_roots() -> Result<()> {
    let home = default_home(std::env::var_os("MINERVA_HOME"), home_env())?;
    let Some(settings) = Settings::load(&home)? else { return Ok(()) };
    let (good, bad) = usable_roots(&settings);
    for root in bad {
        eprintln!("minerva-setup: root skipped, it is not a folder any more: {}", root.display());
    }
    for root in good {
        println!("{}", root.display());
    }
    Ok(())
}

/// What the desktop questions come back with.
struct Answers {
    home: PathBuf,
    settings: Settings,
}

fn wizard(role: Role) -> Result<()> {
    cliclack::intro("minerva setup")?;
    let answers = match role {
        Role::Desktop => ask_desktop()?,
    };
    save(&answers)
}

fn ask_desktop() -> Result<Answers> {
    let user_home = PathBuf::from(home_env().context("$HOME is not set")?);
    // 1. the home folder is not asked: hoot only ever looks at $MINERVA_HOME or ~/Minerva, so a typed
    // elsewhere would be forgotten on the next launch (operator ruling 2026-10-06).
    let home = default_home(std::env::var_os("MINERVA_HOME"), home_env())?;
    check_home(&home).map_err(|e| anyhow::anyhow!("{e}"))?;
    cliclack::log::info(format!("Minerva keeps its chats and settings in {}", home.display()))?;
    let existing = Settings::load(&home)?;
    if existing.is_some() {
        cliclack::log::info(format!("found {}: its values are the defaults below", settings_path(&home).display()))?;
    }

    // 2. the default model
    let found = eidolon_models().map(|text| parse_models(&text));
    let current = existing.as_ref().map(|s| s.default_model.as_str());
    let rows = model_choices(found.as_deref().unwrap_or(&[]), current, 6);
    let initial = current.unwrap_or(DEFAULT_MODEL).to_string();
    let mut model = cliclack::select("Default model for new chats");
    for (id, hint) in &rows {
        model = model.item(id.clone(), id, hint);
    }
    const OTHER: &str = "";
    model = model.item(OTHER.to_string(), "other...", "type a provider:model id");
    let mut model = model.initial_value(initial).interact()?;
    if model == OTHER {
        let known = found.clone().unwrap_or_default();
        model = cliclack::input("Model id (provider:model)")
            .validate(move |text: &String| {
                let text = text.trim();
                if text.is_empty() || text.contains(char::is_whitespace) {
                    return Err("an id has no spaces, e.g. zai:glm-5.3".to_string());
                }
                if !known.is_empty() && !text.contains(':') && text != "mock" {
                    return Err("an id looks like provider:model".to_string());
                }
                Ok(())
            })
            .interact::<String>()?
            .trim()
            .to_string();
    }
    if found.is_none() {
        cliclack::log::remark("(eidolon models did not answer in time; the list above is the built-in short one)")?;
    }

    // 3. project roots
    let mut candidates: Vec<(PathBuf, String)> = vec![(user_home.clone(), "your WSL home".into())];
    if let Some(profile) = windows_profile() {
        candidates.push((profile, "your Windows profile".into()));
    }
    candidates.push((home.clone(), "the Minerva home".into()));
    let mut picked: Vec<PathBuf> = match &existing {
        Some(s) if !s.roots.is_empty() => s.roots.clone(),
        _ => candidates.iter().map(|(p, _)| p.clone()).filter(|p| check_root(p).is_ok() || *p == home).collect(),
    };
    for root in &picked {
        if !candidates.iter().any(|(p, _)| p == root) {
            candidates.push((root.clone(), "from your last setup".into()));
        }
    }
    picked.retain(|p| candidates.iter().any(|(c, _)| c == p));
    let mut roots_prompt = cliclack::multiselect("Folders a chat may work in (space toggles, enter accepts)").required(false);
    for (path, hint) in &candidates {
        roots_prompt = roots_prompt.item(path.clone(), path.display(), hint);
    }
    let mut roots: Vec<PathBuf> = roots_prompt.initial_values(picked).interact()?;
    loop {
        let more: String = cliclack::input("Add another folder (leave empty to continue)")
            .required(false)
            .placeholder("/path/to/projects")
            .validate({
                let user_home = user_home.clone();
                move |text: &String| {
                    if text.trim().is_empty() { Ok(()) } else { check_root(&expand(text, &user_home)) }
                }
            })
            .interact()?;
        if more.trim().is_empty() {
            break;
        }
        let path = expand(&more, &user_home);
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    Ok(Answers { home, settings: Settings { default_model: model, roots } })
}

fn save(answers: &Answers) -> Result<()> {
    let Answers { home, settings } = answers;
    let existing = Settings::load(home)?;
    let mut summary = format!("home    {}\nmodel   {}\nroots   ", home.display(), settings.default_model);
    if settings.roots.is_empty() {
        summary.push_str("(none: chats work only in the Minerva home)");
    }
    for (i, root) in settings.roots.iter().enumerate() {
        if i > 0 {
            summary.push_str("\n        ");
        }
        summary.push_str(&root.display().to_string());
    }
    cliclack::note("About to write", summary)?;
    if existing.as_ref() == Some(settings) {
        cliclack::outro("Nothing changed: that is what minerva.toml already says.")?;
        return Ok(());
    }
    if !cliclack::confirm(format!("Write {}?", settings_path(home).display())).initial_value(true).interact()? {
        cliclack::outro_cancel("Nothing written.")?;
        return Ok(());
    }
    prepare_home(home)?;
    settings.save(home)?;
    cliclack::log::success(format!("wrote {} (mode {:o})", settings_path(home).display(), mode_of(&settings_path(home))?))?;
    cliclack::outro("Done. Next: run `hoot` to open Minerva. (`hoot init` asks these questions again.)")?;
    Ok(())
}

