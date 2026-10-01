//! The CLI's `plugins` noun: what is installed, what it may do, and where it
//! came from. Every verb here is operator-only by construction — a CLI noun,
//! never a tool, and no script reaches any of it. The implementations live
//! beside the noun rather than in `main.rs`, because a growing pile of
//! `plugins-*` verbs all editing one file is how three people lose each
//! other's work. (vault, *Plugin Packaging (spec)*.)

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Subcommand;

use crate::config;

/// Every operator verb about installed plugins, under the `plugins` noun:
/// what is there, what it may do, and where it came from. Operator-only by
/// construction — a CLI noun, never a tool, and no script reaches any of
/// them — and the reason they are nested rather than a growing pile of
/// `plugins-*` commands.
#[derive(Subcommand)]
pub(crate) enum Op {
    /// Vouch for a plugin's verb — or, with no verb named, every verb it
    /// ships — in the permission store: while the tool's script stays
    /// exactly what was vouched, the question the table would ask is
    /// answered by data instead of by you.
    Trust {
        plugin: String,
        /// One verb — the stem (`open`) or the full name (`browser_open`).
        verb: Option<String>,
    },
    /// Take a vouch back. The verb asks again on its next call.
    Untrust {
        plugin: String,
        /// One verb — the stem (`open`) or the full name (`browser_open`).
        verb: Option<String>,
    },
    /// Grant a tool's reach for one source — `secret:NAME`, `file:PATH` or
    /// `env:VAR` — in the permission store. The same act, the same record,
    /// as the yes an ask-once answer writes from a session.
    Grant {
        /// The tool's registered verb (`browser_fetch`) — or, with
        /// `--plugin`, every verb the plugin installs today, one row
        /// each. A group act is a convenience for the operator, never a
        /// stored umbrella: a stored plugin-wide row would be standing
        /// authority over verbs that did not exist when the operator said
        /// yes, so the yes is bound to verbs that exist, and a verb
        /// added tomorrow asks.
        #[arg(long)]
        plugin: Option<String>,
        /// The tool's registered verb (`browser_fetch`).
        tool: Option<String>,
        /// The whole source, not one spelling of it — required, and the
        /// verb's error says so. Optional to clap because an optional
        /// positional may not precede a required one: the required form
        /// made every parse of this verb a debug-build panic.
        source: Option<String>,
    },
    /// Take grants back: every grant of one tool, or one source of it.
    Revoke {
        /// The tool's registered verb (`browser_fetch`).
        tool: String,
        source: Option<String>,
    },
    /// Adopt a plugin again: its tools join the next session and its ui
    /// merges on the next load. Revokes nothing — a plugin disabled and
    /// re-enabled asks nobody again.
    Enable {
        plugin: String,
    },
    /// Stop adopting a plugin and loading its ui: present, listed, and
    /// contributed by nothing. Its vouches and grants are left alone.
    Disable {
        plugin: String,
    },
    /// Install one or more plugins from a git repo: fetch, validate, copy
    /// into the plugins directory, and write the record's source fields.
    /// **Installing does not trust** — the copied verbs land on the gate
    /// like any other caller's, and `trust` is a separate act by a
    /// separate intent.
    Install {
        /// `owner/repo` (GitHub) or a full git URL.
        repo: String,
        /// Folders in the repo carrying a `plugin.rn`; each folder's
        /// basename is the plugin's name.
        paths: Vec<String>,
        /// Every top-level folder carrying a manifest.
        #[arg(long)]
        all: bool,
        /// A branch, a tag, or a full commit. Default: the repo's own HEAD.
        #[arg(long)]
        r#ref: Option<String>,
    },
    /// Re-fetch what the record names and compare bytes at the recorded
    /// ref — version strings are a claim; the commit and the file set are
    /// the record. `--check` stops at saying; local edits refuse without
    /// `--force`; vouch rows are left alone, so an updated verb asks
    /// again by itself.
    Update {
        /// Say what would change; change nothing.
        #[arg(long)]
        check: bool,
        plugins: Vec<String>,
        /// Every plugin the record names.
        #[arg(long)]
        all: bool,
        /// Overwrite local edits, naming them first.
        #[arg(long)]
        force: bool,
    },
    /// Remove an installed plugin: the directory, and its row in the
    /// record. The one act that shrinks the record — a `missing` row is
    /// for a directory that vanished without an act, not for a decision
    /// the operator made. **Vouches and grants are left alone**, the same
    /// trade `disable` makes: they answer nothing while no verb of that
    /// name is loaded, and a reinstall inherits the earlier yes. A
    /// service still running refuses, naming the verb that stops it.
    Uninstall {
        /// The plugin's directory name, as the listing spells it.
        plugin: String,
        /// Delete local edits too, naming them first.
        #[arg(long)]
        force: bool,
    },
    /// Run, stop, probe, or approve a plugin's declared service. A
    /// service is a process on the operator's machine, so start asks
    /// once — by hash of the declaration — and an updated command asks
    /// again: an update cannot inherit the right to run a process.
    Service {
        #[command(subcommand)]
        op: ServiceOp,
    },
}

/// The service noun's verbs, under `plugins service`. Operator-only by
/// construction — a CLI verb, never a tool, and no script reaches any of
/// them — the same line the trust event draws.
#[derive(Subcommand)]
pub(crate) enum ServiceOp {
    /// Approve the declaration once (the ask `start` would otherwise
    /// make) and spawn it detached, waiting for readiness when the
    /// declaration names a port.
    Start {
        /// The plugin whose service to run.
        plugin: String,
    },
    /// Signal the recorded process, but only after its cmdline matches
    /// the record — a pid file can lie.
    Stop {
        plugin: String,
    },
    /// Probe the declaration's port and say what answers. Never trusts a
    /// record.
    Status {
        plugin: String,
    },
    /// Record the approval `start` asks for — the same act, from a
    /// headless shell.
    Approve {
        plugin: String,
    },
}

/// The noun's one entry point: bare, the listing; with a verb, the
/// operator's act on one.
pub(crate) fn run(cfg: &config::Config, dir: Option<PathBuf>, op: Option<Op>) -> anyhow::Result<()> {
        // The verbs first: each is an act on the record, and none of
        // them reads the listing's directory.
        if let Some(op) = op {
            return match op {
                Op::Trust { plugin, verb } => {
                    plugins_trust(cfg, &plugin, verb.as_deref(), true)
                }
                Op::Untrust { plugin, verb } => {
                    plugins_trust(cfg, &plugin, verb.as_deref(), false)
                }
                Op::Grant {
                    plugin,
                    tool,
                    source,
                } => plugins_grant(cfg, plugin.as_deref(), tool.as_deref(), source.as_deref()),
                Op::Revoke { tool, .. } => plugins_revoke(cfg, &tool),
                Op::Enable { plugin } => plugins_set_enabled(cfg, &plugin, true),
                Op::Disable { plugin } => plugins_set_enabled(cfg, &plugin, false),
                Op::Install {
                    repo,
                    paths,
                    all,
                    r#ref,
                } => crate::plugin_fetch::install(cfg, &repo, &paths, all, r#ref.as_deref()),
                Op::Update {
                    check,
                    plugins,
                    all,
                    force,
                } => crate::plugin_fetch::update(cfg, &plugins, all, check, force),
                Op::Uninstall { plugin, force } => {
                    plugins_uninstall(cfg, &plugin, force, &crate::service::services_dir())
                }
                Op::Service { op } => match op {
                    ServiceOp::Start { plugin } => crate::service::start(
                        &cfg.plugins_dir(),
                        &cfg.policy_path(),
                        &crate::service::services_dir(),
                        &plugin,
                    )
                    .map(|line| println!("{line}")),
                    ServiceOp::Stop { plugin } => {
                        crate::service::stop(&crate::service::services_dir(), &plugin)
                            .map(|line| println!("{line}"))
                    }
                    ServiceOp::Status { plugin } => {
                        crate::service::status(
                            &cfg.plugins_dir(),
                            &crate::service::services_dir(),
                            &plugin,
                        )
                        .map(|line| println!("{line}"))
                    }
                    ServiceOp::Approve { plugin } => {
                        crate::service::approve(&cfg.plugins_dir(), &cfg.policy_path(), &plugin)
                            .map(|line| println!("{line}"))
                    }
                },
            };
        }
        // The one v1 verb: read-only presentation. Discovery is lazy
        // by contract — this command is the only reader of the
        // plugins directory in the binary, and it reads it only
        // because it was asked to. A broken plugin is one line here,
        // never a failure of the listing.
        let dir = dir.unwrap_or_else(|| cfg.plugins_dir());
        let plugins = eidolon_rune::plugins::discover(&dir)?;
        // The record, for the provenance lines and the missing pass. A
        // record that cannot parse is a warning and an absent one — the
        // listing reads, never writes, and must not fail on the file it
        // is about to describe.
        let legacy = legacy_grant_lines(cfg)?;
        let record = match eidolon_rune::store::Store::load(&cfg.plugins_dir()) {
            Ok(s) => s,
            Err(e) => {
                println!("warning: {e:#} — listing as if the record were absent");
                eidolon_rune::store::Store::default()
            }
        };
        let mut present: Vec<String> = Vec::new();
        if plugins.is_empty() {
            println!("no plugins installed at {}", dir.display());
        }
        for plugin in plugins {
            match plugin {
                eidolon_rune::plugins::Plugin::Installed { dir, manifest } => {
                    let version = if manifest.version.is_empty() {
                        "-"
                    } else {
                        manifest.version.as_str()
                    };
                    println!("{} {}", manifest.name, version);
                    if !manifest.description.is_empty() {
                        println!("    {}", manifest.description);
                    }
                    println!("    at       {}", dir.display());
                    let record_line = match record.entry(&manifest.name) {
                        Some(entry) if !entry.enabled => "managed (disabled)",
                        Some(_) => "managed",
                        None => "unmanaged — installed by hand",
                    };
                    println!("    record   {record_line}");
                    // The declaration only — a listing that probed the
                    // port would be an egress from a listing, slow and
                    // fragile; `service status` answers what is true
                    // right now.
                    if let Some(svc) = &manifest.service {
                        let readiness = match &svc.readiness {
                            Some(r) => format!(
                                ", readiness 127.0.0.1:{}{}",
                                r.port,
                                r.health.as_deref().unwrap_or_default()
                            ),
                            None => String::new(),
                        };
                        println!(
                            "    service {} {}{}",
                            svc.command,
                            svc.args.join(" "),
                            readiness
                        );
                    }
                    // Where a managed plugin came from, when the record
                    // knows: the source half an install writes and an
                    // update refreshes.
                    if let Some(from) = record.entry(&manifest.name).and_then(|e| e.provenance_line()) {
                        println!("    {from}");
                    }
                    for (plugin, source) in &legacy {
                        if plugin == &manifest.name {
                            println!("    legacy grant — plugin-wide for {source} from an older file: authority over nothing now, the verbs it covered ask again");
                        }
                    }
                    present.push(manifest.name.clone());
                    let payloads = eidolon_rune::plugins::payloads_present(&dir);
                    if !payloads.is_empty() {
                        println!("    payloads {}", payloads.join(", "));
                    }
                    for (key, values) in &manifest.claims {
                        println!("    claims   {key}: {}", values.join(", "));
                    }
                    list_verbs(&dir, &manifest.name, cfg);
                }
                eidolon_rune::plugins::Plugin::Broken { dir, reason } => {
                    let name =
                        dir.file_name().and_then(|n| n.to_str()).unwrap_or("<unnamed>");
                    println!("{name}: {reason}");
                }
            }
        }
        // Entries whose directory is gone: named, never dropped. A
        // record silently forgetting a missing directory would make
        // `plugins disable` look like an uninstall, and the entry is the
        // operator's decision — dropping it is theirs to do.
        for name in record.plugins().keys() {
            if !present.contains(name) {
                println!(
                    "{name}: missing — the record outlives the directory; reinstall it, or drop the entry from {}",
                    eidolon_rune::store::Store::beside(&cfg.plugins_dir()).display()
                );
            }
        }
        Ok(())
}

/// `plugins enable` / `disable`: the operator's decision about one
/// plugin, recorded beside its directory. Disabling adopts nothing and
/// loads no ui — and revokes nothing either, so re-enabling asks nobody
/// again. The file this writes is the one adoption and the ui merge
/// read failing open; a record they cannot parse is an error here
/// rather than a silent clobber.
fn plugins_set_enabled(cfg: &config::Config, plugin: &str, enabled: bool) -> anyhow::Result<()> {
    let mut store = eidolon_rune::store::Store::load(&cfg.plugins_dir()).with_context(|| {
        format!(
            "reading the plugin record beside {}",
            cfg.plugins_dir().display()
        )
    })?;
    store.set_enabled(plugin, enabled);
    store.write().context("writing the plugin record")?;
    if enabled {
        println!("{plugin} enabled — adoption picks it up on the next session, its ui on the next reload");
    } else {
        println!(
            "{plugin} disabled — not adopted, its ui does not load; vouches and grants are left alone, so re-enabling asks nobody again"
        );
    }
    Ok(())
}

/// `plugins uninstall`: remove the directory and forget the row — the one
/// act that shrinks the record.
///
/// Three refusals before anything is deleted, each naming what it found.
/// A service that is still running comes first: a live process is not
/// ours to kill as a side effect of tidying a directory, and the operator
/// has a verb for it. Then local edits, which is the same question
/// `update` asks from the other side — the record knows what it installed,
/// so anything else on disk is somebody's work. Then nothing to remove at
/// all. A plugin the record has never heard of is a directory installed by
/// hand: it goes, and the output says no row was involved.
fn plugins_uninstall(
    cfg: &config::Config,
    plugin: &str,
    force: bool,
    services: &Path,
) -> anyhow::Result<()> {
    let plugins_dir = cfg.plugins_dir();
    let dir = plugins_dir.join(plugin);
    let mut store = eidolon_rune::store::Store::load(&plugins_dir).with_context(|| {
        format!(
            "reading the plugin record beside {}",
            plugins_dir.display()
        )
    })?;
    let entry = store.entry(plugin).cloned();
    if !dir.is_dir() && entry.is_none() {
        anyhow::bail!(
            "nothing named `{plugin}`: no directory at {} and no row in the record",
            dir.display()
        );
    }
    if let Some(pid) = crate::service::running(services, plugin) {
        anyhow::bail!(
            "`{plugin}` has a service running as pid {pid} — `eidolon plugins service stop {plugin}` first, then uninstall"
        );
    }
    let mut removed_dir = false;
    if dir.is_dir() {
        // No baseline is not "everything changed": a plugin nobody
        // recorded — installed by hand, or a row that only says managed —
        // has no file map to differ from, and a person deleting the tree
        // they put there themselves is not an edit to refuse. The check
        // exists for the other case: a tree the record DOES know, with
        // something in it the record does not.
        let edits = match entry.as_ref().filter(|e| !e.files.is_empty()) {
            Some(e) => {
                let installed = crate::plugin_fetch::hash_dir(&dir)?;
                crate::plugin_fetch::local_edits(&e.files, &installed)
            }
            None => Vec::new(),
        };
        if !edits.is_empty() {
            if !force {
                anyhow::bail!(
                    "`{plugin}` has {} local edit(s) the record does not — {} — refusing to delete them without `--force`",
                    edits.len(),
                    edits.join(", ")
                );
            }
            println!(
                "  --force: deleting {} local edit(s) — {}",
                edits.len(),
                edits.join(", ")
            );
        }
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        removed_dir = true;
    }
    // The record last: a failure before this leaves a row naming a
    // directory that is still there, which is the recoverable direction.
    let forgot = store.forget(plugin);
    if forgot {
        store.write().context("writing the plugin record")?;
    }
    println!(
        "{plugin} uninstalled — {}; vouches and grants are left alone, so a reinstall asks nobody again",
        match (removed_dir, forgot) {
            (true, true) => "directory removed, its row forgotten".to_string(),
            (true, false) => {
                "directory removed, and nothing was recorded for it (installed by hand)".to_string()
            }
            (false, true) => "no directory was there; its row was forgotten".to_string(),
            (false, false) => "nothing was there to remove".to_string(),
        }
    );
    Ok(())
}

/// The grant, and its retraction. One door with the ask-once answer inside
/// a session: the same rows, the same composed check, the same file. With
/// `--plugin` the act is a group act — every verb the plugin installs
/// today, one row each — because grouping is a convenience at the moment
/// of the yes, never a stored umbrella over verbs that come later.
fn plugins_grant(
    cfg: &config::Config,
    plugin: Option<&str>,
    tool: Option<&str>,
    source: Option<&str>,
) -> anyhow::Result<()> {
    // How clap hands the verb its words: `--plugin demo file:tok` arrives
    // as tool="file:tok", source=None, because the one positional after a
    // flag is the tool's — but a plugin-wide yes names no verb, so the
    // single bare value beside a --plugin is read as what the operator
    // typed it as: the source. Every other shape is taken literally, and
    // the rules below say what is missing.
    let (plugin, tool, source) = match (plugin, tool, source) {
        (p, t, Some(src)) => (p, t, src),
        (Some(p), Some(t), None) => (Some(p), None, t),
        (None, Some(_), None) | (Some(_), None, None) => {
            anyhow::bail!("name the source — `secret:NAME`, `file:PATH` or `env:VAR`")
        }
        (None, None, None) => {
            anyhow::bail!("name a verb for one row, or --plugin for every verb it installs today")
        }
    };
    let permits =
        std::sync::Arc::new(eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path()));
    let host = eidolon_rune::Host::new(std::env::current_dir()?);
    let table = std::fs::read_to_string(cfg.policy_path())
        .unwrap_or_else(|_| eidolon_rune::policy::DEFAULT_POLICY.to_string());
    permits.attach_checker(std::sync::Arc::new(move |src| {
        eidolon_rune::permits::check_composed(src, &table, &host)
    }));
    let mut rows = permits.rows();
    // exactly one subject: the rule clap cannot express in positionals,
    // so the error carries it
    if plugin.is_some() == tool.is_some() {
        anyhow::bail!("name a verb for one row, or --plugin for every verb it installs today — not both, not neither");
    }
    // the three spellings, and nothing else
    let kind = source.split(':').next().unwrap_or_default();
    if !matches!(kind, "secret" | "file" | "env") || source.contains('\n') {
        anyhow::bail!(
            "`{source}` is not a source — name one of `secret:NAME`, `file:PATH`, `env:VAR`"
        );
    }
    let grant_row = |tool: &str| eidolon_rune::permits::GrantRow {
        tool: Some(tool.to_string()),
        source: source.to_string(),
        granted_by: eidolon_rune::permits::granted_by(),
        granted_at: eidolon_rune::permits::granted_at(),
        wide_plugin: None,
    };
    if let Some(plugin) = &plugin {
        if tool.is_some() {
            anyhow::bail!("name a tool or --plugin, not both");
        }
        let dir = cfg.plugins_dir().join(plugin);
        if !dir.is_dir() {
            anyhow::bail!("no installed plugin `{plugin}` under {}", cfg.plugins_dir().display());
        }
        // the same derivation the door's check uses: the set is what is
        // installed now, so the yes binds to verbs that exist
        let stems = crate::workflow::plugin_stems(&dir, plugin)?;
        if stems.is_empty() {
            anyhow::bail!("`{plugin}` ships no verbs to grant for");
        }
        for stem in &stems {
            rows.grant(grant_row(&format!("{plugin}_{stem}")));
        }
        permits.write(&rows)?;
        println!(
            "{plugin}: {source} granted for {} verb{} — one row each, so a verb added tomorrow asks",
            stems.len(),
            if stems.len() == 1 { "" } else { "s" },
        );
        for stem in &stems {
            println!("  {plugin}_{stem}");
        }
        return Ok(());
    }
    let Some(tool) = tool else {
        anyhow::bail!("name a tool, or --plugin for every verb it installs today");
    };
    rows.grant(grant_row(tool));
    permits.write(&rows)?;
    println!("{tool}: granted {source} — remembered in policy.permits.rn until revoked");
    Ok(())
}

/// The retraction verb: every grant of one tool goes, and the sources ask
/// again. Its own door, because a grant with no source was never a
/// retraction — overloading `source: Option` for it is what let a grant
/// with no source silently revoke.
fn plugins_revoke(cfg: &config::Config, tool: &str) -> anyhow::Result<()> {
    let permits =
        std::sync::Arc::new(eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path()));
    let host = eidolon_rune::Host::new(std::env::current_dir()?);
    let table = std::fs::read_to_string(cfg.policy_path())
        .unwrap_or_else(|_| eidolon_rune::policy::DEFAULT_POLICY.to_string());
    permits.attach_checker(std::sync::Arc::new(move |src| {
        eidolon_rune::permits::check_composed(src, &table, &host)
    }));
    let mut rows = permits.rows();
    let before = rows.grant.len();
    rows.grant.retain(|row| row.tool.as_deref() != Some(tool));
    let went = before - rows.grant.len();
    if went == 0 {
        anyhow::bail!("no grant on record for {tool}");
    }
    permits.write(&rows)?;
    println!("{tool}: {went} grant{} revoked — the source{} ask{} again",
        if went == 1 { "" } else { "s" },
        if went == 1 { "" } else { "s" },
        if went == 1 { "s" } else { "" });
    Ok(())
}

/// Name a legacy wide row where a person reads: the plugin's block in the
/// listing. The row is authority over nothing — its verbs ask again — and
/// the file keeps carrying the fact so it is never silently dropped.
fn legacy_grant_lines(cfg: &config::Config) -> anyhow::Result<Vec<(String, String)>> {
    let permits = eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path());
    let mut out = Vec::new();
    for row in permits.rows().grant {
        if let Some(plugin) = &row.wide_plugin {
            out.push((plugin.clone(), row.source.clone()));
        }
    }
    Ok(out)
}


/// The trust event, and its retraction. One path for both halves of the
/// flag: find the installed plugin, resolve the verbs, rewrite the store
/// whole through the composed check — a permissions file that would break
/// the gate is refused and the previous one stays on disk.
fn plugins_trust(
    cfg: &config::Config,
    plugin: &str,
    verb: Option<&str>,
    trust: bool,
) -> anyhow::Result<()> {
    let dir = cfg.plugins_dir();
    let plugins = eidolon_rune::plugins::discover(&dir)?;
    let installed = plugins.iter().find_map(|p| match p {
        eidolon_rune::plugins::Plugin::Installed { dir, manifest } if manifest.name == plugin => {
            Some(dir.clone())
        }
        _ => None,
    });
    let Some(tools_dir) = installed.map(|d| d.join("tools")) else {
        anyhow::bail!("no installed plugin `{plugin}` under {}", dir.display());
    };
    let mut stems: Vec<String> = std::fs::read_dir(&tools_dir)
        .with_context(|| format!("reading {}", tools_dir.display()))?
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "rn"))
        .filter_map(|e| e.path().file_stem()?.to_str().map(String::from))
        .collect();
    stems.sort();
    // The named verb answers to the stem (`open`) or the full name
    // (`browser_open`); no name at all means every verb the plugin ships.
    let named = verb.map(|v| v.strip_prefix(&format!("{plugin}_")).unwrap_or(v).to_string());
    let chosen: Vec<String> = match &named {
        Some(stem) => {
            if !stems.iter().any(|s| s == stem) {
                anyhow::bail!(
                    "`{plugin}` ships no verb `{stem}` — its tools are: {}",
                    stems.join(", ")
                );
            }
            vec![stem.clone()]
        }
        None => stems,
    };
    if chosen.is_empty() {
        anyhow::bail!("`{plugin}` ships no tools to vouch for");
    }
    let permits =
        std::sync::Arc::new(eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path()));
    // What a session runs is what this write must compile with: the
    // operator's table where there is one, the shipped one where there is
    // not. A host of one's own, because the listing builds none.
    let host = eidolon_rune::Host::new(std::env::current_dir()?);
    let table = std::fs::read_to_string(cfg.policy_path())
        .unwrap_or_else(|_| eidolon_rune::policy::DEFAULT_POLICY.to_string());
    permits.attach_checker(std::sync::Arc::new(move |src| {
        eidolon_rune::permits::check_composed(src, &table, &host)
    }));
    let mut rows = permits.rows();
    if trust {
        for stem in &chosen {
            let path = tools_dir.join(format!("{stem}.rn"));
            let src = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let verb = format!("{plugin}_{stem}");
            rows.vouch.retain(|r| r.verb != verb);
            rows.vouch.push(eidolon_rune::permits::VouchRow {
                plugin: plugin.to_string(),
                verb,
                hash: eidolon_rune::permits::content_hash(&src),
                granted_by: eidolon_rune::permits::granted_by(),
                granted_at: eidolon_rune::permits::granted_at(),
            });
        }
        rows.vouch.sort_by(|a, b| a.verb.cmp(&b.verb));
        permits.write(&rows)?;
        for stem in &chosen {
            println!("{plugin}_{stem}: vouched — it runs without asking while its script stays what it was");
        }
    } else {
        let went = rows.unvouch(plugin, named.as_deref().map(|s| format!("{plugin}_{s}")).as_deref());
        if went == 0 {
            anyhow::bail!(
                "no vouch on record for {plugin}{}",
                verb.map(|v| format!(" {v}")).unwrap_or_default()
            );
        }
        permits.write(&rows)?;
        println!("{plugin}: {} vouch{} removed — the verb{} ask{} again",
            went, if went == 1 { "" } else { "es" },
            if went == 1 { "" } else { "s" }, if went == 1 { "s" } else { "" });
    }
    Ok(())
}


/// Each verb a plugin ships, with its state: vouched (by whom, when — and
/// whether the script is still what was vouched), ruled by a named row of
/// the operator's `policy.rn`, or unvouched, which is to say it asks. Read
/// only, like the listing around it: a broken table lists its verbs as
/// unvouched, which is the truth of what would happen.
fn list_verbs(dir: &std::path::Path, plugin: &str, cfg: &config::Config) {
    let Ok(files) = std::fs::read_dir(dir.join("tools")) else {
        return;
    };
    let mut stems: Vec<String> = files
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "rn"))
        .filter_map(|e| e.path().file_stem()?.to_str().map(String::from))
        .collect();
    stems.sort();
    if stems.is_empty() {
        return;
    }
    let tools = dir.join("tools");
    let permits = eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path());
    // The table's probe is built once, and only when some verb needs it.
    let mut probe: Option<Option<eidolon_rune::policy::ScriptPolicy>> = None;
    for stem in stems {
        let verb = format!("{plugin}_{stem}");
        match permits.vouched(&verb) {
            Some(row) => {
                let hash = std::fs::read_to_string(tools.join(format!("{stem}.rn")))
                    .map(|src| eidolon_rune::permits::content_hash(&src))
                    .ok();
                if hash.as_deref() == Some(row.hash.as_str()) {
                    println!(
                        "    {:<28} vouched by {} at {}",
                        verb, row.granted_by, row.granted_at
                    );
                } else {
                    println!(
                        "    {:<28} vouched by {} at {}, but the script changed — it asks again",
                        verb, row.granted_by, row.granted_at
                    );
                }
            }
            None => {
                if probe.is_none() {
                    probe = Some(
                        std::env::current_dir()
                            .ok()
                            .map(eidolon_rune::Host::new)
                            .map(|host| {
                                eidolon_rune::policy::ScriptPolicy::load(
                                    &cfg.policy_path(),
                                    &host,
                                    cfg.policy_settings().unwrap_or_default(),
                                )
                                .map(|(p, _)| p)
                            })
                            .and_then(|r| r.ok()),
                    );
                }
                let ruled = probe
                    .as_ref()
                    .and_then(|p| p.as_ref())
                    .and_then(|p| p.named_row(&verb).ok())
                    .flatten();
                match ruled {
                    Some(d) => println!("    {:<28} ruled by policy.rn ({d:?})", verb),
                    None => println!("    {:<28} unvouched — asks", verb),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use std::path::Path;

    /// A scratch config whose plugins directory, plugin record and
    /// permission store all live under the test's own root — the
    /// operator's real `~/.config/eidolon/` is never a test surface.
    fn cfg(root: &Path) -> config::Config {
        toml::from_str(&format!(
            "sessions_dir = '{}'\nplugins_dir = '{}'\n[swarm]\nenabled = false\n[policy]\npath = '{}'\n",
            root.join("sessions").display(),
            root.join("plugins").display(),
            root.join("policy.rn").display(),
        ))
        .expect("a scratch config parses")
    }

    /// A spec-shaped plugin: manifest naming the folder, tools naming
    /// themselves under the plugin's namespace.
    fn fixture_plugin(root: &Path, name: &str, stems: &[&str]) {
        let dir = root.join("plugins").join(name);
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::write(
            dir.join("plugin.rn"),
            format!("pub fn manifest() {{ #{{ name: \"{name}\" }} }}\n"),
        )
        .unwrap();
        for stem in stems {
            std::fs::write(
                dir.join("tools").join(format!("{stem}.rn")),
                format!("pub fn manifest() {{ #{{ name: \"{name}_{stem}\" }} }}\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn a_group_act_writes_one_row_per_installed_verb() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        fixture_plugin(root.path(), "demo", &["echo", "fetch"]);
        plugins_grant(&cfg, Some("demo"), None, Some("secret:demo_token")).unwrap();
        let rows = eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path()).rows();
        let mut tools: Vec<String> = rows.grant.iter().filter_map(|r| r.tool.clone()).collect();
        tools.sort();
        assert_eq!(tools, ["demo_echo", "demo_fetch"], "one row per installed verb");
        // the yes binds to verbs that exist: no row for a verb the plugin
        // does not ship, and no umbrella row at all
        assert!(!tools.iter().any(|t| t == "demo_missing"));
        assert!(rows.grant.iter().all(|r| r.wide_plugin.is_none()));
        // the exact-row door the session's ask-once shares answers the
        // same rows
        assert!(rows.granted("demo_echo", "secret:demo_token").is_some());
    }

    /// The surface the binary actually ships: clap's debug assert fires
    /// on exactly this call when a positional ordering cannot parse, and
    /// no function-level test below constructs the parser. Both
    /// spellings of grant parse, a revoke parses, and the two shapes the
    /// errors exist for do not.
    #[test]
    fn the_grant_verb_parses_both_spellings() {
        use clap::CommandFactory as _;
        let parses = |args: &[&str]| {
            crate::Cli::command()
                .try_get_matches_from(args.iter().copied())
                .is_ok()
        };
        assert!(parses(&["eidolon", "plugins", "grant", "--plugin", "browser", "secret:tok"]));
        assert!(parses(&["eidolon", "plugins", "grant", "browser_open", "secret:tok"]));
        assert!(parses(&["eidolon", "plugins", "revoke", "browser_open"]));
        assert!(parses(&["eidolon", "plugins", "grant", "--plugin", "browser"]));
    }

    /// The rules clap cannot carry in positionals: exactly one subject,
    /// and a source to bind it to.
    #[test]
    fn the_subject_and_source_rules_are_errors() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        assert!(plugins_grant(&cfg, Some("demo"), Some("demo_echo"), Some("secret:x")).is_err(), "both subjects refused");
        assert!(plugins_grant(&cfg, None, None, Some("secret:x")).is_err(), "no subject refused");
    }

    /// The group spelling as the operator types it: clap hands the source
    /// over in the tool positional, and the verb reads the shape — same
    /// rows as the resolved spelling.
    #[test]
    fn the_group_spelling_reads_its_source_from_the_one_positional() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        fixture_plugin(root.path(), "demo", &["echo"]);
        plugins_grant(&cfg, Some("demo"), Some("secret:demo_token"), None).unwrap();
        let rows = eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path()).rows();
        assert_eq!(
            rows.grant.iter().filter_map(|r| r.tool.as_deref()).collect::<Vec<_>>(),
            ["demo_echo"]
        );
    }

    #[test]
    fn a_legacy_wide_row_is_not_honoured_and_is_named() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        fixture_plugin(root.path(), "demo", &["echo"]);
        // a file an older binary wrote: the wide subject, one row for the
        // whole plugin
        let mut src = eidolon_rune::permits::compose(&eidolon_rune::permits::Rows::default());
        src = src.replace(
            "const GRANT = [];",
            "const GRANT = [#{ tool: \"\", source: \"secret:old\", wide_plugin: \"demo\", granted_by: \"noah\", granted_at: \"2026-09-28T00:00:00Z\" }];",
        );
        std::fs::create_dir_all(root.path().join("plugins")).unwrap();
        std::fs::write(
            eidolon_rune::permits::beside(&cfg.policy_path()),
            src,
        )
        .unwrap();
        let rows = eidolon_rune::permits::Permits::from_policy_path(&cfg.policy_path()).rows();
        // never honoured
        assert!(rows.granted("demo_echo", "secret:old").is_none());
        assert_eq!(
            rows.grant.first().and_then(|r| r.wide_plugin.as_deref()),
            Some("demo"),
            "carried, never silently dropped"
        );
        // and named where a person reads
        let named = legacy_grant_lines(&cfg).unwrap();
        assert_eq!(named, [("demo".to_string(), "secret:old".to_string())]);
    }

    /// Record a plugin the way an install would: a row, with the file map
    /// the update check and the uninstaller both read as "what I put
    /// here".
    fn record_plugin(root: &Path, name: &str) {
        let dir = root.join("plugins").join(name);
        let mut store = eidolon_rune::store::Store::load(&root.join("plugins")).unwrap();
        let entry = store.entry_mut(name);
        entry.repo = Some("file:///nowhere".into());
        entry.files = crate::plugin_fetch::hash_dir(&dir).unwrap();
        store.write().unwrap();
    }

    #[test]
    fn an_uninstall_removes_the_tree_and_forgets_the_row() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        fixture_plugin(root.path(), "demo", &["echo"]);
        record_plugin(root.path(), "demo");
        // A permission the operator gave, which is not ours to revoke.
        plugins_grant(&cfg, Some("demo"), None, Some("secret:demo_token")).unwrap();
        let permits = eidolon_rune::permits::beside(&cfg.policy_path());
        let before = std::fs::read_to_string(&permits).unwrap();

        plugins_uninstall(&cfg, "demo", false, &root.path().join("services")).unwrap();

        assert!(!root.path().join("plugins/demo").exists(), "the tree is gone");
        let store = eidolon_rune::store::Store::load(&cfg.plugins_dir()).unwrap();
        assert!(store.entry("demo").is_none(), "and the row with it — the one act that shrinks the record");
        assert_eq!(
            std::fs::read_to_string(&permits).unwrap(),
            before,
            "vouches and grants are the operator's yes, and are left alone"
        );
    }

    #[test]
    fn an_uninstall_refuses_local_edits_unless_forced() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        fixture_plugin(root.path(), "demo", &["echo"]);
        record_plugin(root.path(), "demo");
        std::fs::write(root.path().join("plugins/demo/tools/echo.rn"), "// mine now\n").unwrap();

        let err = format!(
            "{:#}",
            plugins_uninstall(&cfg, "demo", false, &root.path().join("services")).unwrap_err()
        );
        assert!(err.contains("local edit") && err.contains("tools/echo.rn"), "{err}");
        assert!(root.path().join("plugins/demo").exists(), "a refusal deletes nothing");

        plugins_uninstall(&cfg, "demo", true, &root.path().join("services")).unwrap();
        assert!(!root.path().join("plugins/demo").exists(), "--force takes the tree as it lies");
    }

    #[test]
    fn an_uninstall_refuses_while_its_service_is_running() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        fixture_plugin(root.path(), "demo", &["echo"]);
        record_plugin(root.path(), "demo");
        let services = root.path().join("services");
        std::fs::create_dir_all(&services).unwrap();
        // A live process, recorded the way `service start` records one.
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        std::fs::write(
            crate::service::pid_path(&services, "demo"),
            serde_json::json!({ "pid": child.id(), "argv": ["sleep", "30"] }).to_string(),
        )
        .unwrap();

        // `spawn` returns before the child has exec'd, and `running()` reads the
        // live cmdline: a pid file written inside that window describes a process
        // that is not yet this one, so the guard sees no service running and the
        // refusal below never fires. Wait for the state this test means to assert
        // rather than assuming it — under full-suite load the window is wide
        // enough to lose, which is how this passed in isolation and failed in the
        // sandbox's check phase every time the suite ran together.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while crate::service::running(&services, "demo").is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the child never came up as `sleep 30` — the pre-exec window this guards"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let err = format!(
            "{:#}",
            plugins_uninstall(&cfg, "demo", true, &services).unwrap_err()
        );
        assert!(
            err.contains("service running") && err.contains("plugins service stop demo"),
            "even --force refuses a live process, and names the verb that stops it: {err}"
        );
        assert!(root.path().join("plugins/demo").exists(), "nothing is deleted underneath it");

        let _ = child.kill();
        let _ = child.wait();
        plugins_uninstall(&cfg, "demo", true, &services).unwrap();
        assert!(!root.path().join("plugins/demo").exists(), "with the process gone it proceeds");
    }

    #[test]
    fn an_uninstall_of_a_hand_installed_plugin_invents_no_record() {
        let root = tempfile::tempdir().unwrap();
        let cfg = cfg(root.path());
        fixture_plugin(root.path(), "demo", &["echo"]);

        plugins_uninstall(&cfg, "demo", false, &root.path().join("services")).unwrap();

        assert!(!root.path().join("plugins/demo").exists(), "the directory goes");
        assert!(
            !eidolon_rune::store::Store::beside(&cfg.plugins_dir()).exists(),
            "and no record file is invented to say so"
        );

        let err = format!(
            "{:#}",
            plugins_uninstall(&cfg, "ghost", false, &root.path().join("services")).unwrap_err()
        );
        assert!(err.contains("nothing named `ghost`"), "{err}");
    }
}