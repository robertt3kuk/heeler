//! Cutting a response body into lines and Server-Sent Events. Chunk boundaries fall anywhere,
//! including inside a UTF-8 sequence, so lines are cut on bytes and decoded afterwards.

use crate::error::GatewayError;

/// The longest line accepted; a provider that sends more is not speaking its protocol.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Splits a byte stream into lines (`\n`, with an optional `\r` before it).
#[derive(Debug, Default)]
pub(crate) struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, GatewayError> {
        let mut lines = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                if self.pending.last() == Some(&b'\r') {
                    self.pending.pop();
                }
                lines.push(String::from_utf8_lossy(&self.pending).into_owned());
                self.pending.clear();
            } else {
                self.pending.push(byte);
                if self.pending.len() > MAX_LINE_BYTES {
                    return Err(GatewayError::Protocol(format!(
                        "a line is longer than {MAX_LINE_BYTES} bytes"
                    )));
                }
            }
        }
        Ok(lines)
    }

    /// The last line when the body ended without a newline.
    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        if self.pending.last() == Some(&b'\r') {
            self.pending.pop();
        }
        let line = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        Some(line)
    }
}

/// One Server-Sent Event: its `event:` name (if any) and its `data:` lines joined by `\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub(crate) event: Option<String>,
    pub(crate) data: String,
}

#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    lines: LineBuffer,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, GatewayError> {
        let lines = self.lines.push(bytes)?;
        Ok(lines.into_iter().filter_map(|l| self.line(&l)).collect())
    }

    /// Flushes an event the body ended in the middle of.
    pub(crate) fn finish(&mut self) -> Vec<SseEvent> {
        let mut events: Vec<SseEvent> = self
            .lines
            .finish()
            .and_then(|l| self.line(&l))
            .into_iter()
            .collect();
        events.extend(self.dispatch());
        events
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        if self.data.is_empty() {
            return None;
        }
        let data = self.data.join("\n");
        self.data.clear();
        Some(SseEvent { event, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: name.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    #[test]
    fn events_survive_any_chunking() {
        let body = "event: a\ndata: one\n\n: comment\ndata: two\r\ndata: lines\r\n\n\ndata: ü\n\n";
        let whole = SseDecoder::default().push(body.as_bytes()).unwrap();
        assert_eq!(
            whole,
            [
                event(Some("a"), "one"),
                event(None, "two\nlines"),
                event(None, "ü")
            ]
        );
        for size in 1..8 {
            let mut decoder = SseDecoder::default();
            let mut events = Vec::new();
            for chunk in body.as_bytes().chunks(size) {
                events.extend(decoder.push(chunk).unwrap());
            }
            events.extend(decoder.finish());
            assert_eq!(events, whole, "chunk size {size}");
        }
    }

    #[test]
    fn an_event_without_a_final_blank_line_is_flushed_at_the_end() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: tail").unwrap().is_empty());
        assert_eq!(decoder.finish(), [event(None, "tail")]);
    }

    #[test]
    fn lines_are_cut_on_newlines_with_optional_carriage_returns() {
        let mut lines = LineBuffer::default();
        assert_eq!(lines.push(b"a\r\nb\nc").unwrap(), ["a", "b"]);
        assert_eq!(lines.push(b"d\n").unwrap(), ["cd"]);
        assert_eq!(lines.finish(), None);
    }
}
