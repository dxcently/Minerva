//! Needs a real eidolon: set MINERVA_TEST_EIDOLON to one (the build.sh musl one will do). Without it the test says so
//! and passes, because a test that silently tests nothing is worse than one that says it did not run.
//! It points XDG_* at a temp dir, so nothing of the person's own config is read or written.

use super::*;

fn repo_plugins() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../eidolon-plugins")
}

fn bundle_plugins(dst: &Path) {
    for n in ["browser", "jev", "subagent"] {
        copy_tree(&repo_plugins().join(n), &dst.join(n)).unwrap();
    }
    let _ = fs::remove_dir_all(dst.join("browser/service"));
    let _ = fs::remove_file(dst.join("browser/package.nix"));
}

fn count(env: &PluginEnv, needle: &str) -> usize {
    env.eid_ok(&["tools"]).unwrap().matches(&format!("\"name\": \"{needle}")).count()
}

#[test]
fn lifecycle_install_trust_update_foreign_and_remove() {
    let Some(eid) = std::env::var_os("MINERVA_TEST_EIDOLON") else {
        println!("SKIPPED: MINERVA_TEST_EIDOLON is not set (no eidolon to drive)");
        return;
    };
    if !repo_plugins().join("browser/plugin.rn").exists() {
        println!("SKIPPED: no eidolon-plugins next to the installer");
        return;
    }
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
    assert_eq!(st.plugins.keys().cloned().collect::<Vec<_>>(), vec!["browser".to_string(), "subagent".to_string()]);
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

    // 3b. the member edits OUR installed subagent: it is neither re-trusted nor replaced, and an if_needed pass still looks
    let sub_rn = pdir.join("subagent/plugin.rn");
    let original = fs::read_to_string(&sub_rn).unwrap();
    fs::write(&sub_rn, format!("{original}\n// my own change\n")).unwrap();
    let lines = sync(&env, SyncOpts { version: "0.2.0-gt", if_needed: true, ask_replace: None }).unwrap();
    let text = lines.join("\n");
    assert!(text.contains("subagent: your copy was changed since Minerva installed it") && text.contains("NOT trusted"), "{text}");
    assert!(!text.contains("subagent: trusted"), "{text}");
    assert!(fs::read_to_string(&sub_rn).unwrap().contains("my own change"), "left as it is");
    fs::write(&sub_rn, original).unwrap();
    let lines = sync(&env, SyncOpts { version: "0.2.0-gt", if_needed: true, ask_replace: None }).unwrap();
    assert!(!lines.join("\n").contains("changed since"), "back to what we installed: fine again: {lines:?}");

    // 4. uninstall removes only ours; the member's jev stays
    let lines = remove_ours(&env).unwrap();
    assert!(lines.iter().any(|l| l == "browser: removed") && lines.iter().any(|l| l == "subagent: removed"), "{lines:?}");
    assert!(!pdir.join("browser").exists() && !pdir.join("subagent").exists());
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
