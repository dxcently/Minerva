//! `/hub/meta/*`: the sources of the loaded and diff tabs (slice R3).
//!
//! The perch runs `eidolon [--config C] tools|plugins|policy-rows` through the stand-in (`src/bin/fake_eidolon/meta.rs`), which records
//! every call as `helper <name> <args> config=<bool> argv=<argv>`; a test reads those lines to count the perch's calls and to see the exact
//! argv. Everything here that asks eidolon needs the stand-in, so it is gated on `--features test-bins`; the gate itself is not.

mod common;

use common::*;

/// The six routes, which the gate covers like every `/hub` path: no credential is the empty 401 whatever the path or the query is.
#[test]
fn meta_is_behind_the_token_and_knows_only_its_six_routes() {
    let perch = Perch::start();
    for path in ["tools", "plugins", "policy", "policy?mode=manual", "connectors", "home", "git?session=x", "nothing", "tools/"] {
        let bare = Request::new(&perch.addr, "GET", &format!("/hub/meta/{path}")).send(&perch.addr);
        assert_eq!((bare.status, bare.body.as_str()), (401, ""), "{path} with no credential: {}", bare.head);
        let wrong = Request::new(&perch.addr, "GET", &format!("/hub/meta/{path}")).bearer("Bearer nope").send(&perch.addr);
        assert_eq!((wrong.status, wrong.body.as_str()), (401, ""), "{path} with a wrong token: {}", wrong.head);
    }
    let unknown = perch.get("/hub/meta/nothing");
    assert_eq!((unknown.status, unknown.body.as_str()), (404, ""), "a route nobody serves");
    assert_eq!(perch.get("/hub/meta/tools/").status, 404, "a trailing slash is another path");
    assert_eq!(perch.post("/hub/meta/tools").status, 404, "only GET has a route");
    assert_eq!(perch.get("/hub/meta/connectors?x=1").status, 400, "a query on a route that takes none");
    assert_eq!(perch.get("/hub/meta/home?mode=auto").status, 400);
    assert_eq!(perch.get("/hub/meta/git").status, 400, "git needs its session");
}

#[cfg(feature = "test-bins")]
mod fake {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    /// A perch with a Minerva home (its `minerva.toml` is `minerva`, its member config is `member`), the stand-in as eidolon, and the
    /// folder everything lives under, which is a `--root` so a chat can be started in a subfolder of it.
    struct Rig {
        base: tempfile::TempDir,
        home: PathBuf,
        perch: Perch,
    }

    #[derive(Default)]
    struct Setup<'a> {
        member: &'a str,
        minerva: &'a str,
        env: &'a [(&'a str, &'a str)],
        args: &'a [&'a str],
        /// `$HOME` is the folder the Minerva home is in, so the home's files are written `~/Minerva/...`.
        home_is_base: bool,
    }

    impl Setup<'_> {
        fn start(self) -> Rig {
            let base = tempfile::tempdir().unwrap();
            let home = base.path().join("Minerva");
            std::fs::create_dir(&home).unwrap();
            if !self.minerva.is_empty() {
                std::fs::write(home.join("minerva.toml"), self.minerva).unwrap();
            }
            let home = home.canonicalize().unwrap();
            let root = base.path().canonicalize().unwrap();
            let mut args: Vec<std::ffi::OsString> = vec!["--root".into(), root.into(), "--home".into(), home.clone().into()];
            args.extend(self.args.iter().map(std::ffi::OsString::from));
            let env = self.env.iter().map(|(k, v)| (k.to_string(), v.to_string()));
            let mut builder = Perch::builder().hub().member_config(self.member).args(args).envs(env);
            if self.home_is_base {
                builder = builder.env("HOME", base.path().canonicalize().unwrap().display().to_string());
            }
            Rig { perch: builder.start(), base, home }
        }
    }

    fn rig() -> Rig {
        Setup { minerva: "default_mode = \"auto\"\n", ..Setup::default() }.start()
    }

    fn json_of(answer: &Answer) -> serde_json::Value {
        assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
        serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON answer: {e}: {:?}", answer.body))
    }

    /// The stand-in's record of the perch's calls to `eidolon <name>`: `helper <name> <args> config=<bool> argv=<argv>`.
    fn calls(perch: &Perch, name: &str) -> Vec<String> {
        perch.fake_helpers().into_iter().filter(|line| line.starts_with(&format!("helper {name} "))).collect()
    }

    fn argv_of(line: &str) -> &str {
        line.split_once(" argv=").map(|(_, argv)| argv).unwrap_or_else(|| panic!("an argv in {line:?}"))
    }

    // -----------------------------------------------------------------------
    // tools, plugins
    // -----------------------------------------------------------------------

    /// The list is eidolon's, in its order, with the first line of each description, and a plugin's tools name the plugin the listing
    /// gave. The command is run as a door is run (`--config <home>/config.toml`) and never with `--live`.
    ///
    /// Mutation: pass `--live` to `eidolon tools`, or drop `--config`, and the argv line goes red.
    #[test]
    fn tools_are_eidolons_own_list_run_with_the_homes_config_and_never_live() {
        let rig = rig();
        let answer = rig.perch.get("/hub/meta/tools");
        assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
        let list = json_of(&answer);
        let rows: Vec<_> = list
            .as_array()
            .expect("an array")
            .iter()
            .map(|t| (t["name"].as_str().unwrap(), t["approval"].as_str().unwrap(), t["plugin"].as_str(), t["desc"].as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("read", "read_only", None, Some("Read a file and return its contents with line numbers.")),
                ("write", "mutating", None, Some("Write content to a file, creating it and any parent directories.")),
                ("bash", "mutating", None, Some("Run a shell command.")),
                ("browser_click", "mutating", Some("browser"), Some("Click an element by ref, in the browser plugin's page.")),
                ("browser_open", "read_only", Some("browser"), Some("Open a URL in the shared headless browser.")),
                ("jev_graph", "read_only", Some("jev"), Some("Run a jev graph.")),
                ("jev_mark", "read_only", Some("jev"), None),
                ("tool_search", "read_only", None, Some("Search the deferred tools.")),
                ("wait_for", "read_only", None, Some("Park until a condition holds.")),
            ]
        );
        let config = rig.home.join("config.toml");
        let ran = calls(&rig.perch, "tools");
        assert_eq!(ran.len(), 1, "one command: {ran:?}");
        assert_eq!(argv_of(&ran[0]), format!("--config {} tools", config.display()), "the exact argv");
        let all = rig.perch.fake_helpers().join("\n");
        assert!(!all.contains("--live"), "never --live: {all}");
        // The attribution came from the plugin listing, asked the same way.
        let plugins = calls(&rig.perch, "plugins");
        assert_eq!(plugins.len(), 1, "{plugins:?}");
        assert_eq!(argv_of(&plugins[0]), format!("--config {} plugins", config.display()));
    }

    /// An answer is trusted for ten minutes: a second read starts no child, and the plugin text a tools read already fetched is shared.
    ///
    /// Mutation: drop the cache (or take the lock after the run) and the call count climbs.
    #[test]
    fn a_second_get_inside_the_ttl_starts_no_child() {
        let rig = rig();
        for path in ["/hub/meta/tools", "/hub/meta/plugins", "/hub/meta/policy?mode=auto", "/hub/meta/policy?mode=manual"] {
            assert_eq!(rig.perch.get(path).status, 200, "{path}");
        }
        let after_first = rig.perch.fake_helpers().len();
        let first_tools = rig.perch.get("/hub/meta/tools").body;
        for path in ["/hub/meta/tools", "/hub/meta/plugins", "/hub/meta/policy?mode=auto", "/hub/meta/policy?mode=manual", "/hub/meta/policy?mode=yolo"] {
            assert_eq!(rig.perch.get(path).status, 200, "{path}");
        }
        assert_eq!(rig.perch.fake_helpers().len(), after_first, "no child for a read the cache holds: {:?}", rig.perch.fake_helpers());
        assert_eq!(rig.perch.get("/hub/meta/tools").body, first_tools, "and the same answer");
        // One command each: tools, plugins (asked by the tools read, then shared), and policy-rows once per config.
        assert_eq!((calls(&rig.perch, "tools").len(), calls(&rig.perch, "plugins").len(), calls(&rig.perch, "policy-rows").len()), (1, 1, 2));
    }

    /// Reads that arrive together cost one command, because the lock is held across the run.
    #[test]
    fn reads_that_arrive_together_cost_one_command() {
        let rig = Setup { env: &[("FAKE_META_SLEEP_MS", "300")], ..Setup::default() }.start();
        let (addr, auth) = (rig.perch.addr.clone(), rig.perch.auth());
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let (addr, auth) = (addr.clone(), auth.clone());
                std::thread::spawn(move || Request::new(&addr, "GET", "/hub/meta/plugins").bearer(auth).send(&addr).status)
            })
            .collect();
        for reader in readers {
            assert_eq!(reader.join().unwrap(), 200);
        }
        assert_eq!(calls(&rig.perch, "plugins").len(), 1, "{:?}", rig.perch.fake_helpers());
    }

    #[test]
    fn plugins_are_the_text_eidolon_printed() {
        let rig = rig();
        let answer = rig.perch.get("/hub/meta/plugins");
        assert_eq!(answer.status, 200, "{}", answer.head);
        let body = json_of(&answer);
        assert_eq!(body.as_object().unwrap().keys().collect::<Vec<_>>(), ["text"]);
        let text = body["text"].as_str().unwrap();
        assert!(text.starts_with("browser 0.1.0\n    one headless Chromium"), "{text}");
        assert!(text.contains("\njev 0.2.0\n") && text.contains("unvouched — asks"), "verbatim, indentation and all: {text}");
    }

    /// The text is bounded twice: a command that prints past the run cap is a 502 and is not read on, and one under it is cut at 64 KiB
    /// with a line that says so.
    ///
    /// Mutation: drop the cut in `parse::text` and the length bound goes red; raise the run cap past 2 MB and the 502 does.
    #[test]
    fn a_long_listing_is_cut_and_an_enormous_one_is_refused() {
        let rig = Setup { env: &[("FAKE_META_BIG", "200000")], ..Setup::default() }.start();
        let body = json_of(&rig.perch.get("/hub/meta/plugins"));
        let text = body["text"].as_str().unwrap();
        assert!(text.len() < 64 * 1024 + 100, "{} bytes", text.len());
        assert!(text.ends_with("[cut: the text is longer than the perch sends]\n"), "{}", &text[text.len() - 80..]);
        assert!(text.starts_with("browser 0.1.0"));
        let rows = json_of(&rig.perch.get("/hub/meta/policy?mode=auto"));
        assert!(rows["rows"].as_str().unwrap().len() < 64 * 1024 + 100, "the policy rows are cut the same way");

        let rig = Setup { env: &[("FAKE_META_BIG", "2000000")], ..Setup::default() }.start();
        let big = rig.perch.get("/hub/meta/plugins");
        assert_eq!((big.status, json_of(&big)["error"].as_str()), (502, Some("eidolon's plugins are over the size limit")), "{}", big.body);
    }

    /// A failure is a sentence and a 502, never the cause, and is not remembered: the next read asks eidolon again.
    #[test]
    fn a_failing_eidolon_is_a_502_with_a_sentence_and_is_asked_again() {
        let rig = Setup { env: &[("FAKE_HELPER_FAIL", "1")], ..Setup::default() }.start();
        for path in ["tools", "plugins", "policy?mode=auto"] {
            let before = rig.perch.fake_helpers().len();
            let answer = rig.perch.get(&format!("/hub/meta/{path}"));
            assert_eq!(answer.status, 502, "{path}: {}{}", answer.head, answer.body);
            let error = json_of(&answer)["error"].as_str().unwrap().to_string();
            assert!(error.starts_with("eidolon could not print its "), "{error}");
            assert!(!error.contains(&rig.home.display().to_string()) && !error.contains("exit"), "the cause is not sent: {error}");
            assert!(rig.perch.fake_helpers().len() > before, "{path}: a command was run");
            let again = rig.perch.fake_helpers().len();
            assert_eq!(rig.perch.get(&format!("/hub/meta/{path}")).status, 502);
            assert!(rig.perch.fake_helpers().len() > again, "{path}: a failure is not cached");
        }
    }

    /// Output that is not a stream of tool objects is a 502 that says the shape, not an empty list that looks complete.
    #[test]
    fn a_tool_listing_that_is_not_json_is_not_a_list() {
        let rig = Setup { env: &[("FAKE_META_BAD", "1")], ..Setup::default() }.start();
        let answer = rig.perch.get("/hub/meta/tools");
        assert_eq!((answer.status, json_of(&answer)["error"].as_str()), (502, Some("eidolon's tools are not in a shape this perch reads")), "{}", answer.body);
    }

    /// A command that does not answer in time is killed and is a 504; the time is `--knob meta_timeout_ms` here so the test is short.
    #[test]
    fn a_command_that_does_not_answer_is_a_504_and_is_gone() {
        let rig = Setup { env: &[("FAKE_META_SLEEP_MS", "20000")], args: &["--knob", "meta_timeout_ms=400"], ..Setup::default() }.start();
        let started = Instant::now();
        let answer = rig.perch.get("/hub/meta/plugins");
        assert_eq!(answer.status, 504, "{}{}", answer.head, answer.body);
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(json_of(&answer)["error"], "eidolon did not print its plugins in time");
        // The run was dropped, and with it the child: nothing that sleeps on this perch's behalf is left to be reaped.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !children_of(rig.perch.child.id()).is_empty() {
            assert!(Instant::now() < deadline, "the perch still has children: {:?}", children_of(rig.perch.child.id()));
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The processes whose parent is `pid`, from `/proc/<n>/stat` (the fourth field, after the parenthesised name).
    fn children_of(pid: u32) -> Vec<u32> {
        let want = pid.to_string();
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .filter(|n| {
                let stat = std::fs::read_to_string(format!("/proc/{n}/stat")).unwrap_or_default();
                stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1).map(|ppid| ppid == want)).unwrap_or(false)
            })
            .collect()
    }

    // -----------------------------------------------------------------------
    // policy
    // -----------------------------------------------------------------------

    /// `mode` picks the config a door of that mode runs with: manual is `config-manual.toml`, auto and yolo are `config.toml` and share one
    /// run. The file is the one eidolon's `yours:` line names; with no `mode` it is the mode a new chat starts in.
    ///
    /// Mutation: ask every mode with `config.toml` and the manual argv (and its `file`) goes red; key the cache by mode and the count does.
    #[test]
    fn policy_asks_eidolon_with_the_config_of_the_mode() {
        let rig = rig();
        let (config, manual_config) = (rig.home.join("config.toml"), rig.home.join("config-manual.toml"));
        let manual = rig.perch.get("/hub/meta/policy?mode=manual");
        assert_eq!(manual.status, 200, "{}{}", manual.head, manual.body);
        let manual = json_of(&manual);
        let policy_manual = rig.home.join("policy-manual.rn").display().to_string();
        assert_eq!(manual["mode"], "manual");
        assert_eq!(manual["file"], policy_manual.as_str(), "the file the manual config names");
        assert!(manual["rows"].as_str().unwrap().starts_with(&format!("yours:   {policy_manual} — table-version 1")), "{manual}");
        assert!(manual["rows"].as_str().unwrap().ends_with("rows:    identical (30 rows)\n"), "the text is eidolon's, whole");

        let auto = json_of(&rig.perch.get("/hub/meta/policy?mode=auto"));
        assert_eq!(auto["mode"], "auto");
        assert!(auto["file"].as_str().unwrap().ends_with("/eidolon/policy.rn"), "the member's policy, not the manual one: {auto}");
        assert_ne!(auto["file"], manual["file"]);
        assert_eq!(json_of(&rig.perch.get("/hub/meta/policy?mode=yolo"))["mode"], "yolo");
        let default = json_of(&rig.perch.get("/hub/meta/policy"));
        assert_eq!((default["mode"].as_str(), &default["rows"]), (Some("auto"), &auto["rows"]), "minerva.toml's default_mode, answered from the same run");

        let ran = calls(&rig.perch, "policy-rows");
        let argv: Vec<&str> = ran.iter().map(|line| argv_of(line)).collect();
        assert_eq!(
            argv,
            [format!("--config {} policy-rows", manual_config.display()), format!("--config {} policy-rows", config.display())],
            "one run per config, in the order asked"
        );
    }

    /// The `file` is a `~` path when it is under `$HOME`; the rows are shown as eidolon printed them.
    #[test]
    fn the_policy_file_is_written_with_a_tilde_under_home() {
        let rig = Setup { minerva: "default_mode = \"auto\"\n", home_is_base: true, ..Setup::default() }.start();
        let manual = json_of(&rig.perch.get("/hub/meta/policy?mode=manual"));
        assert_eq!(manual["file"], "~/Minerva/policy-manual.rn", "{manual}");
        assert!(manual["rows"].as_str().unwrap().contains(&rig.home.display().to_string()), "the rows are verbatim: {manual}");
    }

    /// The guard's status rides along on every read. S0P-a's stub says off with a reason; G1's real one may say on with the file, so
    /// this holds the shape: `on` is a bool, `file` is a string when it is on and `null` when not, and an off guard says why.
    ///
    /// Mutation: leave `guard` out of the answer and the first assert goes red.
    #[test]
    fn policy_carries_the_guard_object() {
        let rig = rig();
        for mode in ["auto", "manual", "yolo"] {
            let body = json_of(&rig.perch.get(&format!("/hub/meta/policy?mode={mode}")));
            let guard = body["guard"].as_object().unwrap_or_else(|| panic!("a guard object in {body}"));
            let on = guard["on"].as_bool().expect("`on` is a bool");
            let file = guard.get("file").unwrap_or_else(|| panic!("`file` is always there, null when off: {body}"));
            if on {
                assert!(file.is_string(), "{body}");
                assert!(!guard.contains_key("why"), "an empty reason is not sent: {body}");
            } else {
                assert!(file.is_null(), "{body}");
                assert!(!guard["why"].as_str().expect("an off guard says why").is_empty(), "{body}");
            }
        }
    }

    #[test]
    fn policy_refuses_a_query_that_is_not_one_known_mode_and_a_manual_that_cannot_be_made() {
        let rig = rig();
        for bad in ["mode=gated", "mode=", "mode=auto&mode=auto", "mode=auto&x=1", "x=auto", "mode=Auto", "mode=%zz"] {
            assert_eq!(rig.perch.get(&format!("/hub/meta/policy?{bad}")).status, 400, "{bad}");
        }
        assert!(calls(&rig.perch, "policy-rows").is_empty(), "a refused query starts no command");
        // Without --home there is no manual config, and eidolon is asked with no --config at all.
        let bare = Perch::builder().hub().start();
        let manual = bare.get("/hub/meta/policy?mode=manual");
        assert_eq!(manual.status, 409, "{}{}", manual.head, manual.body);
        assert!(json_of(&manual)["error"].as_str().unwrap().starts_with("manual mode is not available: manual mode needs --home"), "{}", manual.body);
        let auto = bare.get("/hub/meta/policy?mode=auto");
        assert_eq!(auto.status, 200, "{}{}", auto.head, auto.body);
        let ran = calls(&bare, "policy-rows");
        assert_eq!(ran.len(), 1);
        assert!(ran[0].contains(" config=false ") && argv_of(&ran[0]) == "policy-rows", "no --config: {ran:?}");
    }

    // -----------------------------------------------------------------------
    // connectors, home
    // -----------------------------------------------------------------------

    /// Two booleans, and nothing of the tables: not their URL, not their token file. The config is the Minerva home's generated one, which
    /// carries the member's tables.
    ///
    /// Mutation: copy the table's `url` into the answer and the `http` assert goes red; test for the key instead of a table and the
    /// `mneme = "..."` case does.
    #[test]
    fn connectors_are_two_booleans_and_never_a_url_or_a_token_path() {
        let member = "[mneme]\nurl = \"http://127.0.0.1:9/mcp\"\ntoken_file = \"/secret/mneme.token\"\n\
                      [melete]\nurl = \"https://melete.example/mcp\"\ntoken_file = \"/secret/melete.token\"\n";
        let both = Setup { member, ..Setup::default() }.start();
        let answer = both.perch.get("/hub/meta/connectors");
        assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
        assert_eq!(answer.body, "{\"melete\":true,\"mneme\":true}");
        for word in ["http", "token", "secret", "example"] {
            assert!(!answer.body.contains(word), "{word} in {}", answer.body);
        }
        let none = Setup { member: "default_model = \"mock\"\n", ..Setup::default() }.start();
        assert_eq!(none.perch.get("/hub/meta/connectors").body, "{\"melete\":false,\"mneme\":false}");
        let only = Setup { member: "[melete]\nurl = \"http://x\"\n\n[providers.mneme]\nx = 1\n", ..Setup::default() }.start();
        assert_eq!(only.perch.get("/hub/meta/connectors").body, "{\"melete\":true,\"mneme\":false}", "a table of another name's is not the connector");
        let scalar = Setup { member: "mneme = \"http://x\"\n", ..Setup::default() }.start();
        assert_eq!(scalar.perch.get("/hub/meta/connectors").body, "{\"melete\":false,\"mneme\":false}", "a key that is not a table is not one");
    }

    #[test]
    fn home_says_what_a_new_chat_starts_in_and_who_said_so() {
        let named = rig();
        assert_eq!(named.perch.get("/hub/meta/home").body, "{\"default_mode\":\"auto\",\"source\":\"minerva.toml\"}");
        let manual = Setup { minerva: "default_mode = \"manual\"\ndefault_model = \"mock\"\n", ..Setup::default() }.start();
        assert_eq!(manual.perch.get("/hub/meta/home").body, "{\"default_mode\":\"manual\",\"source\":\"minerva.toml\"}");
        let unset = Setup { minerva: "default_model = \"mock\"\n", ..Setup::default() }.start();
        assert_eq!(unset.perch.get("/hub/meta/home").body, "{\"default_mode\":\"yolo\",\"source\":\"unset\"}", "a file that names no mode");
        let nothing = Setup::default().start();
        assert_eq!(nothing.perch.get("/hub/meta/home").body, "{\"default_mode\":\"yolo\",\"source\":\"unset\"}", "no minerva.toml at all");
        let no_home = Perch::builder().hub().start();
        let answer = no_home.get("/hub/meta/home");
        assert_eq!((answer.status, answer.body.as_str()), (200, "{\"default_mode\":\"yolo\",\"source\":\"unset\"}"), "no --home: no file to name it");
    }

    // -----------------------------------------------------------------------
    // git
    // -----------------------------------------------------------------------

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// A chat started in `dir`, which must be under a `--root`.
    fn chat_in(rig: &Rig, dir: &Path) -> String {
        let answer = rig.perch.post_json("/hub/sessions", serde_json::json!({ "cwd": dir }));
        assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
        body_id(&answer)
    }

    fn git_of(rig: &Rig, id: &str) -> Answer {
        rig.perch.get(&format!("/hub/meta/git?session={id}"))
    }

    fn repo(dir: &Path, head: &str) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), head).unwrap();
    }

    /// The five folders the diff tab meets: a branch, a detached head, a linked worktree (a `.git` file), no repository, and a `.git`
    /// that is a link to a repository outside every root, which is not followed and so looks like no repository at all. git is never run.
    ///
    /// Mutation: follow the link (`metadata` for `symlink_metadata`, or no `O_NOFOLLOW`) and the `stolen` row goes red.
    #[test]
    fn git_says_the_branch_the_detached_sha_the_worktrees_branch_and_nothing() {
        let rig = rig();
        let base = rig.base.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside = outside.path().canonicalize().unwrap();
        repo(&outside, "ref: refs/heads/stolen\n");

        let branch = base.join("branch");
        repo(&branch, "ref: refs/heads/main\n");
        let detached = base.join("detached");
        repo(&detached, &format!("{SHA}\n"));
        let (main, wt) = (base.join("main"), base.join("wt"));
        repo(&main, "ref: refs/heads/trunk\n");
        std::fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        std::fs::write(main.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", main.join(".git/worktrees/wt").display())).unwrap();
        let bare = base.join("norepo");
        std::fs::create_dir_all(&bare).unwrap();
        let linked = base.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(outside.join(".git"), linked.join(".git")).unwrap();
        // The control for the link: the repository it points at is a real one, and a chat in it would be told so.
        assert!(std::fs::read_to_string(linked.join(".git/HEAD")).unwrap().contains("stolen"));

        for (dir, want) in [
            (&branch, "{\"branch\":\"main\"}".to_string()),
            (&detached, format!("{{\"branch\":null,\"sha\":\"{SHA}\"}}")),
            (&wt, "{\"branch\":\"feature/x\"}".to_string()),
            (&bare, "{\"branch\":null}".to_string()),
            (&linked, "{\"branch\":null}".to_string()),
        ] {
            let id = chat_in(&rig, dir);
            let answer = git_of(&rig, &id);
            assert_eq!(answer.status, 200, "{}: {}{}", dir.display(), answer.head, answer.body);
            assert_eq!(answer.body, want, "{}", dir.display());
            assert_eq!(answer.header("content-type").as_deref(), Some("application/json"));
        }
        // Nothing is remembered: the head is read on every request.
        let id = chat_in(&rig, &branch);
        repo(&branch, "ref: refs/heads/second\n");
        assert_eq!(git_of(&rig, &id).body, "{\"branch\":\"second\"}");
        // And git itself was not run: the stand-in door and the helpers are the only processes the perch asked anything of.
        assert!(rig.perch.fake_helpers().iter().all(|line| !line.contains("git")), "{:?}", rig.perch.fake_helpers());
    }

    /// A saved chat is read through the same checks; its folder is judged against `--root` now, and a folder that left says so.
    #[test]
    fn git_for_a_saved_chat_is_read_the_same_and_a_folder_that_left_the_roots_is_a_404_with_a_sentence() {
        let rig = rig();
        let base = rig.base.path().canonicalize().unwrap();
        let (kept, left) = (base.join("kept"), base.join("left"));
        repo(&kept, "ref: refs/heads/main\n");
        repo(&left, "ref: refs/heads/main\n");
        let (a, b) = (chat_in(&rig, &kept), chat_in(&rig, &left));
        for id in [&a, &b] {
            assert_eq!(rig.perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
            assert!(rig.perch.wait_state(id, "kept", Duration::from_secs(5)), "{id} is kept");
        }
        let saved = git_of(&rig, &a);
        assert_eq!((saved.status, saved.body.as_str()), (200, "{\"branch\":\"main\"}"), "{}", saved.head);
        let outside = tempfile::tempdir().unwrap();
        repo(outside.path(), "ref: refs/heads/main\n");
        std::fs::remove_dir_all(&left).unwrap();
        std::os::unix::fs::symlink(outside.path(), &left).unwrap();
        let gone = git_of(&rig, &b);
        assert_eq!(gone.status, 404, "{}{}", gone.head, gone.body);
        assert!(gone.body.contains("folder"), "a sentence for the page: {}", gone.body);
    }

    /// The query is exactly one session; an unknown chat is the tree's 404; a folder the tree would not list (`~/.ssh`, an 8.3 alias) has
    /// no diff tab either, however good the repository in it is.
    ///
    /// Mutation: skip the `tree::resolve` of the folder and the `.ssh` row answers 200.
    #[test]
    fn git_takes_one_session_and_refuses_the_folders_the_tree_refuses() {
        let rig = rig();
        for (query, want) in [
            ("", 400),
            ("session", 400),
            ("session=a&session=b", 400),
            ("session=a&path=x", 400),
            ("path=x", 400),
            ("session=%zz", 400),
            ("session=nobody", 404),
            ("session=..%2Fx", 404),
        ] {
            let answer = rig.perch.get(&format!("/hub/meta/git?{query}"));
            assert_eq!(answer.status, want, "{query:?}: {}{}", answer.head, answer.body);
        }
        let base = rig.base.path().canonicalize().unwrap();
        for denied in [".ssh", "GIT~1", "keys/.gnupg"] {
            let dir = base.join(denied);
            repo(&dir, "ref: refs/heads/main\n");
            let id = chat_in(&rig, &dir);
            let answer = git_of(&rig, &id);
            assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{denied}: {}", answer.head);
        }
        let fine = base.join("fine");
        repo(&fine, "ref: refs/heads/main\n");
        let id = chat_in(&rig, &fine);
        assert_eq!(git_of(&rig, &id).status, 200, "the control: an ordinary folder");
    }
}
