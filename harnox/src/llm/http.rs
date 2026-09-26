//! HTTP defaults for streamed model responses, shared with consumer wrappers.

use std::time::Duration;

/// Bound connection establishment and silent reads, not total generation time.
/// Byte-level reads (including SSE comments and reasoning) reset the idle clock.
/// A peer sending heartbeats forever is still active at this layer; semantic
/// progress and operator cancellation remain the consumer's responsibility.
pub fn streaming_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(300))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const IDLE: Duration = Duration::from_millis(300);

    async fn request(listener: TcpListener) -> TcpStream {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        while !buf.ends_with(b"\r\n\r\n") {
            buf.push(socket.read_u8().await.unwrap());
        }
        socket
    }

    async fn server() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        (listener, url)
    }

    fn client() -> reqwest::Client {
        streaming_client_builder().no_proxy().read_timeout(IDLE).build().unwrap()
    }

    #[tokio::test]
    async fn silent_response_headers_time_out() {
        let (listener, url) = server().await;
        let task = tokio::spawn(async move {
            let _socket = request(listener).await;
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(Duration::from_secs(3), client().get(url).send()).await;
        task.abort();
        assert!(result.expect("HTTP header read stayed hung").unwrap_err().is_timeout());
    }

    #[tokio::test]
    async fn silent_body_errors_after_partial_output() {
        let (listener, url) = server().await;
        let task = tokio::spawn(async move {
            let mut socket = request(listener).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: hello\n\n")
                .await.unwrap();
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let response = client().get(url).send().await.unwrap();
            let mut events = std::pin::pin!(crate::llm::sse::events(response.bytes_stream()));
            assert_eq!(events.next().await.unwrap().unwrap().data, "hello");
            let error = events.next().await.expect("silence must error, not end normally").unwrap_err();
            assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
        }).await;
        task.abort();
        result.expect("HTTP body read stayed hung");
    }

    #[tokio::test]
    async fn heartbeats_allow_streams_longer_than_the_idle_limit() {
        let (listener, url) = server().await;
        let task = tokio::spawn(async move {
            let mut socket = request(listener).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                .await.unwrap();
            // Comments are discarded by the SSE parser, but still count as
            // network activity. A timeout around events.next() would fail here.
            for _ in 0..12 {
                socket.write_all(b": heartbeat\n\n").await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            socket.write_all(b"data: done\n\n").await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            let response = client().get(url).send().await.unwrap();
            let events = crate::llm::sse::events(response.bytes_stream());
            let all: Vec<_> = events.collect().await;
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].as_ref().unwrap().data, "done");
        }).await;
        task.abort();
        result.expect("active stream did not complete");
    }
}
