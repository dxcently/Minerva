//! The shell decomposition algebra: parse a command into the shell's real AST
//! and compose per-leaf verdicts across every structure it can have.
//!
//! The seam is "anything that walks shell structure", so the whole
//! worst-part-wins composition lives in one file. Leaf verdicts come from the
//! consumer's [`LeafTable`].

use std::path::Path;

use conch_parser::ast::{
    AndOr, AndOrList, Arithmetic, Command, ComplexWord, CompoundCommandKind, ListableCommand,
    Parameter, ParameterSubstitution, PipeableCommand, Redirect, RedirectOrCmdWord,
    RedirectOrEnvVar, ShellCompoundCommand, ShellPipeableCommand, SimpleCommand, SimpleWord,
    TopLevelCommand, TopLevelWord, Word,
};
use conch_parser::lexer::Lexer;
use conch_parser::parse::DefaultParser;

use super::scope::{Toolchain, path_within, target_under_tmp};
use super::{Decision, LeafTable, OPAQUE, PolicyContext, Verdict};

// Concrete names for the corner of conch-parser's (heavily generic) AST that a
// `String`-sourced `DefaultParser` actually produces. Spelling them once keeps
// the signatures readable.
type Cmd = TopLevelCommand<String>;
type Wd = TopLevelWord<String>;
type Pipeable = ShellPipeableCommand<String, Wd, Cmd>;
type Compound = ShellCompoundCommand<String, Wd, Cmd>;
type Listable = ListableCommand<Pipeable>;
type AndOrL = AndOrList<Listable>;
type Simple = SimpleCommand<String, Wd, Redirect<Wd>>;
type PSubst = ParameterSubstitution<Parameter<String>, Wd, Cmd, Arithmetic<String>>;
type SWord = SimpleWord<String, Parameter<String>, Box<PSubst>>;

/// The filesystem-mutating verbs whose *targets* must be scope-checked here
/// before the table is asked for a tier. Which of them is destructive is the
/// table's call ([`LeafTable::fs_verb_tier`]); whether the path is in-scope
/// never is.
const FS_VERBS: &[&str] = &[
    "rm", "rmdir", "mv", "cp", "mkdir", "touch", "tee", "ln", "chmod", "chown", "dd", "truncate",
];

/// Classify a command by parsing it into the shell's real structure and walking
/// that, rather than scanning the raw string for metacharacters.
///
/// The guiding principle is *decompose and classify the parts*: no operator is
/// categorically denied for being what it is. A composite command is broken into
/// its constituent commands and each is classified on its own; the whole then
/// takes the tier of its *worst* part. This holds uniformly for pipelines (`|`),
/// and/or chains (`&&`/`||`) and `;`-sequences; for command substitution
/// (`` `cmd` ``/`$(cmd)`), whose inner command is extracted and classified like
/// any other; and for file redirection (`>`/`>>`/`<`), whose destination is
/// scope-checked exactly like a filesystem verb's target, so an in-workspace
/// redirect is a workspace write. Streaming redirects (`2>&1`, `2>/dev/null`,
/// `1>&2`) are recognized structurally as safe.
///
/// What stays denied is only what has no safe decomposition: backgrounding
/// (`&`), the compound commands other than `while`/`until` (subshells, `for`,
/// `if`, `case`), heredocs, and substitution nested more than one level deep.
pub(super) fn classify(command: &str, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    let raw = command.trim();
    if raw.is_empty() {
        return Verdict::refuse("empty command");
    }
    // Neutralize the one provably-inert command-substitution shape — a quoted-
    // heredoc `cat` — before parsing, so the enclosing command classifies with
    // it treated as an opaque literal.
    let cmd = neutralize_inert_cat_heredocs(raw);
    match parse_command(&cmd) {
        Some(cmds) => classify_parsed(&cmds, ctx, table),
        // Fail closed: if the real shell grammar cannot make sense of the
        // input, we cannot reason about what it would do.
        None => Verdict::refuse("unparseable shell command"),
    }
}

/// Agents write multi-line commit messages as `git commit -m "$(cat <<'EOF' …
/// EOF)"`. A quoted-heredoc `cat` substitution like this is *provably inert*: a
/// **quoted** delimiter disables every parameter and command substitution in the
/// body, so the body is static text and the `cat` merely echoes it — there is
/// nothing to execute.
///
/// We recognize exactly that one shape in the raw text and rewrite each
/// qualifying `$(…)` span to [`OPAQUE`], so the enclosing command classifies
/// with the substitution treated as an opaque literal — the same treatment a
/// bare `$VAR` already gets.
///
/// The quoted-vs-unquoted distinction is *load-bearing* and can only be read
/// from the raw source: conch-parser preserves it in the parsed body only when
/// the body actually contains an expansion, so an unquoted delimiter whose body
/// happens to have no `$` parses identically to a quoted one. Hence this
/// lower-level scan.
///
/// The recognizer is deliberately strict — a bare `cat` fed a single
/// quoted-delimiter heredoc (`<<'EOF'`, `<<"EOF"`, `<<\EOF`) with nothing after
/// the terminator line but the closing `)`. Anything else is left intact for the
/// parser, so it stays classified — and denied — exactly as before.
pub(super) fn neutralize_inert_cat_heredocs(cmd: &str) -> String {
    let chars: Vec<char> = cmd.chars().collect();
    let mut out = String::with_capacity(cmd.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$'
            && chars.get(i + 1) == Some(&'(')
            && let Some(end) = match_inert_cat_heredoc(&chars, i)
        {
            out.push(OPAQUE);
            i = end; // index just past the substitution's closing ')'
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Try to match a `$(cat <<QUOTED_DELIM … DELIM)` inert-heredoc substitution
/// whose opening `$(` is at `chars[start]`. On success returns the index just
/// past the closing `)`; on any deviation from the strict shape returns `None`.
fn match_inert_cat_heredoc(chars: &[char], start: usize) -> Option<usize> {
    let mut i = skip_blanks(chars, start + 2); // past "$("
    i = match_keyword(chars, i, "cat")?;
    // `cat` must be a whole word — a real separator, not `catfoo`.
    if !matches!(chars.get(i), Some(' ' | '\t')) {
        return None;
    }
    i = skip_blanks(chars, i);
    // The heredoc operator `<<` — but not `<<<` (here-string) or `<<-`.
    if chars.get(i) != Some(&'<') || chars.get(i + 1) != Some(&'<') {
        return None;
    }
    i += 2;
    if matches!(chars.get(i), Some('<' | '-')) {
        return None;
    }
    i = skip_blanks(chars, i);
    // A *quoted* delimiter is what makes the body inert.
    let (delim, mut i) = parse_quoted_delim(chars, i)?;
    // Nothing but optional blanks may follow the delimiter on its line.
    i = skip_blanks(chars, i);
    if chars.get(i) != Some(&'\n') {
        return None;
    }
    i += 1;
    // Scan body lines until one equals the delimiter exactly (`<<`, not `<<-`,
    // so no leading-tab stripping). The terminator must be alone on its line.
    loop {
        let line_start = i;
        while i < chars.len() && chars[i] != '\n' {
            i += 1;
        }
        let is_terminator = chars[line_start..i].iter().collect::<String>() == delim;
        let had_newline = i < chars.len();
        if had_newline {
            i += 1;
        }
        if is_terminator {
            // Only whitespace may sit between the terminator and the `)`.
            let mut j = i;
            while matches!(chars.get(j), Some(' ' | '\t' | '\n' | '\r')) {
                j += 1;
            }
            return (chars.get(j) == Some(&')')).then_some(j + 1);
        }
        if !had_newline {
            return None; // ran off the end without a terminator line
        }
    }
}

/// Advance past a run of spaces/tabs (never newlines).
fn skip_blanks(chars: &[char], mut i: usize) -> usize {
    while matches!(chars.get(i), Some(' ' | '\t')) {
        i += 1;
    }
    i
}

/// If `kw` sits at `chars[i]`, return the index past it; else `None`.
fn match_keyword(chars: &[char], i: usize, kw: &str) -> Option<usize> {
    let kw: Vec<char> = kw.chars().collect();
    (i + kw.len() <= chars.len() && chars[i..i + kw.len()] == kw[..]).then_some(i + kw.len())
}

/// Parse a *quoted* heredoc delimiter at `chars[i]` — `'EOF'`, `"EOF"`, or the
/// backslash-escaped `\EOF`. These are exactly the quotings that disable
/// expansion in the body. An *unquoted* delimiter returns `None`.
fn parse_quoted_delim(chars: &[char], i: usize) -> Option<(String, usize)> {
    match chars.get(i) {
        Some(q @ ('\'' | '"')) => {
            let mut j = i + 1;
            let mut delim = String::new();
            while let Some(&c) = chars.get(j) {
                if c == *q {
                    return Some((delim, j + 1));
                }
                if c == '\n' {
                    return None; // the quote never closed
                }
                delim.push(c);
                j += 1;
            }
            None
        }
        Some('\\') => {
            let mut j = i + 1;
            let mut delim = String::new();
            while let Some(&c) = chars.get(j) {
                if c.is_alphanumeric() || c == '_' {
                    delim.push(c);
                    j += 1;
                } else {
                    break;
                }
            }
            (!delim.is_empty()).then_some((delim, j))
        }
        _ => None,
    }
}

/// Parse a command line into the shell's real structure with correct quoting and
/// escaping. `None` on any parse error (a hard deny upstream).
fn parse_command(cmd: &str) -> Option<Vec<Cmd>> {
    let parser = DefaultParser::new(Lexer::new(cmd.chars()));
    let mut cmds = Vec::new();
    for result in parser {
        cmds.push(result.ok()?);
    }
    Some(cmds)
}

/// Classify a fully parsed command line. More than one top-level command means
/// `;`/newline sequencing: the parser has already split on the *unquoted*
/// separators (a `;` inside quotes stays data within one command), so the
/// sequence is walked stage-by-stage and takes the tier of its worst stage.
///
/// "Worst stage wins" means any `Deny` stage refuses the whole sequence — a
/// genuinely dangerous command must not become reachable by writing it beside a
/// benign one — but a sequence whose stages are all `Allow`-or-`Flag` is `Flag`,
/// running and surfaced exactly as a lone `Flag` command would. Requiring every
/// stage to be `Allow` made composition *stricter* than its parts deserve:
/// `Flag` is not a block, so refusing `cargo add foo 2>&1 | tail -40` when both
/// stages are individually permitted was a false positive, not a safety
/// property. In Melete it was the loudest single source of unattended-run
/// friction before it was fixed.
fn classify_parsed(cmds: &[Cmd], ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    match cmds {
        [] => Verdict::refuse("empty command"),
        [cmd] => classify_top_command(&cmd.0, ctx, table),
        stages => fold_stages(
            stages.iter().map(|cmd| &cmd.0),
            |cmd| classify_top_command(cmd, ctx, table),
            "unsafe command in sequence blocked",
            "sequence contains flagged stage(s)",
            "safe command sequence",
        ),
    }
}

/// The one worst-stage-wins algebra every flat composition form folds through:
/// any `Deny` stage denies the composite on the spot (short-circuiting, never
/// read-only — a blocked composite is not a read), otherwise any `Flag` stage
/// flags it, and it is read-only only when every stage is.
///
/// Each caller passes its own three reasons so the audit vocabulary stays
/// per-form. A wait-loop deliberately does *not* fold through this: it keeps the
/// stricter Allow-only bar.
fn fold_stages<T>(
    stages: impl IntoIterator<Item = T>,
    classify: impl Fn(T) -> Verdict,
    deny_reason: &'static str,
    flagged_reason: &'static str,
    allow_reason: &'static str,
) -> Verdict {
    let mut read_only = true;
    let mut flagged = false;
    for stage in stages {
        let v = classify(stage);
        match v.decision {
            // The composite inherits the refused stage's *kind*, not just its
            // tier: `echo $(rm -rf /)` must refuse on the merits exactly as
            // `rm -rf /` does, while a composite refused for a shape nobody
            // could reduce stays a question rather than becoming a verdict.
            // A refusal on the merits answers in the composite's own
            // vocabulary — `deny_reason` is the unit an audit groups by,
            // and the part that earned it is there in the line. A refusal
            // for *shape* keeps the part's reason instead: the composite's
            // wording says "unsafe" and "blocked" about a `$(…)` nobody
            // could reduce, and a consumer that turns shape refusals into
            // questions would then ask "unsafe command in sequence blocked
            // — run it anyway?", wrong on both words, while the part's
            // reason ("unsupported command substitution") names the thing
            // the operator is actually being asked about. One session
            // read exactly that line back out of its own log and took an
            // approved call for a silent block.
            Decision::Deny if v.structural => return Verdict::deny_like(&v),
            Decision::Deny => {
                return Verdict { reason: deny_reason.into(), ..Verdict::deny_like(&v) };
            }
            Decision::Flag => flagged = true,
            Decision::Allow => {}
        }
        read_only &= v.read_only;
    }
    if flagged {
        Verdict::flag(flagged_reason, read_only)
    } else {
        Verdict::allow(allow_reason, read_only)
    }
}

/// One top-level command: a backgrounded job (`&`) is always refused — it
/// outlives the classified turn, so nothing downstream can still be watching it
/// — otherwise it is an and/or list walked stage-by-stage.
fn classify_top_command(cmd: &Command<AndOrL>, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    match cmd {
        Command::Job(_) => Verdict::refuse("background command (&) blocked"),
        Command::List(list) => classify_and_or(list, ctx, table),
    }
}

/// An and/or list (`a && b || c`). A lone command keeps its own granular
/// verdict and reason; a real chain takes the tier of its worst stage.
fn classify_and_or(list: &AndOrL, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    if list.rest.is_empty() {
        return classify_listable(&list.first, ctx, table);
    }
    let rest = list.rest.iter().map(|ao| match ao {
        AndOr::And(c) | AndOr::Or(c) => c,
    });
    fold_stages(
        std::iter::once(&list.first).chain(rest),
        |stage| classify_listable(stage, ctx, table),
        "unsafe command in chain blocked",
        "chain contains flagged stage(s)",
        "safe command chain",
    )
}

/// One element of an and/or list: a single command, or a pipeline whose tier is
/// its worst stage's.
fn classify_listable(listable: &Listable, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    match listable {
        ListableCommand::Single(pipeable) => classify_pipeable(pipeable, ctx, table),
        ListableCommand::Pipe(_, stages) => fold_stages(
            stages,
            |stage| classify_pipeable(stage, ctx, table),
            "unsafe pipeline stage blocked",
            "pipeline contains flagged stage(s)",
            "safe pipeline",
        ),
    }
}

/// A pipeable command. A *simple* command is reasoned about directly; a
/// `while`/`until` wait-loop is decomposed; every other compound command
/// (subshell, brace group, `if`/`for`/`case`) or function definition is
/// composition with no clean per-command decomposition in this pass, and is
/// refused.
fn classify_pipeable(pipeable: &Pipeable, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    match pipeable {
        PipeableCommand::Simple(simple) => classify_simple(simple, ctx, table),
        PipeableCommand::Compound(compound) => classify_compound(compound, ctx, table),
        PipeableCommand::FunctionDef(..) => Verdict::refuse("function definition blocked"),
    }
}

/// A compound command. Only `while`/`until … do … done` wait-loops are
/// supported.
///
/// A wait-loop decomposes exactly like a `;`-sequence: its condition and body
/// are ordinary command sequences. The loop is allowed only when *every* part is
/// `Allow` on its own — the one place the flat forms' "Flag composes" rule does
/// not apply, because a flagged command surfaced once is not the same risk as
/// the same command repeated with nobody watching.
///
/// Residual risk decomposition alone cannot cover: *liveness*. Per-command
/// classification asks "is each thing this does safe?"; it cannot ask "will this
/// ever stop?", because repetition is not a mutation. `while true; do :; done`
/// passes the part-by-part gate and spins forever. So one heuristic sits on top:
/// the condition must probe something that can plausibly change over time, so
/// the loop can actually become false. An unrecognized condition fails the guard
/// rather than being assumed to terminate.
fn classify_compound(compound: &Compound, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    let pair = match &compound.kind {
        CompoundCommandKind::While(pair) | CompoundCommandKind::Until(pair) => pair,
        _ => return Verdict::refuse("compound command blocked"),
    };
    if !condition_can_terminate(&pair.guard) {
        return Verdict::refuse("loop condition cannot terminate");
    }
    let mut read_only = true;
    // Redirects attached to the whole loop (`… done > file`) scope-check like
    // any other file redirect; an in-scope write makes the loop a mutation.
    for redirect in &compound.io {
        match classify_redirect(redirect, ctx, table) {
            Ok(is_write) => read_only &= !is_write,
            Err(v) => return v,
        }
    }
    for part in [pair.guard.as_slice(), pair.body.as_slice()] {
        let v = classify_parsed(part, ctx, table);
        if v.decision != Decision::Allow {
            // A part the table merely flagged is refused by the loop's own
            // stricter bar — that is the algebra's rule, so it stays a
            // question; a part refused on the merits stays a refusal.
            let structural = v.decision != Decision::Deny || v.structural;
            return Verdict {
                decision: Decision::Deny,
                reason: "unsafe command in loop blocked".into(),
                read_only: false,
                structural,
            };
        }
        read_only &= v.read_only;
    }
    Verdict::allow("safe wait-loop", read_only)
}

/// Liveness heuristic for a wait-loop's condition: does the guard probe
/// something that can plausibly change over time?
///
/// True iff some guard command's program is a recognized *state probe* — a
/// predicate whose exit status naturally tracks mutable external state: a file
/// test (`test`/`[`), a process check (`pgrep`/`pidof`/`kill`), a
/// content/existence match. A guard made only of constants or always-succeeding
/// commands (`true`, `echo`, `sleep`) matches nothing here and is treated as
/// non-terminating.
///
/// Deliberately small and conservative — extend it as new polling idioms appear.
/// `kill` is here because `while kill -0 $PID` is the standard wait-for-process
/// idiom and Melete's copy of this list omitted it, hard-denying ~30 observed
/// benign loops while classifying a standalone `kill -0 $PID` as a read. That is
/// the kind of gap only the list can fix: it is decomposition algebra, not
/// something a leaf table could express.
fn condition_can_terminate(guard: &[Cmd]) -> bool {
    const PROBES: &[&str] = &[
        "test", "[", "pgrep", "pidof", "kill", "grep", "rg", "stat", "ls", "cat", "diff", "cmp",
    ];
    guard_programs(guard).iter().any(|prog| PROBES.contains(&prog.as_str()))
}

/// The program name of every simple command directly reachable in `cmds` —
/// walking `;`-sequences, `&&`/`||` chains and pipelines, but intentionally not
/// descending into nested compound commands or substitutions. Used only by the
/// liveness heuristic, which just needs to know which probes a guard names.
fn guard_programs(cmds: &[Cmd]) -> Vec<String> {
    let mut progs = Vec::new();
    for cmd in cmds {
        let Command::List(list) = &cmd.0 else { continue };
        let rest = list.rest.iter().map(|ao| match ao {
            AndOr::And(c) | AndOr::Or(c) => c,
        });
        for listable in std::iter::once(&list.first).chain(rest) {
            let stages: Vec<&Pipeable> = match listable {
                ListableCommand::Single(pipeable) => vec![pipeable],
                ListableCommand::Pipe(_, pipeables) => pipeables.iter().collect(),
            };
            for stage in stages {
                if let PipeableCommand::Simple(simple) = stage
                    && let Some(prog) =
                        simple.redirects_or_cmd_words.iter().find_map(|item| match item {
                            RedirectOrCmdWord::CmdWord(w) => Some(flatten_word(w).0),
                            RedirectOrCmdWord::Redirect(_) => None,
                        })
                {
                    progs.push(prog);
                }
            }
        }
    }
    progs
}

/// One simple command: gather its (quote-correct) words and its redirections.
/// Each embedded substitution is decomposed and classified on its own (all must
/// be `Allow`); each redirect is either harmless stream-plumbing or a file
/// operation whose destination is scope-checked. An in-scope file *write*
/// redirect means the command is not read-only even if the program alone is.
fn classify_simple(simple: &Simple, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    let mut redirect_writes = false;
    // Leading `FOO=bar` assignments and any redirects parsed before the program.
    for item in &simple.redirects_or_env_vars {
        match item {
            RedirectOrEnvVar::Redirect(r) => match classify_redirect(r, ctx, table) {
                Ok(is_write) => redirect_writes |= is_write,
                Err(v) => return v,
            },
            RedirectOrEnvVar::EnvVar(_, Some(val)) => {
                if let Some(v) = unsafe_substitution(val, ctx, table) {
                    return v;
                }
            }
            _ => {}
        }
    }
    let mut words: Vec<String> = Vec::new();
    for item in &simple.redirects_or_cmd_words {
        match item {
            RedirectOrCmdWord::Redirect(r) => match classify_redirect(r, ctx, table) {
                Ok(is_write) => redirect_writes |= is_write,
                Err(v) => return v,
            },
            RedirectOrCmdWord::CmdWord(w) => {
                let (literal, subs) = flatten_word(w);
                for inner in &subs {
                    let v = classify_substitution(inner, ctx, table);
                    if !v.is(Decision::Allow) {
                        return Verdict {
                            reason: "unsafe command substitution blocked".into(),
                            ..Verdict::deny_like(&v)
                        };
                    }
                }
                words.push(literal);
            }
        }
    }
    if words.is_empty() {
        // No program to run — only assignments/redirects (a bare `FOO=bar`).
        let decision = table.bash_default(ctx.bash_default);
        return Verdict::tier(decision, "unrecognized command", false);
    }
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let refs = unwrap_nix(&refs, ctx);
    let verdict = classify_program(refs, ctx, table);
    // A write redirect on an unscoped host is itself a filesystem mutation with
    // no scope to check — surface it even when the program alone would be
    // silently allowed (`echo x > /etc/foo` on a box we have no view of).
    if ctx.unscoped && redirect_writes && verdict.is(Decision::Allow) {
        return Verdict::flag("unscoped write redirect", false);
    }
    Verdict { read_only: verdict.read_only && !redirect_writes, ..verdict }
}

/// Flatten a parsed word to the literal text used for classification, and
/// collect every command substitution it embeds so each can be decomposed on its
/// own. Quoted content contributes its literal characters, so a `|`, `;`, `&`,
/// `$` or backtick inside quotes is data, never syntax. Parameter and
/// `${...}`/`$((...))` expansions collapse to [`OPAQUE`]: their value is
/// unknown, so they never match a program or flag a table keys on. A bare `$VAR`
/// is not by itself dangerous — only *command* substitution is.
fn flatten_word(word: &Wd) -> (String, Vec<&[Cmd]>) {
    fn simple<'a>(s: &'a SWord, out: &mut String, subs: &mut Vec<&'a [Cmd]>) {
        match s {
            SimpleWord::Literal(l) | SimpleWord::Escaped(l) => out.push_str(l),
            SimpleWord::Subst(sub) => match sub.as_ref() {
                ParameterSubstitution::Command(cmds) => subs.push(cmds.as_slice()),
                _ => out.push(OPAQUE),
            },
            SimpleWord::Param(_) => out.push(OPAQUE),
            SimpleWord::Star => out.push('*'),
            SimpleWord::Question => out.push('?'),
            SimpleWord::SquareOpen => out.push('['),
            SimpleWord::SquareClose => out.push(']'),
            SimpleWord::Tilde => out.push('~'),
            SimpleWord::Colon => out.push(':'),
        }
    }
    fn part<'a>(w: &'a Word<String, SWord>, out: &mut String, subs: &mut Vec<&'a [Cmd]>) {
        match w {
            Word::Simple(s) => simple(s, out, subs),
            Word::SingleQuoted(s) => out.push_str(s),
            Word::DoubleQuoted(ss) => ss.iter().for_each(|s| simple(s, out, subs)),
        }
    }
    let mut out = String::new();
    let mut subs = Vec::new();
    match &word.0 {
        ComplexWord::Single(w) => part(w, &mut out, &mut subs),
        ComplexWord::Concat(ws) => ws.iter().for_each(|w| part(w, &mut out, &mut subs)),
    }
    (out, subs)
}

/// If any substitution embedded in `word` classifies as something other than
/// `Allow`, the reason to deny by; otherwise `None`.
fn unsafe_substitution(word: &Wd, ctx: &PolicyContext, table: &dyn LeafTable) -> Option<Verdict> {
    for inner in flatten_word(word).1 {
        let v = classify_substitution(inner, ctx, table);
        if !v.is(Decision::Allow) {
            return Some(Verdict {
                reason: "unsafe command substitution blocked".into(),
                ..Verdict::deny_like(&v)
            });
        }
    }
    None
}

/// One command substitution, by decomposing it: reduce it to the `argv` of its
/// single inner command and classify that through the normal path. Anything not
/// cleanly reducible to one simple command — a pipeline, a chain, a compound
/// command, several commands, or a *further nested* substitution — is refused
/// rather than approximated.
fn classify_substitution(cmds: &[Cmd], ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    match substitution_argv(cmds) {
        Some(argv) => {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            let refs = unwrap_nix(&refs, ctx);
            classify_program(refs, ctx, table)
        }
        None => Verdict::refuse("unsupported command substitution"),
    }
}

/// The literal `argv` of a substitution's inner command, when it is a single
/// simple command with no redirects and no *further* nested substitution.
fn substitution_argv(cmds: &[Cmd]) -> Option<Vec<String>> {
    let [cmd] = cmds else { return None };
    let Command::List(list) = &cmd.0 else { return None };
    if !list.rest.is_empty() {
        return None; // an `&&`/`||` chain
    }
    let ListableCommand::Single(pipeable) = &list.first else {
        return None; // a pipeline
    };
    let PipeableCommand::Simple(simple) = pipeable else {
        return None; // compound command or function definition
    };
    if simple.redirects_or_env_vars.iter().any(|i| matches!(i, RedirectOrEnvVar::Redirect(_))) {
        return None;
    }
    let mut argv = Vec::new();
    for item in &simple.redirects_or_cmd_words {
        match item {
            RedirectOrCmdWord::Redirect(_) => return None,
            RedirectOrCmdWord::CmdWord(w) => {
                let (literal, subs) = flatten_word(w);
                if !subs.is_empty() {
                    return None; // nested substitution — deny rather than recurse
                }
                argv.push(literal);
            }
        }
    }
    (!argv.is_empty()).then_some(argv)
}

/// One redirect, against the workspace scope. `Ok(true)` = an allowed file
/// *write* (so the command is no longer read-only); `Ok(false)` = harmless (fd
/// plumbing like `2>&1`, the `/dev/null` bit bucket, or an in-scope read `<`);
/// `Err(verdict)` = refused, carrying the refusal the whole command takes. A
/// substituted target (`>&$(...)`) and heredocs are never trusted.
///
/// The error is a whole `Verdict` rather than a reason because these refusals
/// are not all of one kind: a target this pass cannot scope is a refusal for
/// want of understanding, but a target the table has declared untouchable is a
/// refusal on the merits, and a consumer that turns shape-refusals into
/// questions must not be handed the second as though it were the first.
fn classify_redirect(
    redirect: &Redirect<Wd>,
    ctx: &PolicyContext,
    table: &dyn LeafTable,
) -> Result<bool, Verdict> {
    match redirect {
        // fd duplication (`2>&1`, `1>&2`, `>&-`): stream plumbing, never a file.
        Redirect::DupRead(_, w) | Redirect::DupWrite(_, w) => {
            if flatten_word(w).1.is_empty() {
                Ok(false)
            } else {
                Err(Verdict::refuse("command substitution in redirect blocked"))
            }
        }
        // Reads (`<`) stay read-only-preserving; the target is still scoped.
        Redirect::Read(_, w) => scoped_redirect_target(w, ctx, table).map(|_| false),
        Redirect::Write(_, w)
        | Redirect::ReadWrite(_, w)
        | Redirect::Append(_, w)
        | Redirect::Clobber(_, w) => scoped_redirect_target(w, ctx, table),
        // A heredoc is the command's *stdin*, and the risk lives entirely
        // in what the command does with stdin — which the table already
        // decides: a shell reading its program from stdin is flagged, an
        // interpreter given a script is allowed, and a `cat` into a file
        // is scoped by the redirect beside it. Refusing every heredoc for
        // its shape asked the operator about `python3 - <<'EOF'` sixteen
        // times in one harness's whole history and was answered yes every
        // time — while the same script written to a file and run would
        // have passed silently. The one thing a body can do *outside* its
        // command is expand: an unquoted delimiter runs `$(…)` in the
        // calling shell, so a body carrying a substitution is still a
        // shape this pass will not vouch for. A table that wants to be
        // asked about code on stdin flags the bare interpreter (or its
        // `-`), the way it flags `-c`.
        Redirect::Heredoc(_, w) => {
            if flatten_word(w).1.is_empty() {
                Ok(false)
            } else {
                Err(Verdict::refuse("command substitution in heredoc blocked"))
            }
        }
    }
}

/// A redirect's file target against the workspace scope. `Ok(true)` = an
/// in-scope real file; `Ok(false)` = the `/dev/null` bit bucket, which writes
/// nothing; `Err(verdict)` = a substituted target, one that escapes, no scope
/// at all, or a target the table protects.
fn scoped_redirect_target(
    w: &Wd,
    ctx: &PolicyContext,
    table: &dyn LeafTable,
) -> Result<bool, Verdict> {
    let (target, subs) = flatten_word(w);
    if !subs.is_empty() {
        return Err(Verdict::refuse("command substitution in redirect blocked"));
    }
    if target == "/dev/null" {
        return Ok(false);
    }
    if table.protected_path(&target) {
        // On the merits, exactly as the same path refused as a filesystem
        // verb's target would be: `> f` and `tee f` are one rule, and a rule
        // that holds only until the write is spelled differently is not one.
        return Err(Verdict::deny("target is protected state"));
    }
    match &ctx.workspace_root {
        Some(root) if path_within(Path::new(&target), root) => Ok(true),
        Some(_) => Err(Verdict::refuse("redirection escapes workspace")),
        // On an unscoped host there is no root to check the target against;
        // treat it as a reviewable write rather than deny — the caller
        // escalates an otherwise-Allow command to Flag.
        None if ctx.unscoped => Ok(true),
        None => Err(Verdict::refuse("redirection without workspace scope")),
    }
}

/// If Nix is declared and the command is `nix develop … -c <cmd> …`, the inner
/// command's tokens, so the real work is what gets classified.
fn unwrap_nix<'a>(tokens: &'a [&'a str], ctx: &PolicyContext) -> &'a [&'a str] {
    if ctx.has(Toolchain::Nix)
        && tokens.first() == Some(&"nix")
        && tokens.get(1) == Some(&"develop")
        && let Some(pos) = tokens.iter().position(|t| *t == "-c" || *t == "--command")
        && pos + 1 < tokens.len()
    {
        return &tokens[pos + 1..];
    }
    tokens
}

/// One leaf command's program, already unwrapped from any `nix develop -c` shim.
///
/// Three things happen here and in this order. The consumer's own
/// [`LeafTable::program_override`] is consulted first, so a rule that must sit
/// outside the tunable table can. Filesystem-mutating verbs are then routed
/// through the scope check, because *whether a target path is in-scope* is
/// decomposition, not a tier. Everything else is the table's call.
fn classify_program(tokens: &[&str], ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    if let Some(v) = table.program_override(tokens, ctx) {
        return v;
    }
    let prog = tokens.first().copied().unwrap_or("");
    if FS_VERBS.contains(&prog) {
        return classify_fs_verb(prog, tokens, ctx, table);
    }
    table.classify_program(tokens, ctx)
}

/// A filesystem-mutating verb, by whether every path argument stays inside the
/// workspace. Without a workspace there is no safe scope. The scope check is
/// decomposition; only the destructive/benign tier for an already-in-scope
/// target is the table's call.
fn classify_fs_verb(
    prog: &str,
    tokens: &[&str],
    ctx: &PolicyContext,
    table: &dyn LeafTable,
) -> Verdict {
    let Some(root) = &ctx.workspace_root else {
        if ctx.unscoped {
            // No root exists for a host we have no view of. Rather than
            // silently allow or hard-deny, surface every unscoped filesystem
            // mutation for review.
            return Verdict::flag("unscoped filesystem mutation", false);
        }
        return Verdict::refuse("filesystem mutation without workspace scope");
    };
    let paths: Vec<&str> =
        tokens.iter().skip(1).filter(|t| !t.starts_with('-')).copied().collect();
    if paths.is_empty() {
        return Verdict::refuse("filesystem mutation with no path");
    }
    let mut into_tmp = false;
    for p in &paths {
        if table.protected_path(p) {
            return Verdict::deny("target is protected state");
        }
        let path = Path::new(p);
        if !path_within(path, root) {
            // An escape into `/tmp` is surfaced for review rather than
            // hard-denied; any other escape is a hard Deny.
            if target_under_tmp(path, root) {
                into_tmp = true;
            } else {
                return Verdict::refuse("filesystem mutation escapes workspace");
            }
        }
    }
    if into_tmp {
        return Verdict::flag("filesystem mutation into /tmp", false);
    }
    // In-scope: benign creators and movers apply silently; deleters and
    // overwriters are flagged.
    if table.fs_verb_tier(prog) == Decision::Flag {
        Verdict::flag("destructive op in workspace", false)
    } else {
        Verdict::allow("workspace file op", false)
    }
}
