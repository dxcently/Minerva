//! `GET /api/events`: hello, the branch replayed, every pending ask,
//! `caught-up`, then everything live.
//!
//! Every feed is subscribed before the branch or the pending asks are read,
//! so the worst case is a duplicate (harmless under record ids), never a gap.
//! A lagging subscriber gets a `lagged` frame and the stream ends; the
//! browser's reconnect gets a fresh head, which is a complete catch-up where
//! a patched buffer would not be.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use eidolon_core::agent::Agent;
use eidolon_core::event::EventBus;
use http_body_util::StreamBody;
use hyper::body::Frame;
use tokio::sync::broadcast;

use crate::driver::Handle;
use crate::user::{Ask, AskEvent, WebUser};
use crate::wire;

// `+ Sync` because `BodyExt::boxed` requires it.
pub type Body = StreamBody<
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Frame<Bytes>, Infallible>> + Send + Sync>>,
>;

fn frame(line: &str) -> Frame<Bytes> {
    Frame::data(Bytes::from(format!("data: {line}\n\n")))
}

fn frame_json(w: &impl serde::Serialize) -> Frame<Bytes> {
    frame(&serde_json::to_string(w).expect("wire frame serializes"))
}

#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum Local<'a> {
    Hello {
        session: String,
        cwd: String,
        model: String,
        yolo: bool,
        pending: usize,
        protocol: u32,
    },
    CaughtUp {},
    TurnState {
        running: bool,
    },
    Ask(&'a Ask),
    AskSettled {
        ask_id: u64,
        how: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        answer: Option<String>,
    },
    Lagged {
        dropped: u64,
    },
    /// The door is closing on a quiesce; the stream ends behind it.
    Goodbye {
        text: &'a str,
    },
}

/// `hello`'s fields are a snapshot at connect; a later model change shows on
/// the next reconnect's `hello`.
pub fn body(agent: Arc<Agent>, user: Arc<WebUser>, bus: EventBus, driver: Handle, cwd: String, yolo: bool) -> Body {
    let s = async_stream::stream! {
        let mut events = bus.subscribe();
        let mut asks = user.subscribe();
        let mut turn_state = driver.subscribe_state();
        let mut bye = driver.subscribe_bye();
        let pending = user.pending();

        let session_name = agent.session().lock().await.path().display().to_string();
        let model = agent.model_key();
        yield Ok(frame_json(&Local::Hello {
            session: session_name,
            cwd,
            model,
            yolo,
            pending: pending.len(),
            protocol: crate::PROTOCOL,
        }));

        {
            let session = agent.session().lock().await;
            for w in wire::replay(&session.branch()) {
                yield Ok(frame_json(&w));
            }
        }
        for ask in pending {
            yield Ok(frame_json(&Local::Ask(&ask)));
        }
        yield Ok(frame_json(&Local::CaughtUp {}));

        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Ok(e) => {
                        if let Some(w) = wire::project(&e) {
                            yield Ok(frame_json(&w));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        yield Ok(frame_json(&Local::Lagged { dropped }));
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                a = asks.recv() => match a {
                    Ok(AskEvent::Opened(ask)) => yield Ok(frame_json(&Local::Ask(&ask))),
                    Ok(AskEvent::Settled { ask_id, how }) => {
                        let answer = how.answer().map(str::to_string);
                        yield Ok(frame_json(&Local::AskSettled { ask_id, how: how.how(), answer }));
                    }
                    Err(broadcast::error::RecvError::Lagged(dropped)) => {
                        yield Ok(frame_json(&Local::Lagged { dropped }));
                        return;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                r = turn_state.recv() => match r {
                    Ok(running) => yield Ok(frame_json(&Local::TurnState { running })),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                b = bye.recv() => match b {
                    // Ending the stream is what lets the graceful shutdown drain.
                    Ok(text) => {
                        yield Ok(frame_json(&Local::Goodbye { text: &text }));
                        return;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
        }
    };
    StreamBody::new(Box::pin(s))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use eidolon_core::agent::AgentConfig;
    use eidolon_core::dispatch::Dispatcher;
    use eidolon_core::event::Event;
    use eidolon_core::policy::AllowAll;
    use eidolon_core::session::Session;
    use eidolon_core::testing::ScriptedProvider;
    use eidolon_core::tool::ToolRegistry;
    use eidolon_core::user::{Choice, UserIo};
    use futures_util::StreamExt as _;
    use tokio_util::sync::CancellationToken;

    use super::*;

    async fn fixture(bus_capacity: usize, ask_capacity: usize) -> (Body, Arc<WebUser>, EventBus, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::create(&dir.path().join("t.eid"), "m", dir.path(), None).unwrap();
        let user = Arc::new(WebUser::with_ask_capacity(ask_capacity));
        let io: Arc<dyn UserIo> = user.clone();
        let bus = EventBus::new(bus_capacity);
        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            io,
            bus.clone(),
            Arc::new(tokio::sync::Mutex::new(session)),
            dir.path().to_path_buf(),
        ));
        let agent = Arc::new(Agent::new(
            ScriptedProvider::new(Vec::new()),
            dispatcher,
            AgentConfig { model: "m".into(), ..Default::default() },
        ));
        let driver = Handle::new(agent.clone(), None);
        let head = body(agent, user.clone(), bus.clone(), driver, "/tmp".into(), false);
        (head, user, bus, dir)
    }

    fn frame_text(frame: Result<Frame<Bytes>, Infallible>) -> String {
        let frame = frame.expect("an SSE frame is infallible");
        String::from_utf8(frame.into_data().expect("every frame here is data").to_vec()).unwrap()
    }

    async fn next_frame(stream: &mut Body) -> String {
        tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("the stream must yield, not wait")
            .map(frame_text)
            .expect("the stream must not have ended yet")
    }

    /// An arm that answered `Lagged` without ending the stream fails at the timeout.
    async fn drain(stream: &mut Body) -> Vec<String> {
        let mut frames = Vec::new();
        while let Some(frame) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("the stream must end, not sit in its loop")
        {
            frames.push(frame_text(frame));
        }
        frames
    }

    fn assert_ends_lagged(hello: &str, frames: &[String]) {
        assert!(hello.contains("\"type\":\"hello\""), "the head is hello: {hello}");
        assert_eq!(frames.len(), 2, "caught-up then lagged and nothing else: {frames:?}");
        assert!(frames[0].contains("\"type\":\"caught-up\""), "{frames:?}");
        assert!(frames[1].contains("\"type\":\"lagged\""), "{frames:?}");
        assert!(frames[1].contains("\"dropped\":1"), "one record was skipped: {frames:?}");
    }

    /// Never answered, so the ask stays pending for the rest of the test.
    fn raise(user: &Arc<WebUser>, prompt: &'static str) {
        let user = user.clone();
        let cancel = CancellationToken::new();
        let options = [Choice { label: "a".into(), description: None }];
        drop(tokio::spawn(async move { user.choose(prompt, &options, &cancel).await }));
    }

    /// A one-deep bus: two publishes with nobody polling is a lag, no race needed.
    #[tokio::test]
    async fn a_lagging_bus_subscriber_ends_the_stream_after_a_lagged_frame() {
        let (mut stream, _user, bus, _dir) = fixture(1, 8).await;
        let hello = next_frame(&mut stream).await;
        bus.publish(Event::MessageStart);
        bus.publish(Event::MessageStart);
        assert_ends_lagged(&hello, &drain(&mut stream).await);
    }

    /// Three asks on a two-deep channel the stream has not read. The observer
    /// reads each before the next is raised, so it never lags itself and all
    /// three are sent before the stream is polled, with no sleep.
    #[tokio::test]
    async fn a_lagging_ask_subscriber_ends_the_stream_after_a_lagged_frame() {
        let (mut stream, user, _bus, _dir) = fixture(8, 2).await;
        let hello = next_frame(&mut stream).await;
        assert!(user.pending().is_empty(), "the head was built before any of these asks");
        let mut observer = user.subscribe();
        for prompt in ["first?", "second?", "third?"] {
            raise(&user, prompt);
            match observer.recv().await.unwrap() {
                AskEvent::Opened(ask) => assert!(ask.ask_id() >= 1),
                _ => panic!("expected Opened"),
            }
        }
        assert_eq!(user.pending_count(), 3, "all three are waiting, and the stream has read none");
        assert_ends_lagged(&hello, &drain(&mut stream).await);
    }
}
