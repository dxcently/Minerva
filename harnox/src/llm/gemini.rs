use serde_json::{json, Value};
use crate::llm::provider::ChatRequest;

/// The events the Gemini wire yields, in the vocabulary of the canonical
/// stream: the SSE body is parsed into these and mapped 1:1 onto
/// [`crate::llm::provider::StreamEvent`] by the consumer.
pub enum Event {
    Start { usage: crate::llm::Usage },
    Delta(String),
    ThinkingDelta(String),
    ThinkingSignature(String),
    ToolUseStart { id: String, name: String },
    ToolInputDelta(String),
    BlockStop,
    Stop { stop_reason: crate::llm::StopReason, usage: crate::llm::Usage },
}

pub struct Gemini;

/// Google's Code Assist surface gates on the client's stated identity —
/// the proxy ecosystem bumps this string when requests start failing
/// `403`. These are sent whenever a `project` is set (the Antigravity
/// wrapper); a bare `generateContent` provider sends none of them.
pub fn code_assist_headers() -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), "antigravity/cli/1.1.24 windows/amd64".into()),
        ("requestType".into(), "agent".into()),
        ("requestId".into(), format!("req-{}", uuid::Uuid::new_v4())),
    ]
}

impl Gemini {

    pub fn endpoint(base: &str) -> String {
        format!("{base}/v1internal:streamGenerateContent?alt=sse")
    }

    pub fn request_body(req: ChatRequest, project: Option<String>) -> Value {
        use crate::llm::message::{ContentBlock, ImageSource, Role};

        // Gemini matches a functionResponse to its call by *function name*
        // — the wire has no call ids — so every ToolResult needs the name
        // of the ToolUse it answers, which lives in the preceding assistant
        // message. One pass collects the map before the mapping pass spends
        // it.
        let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for msg in req.wire_messages().as_ref() {
            for block in &msg.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    names.insert(id.clone(), name.clone());
                }
            }
        }

        let mut contents = Vec::new();
        let system = req.system.clone();

        for msg in req.wire_messages().as_ref() {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "model",
            };

            let mut parts = Vec::new();
            let mut pending_signature: Option<Value> = None;

            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        parts.push(json!({"text": text}));
                    }
                    ContentBlock::Image { source, .. } => {
                        if let ImageSource::Base64 { media_type, data } = source {
                            parts.push(json!({
                                "inlineData": {
                                    "mimeType": media_type,
                                    "data": data,
                                }
                            }));
                        }
                    }
                    ContentBlock::ToolUse { name, input, .. } => {
                        let mut part = json!({
                            "functionCall": {
                                "name": name,
                                "args": input.to_value(),
                            }
                        });
                        if let Some(sig) = pending_signature.take() {
                            part["thoughtSignature"] = sig;
                        }
                        parts.push(part);
                    }
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        // `name` is the join key; the response body is the
                        // tool's output — sent as JSON when it parses (tools
                        // that return objects keep their shape) and wrapped
                        // as `{"result": …}` when it does not, which is the
                        // form every Gemini client convention reads.
                        let name = names.get(tool_use_id).cloned()
                            .unwrap_or_else(|| "unknown_function".to_string());
                        let response: Value = match serde_json::from_str(content) {
                            Ok(v @ Value::Object(_)) => v,
                            _ => json!({
                                "result": content,
                                "isError": is_error,
                            }),
                        };
                        parts.push(json!({
                            "functionResponse": {
                                "name": name,
                                "response": response,
                            }
                        }));
                    }
                    // The thought *text* is never replayed; its signature —
                    // where Gemini 3 puts it — is. A signature riding an
                    // empty thinking block is exactly the "signature with
                    // no thought" the wire can produce, and it must survive
                    // the round trip or the next tool-loop request is
                    // rejected.
                    ContentBlock::Thinking { signature, .. } => {
                        if !signature.is_empty() {
                            pending_signature = Some(
                                serde_json::from_str::<Value>(signature)
                                    .unwrap_or(json!(signature)),
                            );
                        }
                    }
                    ContentBlock::RedactedThinking { .. } => {}
                }
            }
            if let Some(sig) = pending_signature.take() {
                parts.push(json!({
                    "thoughtSignature": sig,
                }));
            }
            if !parts.is_empty() {
                contents.push(json!({
                    "role": role,
                    "parts": parts,
                }));
            }
        }
        
        let mode = match req.tool_choice {
            crate::llm::provider::ToolChoice::Auto => "VALIDATED",
            crate::llm::provider::ToolChoice::Required => "ANY",
        };
        let mut request = json!({
            "contents": contents,
            "model": req.model,
            "toolConfig": {
                "functionCallingConfig": {
                    "mode": mode
                }
            }
        });
        
        let mut gen_config = json!({});
        if let Some(t) = req.temperature {
            gen_config["temperature"] = json!(t);
        }
        if req.max_tokens > 0 {
            gen_config["maxOutputTokens"] = json!(req.max_tokens);
        }
        if !gen_config.as_object().unwrap().is_empty() {
            request["generationConfig"] = gen_config;
        }
        
        let tools: Vec<_> = req.tools.into_iter().map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            })
        }).collect();
        if !tools.is_empty() {
            request["tools"] = json!([{
                "functionDeclarations": tools
            }]);
        }
        
        if let Some(sys) = system {
            request["systemInstruction"] = json!({
                "parts": [{"text": sys}]
            });
        }
        
        if let Some(proj) = project {
            let session_id = if req.cache.retention.is_off() {
                format!("-{}", uuid::Uuid::new_v4().as_u128() % 9_000_000_000_000_000_000)
            } else {
                req.cache.key.clone().unwrap_or_else(|| {
                    // Hash the first user text if no session key, or just fallback
                    format!("-{}", uuid::Uuid::new_v4().as_u128() % 9_000_000_000_000_000_000)
                })
            };
            
            request["sessionId"] = json!(session_id);
            request["labels"] = json!({
                "last_step_index": "1",
                "model_enum": req.model,
                "trajectory_id": session_id,
            });

            json!({
                "project": proj,
                "requestId": format!("agent/req-{}/1/traj-1/1", uuid::Uuid::new_v4()),
                "request": request,
                "model": req.model,
                "userAgent": "antigravity",
                "requestType": "agent",
            })
        } else {
            request
        }
    }
    

    
    pub fn stream_events<'a>(
        stream: impl futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Unpin + 'a,
    ) -> impl futures_util::Stream<Item = anyhow::Result<Event>> + 'a {
        // SSE parses Server-Sent Events, we get chunks of "data: ..."
        let sse = crate::llm::sse::events(stream);
        Box::pin(async_stream::try_stream! {
            let mut sse = std::pin::pin!(sse);
            use futures_util::StreamExt;

            yield Event::Start { usage: crate::llm::Usage::default() };

            let mut active_tool: bool = false;
            let mut final_usage = crate::llm::Usage::default();
            while let Some(chunk) = sse.next().await {
                let ev = chunk?;
                let raw_val: Value = match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    // A non-JSON line is not a turn-ender — the surface
                    // pads streams with keepalives; skip and keep reading.
                    Err(_) => continue,
                };
                let val = raw_val.get("response").unwrap_or(&raw_val);

                // Track usage. `candidatesTokenCount` excludes thinking;
                // `thoughtsTokenCount` is billed as output, so it is added.
                if let Some(meta) = val.get("usageMetadata") {
                    let mut u = crate::llm::Usage::default();
                    if let Some(c) = meta.get("promptTokenCount").and_then(|v| v.as_u64()) {
                        u.input_tokens = c;
                    }
                    if let Some(c) = meta.get("candidatesTokenCount").and_then(|v| v.as_u64()) {
                        u.output_tokens = c;
                    }
                    if let Some(c) = meta.get("thoughtsTokenCount").and_then(|v| v.as_u64()) {
                        u.output_tokens += c;
                    }
                    if let Some(c) = meta.get("cachedContentTokenCount").and_then(|v| v.as_u64()) {
                        u.cache_read_input_tokens = c;
                    }
                    final_usage = u;
                }
                
                // Parse candidates
                if let Some(candidates) = val.get("candidates").and_then(|c| c.as_array()) {
                    for cand in candidates {
                        if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    if active_tool {
                                        yield Event::BlockStop;
                                        active_tool = false;
                                    }
                                    yield Event::Delta(text.to_string());
                                }
                                if let Some(thought) = part.get("thought").and_then(|t| t.as_str()) {
                                    if active_tool {
                                        yield Event::BlockStop;
                                        active_tool = false;
                                    }
                                    yield Event::ThinkingDelta(thought.to_string());
                                }
                                if let Some(sig) = part.get("thoughtSignature") {
                                    yield Event::ThinkingSignature(serde_json::to_string(&sig).unwrap_or_default());
                                }
                                if let Some(func) = part.get("functionCall") {
                                    if active_tool {
                                        yield Event::BlockStop;
                                    }
                                    let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                                    // Gemini sends no call ids; a local one is
                                    // minted so the canonical model (and the
                                    // journal) can pair call and result.
                                    let id = format!("call_{}", uuid::Uuid::new_v4().simple());
                                    
                                    yield Event::ToolUseStart { id: id.clone(), name: name.clone() };
                                    active_tool = true;
                                    
                                    if let Some(args) = func.get("args") {
                                        yield Event::ToolInputDelta(serde_json::to_string(&args).unwrap_or_default());
                                    }
                                }
                            }
                        }
                        
                        if let Some(reason) = cand.get("finishReason").and_then(|r| r.as_str()) {
                            if active_tool {
                                yield Event::BlockStop;
                            }
                            
                            let stop_reason = match reason {
                                "STOP" => crate::llm::StopReason::EndTurn,
                                "MAX_TOKENS" => crate::llm::StopReason::MaxTokens,
                                "SAFETY" => crate::llm::StopReason::Refusal,
                                _ if reason.starts_with("STOP_") => crate::llm::StopReason::StopSequence,
                                _ => crate::llm::StopReason::Other,
                            };
                            yield Event::Stop { stop_reason, usage: final_usage };
                            return;
                        }
                    }
                }
            }
            if active_tool {
                yield Event::BlockStop;
            }
            // The one-`Stop` contract: this point is reached only on EOF
            // without a finishReason (the arm above returns), and a stream
            // cut mid-chunk must still settle exactly once — as the OpenAI
            // adapter does at `[DONE]` with no usage — or the turn reads
            // as finished silently.
            {
                yield Event::Stop { stop_reason: crate::llm::StopReason::EndTurn, usage: final_usage };
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::message::{ContentBlock, Json, Message, StopReason};
    use crate::llm::provider::ChatRequest;
    use futures_util::StreamExt;

    #[test]
    fn request_body_shape_with_project_present_and_absent() {
        let req = ChatRequest {
            model: "gemini-2.5-flash".into(),
            system: Some("sys prompt".into()),
            messages: vec![Message::user_text("hello")],
            ..Default::default()
        };

        // Absent project
        let body_bare = Gemini::request_body(req.clone(), None);
        assert!(body_bare.get("project").is_none());
        assert_eq!(body_bare["model"], "gemini-2.5-flash");
        assert_eq!(body_bare["systemInstruction"]["parts"][0]["text"], "sys prompt");
        assert_eq!(body_bare["contents"][0]["role"], "user");
        assert_eq!(body_bare["contents"][0]["parts"][0]["text"], "hello");

        // Present project (Antigravity wrapper)
        let body_wrapped = Gemini::request_body(req, Some("aicode-consumers".into()));
        assert_eq!(body_wrapped["project"], "aicode-consumers");
        assert_eq!(body_wrapped["userAgent"], "antigravity");
        assert_eq!(body_wrapped["requestType"], "agent");
        assert!(body_wrapped["requestId"].as_str().unwrap().starts_with("agent/req-"));
        assert_eq!(body_wrapped["request"]["model"], "gemini-2.5-flash");
    }

    #[test]
    fn function_response_named_by_function() {
        let req = ChatRequest {
            model: "gemini-2.5-flash".into(),
            messages: vec![
                Message::user_text("run grep"),
                Message::assistant(vec![ContentBlock::ToolUse {
                    id: "call_abc123".into(),
                    name: "grep_search".into(),
                    input: Json("{\"pattern\":\"foo\"}".into()),
                }]),
                Message::user(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_abc123".into(),
                    content: "found 2 lines".into(),
                    is_error: false,
                }]),
            ],
            ..Default::default()
        };

        let body = Gemini::request_body(req, None);
        let contents = body["contents"].as_array().unwrap();
        let user_resp_part = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(user_resp_part["name"], "grep_search");
        assert_eq!(user_resp_part["response"]["result"], "found 2 lines");
    }

    #[test]
    fn signature_echo_from_a_thinking_block() {
        let req = ChatRequest {
            model: "gemini-2.5-flash".into(),
            messages: vec![
                Message::user_text("think"),
                Message::assistant(vec![ContentBlock::Thinking {
                    thinking: "let me think".into(),
                    signature: "opaque_sig_999".into(),
                }]),
            ],
            ..Default::default()
        };

        let body = Gemini::request_body(req, None);
        let parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["thoughtSignature"], "opaque_sig_999");
    }

    #[tokio::test]
    async fn usage_mapping_and_finish_reason_map() {
        let sse_data = vec![
            Ok(bytes::Bytes::from("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":3,\"cachedContentTokenCount\":2}}\n\n")),
        ];
        let stream = futures_util::stream::iter(sse_data);
        let events: Vec<_> = Gemini::stream_events(stream).collect().await;

        let mut text_deltas = Vec::new();
        let mut final_stop = None;

        for ev in events {
            match ev.unwrap() {
                Event::Delta(d) => text_deltas.push(d),
                Event::Stop { stop_reason, usage } => final_stop = Some((stop_reason, usage)),
                _ => {}
            }
        }

        assert_eq!(text_deltas, vec!["hi"]);
        let (reason, usage) = final_stop.unwrap();
        assert_eq!(reason, StopReason::EndTurn);
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 8); // 5 candidates + 3 thoughts
        assert_eq!(usage.cache_read_input_tokens, 2);
    }

    #[tokio::test]
    async fn eof_stop_emits_one_stop_event() {
        // Stream closes immediately with no finishReason
        let sse_data: Vec<Result<bytes::Bytes, reqwest::Error>> = vec![];
        let stream = futures_util::stream::iter(sse_data);
        let events: Vec<_> = Gemini::stream_events(stream).collect().await;

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].as_ref().unwrap(), Event::Start { .. }));
        if let Event::Stop { stop_reason, .. } = events[1].as_ref().unwrap() {
            assert_eq!(*stop_reason, StopReason::EndTurn);
        } else {
            panic!("Expected Stop event on EOF");
        }
    }
}

// Removed Provider impl since Gemini is used directly by HttpProvider