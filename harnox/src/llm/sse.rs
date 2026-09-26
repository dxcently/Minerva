//! Server-sent events over a `reqwest` byte stream.
//!
//! Minimal by design: events are separated by a blank line, an event's
//! payload is its `data:` lines joined with `\n`, `event:` names the type,
//! and everything else (`id:`, `retry:`, comments) is ignored. Both `\n\n`
//! and `\r\n\r\n` separators are accepted. A trailing partial event at EOF
//! is discarded — a stream that ends mid-event has already failed.

use bytes::Bytes;
use futures_util::{Stream, StreamExt};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Turn a byte stream into an event stream.
pub fn events<S>(bytes: S) -> impl Stream<Item = anyhow::Result<SseEvent>>
where
    S: Stream<Item = reqwest::Result<Bytes>>,
{
    async_stream::try_stream! {
        let mut buf: Vec<u8> = Vec::new();
        let mut bytes = std::pin::pin!(bytes);
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk?;
            buf.extend_from_slice(&chunk);
            while let Some((end, sep)) = find_separator(&buf) {
                let raw = buf.drain(..end + sep).collect::<Vec<u8>>();
                let text = String::from_utf8_lossy(&raw[..end]);
                if let Some(ev) = parse(&text) {
                    yield ev;
                }
            }
        }
    }
}

fn find_separator(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| (p, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn parse(text: &str) -> Option<SseEvent> {
    let mut ev = SseEvent::default();
    let mut data: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(d) = line.strip_prefix("data:") {
            data.push(d.strip_prefix(' ').unwrap_or(d));
        } else if let Some(e) = line.strip_prefix("event:") {
            ev.event = Some(e.trim().to_string());
        }
    }
    if data.is_empty() && ev.event.is_none() {
        return None;
    }
    ev.data = data.join("\n");
    Some(ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn splits_events_across_chunks() {
        let chunks = vec![
            Ok(Bytes::from_static(b"event: a\ndata: {\"x\":")),
            Ok(Bytes::from_static(b"1}\n\n: comment\ndata: one\ndata: two\n\ndata: tail")),
        ];
        let s = events(futures_util::stream::iter(chunks));
        let got: Vec<SseEvent> = s.map(|e| e.unwrap()).collect().await;
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].event.as_deref(), Some("a"));
        assert_eq!(got[0].data, "{\"x\":1}");
        assert_eq!(got[1].data, "one\ntwo");
    }

    #[tokio::test]
    async fn accepts_crlf_separators() {
        let chunks = vec![Ok(Bytes::from_static(b"data: a\r\n\r\ndata: b\r\n\r\n"))];
        let got: Vec<SseEvent> = events(futures_util::stream::iter(chunks)).map(|e| e.unwrap()).collect().await;
        assert_eq!(got.iter().map(|e| e.data.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }
}
