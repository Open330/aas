//! The short-lived in-process proxy server (port of `proxy/server.ts`).
//!
//! ```text
//!   agent wire ─[agent.parse_request]─▶ COMMON ─[backend.build_request]─▶ upstream
//!   upstream ─[backend.parse_stream_chunk]─▶ COMMON ─[agent.format_stream_chunk]─▶ agent wire
//! ```

use crate::adapters::{pick_agent, pick_backend};
use crate::models::backend_choices;
use crate::retry::{fetch_upstream_with_retry, UpstreamOutcome};
use crate::sse::{SseFramer, ToolAccumulator};
use crate::types::{AgentAdapter, BackendAdapter, CommonEvent, CommonResponse, CommonToolCall, StreamCtx};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Json, Response},
    Router,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// Credential passed to the backend (raw stored secret preferred over an extracted api key).
#[derive(Clone, Default)]
pub struct Credential {
    pub raw: Option<String>,
    pub api_key: Option<String>,
}

pub struct ProxyStartOptions {
    /// frontend wire (the agent binary talks TO the proxy).
    pub source_provider: String,
    /// backend (real upstream the proxy calls).
    pub target_provider: String,
    pub target_credential: Credential,
    pub tmp_dir: Option<PathBuf>,
    pub port: Option<u16>,
}

pub struct ProxyHandle {
    pub url: String,
    pub port: u16,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl ProxyHandle {
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

struct ProxyState {
    agent: Option<Arc<dyn AgentAdapter>>,
    backend: Option<Arc<dyn BackendAdapter>>,
    agent_provider: String,
    backend_provider: String,
    cred: String,
    client: reqwest::Client,
}

/// `targetCredential?.raw || targetCredential?.apiKey || ''` (empty string treated as falsy).
fn pick_cred(c: &Credential) -> String {
    if let Some(raw) = c.raw.as_ref().filter(|s| !s.is_empty()) {
        return raw.clone();
    }
    if let Some(k) = c.api_key.as_ref().filter(|s| !s.is_empty()) {
        return k.clone();
    }
    String::new()
}

pub async fn start_proxy(options: ProxyStartOptions) -> anyhow::Result<ProxyHandle> {
    let agent_provider = options.source_provider.to_lowercase();
    let backend_provider = options.target_provider.to_lowercase();
    let cred = pick_cred(&options.target_credential);

    let state = Arc::new(ProxyState {
        agent: pick_agent(&agent_provider),
        backend: pick_backend(&backend_provider),
        agent_provider,
        backend_provider,
        cred,
        client: reqwest::Client::builder().build()?,
    });

    let app = Router::new().fallback(handle).with_state(state);

    // Bind and KEEP the listener (avoids asx's free-port race). port 0 -> OS picks a free port,
    // unless the caller pinned one.
    let bind_port = options.port.unwrap_or(0);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", bind_port)).await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    Ok(ProxyHandle {
        url,
        port,
        shutdown: Some(shutdown_tx),
        join: Some(join),
    })
}

pub(crate) fn is_inference(method: &Method, path: &str) -> bool {
    method == Method::POST
        && (path.contains("/responses")
            || path.contains("/messages")
            || path.contains("/chat/completions")
            || path.contains("/completions"))
}

pub(crate) fn is_models(method: &Method, path: &str) -> bool {
    method == Method::GET && path.trim_end_matches('/').ends_with("/models")
}

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, Json(value)).into_response()
}

async fn handle(State(st): State<Arc<ProxyState>>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    if is_models(&method, &path) {
        let choices = backend_choices(&st.backend_provider);
        let body = match &st.agent {
            Some(a) => a.format_models(&choices),
            None => {
                let data: Vec<Value> = choices
                    .iter()
                    .map(|m| json!({ "id": m.id, "object": "model", "created": 0, "owned_by": format!("asx-{}", st.backend_provider) }))
                    .collect();
                json!({ "object": "list", "data": data })
            }
        };
        return json_response(StatusCode::OK, body);
    }

    // Non-inference startup checkpoints (auth/status/billing). Real auth is the backend cred.
    if !is_inference(&method, &path) {
        return json_response(StatusCode::OK, json!({ "ok": true, "authenticated": true, "via": "asx-proxy" }));
    }

    let (agent, backend) = match (&st.agent, &st.backend) {
        (Some(a), Some(b)) => (a.clone(), b.clone()),
        (None, _) => return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": { "message": format!("no agent adapter for '{}'", st.agent_provider) } })),
        (_, None) => return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": { "message": format!("no backend adapter for '{}'", st.backend_provider) } })),
    };

    let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(_) => Bytes::new(),
    };
    let body_json: Value = serde_json::from_slice(&body_bytes).unwrap_or_else(|_| Value::Object(Default::default()));

    let common = agent.parse_request(&path, &body_json);
    let up = backend.build_request(&common, &st.cred);

    let outcome = match fetch_upstream_with_retry(&st.client, &up, backend.as_ref()).await {
        Ok(o) => o,
        Err(e) => {
            // Pre-stream failure (only network errors with no Response reach here): safe 500.
            return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": { "message": e.to_string() } }));
        }
    };

    let req_id = short_id();
    let mut ctx = StreamCtx::new(format!("chatcmpl-asx-{req_id}"), now_secs(), common.model.clone(), common.tool_namespaces.clone());

    match outcome {
        UpstreamOutcome::Error { status, detail } => {
            let detail300: String = detail.chars().take(300).collect();
            let msg = format!("[asx-proxy] backend {} error {}: {}", st.backend_provider, status, detail300);
            if common.stream {
                let mut s = String::new();
                s.push_str(&agent.format_stream_chunk(&CommonEvent::Text { text: msg }, &mut ctx));
                s.push_str(&agent.format_stream_chunk(&CommonEvent::Done { finish_reason: None }, &mut ctx));
                build_stream_response(&agent, Body::from(s))
            } else {
                json_response(StatusCode::OK, agent.format_response(&CommonResponse { text: msg, ..Default::default() }, &common))
            }
        }
        UpstreamOutcome::Stream(res) => {
            if common.stream {
                let (tx, rx) = mpsc::channel::<Bytes>(64);
                let agent2 = agent.clone();
                let backend2 = backend.clone();
                tokio::spawn(async move {
                    stream_producer(res, agent2, backend2, ctx, tx).await;
                });
                let body = channel_body(rx);
                build_stream_response(&agent, body)
            } else {
                accumulate_non_stream(res, agent.as_ref(), backend.as_ref(), &common).await
            }
        }
    }
}

/// Build a 200 streaming response using the agent's SSE headers.
fn build_stream_response(agent: &Arc<dyn AgentAdapter>, body: Body) -> Response {
    let mut resp = Response::builder().status(StatusCode::OK);
    for (k, v) in agent.stream_headers() {
        resp = resp.header(k, v);
    }
    resp.body(body).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Wrap an mpsc receiver of Bytes as an axum streaming body.
fn channel_body(rx: mpsc::Receiver<Bytes>) -> Body {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<Bytes, std::convert::Infallible>(b), rx))
    });
    Body::from_stream(stream)
}

/// Flush accumulated tool calls into formatted chunks and clear the accumulator.
pub(crate) fn flush_tools(agent: &dyn AgentAdapter, ctx: &mut StreamCtx, acc: &mut ToolAccumulator) -> Vec<String> {
    let chunks: Vec<String> = acc.list().iter().map(|t| agent.format_stream_chunk(t, ctx)).collect();
    acc.clear();
    chunks
}

/// Route one COMMON event to zero or more frontend SSE chunks. `tool_call_delta` is held in the
/// accumulator; `done` flushes held tool calls first, then emits the done chunk.
pub(crate) fn process_event(agent: &dyn AgentAdapter, ctx: &mut StreamCtx, acc: &mut ToolAccumulator, saw_done: &mut bool, ev: CommonEvent) -> Vec<String> {
    match &ev {
        CommonEvent::ToolCallDelta { .. } => {
            acc.push(&ev);
            Vec::new()
        }
        CommonEvent::Done { .. } => {
            let mut out = flush_tools(agent, ctx, acc);
            *saw_done = true;
            out.push(agent.format_stream_chunk(&ev, ctx));
            out
        }
        _ => vec![agent.format_stream_chunk(&ev, ctx)],
    }
}

/// The happy streaming path: read upstream, transcode, write frontend SSE to the channel. Reproduces
/// asx invariants: buffer+flush tool calls, synthetic terminator on missing `done`, mid-stream error
/// → flush + error text + terminator, client-disconnect cancels upstream reads.
async fn stream_producer(
    res: reqwest::Response,
    agent: Arc<dyn AgentAdapter>,
    backend: Arc<dyn BackendAdapter>,
    mut ctx: StreamCtx,
    tx: mpsc::Sender<Bytes>,
) {
    let mut framer = SseFramer::new();
    let mut acc = ToolAccumulator::new();
    let mut saw_done = false;
    let mut client_closed = false;
    let mut stream_err: Option<String> = None;

    let mut body = std::pin::pin!(res.bytes_stream());

    'outer: loop {
        let next = tokio::select! {
            _ = tx.closed() => { client_closed = true; break 'outer; }
            c = body.next() => c,
        };
        match next {
            Some(Ok(bytes)) => {
                for block in framer.feed(&bytes) {
                    for ev in backend.parse_stream_chunk(&block) {
                        for chunk in process_event(agent.as_ref(), &mut ctx, &mut acc, &mut saw_done, ev) {
                            if tx.send(Bytes::from(chunk)).await.is_err() {
                                client_closed = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }
            Some(Err(e)) => {
                stream_err = Some(e.to_string());
                break;
            }
            None => {
                for block in framer.finish() {
                    for ev in backend.parse_stream_chunk(&block) {
                        for chunk in process_event(agent.as_ref(), &mut ctx, &mut acc, &mut saw_done, ev) {
                            if tx.send(Bytes::from(chunk)).await.is_err() {
                                client_closed = true;
                                break 'outer;
                            }
                        }
                    }
                }
                break;
            }
        }
    }

    if client_closed {
        return; // client already gone — nothing to write
    }

    let tail = finalize_stream(agent.as_ref(), &mut ctx, &mut acc, saw_done, stream_err);
    for chunk in tail {
        if tx.send(Bytes::from(chunk)).await.is_err() {
            break;
        }
    }
}

/// Post-loop terminator logic (extracted for testing). Flushes any held tool calls, then:
///  - mid-stream error → error-text chunk + done;
///  - clean end without a `done` → synthetic warning + done;
///  - otherwise nothing (a real `done` was already emitted).
pub(crate) fn finalize_stream(agent: &dyn AgentAdapter, ctx: &mut StreamCtx, acc: &mut ToolAccumulator, saw_done: bool, stream_err: Option<String>) -> Vec<String> {
    let mut tail: Vec<String> = flush_tools(agent, ctx, acc);
    if let Some(err) = stream_err {
        // Mid-stream break: surface a readable error as SSE text, then a clean done.
        let msg = format!("\n[asx-proxy] stream interrupted: {}", if err.is_empty() { "connection lost".to_string() } else { err });
        tail.push(agent.format_stream_chunk(&CommonEvent::Text { text: msg }, ctx));
        tail.push(agent.format_stream_chunk(&CommonEvent::Done { finish_reason: None }, ctx));
    } else if !saw_done {
        // Stream ended without an explicit done — synthetic warning + done.
        let warn = "\n[asx-proxy] upstream stream ended unexpectedly (connection may have been interrupted)";
        tail.push(agent.format_stream_chunk(&CommonEvent::Text { text: warn.to_string() }, ctx));
        tail.push(agent.format_stream_chunk(&CommonEvent::Done { finish_reason: None }, ctx));
    }
    tail
}

/// Agent wanted non-stream but the backend streams — accumulate text/tools, then format once.
async fn accumulate_non_stream(res: reqwest::Response, agent: &dyn AgentAdapter, backend: &dyn BackendAdapter, common: &crate::types::CommonRequest) -> Response {
    let mut text = String::new();
    let mut finish_reason: Option<String> = None;
    let mut acc = ToolAccumulator::new();
    let mut framer = SseFramer::new();
    let mut stream_err: Option<String> = None;

    let mut body = std::pin::pin!(res.bytes_stream());
    'outer: loop {
        match body.next().await {
            Some(Ok(bytes)) => {
                for block in framer.feed(&bytes) {
                    for ev in backend.parse_stream_chunk(&block) {
                        apply_accumulate(&ev, &mut text, &mut finish_reason, &mut acc);
                    }
                }
            }
            Some(Err(e)) => {
                stream_err = Some(e.to_string());
                break 'outer;
            }
            None => {
                for block in framer.finish() {
                    for ev in backend.parse_stream_chunk(&block) {
                        apply_accumulate(&ev, &mut text, &mut finish_reason, &mut acc);
                    }
                }
                break 'outer;
            }
        }
    }
    if let Some(e) = stream_err {
        text.push_str(&format!("\n[asx-proxy] stream interrupted: {}", if e.is_empty() { "connection lost".to_string() } else { e }));
    }
    let tool_calls: Vec<CommonToolCall> = acc
        .list()
        .into_iter()
        .filter_map(|ev| match ev {
            CommonEvent::ToolCall { id, name, arguments } => Some(CommonToolCall { id, name, arguments }),
            _ => None,
        })
        .collect();
    let resp = CommonResponse { text, tool_calls, finish_reason };
    json_response(StatusCode::OK, agent.format_response(&resp, common))
}

fn apply_accumulate(ev: &CommonEvent, text: &mut String, finish_reason: &mut Option<String>, acc: &mut ToolAccumulator) {
    match ev {
        CommonEvent::Text { text: t } => text.push_str(t),
        CommonEvent::ToolCallDelta { .. } => acc.push(ev),
        CommonEvent::Done { finish_reason: fr } => *finish_reason = fr.clone(),
        _ => {}
    }
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
