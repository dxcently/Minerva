//! Tool search: a registry's deferred half, and the one tool that reaches
//! into it.
//!
//! A registry used to be the request's tool list, whole. That was fine at
//! nine tools and is not fine at fifty: Melete alone publishes forty-odd,
//! each with a paragraph of description and a schema, and a session that
//! never starts a job on that box would still pay for all of them on every
//! request. So a manifest may say `deferred: true`, and a deferred tool is
//! **registered but not offered** — the dispatcher runs it by name like any
//! other, the policy hook classifies it, the log journals it, but its
//! definition is not in the request until the branch has *reached* for it.
//!
//! ## Reaching
//!
//! Two things reach a deferred tool, and both are on the branch already:
//!
//! - a `tool_search` call whose query finds it (the top [`LIMIT`] hits of
//!   every search the branch has made), and
//! - a call to it by name — a model that remembers a tool from an earlier
//!   context, or an operator's dispatch line, has said which tool it wants
//!   without a search.
//!
//! [`Reach::of`] reads both off the branch, and [`offered`] turns that into
//! the request's tool list: everything not deferred, plus every deferred
//! tool the branch has reached, in registration order. It is recomputed
//! from the log per request and **stored nowhere** — which is what makes a
//! resumed session, an adopted one and a fork below the search all offer
//! exactly what their branch has earned, with no record kind and nothing to
//! replay. The one condition that arrangement rests on is that a search is
//! a *pure function* of the query and the registry: the same query re-run at
//! request time must find what it found when the model ran it, so the
//! ranking below has no clock, no randomness and no memory.
//!
//! ## What a search costs
//!
//! The tools are the front of the prompt on every wire, so a search that
//! adds one is one cache miss on the next request — the same price as a
//! `:persona` or a session note, paid once per tool and never again on that
//! branch. That is why the hits are activated all at once and stay
//! activated, rather than being offered for one call and withdrawn.
//!
//! ## The tool itself
//!
//! [`ToolSearch`] is a native tool in the core, beside `choices_user`, for
//! the reason `choices_user` is one: it is a seam of the harness and not a primitive
//! anybody rewrites. It needs the registry, and the registry owns it, so
//! the link is a `Weak<Dispatcher>` set after construction ([`ToolSearch::attach`]),
//! exactly as the Rune host's is. It is never deferred itself, and must not
//! be — a search tool nobody can find is no search tool.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::dispatch::Dispatcher;
use crate::policy::Approval;
use crate::provider::ToolDef;
use crate::session::{RecordKind, Session};
use crate::tool::{CallContext, Tool, ToolManifest, ToolOutput, ToolRegistry};

/// The search tool's name, which a reach scan looks for on the branch.
pub const NAME: &str = "tool_search";

/// How many hits a search activates. The same constant on both sides —
/// the report the model reads and the re-run at request time — so the
/// two cannot disagree about which tools a query opened.
pub const LIMIT: usize = 8;

/// What the branch has reached for among the deferred tools: the names it
/// has called, and the queries it has searched with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reach {
    pub called: HashSet<String>,
    pub queries: Vec<String>,
}

impl Reach {
    /// Note one call on the branch: a search contributes its query, any
    /// other tool contributes its name.
    pub fn note(&mut self, name: &str, input: &Value) {
        if name == NAME {
            if let Some(q) = input.get("query").and_then(Value::as_str) {
                let q = q.trim();
                if !q.is_empty() {
                    self.queries.push(q.to_string());
                }
            }
        } else {
            self.called.insert(name.to_string());
        }
    }

    /// Everything the current branch has called or searched for: every
    /// `tool_use` in an assistant message and every call the operator
    /// made, struck records excluded because they are out of the
    /// conversation. The whole branch and not only what follows a
    /// compaction — a tool the summary refers to is one the model may
    /// well reach for again, and an offered tool costs context, not
    /// correctness.
    pub fn of(session: &Session) -> Reach {
        let struck = session.excluded();
        let mut reach = Reach::default();
        for r in session.branch() {
            if struck.contains(&r.id) {
                continue;
            }
            match &r.kind {
                RecordKind::AssistantMessage(m) => {
                    for (_, name, input) in m.tool_uses() {
                        reach.note(name, &input.to_value());
                    }
                }
                RecordKind::UserToolCall { name, input, .. } => reach.note(name, &input.to_value()),
                _ => {}
            }
        }
        reach
    }
}

/// The query's terms: lowercased, split on anything that is not a letter
/// or digit, so `run_code_task`, `run-code-task` and `run code task` are
/// the same three words.
fn terms(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// How well one manifest answers a query; zero is no match at all.
fn score(m: &ToolManifest, query: &str, qterms: &[String]) -> u32 {
    let name = m.name.to_lowercase();
    let name_terms = terms(&m.name);
    let desc = m.description.to_lowercase();
    let mut s = 0;
    if !query.is_empty() && name == query.trim().to_lowercase() {
        s += 10;
    }
    for t in qterms {
        if name_terms.iter().any(|n| n == t) {
            s += 4;
        } else if name.contains(t.as_str()) {
            s += 2;
        }
        if desc.contains(t.as_str()) {
            s += 1;
        }
    }
    s
}

/// The tools a query finds, best first, at most `limit` of them; and how
/// many more matched below the cut. Every registered tool is searched,
/// deferred or not, because the search is also how a model finds out what
/// it already has. Pure: the same query on the same registry always ranks
/// the same, which [`Reach`] depends on.
pub fn search(reg: &ToolRegistry, query: &str, limit: usize) -> (Vec<ToolManifest>, usize) {
    let qterms = terms(query);
    if qterms.is_empty() {
        return (Vec::new(), 0);
    }
    let mut hits: Vec<(u32, usize, ToolManifest)> = reg
        .manifests()
        .into_iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let s = score(&m, query, &qterms);
            (s > 0).then_some((s, i, m))
        })
        .collect();
    // Best first; registration order breaks ties, so the ranking is a
    // function of the registry and nothing else.
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let more = hits.len().saturating_sub(limit);
    hits.truncate(limit);
    (hits.into_iter().map(|(_, _, m)| m).collect(), more)
}

/// The names the request offers: every tool that is not deferred, plus
/// every deferred one the branch has reached — called by name, or found
/// by a search re-run here. Registration order throughout, so the list
/// is stable across requests on one branch.
pub fn offered(reg: &ToolRegistry, reach: &Reach) -> Vec<String> {
    let mut opened: HashSet<String> = reach.called.clone();
    for q in &reach.queries {
        for m in search(reg, q, LIMIT).0 {
            opened.insert(m.name);
        }
    }
    reg.manifests()
        .into_iter()
        .filter(|m| !m.deferred || opened.contains(&m.name))
        .map(|m| m.name)
        .collect()
}

/// The model-facing definitions of [`offered`].
pub fn offered_defs(reg: &ToolRegistry, reach: &Reach) -> Vec<ToolDef> {
    let names = offered(reg, reach);
    reg.manifests()
        .into_iter()
        .filter(|m| names.contains(&m.name))
        .map(|m| m.for_model())
        .collect()
}

/// The first sentence of a description, for a listing that must stay one
/// line per tool.
fn first_sentence(s: &str) -> String {
    let s = s.trim();
    let end = s.find(". ").map(|i| i + 1).unwrap_or(s.len());
    let mut out: String = s[..end].chars().take(140).collect();
    if out.len() < end {
        out.push('…');
    }
    out
}

/// What the model reads back from a search. The hits are returned whole
/// — name, description and schema — because they are about to be in the
/// request anyway and a model should not have to call a tool it has only
/// seen the name of. An empty query lists every deferred tool by name
/// and first sentence, which opens nothing.
pub fn report(reg: &ToolRegistry, query: &str) -> String {
    let deferred: Vec<ToolManifest> = reg.manifests().into_iter().filter(|m| m.deferred).collect();
    if terms(query).is_empty() {
        if deferred.is_empty() {
            return "No deferred tools are registered; every tool is already offered.".into();
        }
        let mut out = format!(
            "{} deferred tool(s) can be found by name or keyword and are not offered until found:\n",
            deferred.len()
        );
        for m in &deferred {
            out.push_str(&format!(
                "- {} — {}\n",
                m.name,
                first_sentence(&m.description)
            ));
        }
        return out;
    }
    let (hits, more) = search(reg, query, LIMIT);
    if hits.is_empty() {
        return format!(
            "No tool matches {query:?}. {} deferred tool(s) exist; try another keyword, or an empty query to list them all.",
            deferred.len()
        );
    }
    let defs: Vec<Value> = hits
        .iter()
        .map(|m| json!({ "name": m.name, "description": m.description, "input_schema": m.input_schema }))
        .collect();
    let mut out = format!(
        "{} tool(s) match {query:?} and can now be called:\n\n{}\n",
        hits.len(),
        Value::Array(defs)
    );
    if more > 0 {
        // Named so the model can call one directly or search again, since
        // only the hits above were opened by this search.
        let (rest, _) = search(reg, query, LIMIT + more);
        let names: Vec<&str> = rest.iter().skip(LIMIT).map(|m| m.name.as_str()).collect();
        out.push_str(&format!(
            "\n{more} more match but were not opened by this search — search with a narrower query, or call one by name: {}\n",
            names.join(", ")
        ));
    }
    out
}

/// The tool. See the module docs.
pub struct ToolSearch {
    manifest: ToolManifest,
    dispatcher: OnceLock<Weak<Dispatcher>>,
}

impl Default for ToolSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolSearch {
    pub fn new() -> Self {
        ToolSearch {
            manifest: ToolManifest {
                name: NAME.into(),
                description: "Find tools that are registered but not yet offered to you. Search by keyword or name (e.g. \"schedule\", \"melete run code task\"); the matches are returned with their schemas and become callable at once. An empty query lists every deferred tool by name.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "Keywords or a tool name; empty to list everything deferred" } },
                    "required": ["query"]
                }),
                approval: Approval::ReadOnly,
                prompt: None,
                render: None,
                deferred: false,
            },
            dispatcher: OnceLock::new(),
        }
    }

    /// Link the dispatcher whose registry this searches. The registry owns
    /// this tool, so the link is weak and set after construction, as the
    /// Rune host's is.
    pub fn attach(&self, d: &Arc<Dispatcher>) {
        let _ = self.dispatcher.set(Arc::downgrade(d));
    }
}

#[async_trait]
impl Tool for ToolSearch {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    async fn call(&self, input: Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        let Some(d) = self.dispatcher.get().and_then(Weak::upgrade) else {
            return Ok(ToolOutput::error(
                "tool_search has no registry attached yet",
            ));
        };
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(ToolOutput::ok(report(d.registry(), query)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub(ToolManifest);

    #[async_trait]
    impl Tool for Stub {
        fn manifest(&self) -> &ToolManifest {
            &self.0
        }
        async fn call(&self, _: Value, _: CallContext) -> anyhow::Result<ToolOutput> {
            Ok(ToolOutput::ok(""))
        }
    }

    fn tool(name: &str, description: &str, deferred: bool) -> Arc<dyn Tool> {
        Arc::new(Stub(ToolManifest {
            name: name.into(),
            description: description.into(),
            input_schema: json!({ "type": "object" }),
            approval: Approval::ReadOnly,
            prompt: None,
            render: None,
            deferred,
        }))
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(tool("read", "Read a file.", false));
        r.register(tool("bash", "Run a shell command.", false));
        r.register(tool(
            "melete_run_code_task",
            "Start an autonomous coding run on a repo, ending in a PR.",
            true,
        ));
        r.register(tool(
            "melete_schedule_code_task",
            "Later, at a time or after another job.",
            true,
        ));
        r.register(tool(
            "melete_job_status",
            "The outcome of one run by id.",
            true,
        ));
        r.register(tool("melete_list_runs", "Every run, newest first.", true));
        r
    }

    #[test]
    fn a_name_outranks_a_description_and_ties_break_by_registration() {
        let r = registry();
        let (hits, more) = search(&r, "code task", LIMIT);
        let names: Vec<&str> = hits.iter().map(|m| m.name.as_str()).collect();
        // Both carry "code" and "task" in the name; the earlier-registered wins the tie.
        assert_eq!(
            &names[..2],
            &["melete_run_code_task", "melete_schedule_code_task"]
        );
        assert_eq!(more, 0);
        // A description-only hit ranks below a name hit but is still found.
        let (hits, _) = search(&r, "run", LIMIT);
        assert_eq!(hits[0].name, "melete_run_code_task");
        assert!(
            hits.iter().any(|m| m.name == "bash"),
            "\"Run a shell command\" matches on the description"
        );
        // An exact name is first whatever else matches.
        let (hits, _) = search(&r, "melete_job_status", LIMIT);
        assert_eq!(hits[0].name, "melete_job_status");
        // Nothing, and no terms, find nothing.
        assert!(search(&r, "   ", LIMIT).0.is_empty());
        assert!(search(&r, "zzz", LIMIT).0.is_empty());
    }

    #[test]
    fn the_cut_is_reported_and_the_ranking_is_stable() {
        let r = registry();
        let (hits, more) = search(&r, "melete", 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(more, 2);
        // Re-running the same query yields the same list — what `Reach` rests on.
        assert_eq!(search(&r, "melete", 2).0, hits);
    }

    #[test]
    fn deferred_tools_are_offered_only_once_reached() {
        let r = registry();
        let none = Reach::default();
        assert_eq!(offered(&r, &none), vec!["read", "bash"]);
        // A search opens its hits, in registration order, beside the rest.
        let mut reach = Reach::default();
        reach.note(NAME, &json!({ "query": "code task" }));
        assert_eq!(
            offered(&r, &reach),
            vec![
                "read",
                "bash",
                "melete_run_code_task",
                "melete_schedule_code_task"
            ]
        );
        // A call by name opens that tool and nothing else.
        let mut reach = Reach::default();
        reach.note("melete_list_runs", &json!({}));
        assert_eq!(
            offered(&r, &reach),
            vec!["read", "bash", "melete_list_runs"]
        );
        // A search with no terms opens nothing.
        let mut reach = Reach::default();
        reach.note(NAME, &json!({ "query": " " }));
        assert_eq!(offered(&r, &reach), vec!["read", "bash"]);
        // The defs follow the same list.
        let mut reach = Reach::default();
        reach.note(NAME, &json!({ "query": "status" }));
        let defs = offered_defs(&r, &reach);
        assert_eq!(defs.last().unwrap().name, "melete_job_status");
    }

    #[test]
    fn the_report_carries_schemas_for_hits_and_names_for_the_rest() {
        let r = registry();
        let text = report(&r, "code task");
        assert!(text.starts_with("2 tool(s) match \"code task\""), "{text}");
        assert!(
            text.contains("\"input_schema\""),
            "hits come with their schema"
        );
        // An empty query lists the deferred tools and opens nothing.
        let listing = report(&r, "");
        assert!(listing.starts_with("4 deferred tool(s)"), "{listing}");
        assert!(listing.contains("- melete_job_status — The outcome of one run by id."));
        assert!(
            !listing.contains("- read"),
            "an offered tool is not a deferred one"
        );
        // A miss says how to go on.
        assert!(report(&r, "zzz").contains("No tool matches \"zzz\""));
    }

    #[test]
    fn a_registry_of_offered_tools_reports_so() {
        let mut r = ToolRegistry::new();
        r.register(tool("read", "Read a file.", false));
        assert!(report(&r, "").contains("every tool is already offered"));
    }

    // The manifest promises a search's hits "become callable at once".
    // What makes that true is that nothing is stored between requests:
    // the search lands on the branch as a `tool_use` record the moment
    // the model emits it, and the continuation request of the same turn
    // recomputes the offered list from the branch and finds the hit
    // there — no restart, no turn boundary, no cache to invalidate.
    #[test]
    fn a_search_on_the_branch_opens_its_hits_for_the_next_request_of_the_same_turn() {
        use crate::message::{ContentBlock, Message};
        let r = registry();
        let dir = tempfile::tempdir().unwrap();
        let mut session =
            Session::create(&dir.path().join("s.eid"), "test", dir.path(), None).unwrap();
        // Before the search, the branch reaches nothing deferred.
        assert_eq!(offered(&r, &Reach::of(&session)), vec!["read", "bash"]);
        // The model calls tool_search; the loop journals the assistant
        // message carrying the tool_use before the next request is built.
        session
            .append(RecordKind::AssistantMessage(Message::assistant(vec![
                ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: NAME.into(),
                    input: json!({ "query": "code task" }).into(),
                },
            ])))
            .unwrap();
        // The same turn's next request offers the hits.
        let offered = offered(&r, &Reach::of(&session));
        assert!(
            offered.contains(&"melete_run_code_task".to_string()),
            "{offered:?}"
        );
        assert!(
            offered.contains(&"melete_schedule_code_task".to_string()),
            "{offered:?}"
        );
        // And nothing else opened: the misses stay deferred.
        assert!(
            !offered.contains(&"melete_job_status".to_string()),
            "{offered:?}"
        );
    }
}
