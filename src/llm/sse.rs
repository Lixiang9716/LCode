//! Minimal SSE (Server-Sent Events) parsing for streaming LLM calls
//! (G11).
//!
//! Both providers stream with `stream: true` and receive
//! `text/event-stream` lines. The full SSE spec is not needed: lines are
//! split on `\n` (chunks may end anywhere, so partial lines are buffered
//! across chunk boundaries), and the payload of every `data:` line is
//! emitted. Comments, `event:`/`id:`/`retry:` fields and blank lines are
//! skipped — each provider maps the payloads to [`StreamEvent`]s.

use futures::stream::{unfold, BoxStream};
use futures::StreamExt;
use serde_json::Value;

/// One `data:` payload of an SSE event.
#[derive(Debug, Clone, PartialEq)]
pub enum SseData {
    /// A JSON payload.
    Json(Value),
    /// The OpenAI end-of-stream marker `data: [DONE]`.
    Done,
    /// Any other payload (blank keep-alives, non-JSON text); stream
    /// consumers skip it.
    Other(String),
}

/// Classify a single `data:` line payload.
///
/// A payload equal to `[DONE]` marks the OpenAI end-of-stream sentinel;
/// anything that parses as JSON becomes [`SseData::Json`]; everything
/// else (blank keep-alives, non-JSON text) is [`SseData::Other`].
#[doc(hidden)]
pub fn parse_data_payload(payload: &str) -> SseData {
    let trimmed = payload.trim();
    if trimmed == "[DONE]" {
        SseData::Done
    } else if let Ok(json) = serde_json::from_str(trimmed) {
        SseData::Json(json)
    } else {
        SseData::Other(trimmed.to_string())
    }
}

/// Incremental SSE line splitter: buffers bytes across chunk boundaries
/// and pops complete `data:` line payloads one at a time.
///
/// The buffer stays raw bytes so a multi-byte UTF-8 character split
/// across chunks is reassembled before decoding (a delta stream must not
/// corrupt non-ASCII text).
#[derive(Debug, Default)]
pub struct SseLineParser {
    buf: Vec<u8>,
}

impl SseLineParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes (e.g. one `reqwest` body chunk).
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete `data:` line payload, or `None` when no
    /// complete `data:` line is buffered. Non-`data:` lines (comments,
    /// blank lines, other fields) are skipped internally.
    pub fn pop_ready(&mut self) -> Option<String> {
        loop {
            let pos = self.buf.iter().position(|&b| b == b'\n')?;
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = trim_cr(&line);
            let Ok(line) = std::str::from_utf8(line) else { continue };
            if let Some(payload) = line.strip_prefix("data:") {
                return Some(payload.trim().to_string());
            }
        }
    }

    /// Flush a trailing unterminated `data:` line (the stream ended
    /// without a final newline). `None` when nothing is buffered.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let rest = trim_cr(&rest);
        let line = std::str::from_utf8(rest).ok()?;
        Some(line.strip_prefix("data:")?.trim().to_string())
    }
}

/// Strip one trailing carriage return (the `\r` of a `\r\n` line ending).
fn trim_cr(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

/// Consume a `reqwest` response body as a stream of [`SseData`] events.
///
/// Chunk boundaries may split SSE lines anywhere, so partial lines are
/// buffered until the next `\n`. This is the only function here that
/// touches the network — the pure parsing pieces (`SseLineParser`,
/// `parse_data_payload`) are unit-tested directly.
pub fn sse_stream(response: reqwest::Response) -> BoxStream<'static, anyhow::Result<SseData>> {
    let bytes = response.bytes_stream();
    Box::pin(unfold((bytes, SseLineParser::new()), |(mut stream, mut parser)| async move {
        next_sse_event(&mut stream, &mut parser).await.map(|event| (event, (stream, parser)))
    }))
}

/// Pull the next SSE event: pop a ready `data:` payload, or read more
/// chunks until one completes (or the stream ends).
async fn next_sse_event<S, B, E>(
    stream: &mut S,
    parser: &mut SseLineParser,
) -> Option<anyhow::Result<SseData>>
where
    S: futures::Stream<Item = Result<B, E>> + Unpin + Send,
    B: std::borrow::Borrow<[u8]>,
    E: std::fmt::Display,
{
    loop {
        if let Some(payload) = parser.pop_ready() {
            return Some(Ok(parse_data_payload(&payload)));
        }
        match stream.next().await {
            Some(Ok(chunk)) => parser.push(chunk.borrow()),
            Some(Err(e)) => return Some(Err(anyhow::anyhow!("SSE read error: {e}"))),
            None => return parser.finish().map(|payload| Ok(parse_data_payload(&payload))),
        }
    }
}
