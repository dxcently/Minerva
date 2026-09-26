//! The algebra's contract, as assertions.
//!
//! These test the *decomposition*, not any particular leaf table — a consumer's
//! table gets its own pinned corpus. The stand-in table below is deliberately
//! tiny and obvious, so a failure here is always a statement about composition,
//! scoping or parsing rather than about which programs someone considers safe.

use std::borrow::Cow;
use std::path::Path;

use super::*;
use super::shell;

/// A minimal leaf table: a handful of reads, one flagged mutator, one hard
/// deny, and everything else on the caller's posture.
#[derive(Default)]
struct Table {
    /// Set to pin `cargo` regardless of the table, the way a consumer's
    /// resource rule would.
    override_cargo: bool,
    /// Paths whose basename makes them untouchable.
    protected: &'static [&'static str],
}

impl LeafTable for Table {
    fn classify_program(&self, argv: &[&str], _ctx: &PolicyContext) -> Verdict {
        match argv.first().copied().unwrap_or("") {
            "ls" | "cat" | "grep" | "echo" | "test" | "[" | "true" | "sleep" | "pgrep" | "kill" => {
                Verdict::allow("safe read", true)
            }
            "git" => match argv.get(1).copied().unwrap_or("") {
                "status" | "log" | "diff" => Verdict::allow("git read", true),
                "push" => Verdict::flag("git push", false),
                _ => Verdict::allow("git write", false),
            },
            "curl" => Verdict::flag("network fetch", false),
            "shutdown" => Verdict::deny("never"),
            _ => {
                let decision = self.bash_default(_ctx.bash_default);
                Verdict::tier(decision, "unrecognized command", false)
            }
        }
    }

    fn fs_verb_tier(&self, prog: &str) -> Decision {
        match prog {
            "rm" | "rmdir" | "dd" | "truncate" | "chmod" | "chown" => Decision::Flag,
            _ => Decision::Allow,
        }
    }

    fn bash_default(&self, posture: BashDefault) -> Decision {
        match posture {
            BashDefault::Allow => Decision::Allow,
            BashDefault::Flag => Decision::Flag,
            BashDefault::Deny => Decision::Deny,
        }
    }

    fn program_override(&self, argv: &[&str], _ctx: &PolicyContext) -> Option<Verdict> {
        (self.override_cargo && argv.first() == Some(&"cargo"))
            .then(|| Verdict::deny("cargo pinned by the consumer"))
    }

    fn protected_path(&self, path: &str) -> bool {
        let base = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or("");
        self.protected.contains(&base)
    }
}

fn ws() -> PolicyContext {
    PolicyContext::in_workspace("/ws")
}

fn go(cmd: &str) -> Verdict {
    classify_command(cmd, &ws(), &Table::default())
}

fn go_in(cmd: &str, ctx: &PolicyContext) -> Verdict {
    classify_command(cmd, ctx, &Table::default())
}

fn tier(cmd: &str) -> Decision {
    go(cmd).decision
}

// --- the composition rule ------------------------------------------------

#[test]
fn a_composite_is_as_permissive_as_its_worst_stage() {
    // All-allow composes to allow, in every flat form.
    assert_eq!(tier("ls && cat f"), Decision::Allow);
    assert_eq!(tier("ls | grep x"), Decision::Allow);
    assert_eq!(tier("ls; cat f"), Decision::Allow);

    // A flagged stage flags the composite — it does not block it. This is the
    // false positive that made composition stricter than its parts deserved.
    assert_eq!(tier("git push && ls"), Decision::Flag);
    assert_eq!(tier("curl http://x | grep y"), Decision::Flag);
    assert_eq!(tier("ls; git push"), Decision::Flag);

    // A denied stage denies the whole thing, wherever it sits.
    assert_eq!(tier("ls && shutdown"), Decision::Deny);
    assert_eq!(tier("shutdown | grep x"), Decision::Deny);
    assert_eq!(tier("ls; shutdown; cat f"), Decision::Deny);
}

#[test]
fn a_denied_composite_is_never_reported_as_a_read() {
    let v = go("ls && shutdown");
    assert_eq!(v.decision, Decision::Deny);
    assert!(!v.read_only, "a blocked composite must not count as an observation");
}

#[test]
fn read_only_survives_only_when_every_stage_is_a_read() {
    assert!(go("ls | grep x").read_only);
    assert!(!go("ls && git commit").read_only);
}

// --- quoting is data, not syntax ----------------------------------------

#[test]
fn metacharacters_inside_quotes_are_never_syntax() {
    // Each of these is ONE command whose argument merely contains the
    // character. A scanner over the raw text would see a pipe, a chain, a
    // background job, a substitution.
    for cmd in [
        r#"echo "a | b""#,
        r#"echo "a && b""#,
        r#"echo "a ; b""#,
        r#"echo "a & b""#,
        r#"echo 'a $(shutdown) b'"#,
        r#"echo "a > b""#,
    ] {
        assert_eq!(tier(cmd), Decision::Allow, "{cmd} should classify as its single safe command");
    }
}

#[test]
fn an_unknown_expansion_cannot_impersonate_a_program() {
    // `$FOO` collapses to OPAQUE, which matches nothing in any table.
    let v = go("$FOO --flag");
    assert_eq!(v.decision, Decision::Flag, "an opaque program falls to the posture");
    assert_eq!(v.reason, "unrecognized command");
}

// --- substitution --------------------------------------------------------

#[test]
fn a_substitution_is_classified_on_its_own_merits() {
    assert_eq!(tier("echo $(ls)"), Decision::Allow);
    assert_eq!(tier("echo $(shutdown)"), Decision::Deny);
    // A substitution that is not a single simple command is refused rather
    // than approximated.
    assert_eq!(tier("echo $(ls | grep x)"), Decision::Deny);
    assert_eq!(tier("echo $(echo $(ls))"), Decision::Deny);
}

#[test]
fn a_substitution_in_an_assignment_is_still_classified() {
    assert_eq!(tier("FOO=$(shutdown) ls"), Decision::Deny);
}

// --- a heredoc is stdin ---------------------------------------------------

/// A heredoc body is the command's stdin, so the command is classified for
/// what it does with stdin and the body is not a shape to refuse. A body
/// that would expand a substitution in the calling shell still is.
#[test]
fn a_heredoc_is_the_commands_stdin() {
    // The command is what is classified; the body rides along as data.
    let v = go("cat <<'EOF'\nhi\nEOF");
    assert_eq!(v.decision, Decision::Allow, "{}", v.reason);
    assert!(!v.structural);
    assert!(v.read_only, "stdin is not a write");
    // A flagged command keeps its flag, a refused one its refusal.
    assert_eq!(tier("curl <<'EOF'\nx\nEOF"), Decision::Flag);
    let v = go("shutdown <<'EOF'\nx\nEOF");
    assert_eq!(v.decision, Decision::Deny);
    assert!(!v.structural, "on the merits: the command, not the shape");
    // The body's own text is data, not a command line.
    let v = go("cat <<'EOF'\nshutdown\nEOF");
    assert_eq!(v.decision, Decision::Allow, "the body is not parsed as shell");
    // Into a file: the write is scoped by the redirect beside it.
    let v = go("cat <<'EOF' > /ws/notes.txt\nhello\nEOF");
    assert_eq!(v.decision, Decision::Allow, "{}", v.reason);
    assert!(!v.read_only, "a heredoc written to a file is a write");
    let v = go("cat <<'EOF' > /etc/motd\nhello\nEOF");
    assert_eq!(v.decision, Decision::Deny);
    assert!(v.structural, "escapes the workspace: a shape refusal, as `> /etc/motd` is");
    // An unquoted delimiter expands in the calling shell: a substitution
    // in the body runs there, and that is still refused for its shape.
    let v = go("cat <<EOF\n$(shutdown)\nEOF");
    assert_eq!(v.decision, Decision::Deny);
    assert!(v.structural);
    assert_eq!(v.reason, "command substitution in heredoc blocked");
    // A parameter expansion in the body is not a command.
    let v = go("cat <<EOF\nhome is $HOME\nEOF");
    assert_eq!(v.decision, Decision::Allow, "{}", v.reason);
}

// --- the inert heredoc ---------------------------------------------------

#[test]
fn only_the_bare_quoted_cat_heredoc_shape_is_neutralized() {
    let opaque = format!("git commit -m \"{OPAQUE}\"");

    // All three quotings that make a heredoc body inert qualify.
    for cmd in [
        "git commit -m \"$(cat <<'EOF'\nline one\nline two\nEOF\n)\"",
        "git commit -m \"$(cat <<\"EOF\"\nmsg\nEOF\n)\"",
        "git commit -m \"$(cat <<\\EOF\nmsg\nEOF\n)\"",
    ] {
        assert_eq!(shell::neutralize_inert_cat_heredocs(cmd), opaque, "{cmd}");
    }
    // The `&&`-chain form neutralizes the substitution and leaves the chain.
    assert_eq!(
        shell::neutralize_inert_cat_heredocs(
            "git add src/x.rs && git commit -m \"$(cat <<'EOF'\nmsg\nEOF\n)\""
        ),
        format!("git add src/x.rs && {opaque}"),
    );

    // Every deviation passes through untouched: an *unquoted* delimiter (the
    // body would still expand), a file argument, a trailing command inside the
    // substitution, a non-heredoc substitution, no substitution at all.
    for intact in [
        "git commit -m \"$(cat <<EOF\nmsg\nEOF\n)\"",
        "git commit -m \"$(cat notes.txt <<'EOF'\nmsg\nEOF\n)\"",
        "git commit -m \"$(cat <<'EOF'\nmsg\nEOF\n; rm -rf /)\"",
        "git commit -m \"$(cat somefile.txt)\"",
        "echo $(cat somefile.txt)",
        "cp /ws/src/x.rs /tmp/foo.rs",
    ] {
        assert_eq!(shell::neutralize_inert_cat_heredocs(intact), intact, "{intact}");
    }
}

#[test]
fn a_real_commit_message_heredoc_classifies_as_the_commit() {
    let v = go("git add src/x.rs && git commit -m \"$(cat <<'EOF'\nmsg\nEOF\n)\"");
    assert_eq!(v.decision, Decision::Allow);
}

// --- redirects -----------------------------------------------------------

#[test]
fn stream_plumbing_is_recognized_structurally() {
    assert_eq!(tier("ls 2>&1"), Decision::Allow);
    assert!(go("ls 2>/dev/null").read_only, "/dev/null writes nothing");
    assert!(!go("ls > /ws/out.txt").read_only, "an in-scope file write is a mutation");
    assert_eq!(tier("ls > /ws/out.txt"), Decision::Allow);
}

#[test]
fn a_redirect_out_of_the_workspace_is_refused() {
    let v = go("ls > /etc/passwd");
    assert_eq!(v.decision, Decision::Deny);
    assert_eq!(v.reason, "redirection escapes workspace");
}

#[test]
fn a_substituted_redirect_target_is_never_trusted() {
    assert_eq!(tier("ls > $(echo /ws/x)"), Decision::Deny);
}

#[test]
fn a_redirect_with_no_workspace_scope_is_refused() {
    let ctx = PolicyContext::default();
    let v = go_in("ls > out.txt", &ctx);
    assert_eq!(v.decision, Decision::Deny);
    assert_eq!(v.reason, "redirection without workspace scope");
}

// --- filesystem scope ----------------------------------------------------

#[test]
fn a_filesystem_verb_is_scoped_before_it_is_tiered() {
    assert_eq!(tier("mkdir /ws/sub"), Decision::Allow);
    assert_eq!(tier("rm /ws/sub/f"), Decision::Flag); // in scope, destructive
    assert_eq!(tier("rm /etc/passwd"), Decision::Deny); // escapes
    assert_eq!(tier("rm ../../etc/passwd"), Decision::Deny); // escapes lexically
}

#[test]
fn an_escape_into_tmp_is_surfaced_rather_than_refused() {
    let v = go("cp /ws/src/x.rs /tmp/foo.rs");
    assert_eq!(v.decision, Decision::Flag);
    assert_eq!(v.reason, "filesystem mutation into /tmp");
}

#[test]
fn a_filesystem_verb_without_a_workspace_is_refused() {
    let ctx = PolicyContext::default();
    assert_eq!(go_in("rm f", &ctx).decision, Decision::Deny);
}

#[test]
fn an_unscoped_host_surfaces_rather_than_refuses() {
    let ctx = PolicyContext { unscoped: true, ..PolicyContext::default() };
    assert_eq!(go_in("rm /var/log/x", &ctx).decision, Decision::Flag);
    // An otherwise-silent command with a write redirect is escalated too.
    assert_eq!(go_in("ls > /var/log/x", &ctx).decision, Decision::Flag);
}

#[test]
fn protected_state_is_refused_even_inside_the_workspace() {
    let table = Table { protected: &["secrets.db"], ..Table::default() };
    // However the write is spelled — a filesystem verb, or a redirect — the
    // same rule refuses it, and refuses it the same *way*: on the merits, so a
    // consumer that turns shape-refusals into a question cannot be asked to
    // waive a path the table declared untouchable.
    for cmd in ["rm /ws/secrets.db", "tee /ws/secrets.db", "ls > /ws/secrets.db"] {
        let v = classify_command(cmd, &ws(), &table);
        assert_eq!(v.decision, Decision::Deny, "{cmd}");
        assert!(!v.structural, "{cmd} refuses on the merits, not for want of understanding");
    }
    // The scope check around it is still a shape refusal: a path this pass
    // cannot place is a fair thing to ask a person about.
    let v = classify_command("ls > /elsewhere/out.txt", &ws(), &table);
    assert_eq!(v.decision, Decision::Deny);
    assert!(v.structural);
}

// --- loops ---------------------------------------------------------------

#[test]
fn a_wait_loop_that_probes_state_is_allowed() {
    assert_eq!(tier("while test -f /ws/lock; do sleep 1; done"), Decision::Allow);
    assert_eq!(tier("while kill -0 123; do sleep 1; done"), Decision::Allow);
}

#[test]
fn a_loop_that_cannot_terminate_is_refused_however_safe_its_parts() {
    let v = go("while true; do echo hi; done");
    assert_eq!(v.decision, Decision::Deny);
    assert_eq!(v.reason, "loop condition cannot terminate");
}

#[test]
fn a_loop_keeps_the_stricter_allow_only_bar() {
    // `git push` merely flags on its own; repeated by a loop nobody is
    // watching, it is refused.
    assert_eq!(tier("ls; git push"), Decision::Flag);
    assert_eq!(tier("while test -f /ws/lock; do git push; done"), Decision::Deny);
}

// --- what has no safe decomposition -------------------------------------

#[test]
fn forms_with_no_decomposition_are_refused() {
    for (cmd, reason) in [
        ("ls &", "background command (&) blocked"),
        ("for f in a b; do echo $f; done", "compound command blocked"),
        ("if true; then ls; fi", "compound command blocked"),
        ("(ls)", "compound command blocked"),
        ("f() { ls; }", "function definition blocked"),
    ] {
        let v = go(cmd);
        assert_eq!(v.decision, Decision::Deny, "{cmd}");
        assert_eq!(v.reason, Cow::Borrowed(reason), "{cmd}");
    }
}

#[test]
fn an_unparseable_command_fails_closed() {
    let v = go("ls |");
    assert_eq!(v.decision, Decision::Deny);
    assert_eq!(v.reason, "unparseable shell command");
}

#[test]
fn an_empty_command_is_refused() {
    assert_eq!(go("   ").decision, Decision::Deny);
}

// --- the consumer's seams ------------------------------------------------

#[test]
fn a_program_override_is_consulted_before_anything_else() {
    let table = Table { override_cargo: true, ..Table::default() };
    let v = classify_command("cargo build", &ws(), &table);
    assert_eq!(v.decision, Decision::Deny);
    assert_eq!(v.reason, "cargo pinned by the consumer");
    // And it cannot be composed around.
    assert_eq!(
        classify_command("ls && cargo build", &ws(), &table).decision,
        Decision::Deny
    );
    // Without it, the same command is just an unrecognized program.
    assert_eq!(tier("cargo build"), Decision::Flag);
}

#[test]
fn the_posture_governs_only_what_matched_nothing() {
    for (posture, expected) in [
        (BashDefault::Allow, Decision::Allow),
        (BashDefault::Flag, Decision::Flag),
        (BashDefault::Deny, Decision::Deny),
    ] {
        let ctx = ws().with_default(posture);
        assert_eq!(go_in("someprog --x", &ctx).decision, expected, "{posture:?}");
        // A recognized command is unaffected by the posture.
        assert_eq!(go_in("ls", &ctx).decision, Decision::Allow, "{posture:?}");
    }
}

#[test]
fn a_nix_develop_shim_classifies_its_inner_command() {
    let ctx = ws().with_toolchain(Toolchain::Nix);
    assert_eq!(go_in("nix develop -c ls", &ctx).decision, Decision::Allow);
    assert_eq!(go_in("nix develop -c shutdown", &ctx).decision, Decision::Deny);
    // Without the toolchain declared, the shim is not unwrapped and `nix` is
    // just an unrecognized program.
    assert_eq!(tier("nix develop -c shutdown"), Decision::Flag);
}

// --- geometry ------------------------------------------------------------

#[test]
fn containment_is_lexical_and_works_on_paths_that_do_not_exist() {
    let root = Path::new("/ws");
    assert!(path_within(Path::new("sub/new.txt"), root));
    assert!(path_within(Path::new("/ws/a/../b"), root));
    assert!(!path_within(Path::new("/ws/../etc"), root));
    assert!(!path_within(Path::new("../etc"), root));
}

#[test]
fn worse_is_the_composition_rule() {
    assert_eq!(Decision::Allow.worse(Decision::Flag), Decision::Flag);
    assert_eq!(Decision::Flag.worse(Decision::Deny), Decision::Deny);
    assert_eq!(Decision::Allow.worse(Decision::Allow), Decision::Allow);
}

// --- merits vs. shape ----------------------------------------------------

#[test]
fn a_refusal_says_whether_it_was_the_table_or_the_algebra() {
    // The table knows this command and will not have it.
    let v = go("shutdown");
    assert_eq!(v.decision, Decision::Deny);
    assert!(!v.structural, "a table verdict is a refusal on the merits");

    // The algebra cannot reduce these at all.
    for cmd in ["(ls)", "ls &", "ls |", "for f in a; do ls; done", "rm /etc/passwd"] {
        let v = go(cmd);
        assert_eq!(v.decision, Decision::Deny, "{cmd}");
        assert!(v.structural, "{cmd} is refused for its shape, not on the merits");
    }
}

#[test]
fn a_composite_inherits_the_kind_of_the_part_that_refused_it() {
    // Wrapping a forbidden command must not launder it into a question.
    for cmd in ["echo $(shutdown)", "ls && shutdown", "shutdown | grep x"] {
        let v = go(cmd);
        assert_eq!(v.decision, Decision::Deny, "{cmd}");
        assert!(!v.structural, "{cmd} must refuse on the merits like its part");
    }
    // A composite refused for a shape stays a shape refusal.
    let v = go("echo $(ls | grep x)");
    assert_eq!(v.decision, Decision::Deny);
    assert!(v.structural);
}

/// The composite's own reason is for a refusal on the merits. A shape
/// refusal names the shape — the part's reason travels up — because that
/// is what a consumer asking "run it anyway?" has to put in the question,
/// and "unsafe command in sequence blocked" is wrong on both words there.
#[test]
fn a_composite_refused_for_shape_says_which_shape() {
    let part = go("echo $(ls | grep x)");
    assert!(part.structural);
    for cmd in ["ls; echo $(ls | grep x)", "ls && echo $(ls | grep x)", "echo $(ls | grep x) | cat"] {
        let v = go(cmd);
        assert_eq!(v.decision, Decision::Deny, "{cmd}");
        assert!(v.structural, "{cmd}");
        assert_eq!(v.reason, part.reason, "{cmd} names the part's shape, not the composite");
    }
    // The merits case keeps the composite's vocabulary.
    assert_eq!(go("ls; shutdown").reason, "unsafe command in sequence blocked");
}

#[test]
fn a_loop_over_a_merely_flagged_command_is_a_shape_refusal() {
    let v = go("while test -f /ws/lock; do git push; done");
    assert_eq!(v.decision, Decision::Deny);
    assert!(v.structural, "the loop rule refused it, not the table");

    let v = go("while test -f /ws/lock; do shutdown; done");
    assert_eq!(v.decision, Decision::Deny);
    assert!(!v.structural, "the table refused the body");
}

#[test]
fn an_mcp_qualified_tool_reduces_to_its_own_name() {
    // However a tool was reached, the table sees one name.
    assert_eq!(bare_tool_name("mcp__eidolon__bash"), "bash");
    assert_eq!(bare_tool_name("mcp__mneme__edit_note"), "edit_note");
    // A bare name and a name with no qualification are already reduced.
    assert_eq!(bare_tool_name("Bash"), "Bash");
    assert_eq!(bare_tool_name(""), "");
    // The last segment wins, so a server whose own name contains the separator
    // cannot smuggle a different tool name past the table.
    assert_eq!(bare_tool_name("mcp__a__b__status"), "status");
}
