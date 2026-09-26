//! The canonical conversation model.
//!
//! This is the Anthropic Messages API shape, deliberately: the native wire is
//! Anthropic's (Kimi speaks it too), and normalising to a lowest common
//! denominator would cost thinking blocks, cache control and signatures that
//! this stack cares about. The OpenAI-compatible adapter translates *from*
//! this model, never the other way round.
//!
//! Every type here derives `serde` (for the wire) and, under the `bitcode`
//! feature, `bitcode` (for a consumer's session log). bitcode is not
//! self-describing, so a tool's JSON input is carried as [`Json`] — the raw
//! text — rather than as `serde_json::Value`. Parse at the edges.

use serde::{Deserialize, Serialize};

/// Raw JSON text. Used wherever a log has to carry an arbitrary JSON document
/// (tool inputs, tool result payloads) without a schema.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "bitcode", derive(bitcode::Encode, bitcode::Decode))]
#[serde(transparent)]
pub struct Json(pub String);

impl Json {
    /// Serialise a value into JSON text.
    pub fn from_value(v: &serde_json::Value) -> Self {
        Json(v.to_string())
    }

    /// Parse the text back into a value. Invalid text yields `Value::Null`
    /// rather than an error: a torn tool input from a cancelled stream is
    /// still a valid *record*, just not a usable input.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::from_str(&self.0).unwrap_or(serde_json::Value::Null)
    }

    /// Strict parse, for callers that must know.
    pub fn parse(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::from_str(&self.0)
    }
}

impl From<serde_json::Value> for Json {
    fn from(v: serde_json::Value) -> Self {
        Json::from_value(&v)
    }
}

/// Who authored a message. Only two roles exist on the wire; the system
/// prompt travels as a request field, not a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bitcode", derive(bitcode::Encode, bitcode::Decode))]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Where an image's bytes come from. This is the Anthropic `source` union
/// verbatim — tag included — so the Anthropic wire form of an image block
/// is this type's own serde output and not a second hand-written shape.
///
/// [`Url`](ImageSource::Url) is carried even though nothing in the harness
/// produces one today: both wires accept a URL, dropping it would make the
/// model unable to say what the API permits, and a variant that exists
/// costs a match arm rather than a redesign later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bitcode", derive(bitcode::Encode, bitcode::Decode))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// The bytes, base64-encoded, with the media type that reads them.
    Base64 { media_type: String, data: String },
    /// A URL the endpoint fetches for itself.
    Url { url: String },
}

impl ImageSource {
    /// The media type, where the source knows it. A URL source does not.
    pub fn media_type(&self) -> Option<&str> {
        match self {
            ImageSource::Base64 { media_type, .. } => Some(media_type),
            ImageSource::Url { .. } => None,
        }
    }

    /// The `data:` URI form, which is what the OpenAI-compatible wire wants
    /// wherever the Anthropic wire wants a `source` object.
    pub fn data_uri(&self) -> String {
        match self {
            ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
            ImageSource::Url { url } => url.clone(),
        }
    }

    /// Decoded size in bytes, for a description. Base64 is 4 characters per
    /// 3 bytes, less the padding — computed rather than decoded, since this
    /// is only ever used to write a number in a sentence.
    pub fn byte_len(&self) -> Option<usize> {
        match self {
            ImageSource::Base64 { data, .. } => {
                let pad = data.bytes().rev().take_while(|&b| b == b'=').count();
                Some(data.len() / 4 * 3 - pad)
            }
            ImageSource::Url { .. } => None,
        }
    }
}

/// One block of a message's content. Mirrors the Anthropic content-block
/// union; `ToolResult` is only valid in a `User` message and `ToolUse` /
/// `Thinking` only in an `Assistant` one, but that is enforced by
/// construction in the agent loop rather than by the type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bitcode", derive(bitcode::Encode, bitcode::Decode))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Extended-thinking block. `signature` must be replayed verbatim on the
    /// next request or the API rejects the turn — which is why thinking is
    /// journaled rather than discarded.
    Thinking {
        thinking: String,
        signature: String,
    },
    /// Thinking the API has redacted; opaque, replayed as-is.
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Json,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    /// An image, valid in a `User` message. Appended last on purpose:
    /// bitcode encodes an enum by variant *order*, so a variant inserted
    /// among the others would silently re-read every image-free log that
    /// already exists.
    ///
    /// `alt` is not a wire field on either wire. It is what the block is
    /// called when it cannot be shown — the filename in a transcript, and
    /// the placeholder a model that cannot see gets instead of the bytes
    /// (see [`downgrade_images`]). An image whose provenance is gone reads
    /// as `image` and nothing worse.
    Image {
        source: ImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
    },
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        ContentBlock::Text { text: s.into() }
    }

    /// An image from bytes already in hand. `alt` is what to call it when
    /// the bytes cannot be shown — a filename, usually.
    pub fn image(media_type: impl Into<String>, data: impl Into<String>, alt: Option<String>) -> Self {
        ContentBlock::Image { source: ImageSource::Base64 { media_type: media_type.into(), data: data.into() }, alt }
    }

    /// How this block reads when it has to be a sentence rather than
    /// itself — the placeholder a non-vision model is given, and the label
    /// a consumer draws where it cannot draw pixels. Only images have one.
    pub fn image_description(&self) -> Option<String> {
        let ContentBlock::Image { source, alt } = self else { return None };
        let mut parts = Vec::new();
        if let Some(a) = alt.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
            parts.push(a.to_string());
        }
        if let Some(t) = source.media_type() {
            parts.push(t.to_string());
        }
        if let Some(n) = source.byte_len() {
            parts.push(human_bytes(n));
        }
        if let ImageSource::Url { url } = source {
            parts.push(url.clone());
        }
        Some(if parts.is_empty() { "image".into() } else { parts.join(", ") })
    }
}

/// `240 KB`. Two significant figures is all a label wants.
fn human_bytes(n: usize) -> String {
    const K: usize = 1024;
    match n {
        0..K => format!("{n} B"),
        K..1_048_576 => format!("{} KB", n / K),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// A message on the conversation branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bitcode", derive(bitcode::Encode, bitcode::Decode))]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(content: Vec<ContentBlock>) -> Self {
        Message { role: Role::User, content }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Message::user(vec![ContentBlock::text(text)])
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Message { role: Role::Assistant, content }
    }

    /// Every `ToolUse` block, in order.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Json)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    /// Does this message carry any image block?
    pub fn has_images(&self) -> bool {
        self.content.iter().any(|b| matches!(b, ContentBlock::Image { .. }))
    }

    /// Every image block, in order.
    pub fn images(&self) -> impl Iterator<Item = (&ImageSource, Option<&str>)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::Image { source, alt } => Some((source, alt.as_deref())),
            _ => None,
        })
    }

    /// Concatenated text blocks — what a user sees as "the reply".
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.content {
            if let ContentBlock::Text { text } = b {
                out.push_str(text);
            }
        }
        out
    }

    /// Concatenated thinking blocks — what the model reasoned before it
    /// answered, and *not* what a caller may present as the answer.
    ///
    /// Beside [`Message::text`] rather than folded into it because the two
    /// answer different questions and one of them is empty far more often
    /// than the other: a reply that is thinking and nothing else is a real
    /// outcome, and a caller that summed the two would print reasoning where
    /// it meant to print an answer. Redacted thinking is not here either —
    /// there is nothing in it to show.
    pub fn thinking(&self) -> String {
        let mut out = String::new();
        for b in &self.content {
            if let ContentBlock::Thinking { thinking, .. } = b {
                out.push_str(thinking);
            }
        }
        out
    }
}

/// Replace every image block with a line of text saying what was there.
/// Returns how many were replaced.
///
/// A model with no vision does not ignore an image block: the endpoint
/// rejects the whole request, so one attached screenshot breaks every
/// subsequent turn on that branch rather than the one it was attached to.
/// Degrading is therefore not a courtesy, it is what keeps a model switch
/// from stranding a session.
///
/// It runs on the way to the wire and never on the log — the branch keeps
/// the image, so switching back to a model that *can* see shows it the
/// picture rather than the apology.
///
/// The check that decides whether to call this is deliberately narrow: see
/// [`ChatRequest::vision`](super::provider::ChatRequest::vision), which is
/// three-valued because "we were not told" must not read as "cannot see".
pub fn downgrade_images(messages: &mut [Message]) -> usize {
    let mut n = 0;
    for m in messages.iter_mut() {
        for b in m.content.iter_mut() {
            if let Some(d) = b.image_description() {
                *b = ContentBlock::text(format!("[image not shown to this model: {d}]"));
                n += 1;
            }
        }
    }
    n
}

/// Why the model stopped. `Refusal` is surfaced rather than folded into
/// `EndTurn` because it changes what the loop should do next (nothing).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bitcode", derive(bitcode::Encode, bitcode::Decode))]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
    /// The stream was cancelled locally before the API reported a reason.
    Cancelled,
    /// Wire value we don't know. Kept as data, not an error.
    Other,
    /// The harness ended the turn because it is waiting on a condition the
    /// session asked for — a park (`eidolon_core::wait`) — rather than
    /// because the model had nothing to say. No wire value produces it and
    /// no endpoint can report it: it is the harness's own settle, and the
    /// session wakes when the condition fires or its deadline arrives.
    ///
    /// Appended after `Other` rather than sorted into the list, because
    /// bitcode encodes variant order and this enum rides inside a
    /// journaled `TurnSettled`.
    Waiting,
    /// The harness ended the turn because the operator exited a question
    /// (`choices_user`) without answering it. Like `Waiting`, no wire
    /// value produces it and no endpoint can report it: it is the
    /// harness's own settle, taken because feeding the dismissed question
    /// back as an ordinary result buys nothing but the same question
    /// again. Appended after `Waiting` for the reason `Waiting` states.
    Dismissed,
}

/// Token accounting for one model call. Additive: sum per turn, per session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "bitcode", derive(bitcode::Encode, bitcode::Decode))]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, o: Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_creation_input_tokens += o.cache_creation_input_tokens;
        self.cache_read_input_tokens += o.cache_read_input_tokens;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_blocks_serialise_to_the_anthropic_tagged_shape() {
        let m = Message::assistant(vec![
            ContentBlock::text("hi"),
            ContentBlock::ToolUse { id: "t1".into(), name: "read".into(), input: Json("{\"p\":1}".into()) },
        ]);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][1]["type"], "tool_use");
        // The input is JSON *text* in the model (a log-friendly form), not an object.
        assert_eq!(v["content"][1]["input"], "{\"p\":1}");
        let back: Message = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn an_image_block_is_the_anthropic_source_union_verbatim() {
        let m = Message::user(vec![ContentBlock::text("look"), ContentBlock::image("image/png", "AAAA", Some("shot.png".into()))]);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["content"][1]["type"], "image");
        assert_eq!(v["content"][1]["source"]["type"], "base64");
        assert_eq!(v["content"][1]["source"]["media_type"], "image/png");
        assert_eq!(v["content"][1]["source"]["data"], "AAAA");
        let back: Message = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
    }

    /// The variant is last in the enum because bitcode encodes an enum by
    /// variant order. This pins the position: a variant added above `Image`
    /// re-reads every image block in every existing log as something else.
    #[cfg(feature = "bitcode")]
    #[test]
    fn image_is_the_last_content_block_variant() {
        let blocks = [
            ContentBlock::text("t"),
            ContentBlock::Thinking { thinking: "t".into(), signature: "s".into() },
            ContentBlock::RedactedThinking { data: "d".into() },
            ContentBlock::ToolUse { id: "i".into(), name: "n".into(), input: Json("{}".into()) },
            ContentBlock::ToolResult { tool_use_id: "i".into(), content: "c".into(), is_error: false },
            ContentBlock::image("image/png", "AAAA", None),
        ];
        for b in &blocks {
            let bytes = bitcode::encode(b);
            assert_eq!(&bitcode::decode::<ContentBlock>(&bytes).unwrap(), b);
        }
    }

    #[test]
    fn a_downgraded_image_says_what_was_there() {
        let mut msgs = vec![Message::user(vec![
            ContentBlock::text("what is wrong here?"),
            ContentBlock::image("image/png", "A".repeat(4096), Some("layout.png".into())),
        ])];
        assert_eq!(downgrade_images(&mut msgs), 1);
        let t = msgs[0].text();
        assert!(t.contains("what is wrong here?"), "{t}");
        assert!(t.contains("layout.png"), "{t}");
        assert!(t.contains("image/png"), "{t}");
        assert!(t.contains("3 KB"), "{t}");
        assert!(!msgs[0].has_images());
    }

    /// An image whose provenance is gone still degrades to a sentence
    /// rather than to nothing — a dropped block would leave the model
    /// answering a question about a picture it was never told existed.
    #[test]
    fn an_unlabelled_image_still_degrades_to_something() {
        let mut msgs = vec![Message::user(vec![ContentBlock::Image {
            source: ImageSource::Url { url: "https://example.invalid/a.png".into() },
            alt: None,
        }])];
        assert_eq!(downgrade_images(&mut msgs), 1);
        assert!(msgs[0].text().contains("example.invalid"));
    }

    #[test]
    fn base64_length_is_read_without_decoding() {
        // 3 bytes -> "AAAA"; 4 bytes -> "AAAAAA==".
        assert_eq!(ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() }.byte_len(), Some(3));
        assert_eq!(ImageSource::Base64 { media_type: "image/png".into(), data: "AAAAAA==".into() }.byte_len(), Some(4));
    }

    #[test]
    fn a_data_uri_is_what_the_openai_wire_wants() {
        let s = ImageSource::Base64 { media_type: "image/jpeg".into(), data: "QUJD".into() };
        assert_eq!(s.data_uri(), "data:image/jpeg;base64,QUJD");
        // A URL source is already a URL; it is not wrapped again.
        assert_eq!(ImageSource::Url { url: "https://x/a.png".into() }.data_uri(), "https://x/a.png");
    }

    #[test]
    fn json_newtype_is_lenient_on_read_and_strict_on_parse() {
        let torn = Json("{\"a\":".into());
        assert_eq!(torn.to_value(), serde_json::Value::Null);
        assert!(torn.parse().is_err());
        assert_eq!(Json::from(serde_json::json!({"a": 1})).to_value()["a"], 1);
    }

    #[test]
    fn tool_result_error_flag_is_omitted_when_false() {
        let b = ContentBlock::ToolResult { tool_use_id: "t".into(), content: "ok".into(), is_error: false };
        assert!(serde_json::to_string(&b).unwrap().contains("is_error").not());
        let b: ContentBlock = serde_json::from_str(r#"{"type":"tool_result","tool_use_id":"t","content":"x"}"#).unwrap();
        assert!(matches!(b, ContentBlock::ToolResult { is_error: false, .. }));
    }

    #[test]
    fn usage_is_additive() {
        let mut total = Usage::default();
        total += Usage { input_tokens: 1, output_tokens: 2, cache_creation_input_tokens: 3, cache_read_input_tokens: 4 };
        total += Usage { input_tokens: 10, output_tokens: 20, cache_creation_input_tokens: 30, cache_read_input_tokens: 40 };
        assert_eq!(total, Usage { input_tokens: 11, output_tokens: 22, cache_creation_input_tokens: 33, cache_read_input_tokens: 44 });
    }

    #[cfg(feature = "bitcode")]
    #[test]
    fn bitcode_round_trips_a_message() {
        let m = Message::assistant(vec![
            ContentBlock::Thinking { thinking: "hmm".into(), signature: "sig".into() },
            ContentBlock::ToolUse { id: "t1".into(), name: "read".into(), input: Json("{}".into()) },
        ]);
        let bytes = bitcode::encode(&m);
        let back: Message = bitcode::decode(&bytes).unwrap();
        assert_eq!(back, m);
    }

    use std::ops::Not as _;
}
