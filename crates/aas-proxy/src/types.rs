//! ASX Proxy — hub-and-spoke transcoder COMMON types (port of `proxy/types.ts`).
//!
//! ```text
//!   agent wire ──parse──▶ COMMON ──build──▶ backend wire
//!   agent wire ◀─format── COMMON ◀─parse─── backend wire
//! ```
//!
//! Every adapter only knows its own wire <-> COMMON. N adapters, not N*N converters.

use serde_json::Value;

/// A single tool call the model requested. `arguments` is the raw JSON string of args
/// (kept as a string so it round-trips losslessly across wire formats).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CommonToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A conversation message in COMMON form. `role` is the raw wire role string
/// (`system` | `user` | `assistant` | `tool`) to mirror asx's string comparisons.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CommonMessage {
    pub role: String,
    pub content: String,
    /// assistant turns may carry the tool calls the model requested this turn.
    pub tool_calls: Option<Vec<CommonToolCall>>,
    /// tool turns carry the result of a prior call; `tool_call_id` links back to it.
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    /// tool turns: the tool execution failed (Anthropic `tool_result.is_error`).
    pub is_error: Option<bool>,
}

/// A tool the agent exposes to the model, normalized to name + JSON-schema params.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CommonToolDef {
    pub name: String,
    pub description: Option<String>,
    /// JSON schema object.
    pub parameters: Option<Value>,
    /// strict structured-args enforcement (OpenAI/Chat).
    pub strict: Option<bool>,
    /// provider built-in tool type passthrough (e.g. Anthropic `bash_20250124`).
    pub builtin_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct CommonRequest {
    /// model the agent asked for (backend usually maps to its own).
    pub model: String,
    /// system prompt / instructions.
    pub system: Option<String>,
    pub messages: Vec<CommonMessage>,
    /// normalized tool definitions (translated across wires).
    pub tools: Option<Vec<CommonToolDef>>,
    /// Codex namespace tool groups flattened into `${ns}__${name}` defs; the response side
    /// needs this list to split flat names back into namespaced calls.
    pub tool_namespaces: Option<Vec<String>>,
    /// pass-through hint (`auto` | `none` | `required` | `{name}`).
    pub tool_choice: Option<Value>,
    /// allow the model to emit multiple tool calls per turn.
    pub parallel_tool_calls: Option<bool>,
    pub stream: bool,
    pub max_tokens: Option<i64>,
    pub temperature: Option<f64>,
    pub reasoning_effort: Option<String>,
}

/// Events flow backend -> COMMON -> agent.
///   - Backends emit `Text`, `ToolCallDelta` (fragments), `Done`, `Error`.
///   - The proxy server accumulates `ToolCallDelta` by index into a complete `ToolCall` event,
///     which is what agent adapters consume.
#[derive(Clone, Debug, PartialEq)]
pub enum CommonEvent {
    Text {
        text: String,
    },
    ToolCallDelta {
        index: i64,
        id: Option<String>,
        name: Option<String>,
        args_delta: Option<String>,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    Done {
        finish_reason: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct CommonResponse {
    pub text: String,
    pub tool_calls: Vec<CommonToolCall>,
    pub finish_reason: Option<String>,
}

/// An upstream HTTP call built by a backend adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct UpstreamRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Per-response streaming context, threaded through `format_stream_chunk`. Mirrors asx `StreamCtx`
/// but with the lazily-initialized fields given concrete defaults.
#[derive(Clone, Debug)]
pub struct StreamCtx {
    pub id: String,
    pub created: i64,
    pub model: String,
    pub first: bool,
    /// accumulated text (for adapters that need the full text at `done`).
    pub acc: String,
    /// stable output item id (Responses wire).
    pub item_id: Option<String>,
    /// agent has opened its text block/item.
    pub text_open: bool,
    /// output/content index reserved for streamed text.
    pub text_index: i64,
    /// next free output index (text item + one per tool call).
    pub next_index: i64,
    /// wire output items assembled so far (for final framing); also used by claude/grok to count
    /// emitted tool calls (they push the id as a JSON string).
    pub items: Vec<Value>,
    /// from `CommonRequest` — split `${ns}__${name}` back into namespaced calls.
    pub tool_namespaces: Option<Vec<String>>,
}

impl StreamCtx {
    pub fn new(
        id: String,
        created: i64,
        model: String,
        tool_namespaces: Option<Vec<String>>,
    ) -> Self {
        StreamCtx {
            id,
            created,
            model,
            first: true,
            acc: String::new(),
            item_id: None,
            text_open: false,
            text_index: 0,
            next_index: 0,
            items: Vec::new(),
            tool_namespaces,
        }
    }
}

/// Provider acting as the AGENT (the binary talking TO the proxy).
pub trait AgentAdapter: Send + Sync {
    /// Parse an incoming inference request into COMMON.
    fn parse_request(&self, path: &str, body: &Value) -> CommonRequest;
    /// HTTP headers for a streaming response.
    fn stream_headers(&self) -> Vec<(String, String)>;
    /// Turn one COMMON event into wire SSE text (may be empty for ignored events). `ctx` persists
    /// across the response (id/created/model, first-chunk flag).
    fn format_stream_chunk(&self, ev: &CommonEvent, ctx: &mut StreamCtx) -> String;
    /// Non-stream: full wire response body.
    fn format_response(&self, resp: &CommonResponse, req: &CommonRequest) -> Value;
    /// `GET /models` body in this agent's wire format (drives its `/model` picker).
    fn format_models(&self, choices: &[crate::models::BackendChoice]) -> Value;
}

/// Provider acting as the BACKEND (the real upstream the proxy calls).
pub trait BackendAdapter: Send + Sync {
    /// Build the upstream HTTP call from COMMON + the profile credential (raw stored secret).
    fn build_request(&self, req: &CommonRequest, cred: &str) -> UpstreamRequest;
    /// Parse one upstream SSE event block into COMMON events.
    fn parse_stream_chunk(&self, event_block: &str) -> Vec<CommonEvent>;
    /// Whether this backend defines a body-level retry hook (only z.ai does). Mirrors asx's
    /// `!!backend?.isRetryable` presence check.
    fn has_is_retryable(&self) -> bool {
        false
    }
    /// Provider-specific retry decision from a response body (e.g. z.ai returns a 200 whose body
    /// carries `{"error":{"code":"1305"}}` on overload).
    fn is_retryable(&self, _status: u16, _body: &str) -> bool {
        false
    }
}
