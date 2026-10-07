//! `GET /hub/runs/graphs` and `GET /hub/runs/graphs/<g>` (slice R1b): the graphs the new-run form offers, and one graph's JSON with its hash.
//!
//! ```text
//!   GET /hub/runs/graphs       200 [{name, sha256, bytes}] in --run-graphs order: the graphs a start would accept (the name is in the list,
//!                                  and both <plugins>/jev/workflows/<g>.rn and <plugins>/jev/graphs/<g>.json are there and usable)
//!                              503 {error, why} when the perch has no --plugins-dir
//!   GET /hub/runs/graphs/<g>   200 {name, sha256, bytes, graph}: `graph` is <plugins>/jev/graphs/<g>.json, byte for byte as it is on disk
//!                              404 (empty) a name outside --run-graphs, one that is not a bare name, or a missing workflow or graph file
//!                              413 {error} a graph file over 1 MiB        502 {error} a file that is not a JSON object     503 as above
//! ```
//!
//! The graph is the `jev/graphs/<g>.json` file, not the workflow (`workflows/<g>.rn` is the program, and a start needs it, so a graph
//! without one is not offered). The page reads `meta.jev.input.properties` out of the graph for the form's fields; the perch reads nothing
//! out of it beyond "this is a JSON object". `sha256` is the lower-case hex of the file's bytes, the hash the workflow pins (`PIN` in
//! `wiki-hop.rn`, there written `sha256:<hex>`).
//!
//! **The graph is embedded, not re-serialized.** `serde_json` here keeps no key order, and the order of `states` is the order the file
//! gives them, which the drawing may show; so the answer is assembled around the file's own bytes after they have been parsed once to
//! prove they are a JSON object (a BOM or a half-written file is a 502, never a broken answer).
//!
//! **Bounds.** A name is a bare name from the operator's list before it is joined to a path (the same check a start makes), the file is
//! opened without following a link or blocking ([`crate::rawfile::open_regular`]) and read to at most 1 MiB, and a request with a query
//! is a 400: nothing here takes one.

use std::io::Read as _;
use std::path::Path;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::header::HeaderValue;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::Value;

use super::state::refuse;
use crate::serve::{BoxBody, Ctx, status};

/// The most a graph file may be: the largest of the six in eidolon-plugins is 11 KB.
pub const MAX_GRAPH: u64 = 1024 * 1024;

fn full(bytes: impl Into<Bytes>) -> BoxBody {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed_unsync()
}

/// A graph file as it was read, with what the listing says of it.
struct Graph {
    name: String,
    sha256: String,
    bytes: Vec<u8>,
}

/// Why a name gave no graph.
enum Why {
    /// Either file is not there (or is not a regular file).
    Missing,
    TooBig,
    /// It is there and it is not a JSON object.
    Broken,
}

/// `<plugins>/jev/graphs/<name>.json`, once `<plugins>/jev/workflows/<name>.rn` is there too. `name` is a bare name (the caller checked).
fn read(plugins: &Path, name: &str) -> Result<Graph, Why> {
    let jev = plugins.join("jev");
    // As a start checks it: what cannot be started is not offered.
    if !std::fs::metadata(jev.join("workflows").join(format!("{name}.rn"))).is_ok_and(|meta| meta.is_file()) {
        return Err(Why::Missing);
    }
    let (file, _) = crate::rawfile::open_regular(&jev.join("graphs").join(format!("{name}.json"))).map_err(|_| Why::Missing)?;
    // One byte past the cap is how "over" is told from "exactly the cap", and it bounds the read whatever the file does meanwhile.
    let mut bytes = Vec::new();
    file.take(MAX_GRAPH + 1).read_to_end(&mut bytes).map_err(|_| Why::Missing)?;
    if bytes.len() as u64 > MAX_GRAPH {
        return Err(Why::TooBig);
    }
    if !serde_json::from_slice::<Value>(&bytes).is_ok_and(|graph| graph.is_object()) {
        return Err(Why::Broken);
    }
    Ok(Graph { name: name.to_string(), sha256: harnox::crypto::hex(&harnox::crypto::sha256(&bytes)), bytes })
}

/// `{"name": ..., "sha256": ..., "bytes": N` with no closing brace: what both answers start with.
fn head(graph: &Graph) -> String {
    let name = serde_json::to_string(&graph.name).unwrap_or_else(|_| "\"\"".into());
    format!("{{\"name\":{name},\"sha256\":\"{}\",\"bytes\":{}", graph.sha256, graph.bytes.len())
}

/// The one-graph answer: the head, then the file's own bytes as `graph`.
fn one_body(graph: &Graph) -> Vec<u8> {
    let mut body = head(graph).into_bytes();
    body.extend_from_slice(b",\"graph\":");
    body.extend_from_slice(&graph.bytes);
    body.push(b'}');
    body
}

fn list_body(graphs: &[Graph]) -> Vec<u8> {
    let mut body = vec![b'['];
    for (n, graph) in graphs.iter().enumerate() {
        if n > 0 {
            body.push(b',');
        }
        body.extend_from_slice(head(graph).as_bytes());
        body.push(b'}');
    }
    body.push(b']');
    body
}

fn answer(body: Vec<u8>) -> Response<BoxBody> {
    Response::builder().status(StatusCode::OK).header("content-type", HeaderValue::from_static("application/json")).body(full(body)).unwrap()
}

/// `rest` is `""` for the list and the graph's name for one graph; a name outside `--run-graphs` is the empty 404.
pub async fn get(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let (method, query) = (req.method().clone(), req.uri().query().unwrap_or("").to_string());
    drop(req);
    if method != Method::GET {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    if !query.is_empty() {
        return status(StatusCode::BAD_REQUEST);
    }
    // The name is a bare name from the operator's list before it is joined to any path.
    if !rest.is_empty() && (!crate::graph_name_ok(rest) || !ctx.opts.run_graphs.iter().any(|offered| offered == rest)) {
        return status(StatusCode::NOT_FOUND);
    }
    let Some(plugins) = ctx.opts.plugins_dir.clone() else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "this perch has no --plugins-dir, so there are no graphs to run", Value::Null);
    };
    let names: Vec<String> = match rest {
        "" => ctx.opts.run_graphs.iter().filter(|name| crate::graph_name_ok(name)).cloned().collect(),
        name => vec![name.to_string()],
    };
    // Blocking reads, off the reactor.
    let Ok(found) = tokio::task::spawn_blocking(move || names.iter().map(|name| read(&plugins, name)).collect::<Vec<_>>()).await else {
        return status(StatusCode::INTERNAL_SERVER_ERROR);
    };
    if rest.is_empty() {
        let graphs: Vec<Graph> = found.into_iter().filter_map(Result::ok).collect();
        return answer(list_body(&graphs));
    }
    match found.into_iter().next() {
        Some(Ok(graph)) => answer(one_body(&graph)),
        Some(Err(Why::Missing)) | None => status(StatusCode::NOT_FOUND),
        Some(Err(Why::TooBig)) => refuse(StatusCode::PAYLOAD_TOO_LARGE, "that graph file is over 1 MiB", Value::Null),
        Some(Err(Why::Broken)) => refuse(StatusCode::BAD_GATEWAY, "that graph file is not a JSON object", Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(name: &str, text: &str) -> Graph {
        Graph { name: name.to_string(), sha256: "ab".repeat(32), bytes: text.as_bytes().to_vec() }
    }

    #[test]
    fn the_answer_is_json_around_the_files_own_bytes_in_the_files_own_order() {
        let g = graph("wiki-hop", "{\n  \"states\": {\"open\": 1, \"arrived\": 2, \"failed\": 3},\n  \"id\": \"x\"\n}\n");
        let body = String::from_utf8(one_body(&g)).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!((parsed["name"].as_str(), parsed["bytes"].as_u64()), (Some("wiki-hop"), Some(g.bytes.len() as u64)));
        assert_eq!(parsed["graph"]["id"], "x");
        // The file's text is in the answer untouched, so the keys come in the order the file has them (open before arrived).
        assert!(body.contains(std::str::from_utf8(&g.bytes).unwrap().trim_end()), "{body}");
        assert!(body.find("\"open\"").unwrap() < body.find("\"arrived\"").unwrap(), "{body}");
    }

    #[test]
    fn the_list_is_an_array_of_heads_and_empty_is_still_an_array() {
        assert_eq!(list_body(&[]), b"[]");
        let two = [graph("a", "{}"), graph("b-2", "{}")];
        let parsed: Value = serde_json::from_slice(&list_body(&two)).unwrap();
        let names: Vec<&str> = parsed.as_array().unwrap().iter().map(|entry| entry["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["a", "b-2"]);
        assert!(parsed[0].get("graph").is_none(), "the list carries no graph: {parsed}");
    }
}
