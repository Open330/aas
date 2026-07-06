//! SSE framing + streamed tool-call accumulation (port of `forEachUpstreamEvent` framing and
//! `toolAccumulator` from `proxy/server.ts`).

use crate::types::CommonEvent;
use std::collections::HashMap;

/// Frames an upstream SSE byte stream into event blocks split on blank lines (`\n\n`), over a
/// CRLF-normalized buffer, with UTF-8 decoding that tolerates multi-byte chars split across reads.
pub struct SseFramer {
    /// Bytes not yet forming a complete UTF-8 code point (an incomplete tail from the last feed).
    pending: Vec<u8>,
    /// Decoded, not-yet-framed text.
    buf: String,
}

impl Default for SseFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl SseFramer {
    pub fn new() -> Self {
        SseFramer { pending: Vec::new(), buf: String::new() }
    }

    /// Decode the valid UTF-8 prefix of `pending` into `buf`. When `flush`, decode the remainder
    /// lossily (end of stream, no more bytes coming).
    fn decode(&mut self, flush: bool) {
        match std::str::from_utf8(&self.pending) {
            Ok(s) => {
                self.buf.push_str(s);
                self.pending.clear();
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // Safe: bytes [..valid] are valid UTF-8 by definition of valid_up_to.
                self.buf.push_str(unsafe { std::str::from_utf8_unchecked(&self.pending[..valid]) });
                let rest = self.pending.split_off(valid);
                self.pending = rest;
                if flush {
                    self.buf.push_str(&String::from_utf8_lossy(&self.pending));
                    self.pending.clear();
                }
            }
        }
    }

    /// Normalize CRLF over the whole buffer, then split off every complete `\n\n`-terminated block.
    fn frame(&mut self) -> Vec<String> {
        if self.buf.contains('\r') {
            self.buf = self.buf.replace("\r\n", "\n");
        }
        let mut blocks = Vec::new();
        while let Some(idx) = self.buf.find("\n\n") {
            let block = self.buf[..idx].to_string();
            self.buf.replace_range(..idx + 2, "");
            blocks.push(block);
        }
        blocks
    }

    /// Feed a chunk of bytes, returning any complete SSE blocks that became available.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        self.decode(false);
        self.frame()
    }

    /// Finish the stream: flush any trailing decoder bytes and return remaining complete blocks
    /// plus a final trailing block (if it is not blank), mirroring asx's trailing-block handling.
    pub fn finish(&mut self) -> Vec<String> {
        self.decode(true);
        let mut blocks = self.frame();
        let rest = std::mem::take(&mut self.buf);
        if !rest.trim().is_empty() {
            blocks.push(rest);
        }
        blocks
    }
}

/// Merge streamed `tool_call_delta` fragments (keyed by wire index) into complete tool calls,
/// preserving first-seen order. id/name land on the opening fragment; args arrive in pieces.
#[derive(Default)]
pub struct ToolAccumulator {
    by_index: HashMap<i64, ToolAcc>,
    order: Vec<i64>,
}

#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    args: String,
}

impl ToolAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge one `CommonEvent::ToolCallDelta` (non-delta events are ignored).
    pub fn push(&mut self, ev: &CommonEvent) {
        if let CommonEvent::ToolCallDelta { index, id, name, args_delta } = ev {
            if !self.by_index.contains_key(index) {
                self.by_index.insert(*index, ToolAcc::default());
                self.order.push(*index);
            }
            let t = self.by_index.get_mut(index).unwrap();
            if let Some(id) = id {
                if !id.is_empty() {
                    t.id = id.clone();
                }
            }
            if let Some(name) = name {
                if !name.is_empty() {
                    t.name = name.clone();
                }
            }
            if let Some(delta) = args_delta {
                if !delta.is_empty() {
                    t.args.push_str(delta);
                }
            }
        }
    }

    /// Complete tool calls in first-seen order, as `CommonEvent::ToolCall` events.
    pub fn list(&self) -> Vec<CommonEvent> {
        self.order
            .iter()
            .map(|i| {
                let t = &self.by_index[i];
                CommonEvent::ToolCall { id: t.id.clone(), name: t.name.clone(), arguments: t.args.clone() }
            })
            .collect()
    }

    pub fn clear(&mut self) {
        self.by_index.clear();
        self.order.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}
