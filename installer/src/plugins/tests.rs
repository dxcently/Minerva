//! Needs a real eidolon: set MINERVA_TEST_EIDOLON to one (the build.sh musl one will do). Without it the lifecycle test says so
//! and passes, because a test that silently tests nothing is worse than one that says it did not run. Its check that plugins/
//! carries term runs first, with or without an eidolon: that one FAILS when term is missing, it is never skipped.
//! It points XDG_* at a temp dir, so nothing of the person's own config is read or written.

use super::*;

fn repo_plugins() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../eidolon-plugins")
}

/// The source the perch's agent gate writes the term token to (perch/src/agent/README.md), spelled out here and not taken from
/// plugins.rs, so a wrong constant there fails these tests.
const TERM_TOKEN_SPEC: &str = "file:~/.config/eidolon/minerva-term.token";

/// Minerva's own plugins (term, and mail once it lands), a second source beside eidolon-plugins/.
fn minerva_plugins() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins")
}

/// The folders of plugins/ that build.sh bundles: every one with a plugin.rn.
fn minerva_plugin_names() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(minerva_plugins())
        .map(|rd| rd.flatten().filter(|e| e.path().join("plugin.rn").is_file()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn bundle_plugins(dst: &Path) {
    for n in ["browser", "jev", "subagent"] {
        copy_tree(&repo_plugins().join(n), &dst.join(n)).unwrap();
    }
    let _ = fs::remove_dir_all(dst.join("browser/service"));
    let _ = fs::remove_file(dst.join("browser/package.nix"));
    // what build.sh bundles from plugins/: every folder with a plugin.rn, without its tests
    for n in minerva_plugin_names() {
        copy_tree(&minerva_plugins().join(&n), &dst.join(&n)).unwrap();
        let _ = fs::remove_dir_all(dst.join(&n).join("tests"));
    }
}

/// A sixth plugin that no rule in trust_one names: a manifest and one read tool, written into the bundle beside the others.
fn write_sixth(src: &Path) {
    let d = src.join("zzprobe");
    fs::create_dir_all(d.join("tools")).unwrap();
    fs::write(d.join("plugin.rn"), "pub fn manifest() {\n    #{\n        name: \"zzprobe\",\n        version: \"0.0.1\",\n    }\n}\n").unwrap();
    fs::write(
        d.join("tools/run.rn"),
        "pub fn manifest() {\n    #{\n        name: \"zzprobe_run\",\n        description: \"a probe\",\n        approval: \"read_only\",\n        input_schema: #{ \"type\": \"object\", \"properties\": #{} },\n    }\n}\n\npub async fn call(input) {\n    Ok(\"ok\")\n}\n",
    )
    .unwrap();
}

/// A stand-in for eidolon that only writes down what it was asked, one line per call: the arguments joined by single spaces.
fn fake_eidolon(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("argv.log");
    let bin = dir.join("fake-eidolon");
    fs::write(&bin, format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display())).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

fn asked(log: &Path) -> Vec<String> {
    fs::read_to_string(log).unwrap_or_default().lines().map(str::to_string).collect()
}

fn count(env: &PluginEnv, needle: &str) -> usize {
    env.eid_ok(&["tools"]).unwrap().matches(&format!("\"name\": \"{needle}")).count()
}

/// D6 on a real permits store (`<root>/cfg/eidolon/policy.permits.rn`): term's list, read, open and close are vouched, term_send is
/// NOT, and all five verbs are granted the token file.
fn assert_term_d6(root: &Path, env: &PluginEnv) {
    let permits = fs::read_to_string(root.join("cfg/eidolon/policy.permits.rn")).unwrap();
    let vouch = permits.lines().find(|l| l.starts_with("const VOUCHE")).unwrap_or("");
    let grant = permits.lines().find(|l| l.starts_with("const GRANT")).unwrap_or("");
    for v in TERM_VOUCHED {
        assert!(vouch.contains(&format!("verb: \"term_{v}\"")), "term_{v} should be vouched: {vouch}");
    }
    assert!(!vouch.contains("term_send"), "term_send must never be vouched: {vouch}");
    for v in TERM_VERBS {
        assert!(grant.contains(&format!("tool: \"term_{v}\", source: \"{TERM_TOKEN_SPEC}\"")), "term_{v} should be granted the token file: {grant}");
    }
    let listing = env.eid_ok(&["plugins"]).unwrap();
    let line = |verb: &str| listing.lines().find(|l| l.trim_start().starts_with(verb)).unwrap_or("").to_string();
    for v in TERM_VOUCHED {
        let l = line(&format!("term_{v} "));
        assert!(l.contains("vouched by") && !l.contains("script changed") && !l.contains("unvouched"), "{l}");
    }
    assert!(line("term_send ").contains("unvouched"), "term_send is listed as asking: {}", line("term_send "));
}

/// The tests that point XDG_CONFIG_HOME and friends at a temp dir take this: `sync` reads them, and this test process has one environment.
static XDG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn lifecycle_install_trust_update_foreign_and_remove() {
    // plugins/ has to carry term, with or without an eidolon to drive: a checkout without it fails here, it is not skipped
    assert!(minerva_plugin_names().iter().any(|n| n == "term"), "plugins/ has no term: the lifecycle test has to see it");
    let Some(eid) = std::env::var_os("MINERVA_TEST_EIDOLON") else {
        println!("SKIPPED: MINERVA_TEST_EIDOLON is not set (no eidolon to drive)");
        return;
    };
    if !repo_plugins().join("browser/plugin.rn").exists() {
        println!("SKIPPED: no eidolon-plugins next to the installer");
        return;
    }
    let _xdg = XDG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", r.join("cfg"));
        std::env::set_var("XDG_DATA_HOME", r.join("data"));
        std::env::set_var("XDG_STATE_HOME", r.join("state"));
        std::env::set_var("XDG_CACHE_HOME", r.join("cache"));
    }
    let src = r.join("bundle-plugins");
    bundle_plugins(&src);
    write_sixth(&src);
    let env = PluginEnv { eidolon: PathBuf::from(eid), bin_dir: r.join("bin"), src: src.clone(), layout: Layout::new(r.join("prefix")) };
    let pdir = plugins_dir().unwrap();

    // the member already has a plugin called jev, of their own
    fs::create_dir_all(pdir.join("jev")).unwrap();
    copy_tree(&src.join("jev"), &pdir.join("jev")).unwrap();
    fs::write(pdir.join("jev/MINE.txt"), "mine").unwrap();

    // 1. first sync: browser and subagent are ours, jev is left alone
    let lines = sync(&env, SyncOpts { version: "0.1.0-gt", if_needed: false, ask_replace: None }).unwrap();
    let text = lines.join("\n");
    assert!(text.contains("jev: you already have a plugin of that name; left exactly as it is"), "{text}");
    assert!(text.contains("browser: installed") && text.contains("subagent: installed"), "{text}");
    assert!(pdir.join("jev/MINE.txt").exists());
    assert_eq!(count(&env, "browser_"), 7);
    assert_eq!(count(&env, "subagent_"), 7);
    let listing = env.eid_ok(&["plugins"]).unwrap();
    assert!(listing.contains("vouched"), "{listing}");
    let st = load_state(&env.layout);
    // ours: browser and subagent, and every folder of plugins/ the bundle carries (term; mail when it lands)
    let mut ours = vec!["browser".to_string(), "subagent".to_string(), "zzprobe".to_string()];
    ours.extend(minerva_plugin_names());
    ours.sort();
    let has_mail = ours.iter().any(|n| n == "mail");
    assert!(text.contains("term: installed"), "{text}");
    assert_eq!(count(&env, "term_"), 5);
    assert_term_d6(r, &env);
    // the sixth plugin is installed and vouched for nothing: no rule names it, so eidolon's listing has its verb asking
    assert!(text.contains("zzprobe: installed") && text.contains("zzprobe: installed, not vouched (no rule in trust_one)"), "{text}");
    let permits = fs::read_to_string(r.join("cfg/eidolon/policy.permits.rn")).unwrap();
    let vouch = permits.lines().find(|l| l.starts_with("const VOUCHE")).unwrap_or("");
    assert!(!vouch.contains("zzprobe"), "a plugin no rule names is vouched for nothing: {vouch}");
    let listing = env.eid_ok(&["plugins"]).unwrap();
    let verb_line = listing.lines().find(|l| l.trim_start().starts_with("zzprobe_run ")).unwrap_or("");
    assert!(verb_line.contains("unvouched"), "zzprobe_run should be listed as asking: {verb_line}");
    if has_mail {
        // mail is granted its token file and never vouched: it asks in auto and in manual
        let permits = fs::read_to_string(r.join("cfg/eidolon/policy.permits.rn")).unwrap();
        let vouch = permits.lines().find(|l| l.starts_with("const VOUCHE")).unwrap_or("");
        let grant = permits.lines().find(|l| l.starts_with("const GRANT")).unwrap_or("");
        assert!(!vouch.contains("mail_send"), "mail_send must never be vouched: {vouch}");
        assert!(grant.contains("tool: \"mail_send\", source: \"file:~/.config/eidolon/minerva-mail.token\""), "{grant}");
    }
    assert_eq!(st.plugins.keys().cloned().collect::<Vec<_>>(), ours);
    assert_eq!(st.plugins["browser"].how, "stock", "went through `eidolon plugins install` (git is needed for that)");

    // 2. a second sync with nothing new says nothing
    assert!(sync(&env, SyncOpts { version: "0.1.0-gt", if_needed: true, ask_replace: None }).unwrap().is_empty());

    // 3. an update: the bundled browser changes; our copy is replaced and stays trusted
    let open = src.join("browser/tools/open.rn");
    let mut body = fs::read_to_string(&open).unwrap();
    body.push_str("\n// a change in a newer Minerva\n");
    fs::write(&open, body).unwrap();
    let lines = sync(&env, SyncOpts { version: "0.2.0-gt", if_needed: true, ask_replace: None }).unwrap();
    assert!(lines.join("\n").contains("browser: installed"), "{lines:?}");
    assert!(fs::read_to_string(pdir.join("browser/tools/open.rn")).unwrap().contains("a change in a newer Minerva"));
    let tools = env.eid_ok(&["plugins"]).unwrap();
    let browser_part: String = tools.lines().skip_while(|l| !l.starts_with("browser ")).take_while(|l| !l.starts_with("jev ")).collect::<Vec<_>>().join("\n");
    assert!(browser_part.contains("browser_open") && browser_part.contains("vouched"), "still trusted after the update: {browser_part}");
    assert!(!browser_part.contains("not vouched") && !browser_part.contains("unvouched"), "{browser_part}");

    // 3a. an update of term: replaced, and trust is re-applied the same way, so term_send is still not vouched
    let send = src.join("term/tools/send.rn");
    let mut body = fs::read_to_string(&send).unwrap();
    body.push_str("\n// a change in a newer Minerva\n");
    fs::write(&send, body).unwrap();
    let lines = sync(&env, SyncOpts { version: "0.2.1-gt", if_needed: true, ask_replace: None }).unwrap();
    assert!(lines.join("\n").contains("term: installed"), "{lines:?}");
    assert!(fs::read_to_string(pdir.join("term/tools/send.rn")).unwrap().contains("a change in a newer Minerva"));
    assert_term_d6(r, &env);

    // 3b. the member edits OUR installed subagent: it is neither re-trusted nor replaced, and an if_needed pass still looks
    let sub_rn = pdir.join("subagent/plugin.rn");
    let original = fs::read_to_string(&sub_rn).unwrap();
    fs::write(&sub_rn, format!("{original}\n// my own change\n")).unwrap();
    let lines = sync(&env, SyncOpts { version: "0.2.0-gt", if_needed: true, ask_replace: None }).unwrap();
    let text = lines.join("\n");
    assert!(text.contains("subagent: your copy was changed since Minerva installed it") && text.contains("not re-trusted"), "{text}");
    assert!(text.contains("earlier vouches of unchanged verbs and the token grants still stand"), "{text}");
    assert!(!text.contains("subagent: trusted"), "{text}");
    assert!(fs::read_to_string(&sub_rn).unwrap().contains("my own change"), "left as it is");
    fs::write(&sub_rn, original).unwrap();
    let lines = sync(&env, SyncOpts { version: "0.2.0-gt", if_needed: true, ask_replace: None }).unwrap();
    assert!(!lines.join("\n").contains("changed since"), "back to what we installed: fine again: {lines:?}");

    // 4. uninstall removes only ours; the member's jev stays
    let lines = remove_ours(&env).unwrap();
    assert!(lines.iter().any(|l| l == "browser: removed") && lines.iter().any(|l| l == "subagent: removed"), "{lines:?}");
    assert!(!pdir.join("browser").exists() && !pdir.join("subagent").exists());
    assert!(lines.iter().any(|l| l == "term: removed"), "{lines:?}");
    assert!(!pdir.join("term").exists());
    let permits = fs::read_to_string(r.join("cfg/eidolon/policy.permits.rn")).unwrap();
    let rows: Vec<&str> = permits.lines().filter(|l| l.starts_with("const VOUCHE") || l.starts_with("const GRANT")).collect();
    assert!(rows.iter().all(|l| !l.contains("term_")), "uninstall leaves no term vouch or grant: {rows:?}");
    assert!(!lines.iter().any(|l| l.contains("still on record")), "a clean uninstall lists nothing left: {lines:?}");
    assert!(pdir.join("jev/MINE.txt").exists());
    assert!(!state_path(&env.layout).exists());

    // 5. with a yes, the member's own is replaced and becomes ours
    let mut asked = Vec::new();
    let mut yes = |n: &str| {
        asked.push(n.to_string());
        true
    };
    let lines = sync(&env, SyncOpts { version: "0.2.0-gt", if_needed: false, ask_replace: Some(&mut yes) }).unwrap();
    assert_eq!(asked, vec!["jev".to_string()]);
    assert!(lines.join("\n").contains("jev: installed"), "{lines:?}");
    assert!(!pdir.join("jev/MINE.txt").exists());
    assert_eq!(count(&env, "jev_"), 15);
    remove_ours(&env).unwrap();
    assert!(fs::read_dir(&pdir).map(|d| d.flatten().all(|e| e.file_name().to_string_lossy().starts_with('.'))).unwrap_or(true));
}

#[test]
fn a_config_that_moves_the_plugins_dir_is_respected() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("cfg");
    fs::create_dir_all(cfg.join("eidolon")).unwrap();
    fs::write(cfg.join("eidolon/config.toml"), "plugins_dir = '/somewhere'\n").unwrap();
    // This test must not race the lifecycle test over XDG_CONFIG_HOME, so it checks the parser on a path directly.
    let text = fs::read_to_string(cfg.join("eidolon/config.toml")).unwrap();
    assert!(text.lines().any(|l| l.trim_start().starts_with("plugins_dir")));
}

#[test]
fn hash_and_version_helpers() {
    let t = tempfile::tempdir().unwrap();
    fs::create_dir_all(t.path().join("a/sub")).unwrap();
    fs::write(t.path().join("a/plugin.rn"), "pub fn manifest() {\n    #{\n        name: \"a\",\n        version: \"1.2.3\",\n    }\n}\n").unwrap();
    fs::write(t.path().join("a/sub/x"), "1").unwrap();
    let h1 = hash_tree(&t.path().join("a")).unwrap();
    assert_eq!(version_of(&t.path().join("a")), "1.2.3");
    fs::write(t.path().join("a/sub/x"), "2").unwrap();
    assert_ne!(h1, hash_tree(&t.path().join("a")).unwrap());
}

fn fake_env(t: &Path) -> (PluginEnv, PathBuf) {
    let (bin, log) = fake_eidolon(t);
    (PluginEnv { eidolon: bin, bin_dir: t.join("bin"), src: t.join("src"), layout: Layout::new(t.join("prefix")) }, log)
}

/// D6, as the argv eidolon is given: term takes back every vouch first, then is vouched verb by verb (list, read, open, close) and
/// NEVER send, and all five verbs are granted the token file. This is the test that fails if anything makes term_send run in auto
/// without an ask.
#[test]
fn term_is_vouched_per_verb_and_send_is_never_vouched() {
    let t = tempfile::tempdir().unwrap();
    let (env, log) = fake_env(t.path());
    let mut out = Vec::new();
    env.trust_one("term", &mut out).unwrap();
    let mut want = vec!["plugins untrust term".to_string()];
    want.extend(["list", "read", "open", "close"].iter().map(|v| format!("plugins trust term {v}")));
    want.extend(["list", "open", "read", "send", "close"].iter().map(|v| format!("plugins grant term_{v} {TERM_TOKEN_SPEC}")));
    assert_eq!(asked(&log), want);
    assert_eq!(out, ["term: trusted list, read, open and close; term_send is not vouched and asks"]);
}

/// Whatever the plugin, no call ever vouches a send verb, and no bare `plugins trust <name>` is issued for a name outside browser,
/// jev and subagent (a bare trust vouches every verb of the plugin, so for term it would vouch term_send).
#[test]
fn no_plugin_name_makes_a_vouch_for_term_send() {
    let t = tempfile::tempdir().unwrap();
    let (env, log) = fake_env(t.path());
    let mut out = Vec::new();
    for n in ["term", "mail", "browser", "jev", "subagent", "other"] {
        env.trust_one(n, &mut out).unwrap();
    }
    let all = asked(&log);
    assert!(!all.is_empty());
    for l in &all {
        let Some(rest) = l.strip_prefix("plugins trust ") else { continue };
        match rest.split_once(' ') {
            // a bare trust vouches the whole plugin: only the three that were always vouched whole
            None => assert!(["browser", "jev", "subagent"].contains(&rest), "a bare trust of {rest} vouches every verb: {l}"),
            // a verb trust: only term's vouched verbs, never a send
            Some((name, verb)) => {
                assert_eq!(name, "term", "{l}");
                assert!(TERM_VOUCHED.contains(&verb), "a vouch of a verb that is not vouched: {l}");
            }
        }
    }
    assert!(!all.iter().any(|l| l.contains("other")), "a plugin with no rule got a call: {all:?}");
    // the vouch calls for term are exactly the four, and the grant calls cover all five
    let trusts: Vec<&String> = all.iter().filter(|l| l.starts_with("plugins trust term")).collect();
    assert_eq!(trusts.len(), 4, "{trusts:?}");
    assert_eq!(all.iter().filter(|l| l.starts_with("plugins grant term_")).count(), 5);
}

/// mail: take back any vouch, then a grant of the token file and no trust at all, so mail_send asks in manual and in auto.
#[test]
fn mail_is_granted_its_token_and_never_vouched() {
    let t = tempfile::tempdir().unwrap();
    let (env, log) = fake_env(t.path());
    let mut out = Vec::new();
    env.trust_one("mail", &mut out).unwrap();
    assert_eq!(asked(&log), ["plugins untrust mail", "plugins grant mail_send file:~/.config/eidolon/minerva-mail.token"]);
    assert!(!asked(&log).iter().any(|l| l.starts_with("plugins trust")), "{:?}", asked(&log));
}

/// Every folder the bundle carries has an explicit rule in trust_one. A plugins/<name> without one is installed and vouches nothing,
/// and this test is what makes that a decision someone writes down: it fails until the arm exists.
#[test]
fn every_bundled_plugin_has_an_explicit_arm() {
    let t = tempfile::tempdir().unwrap();
    let (env, _) = fake_env(t.path());
    let mut names: Vec<String> = minerva_plugin_names();
    assert!(names.iter().any(|n| n == "term"), "plugins/ has no term");
    names.extend(["browser", "jev", "subagent"].iter().map(|n| n.to_string()));
    for n in names {
        let mut out = Vec::new();
        env.trust_one(&n, &mut out).unwrap();
        assert!(!out.iter().any(|l| l.contains("no rule in trust_one")), "{n} has no explicit arm in trust_one: {out:?}");
    }
}

/// A name with no arm vouches and grants nothing: no call at all, and one line that says so.
#[test]
fn an_unknown_plugin_is_installed_and_vouches_nothing() {
    let t = tempfile::tempdir().unwrap();
    let (env, log) = fake_env(t.path());
    let mut out = Vec::new();
    env.trust_one("other", &mut out).unwrap();
    assert!(asked(&log).is_empty(), "{:?}", asked(&log));
    assert_eq!(out, ["other: installed, not vouched (no rule in trust_one)"]);
}

/// A fake eidolon that answers `plugins untrust` with a failure saying `answer` (the call is still written down).
fn fake_eidolon_untrust(dir: &Path, answer: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("argv.log");
    let bin = dir.join("fake-eidolon");
    let body = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$1 $2\" = 'plugins untrust' ]; then echo '{answer}' >&2; exit 1; fi\nexit 0\n",
        log.display()
    );
    fs::write(&bin, body).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

/// eidolon's "no vouch on record" is what `untrust` says on a first install: nothing to take back, not a failure, so the pass goes on.
#[test]
fn an_untrust_with_nothing_to_take_back_is_not_a_failure() {
    let t = tempfile::tempdir().unwrap();
    let (bin, log) = fake_eidolon_untrust(t.path(), "no vouch on record for term");
    let env = PluginEnv { eidolon: bin, bin_dir: t.path().join("bin"), src: t.path().join("src"), layout: Layout::new(t.path().join("prefix")) };
    let mut out = Vec::new();
    env.trust_one("term", &mut out).unwrap();
    assert_eq!(asked(&log).len(), 1 + 4 + 5, "{:?}", asked(&log));
    assert_eq!(out.len(), 1);
}

/// Any other failure of `untrust` stops the pass before a single vouch: tools keep asking, and the error says which call failed.
#[test]
fn a_failed_untrust_stops_the_term_pass_before_any_vouch() {
    let t = tempfile::tempdir().unwrap();
    let (bin, log) = fake_eidolon_untrust(t.path(), "could not write the permits store");
    let env = PluginEnv { eidolon: bin, bin_dir: t.path().join("bin"), src: t.path().join("src"), layout: Layout::new(t.path().join("prefix")) };
    let mut out = Vec::new();
    let e = env.trust_one("term", &mut out).unwrap_err().to_string();
    assert!(e.contains("plugins untrust term failed"), "{e}");
    assert_eq!(asked(&log), ["plugins untrust term"]);
    assert!(out.is_empty());
}

/// The other plugins are exactly as before: browser's six grants and service approval, jev and subagent a plain trust.
#[test]
fn the_other_plugins_are_trusted_as_before() {
    let t = tempfile::tempdir().unwrap();
    let (env, log) = fake_env(t.path());
    let mut out = Vec::new();
    env.trust_one("jev", &mut out).unwrap();
    assert_eq!(asked(&log), ["plugins trust jev"]);
    fs::remove_file(&log).unwrap();
    env.trust_one("browser", &mut out).unwrap();
    let mut want = vec!["plugins trust browser".to_string()];
    for v in BROWSER_VERBS {
        want.push(format!("plugins grant browser_{v} file:~/.config/eidolon/browser.token"));
    }
    want.push("plugins service approve browser".into());
    assert_eq!(asked(&log), want);
    assert_eq!(out, ["jev: trusted", "browser: trusted"]);
}

/// A failed stock call is an error, not a silent half-trust: the first call (the untrust) fails, so no vouch and no grant is made.
#[test]
fn a_failing_eidolon_stops_the_term_trust() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let (mut env, _) = fake_env(t.path());
    let bin = t.path().join("failing");
    fs::write(&bin, "#!/bin/sh\necho no >&2\nexit 1\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    env.eidolon = bin;
    let mut out = Vec::new();
    let e = env.trust_one("term", &mut out).unwrap_err().to_string();
    assert!(e.contains("plugins untrust term failed"), "{e}");
    assert!(out.is_empty());
}

/// The rows of the permits store that still belong to a plugin: its vouches by `plugin`, the grants of its verbs. Only those.
#[test]
fn rows_left_names_only_what_is_still_on_record_for_the_plugin() {
    let t = tempfile::tempdir().unwrap();
    let store = t.path().join("policy.permits.rn");
    fs::write(
        &store,
        "const VOUCHE = [#{ plugin: \"term\", verb: \"term_list\", hash: \"h\" }, #{ plugin: \"subagent\", verb: \"subagent_run\", hash: \"h\" }];\n\
         const GRANT = [#{ tool: \"term_send\", source: \"file:~/.config/eidolon/minerva-term.token\" }, #{ tool: \"other_tool\", source: \"env:X\" }];\n\
         const ATTEST = [];\n",
    )
    .unwrap();
    assert_eq!(rows_left(&store, "term", &granted_verbs("term")), ["vouch of term_list", "grant of term_send on file:~/.config/eidolon/minerva-term.token"]);
    assert!(rows_left(&store, "mail", &granted_verbs("mail")).is_empty());
    assert!(rows_left(&t.path().join("absent.rn"), "term", &granted_verbs("term")).is_empty());
}

/// A member who deleted term's folder leaves its vouches behind: eidolon's untrust bails on the missing folder. The uninstall says
/// so and lists the rows still on record, rather than reporting a clean removal.
#[test]
fn remove_ours_says_what_it_could_not_take_back() {
    let t = tempfile::tempdir().unwrap();
    let (bin, log) = fake_eidolon_untrust(t.path(), "no installed plugin term under the plugins dir");
    let env = PluginEnv { eidolon: bin, bin_dir: t.path().join("bin"), src: t.path().join("src"), layout: Layout::new(t.path().join("prefix")) };
    let store = t.path().join("policy.permits.rn");
    fs::write(&store, "const VOUCHE = [#{ plugin: \"term\", verb: \"term_open\", hash: \"h\" }];\nconst GRANT = [];\n").unwrap();
    let mut st = State::default();
    st.plugins.insert("term".into(), Ours { version: "0.1.0".into(), hash: "h".into(), how: "stock".into(), installed: "i".into() });
    save_state(&env.layout, &st).unwrap();
    let lines = remove_ours_from(&env, Some(&store)).unwrap();
    assert!(asked(&log).contains(&"plugins untrust term".to_string()), "{:?}", asked(&log));
    assert!(lines.iter().any(|l| l.starts_with("term: could not take its vouches back (no installed plugin term")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("term: removed")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("term: still on record in ") && l.ends_with(": vouch of term_open")), "{lines:?}");
}

/// Uninstall takes back what install put: the vouches (`untrust`, the whole plugin) and every grant, for term and mail.
#[test]
fn remove_ours_revokes_the_term_and_mail_grants() {
    let t = tempfile::tempdir().unwrap();
    let (env, log) = fake_env(t.path());
    let mut st = State::default();
    for n in ["term", "mail"] {
        st.plugins.insert(n.into(), Ours { version: "0.1.0".into(), hash: "h".into(), how: "stock".into(), installed: "i".into() });
    }
    save_state(&env.layout, &st).unwrap();
    // no store named: this test does not look at the member's own permits file
    let lines = remove_ours_from(&env, None).unwrap();
    assert_eq!(lines, ["mail: removed", "term: removed"]);
    let all = asked(&log);
    for want in [
        "plugins untrust term".to_string(),
        "plugins untrust mail".to_string(),
        "plugins uninstall term --force".to_string(),
        "plugins uninstall mail --force".to_string(),
        "plugins revoke mail_send file:~/.config/eidolon/minerva-mail.token".to_string(),
    ] {
        assert!(all.contains(&want), "{want} is missing from {all:?}");
    }
    for v in TERM_VERBS {
        let want = format!("plugins revoke term_{v} {TERM_TOKEN_SPEC}");
        assert!(all.contains(&want), "{want} is missing from {all:?}");
    }
    assert!(!state_path(&env.layout).exists(), "the record of what is ours goes with them");
}

/// A fake eidolon that fails the call `needle` (the whole argv, space-joined) the first time only, a flag file saying it has failed once.
/// Every call is written down, as `fake_eidolon` does.
fn fake_eidolon_failing_once(dir: &Path, needle: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("argv.log");
    let flag = dir.join("failed-once");
    let bin = dir.join("fake-eidolon");
    let body = format!(
        "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$*\" = '{needle}' ] && [ ! -e '{}' ]; then touch '{}'; echo 'could not write' >&2; exit 1; fi\nexit 0\n",
        log.display(),
        flag.display(),
        flag.display()
    );
    fs::write(&bin, body).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    (bin, log)
}

/// A failed trust right after a first install is recovered by the next pass. The plugin is on record before its trust is applied, so
/// the next pass finds it as ours and unchanged and runs its whole arm again. Without that record the folder would be the member's
/// own on the next pass, and term would stay untrusted until the member deleted it.
#[test]
fn a_failed_first_trust_is_retried_by_the_next_pass() {
    let _xdg = XDG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", r.join("cfg"));
        std::env::set_var("XDG_DATA_HOME", r.join("data"));
        std::env::set_var("XDG_STATE_HOME", r.join("state"));
        std::env::set_var("XDG_CACHE_HOME", r.join("cache"));
    }
    let src = r.join("bundle-plugins");
    copy_tree(&minerva_plugins().join("term"), &src.join("term")).unwrap();
    let (bin, log) = fake_eidolon_failing_once(r, "plugins trust term list");
    let env = PluginEnv { eidolon: bin, bin_dir: r.join("bin"), src, layout: Layout::new(r.join("prefix")) };
    let pdir = plugins_dir().unwrap();

    // the first pass lands term and records it, then its first trust fails: the pass stops with that error
    let e = sync(&env, SyncOpts { version: "0.1.0-rt", if_needed: false, ask_replace: None }).unwrap_err().to_string();
    assert!(e.contains("plugins trust term list failed"), "{e}");
    assert!(pdir.join("term/plugin.rn").exists());
    let st = load_state(&env.layout);
    assert!(st.plugins.contains_key("term"), "a plugin whose trust failed is still on record");
    assert!(st.stamp.is_empty(), "a pass that failed does not stamp the version");

    // the next pass finds term ours and unchanged, and runs its whole arm again: the untrust is asked again
    let lines = sync(&env, SyncOpts { version: "0.1.0-rt", if_needed: true, ask_replace: None }).unwrap();
    let text = lines.join("\n");
    assert!(text.contains("term: trusted list, read, open and close"), "{text}");
    assert!(!text.contains("left exactly as it is"), "{text}");
    let calls = asked(&log);
    assert_eq!(calls.iter().filter(|c| c.as_str() == "plugins untrust term").count(), 2, "{calls:?}");
    assert_eq!(load_state(&env.layout).stamp, "0.1.0-rt");
}
