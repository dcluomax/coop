//! Minimal Server-Sent Events (`text/event-stream`) framing.
//!
//! Both the Anthropic and OpenAI streaming endpoints emit SSE: events are
//! separated by a blank line and each carries one or more `data:` fields. This
//! module turns a `reqwest` byte stream into a stream of the decoded `data:`
//! payloads (one `String` per event), buffering across network chunk
//! boundaries. Comment lines (`:`), `event:`/`id:` fields, and blank keepalives
//! are dropped — callers only ever see `data:` payloads, which they parse as
//! JSON (or recognise the `[DONE]` sentinel).

use bytes::Bytes;
use coopd_core::{CoreError, Result};
use futures::{Stream, StreamExt};
use std::collections::VecDeque;

/// Extract every *complete* SSE event from `buf`, returning the joined `data:`
/// payload of each. Incomplete trailing bytes (an event not yet terminated by a
/// blank line) are left in `buf` for the next network chunk.
///
/// Multi-line `data:` fields within one event are joined with `\n`, per the
/// SSE spec. Events that carry no `data:` line (pure comments/keepalives) are
/// skipped.
pub(crate) fn drain_events(buf: &mut String) -> Vec<String> {
    // Normalise CRLF so the `\n\n` separator search is uniform.
    if buf.contains('\r') {
        *buf = buf.replace("\r\n", "\n").replace('\r', "\n");
    }
    let mut events = Vec::new();
    while let Some(pos) = buf.find("\n\n") {
        let block: String = buf.drain(..pos + 2).collect();
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                // A single optional leading space after the colon is stripped.
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
        }
        if !data_lines.is_empty() {
            events.push(data_lines.join("\n"));
        }
    }
    events
}

/// Decode a `reqwest` byte stream into successive SSE `data:` payloads.
///
/// The returned stream yields `Ok(payload)` for each event and `Err(..)` if the
/// underlying transport fails. It ends when the upstream byte stream ends; any
/// final unterminated event is flushed before completion.
pub fn sse_data_stream<S>(stream: S) -> impl Stream<Item = Result<String>> + Send + 'static
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    let state = (stream.boxed(), String::new(), VecDeque::<String>::new());
    futures::stream::unfold(state, |(mut s, mut buf, mut queue)| async move {
        loop {
            if let Some(ev) = queue.pop_front() {
                return Some((Ok(ev), (s, buf, queue)));
            }
            match s.next().await {
                Some(Ok(bytes)) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    for ev in drain_events(&mut buf) {
                        queue.push_back(ev);
                    }
                }
                Some(Err(e)) => {
                    return Some((
                        Err(CoreError::Other(format!("sse transport: {e}"))),
                        (s, buf, queue),
                    ));
                }
                None => {
                    // Flush a trailing event that lacked its final blank line.
                    if !buf.trim().is_empty() {
                        buf.push_str("\n\n");
                        for ev in drain_events(&mut buf) {
                            queue.push_back(ev);
                        }
                    }
                    return queue.pop_front().map(|ev| (Ok(ev), (s, buf, queue)));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drains_single_event() {
        let mut buf = "data: hello\n\n".to_string();
        assert_eq!(drain_events(&mut buf), vec!["hello".to_string()]);
        assert!(buf.is_empty());
    }

    #[test]
    fn leaves_incomplete_event_buffered() {
        let mut buf = "data: part".to_string();
        assert!(drain_events(&mut buf).is_empty());
        assert_eq!(buf, "data: part");
        buf.push_str("ial\n\n");
        assert_eq!(drain_events(&mut buf), vec!["partial".to_string()]);
    }

    #[test]
    fn joins_multiline_data_and_skips_comments() {
        let mut buf = ": keepalive\ndata: a\ndata: b\n\n".to_string();
        assert_eq!(drain_events(&mut buf), vec!["a\nb".to_string()]);
    }

    #[test]
    fn handles_crlf_and_event_fields() {
        let mut buf = "event: message\r\ndata: x\r\n\r\n".to_string();
        assert_eq!(drain_events(&mut buf), vec!["x".to_string()]);
    }

    #[tokio::test]
    async fn stream_splits_across_chunk_boundaries() {
        // The event is split mid-payload across two network chunks.
        let chunks: Vec<reqwest::Result<Bytes>> = vec![
            Ok(Bytes::from_static(b"data: hel")),
            Ok(Bytes::from_static(b"lo\n\ndata: world\n\n")),
        ];
        let s = sse_data_stream(futures::stream::iter(chunks));
        let got: Vec<String> = s.map(|r| r.unwrap()).collect().await;
        assert_eq!(got, vec!["hello".to_string(), "world".to_string()]);
    }

    #[tokio::test]
    async fn stream_flushes_trailing_unterminated_event() {
        let chunks: Vec<reqwest::Result<Bytes>> = vec![Ok(Bytes::from_static(b"data: tail"))];
        let s = sse_data_stream(futures::stream::iter(chunks));
        let got: Vec<String> = s.map(|r| r.unwrap()).collect().await;
        assert_eq!(got, vec!["tail".to_string()]);
    }
}
