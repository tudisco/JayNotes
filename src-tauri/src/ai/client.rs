//! OpenAI-compatible chat-completions client with streaming (SSE) support.
//!
//! Deliberately dependency-light: `reqwest` for the HTTP/TLS plumbing plus a
//! hand-rolled Server-Sent-Events parser. No vendor SDK. The parser
//! ([`SseParser`]) and the delta accumulator ([`StreamAccumulator`]) are pure,
//! synchronous, and unit-tested; only [`ChatClient`]/[`consume_stream`] touch
//! the network.
//!
//! ## Provider quirks handled here
//!
//! * Auth header omitted entirely when the API key is empty (Ollama, local).
//! * `temperature` omitted when `None`; `tools`/`tool_choice` omitted when the
//!   turn sends no tools — some strict providers reject null/empty fields.
//! * Tool-call deltas arrive fragmented: the `id`/`name` land in the first
//!   chunk and `arguments` accumulate character-by-character across many later
//!   chunks, keyed by the call's `index`. Multiple parallel calls interleave.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Notify;

use super::settings::AiSettings;

// ---------------------------------------------------------------------------
// Wire message types (OpenAI chat-completions shape)
// ---------------------------------------------------------------------------

/// A single chat message in the OpenAI request/response shape. `content` is
/// omitted when `None` (assistant turns that are pure tool calls); `tool_calls`
/// and `tool_call_id` are omitted when empty so strict providers stay happy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<ApiToolCall>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_call_id: Option<String>,
}

impl ApiMessage {
    pub fn system(text: impl Into<String>) -> Self {
        ApiMessage {
            role: "system".into(),
            content: Some(text.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn user(text: impl Into<String>) -> Self {
        ApiMessage {
            role: "user".into(),
            content: Some(text.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn assistant(content: Option<String>, tool_calls: Vec<ApiToolCall>) -> Self {
        ApiMessage {
            role: "assistant".into(),
            content,
            tool_calls,
            tool_call_id: None,
        }
    }
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ApiMessage {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A finalized function tool call requested by the assistant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ApiFunctionCall,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiFunctionCall {
    pub name: String,
    /// Raw JSON string of arguments (per the OpenAI spec).
    pub arguments: String,
}

// ---------------------------------------------------------------------------
// Streaming chunk types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    /// Separate reasoning channel, MiniMax / DeepSeek style.
    #[serde(default)]
    reasoning_content: Option<String>,
    /// Separate reasoning channel, OpenRouter style.
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// SSE parser
// ---------------------------------------------------------------------------

/// Incremental Server-Sent-Events parser. Feed it raw byte chunks as they
/// arrive off the socket; it emits the `data:` payload string of every complete
/// event (comment lines and non-`data` fields are ignored). Multi-line `data`
/// fields within one event are joined with `\n`, per the SSE spec.
#[derive(Default)]
pub struct SseParser {
    /// Bytes received but not yet terminated by a newline.
    buf: String,
    /// `data:` lines collected for the event currently being assembled.
    pending: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk of bytes and returns any newly completed event payloads.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        // Process every complete line (terminated by '\n') in the buffer.
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            self.consume_line(line, &mut out);
        }
        out
    }

    fn consume_line(&mut self, line: &str, out: &mut Vec<String>) {
        if line.is_empty() {
            // Blank line = event boundary: flush the collected data lines.
            if !self.pending.is_empty() {
                out.push(self.pending.join("\n"));
                self.pending.clear();
            }
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }
        if let Some(rest) = line.strip_prefix("data:") {
            // A single optional leading space after the colon is stripped.
            self.pending.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
        // Other fields (event:, id:, retry:) are irrelevant here.
    }
}

// ---------------------------------------------------------------------------
// Reasoning vs. content splitting
// ---------------------------------------------------------------------------

/// A piece of streamed assistant output. Reasoning (a model's private thinking)
/// is surfaced separately so the UI can collapse it and — crucially — so it is
/// never folded into the assistant `content` that gets sent back to the API on
/// later turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamPiece {
    /// Visible assistant text.
    Content(String),
    /// Model reasoning / chain-of-thought (display only).
    Reasoning(String),
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// State of the inline `<think>…</think>` splitter (see [`ThinkSplitter`]).
#[derive(Default, Debug, PartialEq, Eq)]
enum ThinkState {
    /// Start of the content stream: still deciding whether it opens with a
    /// `<think>` tag.
    #[default]
    Start,
    /// Inside a `<think>` block: text routes to reasoning until `</think>`.
    InThink,
    /// Past any leading think block: everything is visible content (a later
    /// `<think>` mid-stream is treated as literal text).
    Normal,
}

/// Splits a `delta.content` stream that carries reasoning inline as
/// `<think>…</think>` tags at its very start. Tags may be split across chunks,
/// so a conservative buffer holds back any suffix that could still grow into a
/// tag. Providers that use a separate `reasoning_content`/`reasoning` field
/// never reach here — this only handles the tag-in-content shape.
#[derive(Default)]
struct ThinkSplitter {
    state: ThinkState,
    /// Bytes not yet classified (a possible partial tag).
    buf: String,
}

impl ThinkSplitter {
    /// Feeds a content delta, pushing classified pieces onto `out`.
    fn feed(&mut self, text: &str, out: &mut Vec<StreamPiece>) {
        self.buf.push_str(text);
        loop {
            match self.state {
                ThinkState::Start => {
                    let trimmed = self.buf.trim_start();
                    let lead = self.buf.len() - trimmed.len();
                    if trimmed.is_empty() {
                        return; // only whitespace so far — wait for more
                    }
                    if trimmed.len() < THINK_OPEN.len() {
                        if THINK_OPEN.starts_with(trimmed) {
                            return; // could still become `<think>` — keep buffering
                        }
                        self.state = ThinkState::Normal; // definitely not a think block
                    } else if trimmed.starts_with(THINK_OPEN) {
                        self.buf.drain(..lead + THINK_OPEN.len());
                        self.state = ThinkState::InThink;
                    } else {
                        self.state = ThinkState::Normal;
                    }
                }
                ThinkState::InThink => {
                    if let Some(i) = self.buf.find(THINK_CLOSE) {
                        let reasoning: String = self.buf.drain(..i).collect();
                        self.buf.drain(..THINK_CLOSE.len());
                        if !reasoning.is_empty() {
                            out.push(StreamPiece::Reasoning(reasoning));
                        }
                        self.state = ThinkState::Normal;
                    } else {
                        // Emit all but a suffix that might be a partial `</think>`.
                        let keep = partial_suffix_len(&self.buf, THINK_CLOSE);
                        let emit = self.buf.len() - keep;
                        if emit > 0 {
                            let reasoning: String = self.buf.drain(..emit).collect();
                            out.push(StreamPiece::Reasoning(reasoning));
                        }
                        return;
                    }
                }
                ThinkState::Normal => {
                    if !self.buf.is_empty() {
                        out.push(StreamPiece::Content(std::mem::take(&mut self.buf)));
                    }
                    return;
                }
            }
        }
    }
}

/// Longest `k` (`0 < k < tag.len()`) such that `buf` ends with `tag[..k]` — the
/// length of a trailing partial-tag suffix to hold back. `tag` is ASCII.
fn partial_suffix_len(buf: &str, tag: &str) -> usize {
    let max = tag.len().saturating_sub(1).min(buf.len());
    (1..=max)
        .rev()
        .find(|&k| buf.as_bytes().ends_with(tag[..k].as_bytes()))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Delta accumulator
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

/// Accumulates streamed deltas into a single completed turn. Content tokens are
/// returned from [`ingest`](Self::ingest) as they arrive so the caller can emit
/// them live; tool-call fragments are stitched together internally and surfaced
/// by [`finish`](Self::finish).
#[derive(Default)]
pub struct StreamAccumulator {
    content: String,
    /// Accumulated reasoning text (display only; never sent back to the API).
    reasoning: String,
    /// Splits inline `<think>…</think>` reasoning out of the content stream.
    think: ThinkSplitter,
    /// Tool calls keyed by their streamed `index`, so out-of-order fragments
    /// and multiple parallel calls all land in the right slot.
    tool_calls: BTreeMap<usize, ToolCallBuilder>,
    finish_reason: Option<String>,
    done: bool,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// True once a `[DONE]` sentinel has been seen.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Ingests one SSE `data:` payload, returning the classified pieces
    /// (content and/or reasoning) it carried, in order. `[DONE]` marks
    /// completion. Unparseable payloads are ignored (defensive against provider
    /// noise).
    pub fn ingest(&mut self, payload: &str) -> Vec<StreamPiece> {
        let payload = payload.trim();
        if payload.is_empty() {
            return Vec::new();
        }
        if payload == "[DONE]" {
            self.done = true;
            return Vec::new();
        }
        let chunk: ChatChunk = match serde_json::from_str(payload) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let mut pieces = Vec::new();
        for choice in chunk.choices {
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
            // Separate reasoning channels (MiniMax/DeepSeek, OpenRouter) arrive
            // before content and are unconditionally reasoning.
            for r in [choice.delta.reasoning_content, choice.delta.reasoning]
                .into_iter()
                .flatten()
            {
                if !r.is_empty() {
                    self.reasoning.push_str(&r);
                    pieces.push(StreamPiece::Reasoning(r));
                }
            }
            if let Some(text) = choice.delta.content {
                if !text.is_empty() {
                    let mut split = Vec::new();
                    self.think.feed(&text, &mut split);
                    for piece in split {
                        match &piece {
                            StreamPiece::Content(c) => self.content.push_str(c),
                            StreamPiece::Reasoning(r) => self.reasoning.push_str(r),
                        }
                        pieces.push(piece);
                    }
                }
            }
            for tc in choice.delta.tool_calls {
                let entry = self.tool_calls.entry(tc.index).or_default();
                if let Some(id) = tc.id {
                    if !id.is_empty() {
                        entry.id = id;
                    }
                }
                if let Some(func) = tc.function {
                    if let Some(name) = func.name {
                        if !name.is_empty() {
                            entry.name.push_str(&name);
                        }
                    }
                    if let Some(args) = func.arguments {
                        entry.arguments.push_str(&args);
                    }
                }
            }
        }
        pieces
    }

    /// Consumes the accumulator into a finished turn.
    pub fn finish(self) -> FinishedTurn {
        let tool_calls: Vec<ApiToolCall> = self
            .tool_calls
            .into_values()
            .filter(|b| !b.name.is_empty())
            .enumerate()
            .map(|(i, b)| ApiToolCall {
                id: if b.id.is_empty() {
                    format!("call_{i}")
                } else {
                    b.id
                },
                kind: "function".into(),
                function: ApiFunctionCall {
                    name: b.name,
                    arguments: b.arguments,
                },
            })
            .collect();
        FinishedTurn {
            content: self.content,
            reasoning: self.reasoning,
            tool_calls,
            finish_reason: self.finish_reason,
            cancelled: false,
        }
    }
}

/// The outcome of consuming one streamed assistant turn.
#[derive(Debug, Clone)]
pub struct FinishedTurn {
    pub content: String,
    /// Accumulated reasoning text (display only; never sent back to the API).
    pub reasoning: String,
    pub tool_calls: Vec<ApiToolCall>,
    pub finish_reason: Option<String>,
    pub cancelled: bool,
}

impl FinishedTurn {
    /// True when the model wants to call tools (explicit finish_reason or, for
    /// providers that omit it, simply because tool calls are present).
    pub fn wants_tools(&self) -> bool {
        !self.tool_calls.is_empty()
            || self.finish_reason.as_deref() == Some("tool_calls")
    }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// A configured client for one provider endpoint.
pub struct ChatClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    temperature: Option<f32>,
}

impl ChatClient {
    pub fn new(settings: &AiSettings) -> Result<Self, String> {
        if settings.base_url.trim().is_empty() {
            return Err("No AI base URL is configured. Set one in AI settings.".into());
        }
        if settings.model.trim().is_empty() {
            return Err("No AI model is configured. Set one in AI settings.".into());
        }
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("Could not build HTTP client: {e}"))?;
        Ok(ChatClient {
            http,
            base_url: settings.base_url.trim_end_matches('/').to_string(),
            api_key: settings.api_key.clone(),
            model: settings.model.clone(),
            temperature: settings.temperature,
        })
    }

    /// Builds a client for model-listing / probing, where only `base_url` and
    /// `api_key` matter (the model may be blank).
    pub fn for_listing(settings: &AiSettings) -> Result<Self, String> {
        if settings.base_url.trim().is_empty() {
            return Err("No AI base URL is configured.".into());
        }
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("Could not build HTTP client: {e}"))?;
        Ok(ChatClient {
            http,
            base_url: settings.base_url.trim_end_matches('/').to_string(),
            api_key: settings.api_key.clone(),
            model: settings.model.clone(),
            temperature: settings.temperature,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    /// Opens a streaming chat-completions request. `tools` is the JSON tool
    /// schema array; pass an empty slice (or `use_tools = false`) to omit tools
    /// and `tool_choice` entirely for the final wrap-up turn.
    pub async fn stream(
        &self,
        messages: &[ApiMessage],
        tools: &[Value],
        use_tools: bool,
    ) -> Result<reqwest::Response, String> {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
        });
        let map = body.as_object_mut().unwrap();
        if let Some(temp) = self.temperature {
            map.insert("temperature".into(), serde_json::json!(temp));
        }
        if use_tools && !tools.is_empty() {
            map.insert("tools".into(), serde_json::json!(tools));
            map.insert("tool_choice".into(), serde_json::json!("auto"));
        }

        let mut req = self.http.post(self.endpoint("chat/completions")).json(&body);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Request to AI provider failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("AI provider returned {status}: {}", truncate(&text, 500)));
        }
        Ok(resp)
    }

    /// Lists model ids via `GET {base_url}/models`. Best-effort: returns a clear
    /// error when the endpoint is missing or its shape is unexpected.
    pub async fn list_models(&self) -> Result<Vec<String>, String> {
        let mut req = self.http.get(self.endpoint("models"));
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Could not reach {}: {e}", self.endpoint("models")))?;
        if !resp.status().is_success() {
            return Err(format!(
                "Model listing not supported by this provider (HTTP {})",
                resp.status()
            ));
        }
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Could not parse model list: {e}"))?;
        parse_model_ids(&json)
            .ok_or_else(|| "Provider returned an unrecognized model list format".to_string())
    }
}

/// Extracts model ids from the common `{ "data": [ { "id": ... } ] }` shape,
/// falling back to a bare array of ids/objects.
fn parse_model_ids(json: &Value) -> Option<Vec<String>> {
    let arr = json
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| json.as_array())?;
    let mut ids: Vec<String> = arr
        .iter()
        .filter_map(|m| {
            m.get("id")
                .and_then(|v| v.as_str())
                .or_else(|| m.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    ids.sort();
    ids.dedup();
    Some(ids)
}

/// Reads a streamed response to completion, invoking `on_piece` for each
/// classified piece (content or reasoning) as it arrives and stitching
/// tool-call deltas together. Aborts promptly (leaving `cancelled = true` and
/// preserving partial content) when `cancel` is notified.
pub async fn consume_stream<F: FnMut(StreamPiece)>(
    mut resp: reqwest::Response,
    mut on_piece: F,
    cancel: &Notify,
) -> Result<FinishedTurn, String> {
    let mut parser = SseParser::new();
    let mut acc = StreamAccumulator::new();
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => {
                let mut turn = acc.finish();
                turn.cancelled = true;
                return Ok(turn);
            }
            chunk = resp.chunk() => {
                match chunk {
                    Ok(Some(bytes)) => {
                        for payload in parser.push(&bytes) {
                            for piece in acc.ingest(&payload) {
                                on_piece(piece);
                            }
                        }
                        if acc.is_done() {
                            return Ok(acc.finish());
                        }
                    }
                    Ok(None) => return Ok(acc.finish()),
                    Err(e) => return Err(format!("Stream read error: {e}")),
                }
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs raw SSE text through the parser + accumulator, returning the emitted
    /// pieces and the finished turn.
    fn drive(sse: &[u8]) -> (Vec<StreamPiece>, FinishedTurn) {
        let mut parser = SseParser::new();
        let mut acc = StreamAccumulator::new();
        let mut pieces = Vec::new();
        for payload in parser.push(sse) {
            pieces.extend(acc.ingest(&payload));
        }
        (pieces, acc.finish())
    }

    /// Just the visible-content strings from a piece stream.
    fn contents(pieces: &[StreamPiece]) -> Vec<String> {
        pieces
            .iter()
            .filter_map(|p| match p {
                StreamPiece::Content(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    /// Just the reasoning strings from a piece stream.
    fn reasonings(pieces: &[StreamPiece]) -> Vec<String> {
        pieces
            .iter()
            .filter_map(|p| match p {
                StreamPiece::Reasoning(r) => Some(r.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_plain_content_tokens_and_done() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\", world\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (pieces, turn) = drive(sse.as_bytes());
        assert_eq!(contents(&pieces), vec!["Hello", ", world"]);
        assert!(reasonings(&pieces).is_empty());
        assert_eq!(turn.content, "Hello, world");
        assert!(turn.reasoning.is_empty());
        assert_eq!(turn.finish_reason.as_deref(), Some("stop"));
        assert!(turn.tool_calls.is_empty());
        assert!(!turn.wants_tools());
    }

    #[test]
    fn reasoning_content_field_routes_to_reasoning() {
        // MiniMax / DeepSeek shape: a separate `reasoning_content` delta stream,
        // then the visible answer in `content`.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think.\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Answer.\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (pieces, turn) = drive(sse.as_bytes());
        assert_eq!(reasonings(&pieces), vec!["Let me ", "think."]);
        assert_eq!(contents(&pieces), vec!["Answer."]);
        assert_eq!(turn.reasoning, "Let me think.");
        assert_eq!(turn.content, "Answer.");
    }

    #[test]
    fn reasoning_field_openrouter_shape_routes_to_reasoning() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"hmm\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (pieces, turn) = drive(sse.as_bytes());
        assert_eq!(reasonings(&pieces), vec!["hmm"]);
        assert_eq!(turn.content, "Hi");
        assert_eq!(turn.reasoning, "hmm");
    }

    #[test]
    fn think_tags_inline_split_reasoning_from_content() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<think>reasoning here</think>visible\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (pieces, turn) = drive(sse.as_bytes());
        assert_eq!(reasonings(&pieces), vec!["reasoning here"]);
        assert_eq!(contents(&pieces), vec!["visible"]);
        assert_eq!(turn.reasoning, "reasoning here");
        assert_eq!(turn.content, "visible");
    }

    #[test]
    fn think_tags_split_across_chunk_boundaries() {
        // Open tag, close tag, AND content all split mid-token across chunks.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<thi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"nk>deep \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"thoughts</thi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"nk>the answer\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (pieces, turn) = drive(sse.as_bytes());
        assert_eq!(turn.reasoning, "deep thoughts");
        assert_eq!(turn.content, "the answer");
        // No partial tag ever leaked into either channel.
        assert!(!contents(&pieces).join("").contains('<'));
        assert!(!reasonings(&pieces).join("").contains('<'));
    }

    #[test]
    fn think_tag_only_no_content() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<think>just thinking</think>\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (_, turn) = drive(sse.as_bytes());
        assert_eq!(turn.reasoning, "just thinking");
        assert_eq!(turn.content, "");
    }

    #[test]
    fn think_tag_mid_content_is_literal_text() {
        // A `<think>` that appears after real content has begun is NOT a
        // reasoning delimiter — it stays as visible text.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"real text \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"<think>not reasoning</think>\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (_, turn) = drive(sse.as_bytes());
        assert!(turn.reasoning.is_empty());
        assert_eq!(turn.content, "real text <think>not reasoning</think>");
    }

    #[test]
    fn assembles_tool_call_arguments_split_across_chunks() {
        // id + name arrive first; arguments dribble in across several chunks.
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"search_notes\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"que\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ry\\\":\\\"cats\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (pieces, turn) = drive(sse.as_bytes());
        assert!(contents(&pieces).is_empty());
        assert!(turn.wants_tools());
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].id, "call_a");
        assert_eq!(turn.tool_calls[0].function.name, "search_notes");
        assert_eq!(turn.tool_calls[0].function.arguments, "{\"query\":\"cats\"}");
    }

    #[test]
    fn handles_multiple_parallel_tool_calls() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"read_note\",\"arguments\":\"{\\\"path\\\":\\\"a.md\\\"}\"}},{\"index\":1,\"id\":\"b\",\"function\":{\"name\":\"read_note\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"b.md\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (_, turn) = drive(sse.as_bytes());
        assert_eq!(turn.tool_calls.len(), 2);
        assert_eq!(turn.tool_calls[0].function.name, "read_note");
        assert_eq!(turn.tool_calls[0].function.arguments, "{\"path\":\"a.md\"}");
        assert_eq!(turn.tool_calls[1].id, "b");
        assert_eq!(turn.tool_calls[1].function.arguments, "{\"path\":\"b.md\"}");
    }

    #[test]
    fn parser_handles_bytes_split_mid_line() {
        // Feed the same event in two arbitrary byte splits.
        let mut parser = SseParser::new();
        let mut acc = StreamAccumulator::new();
        let full = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n";
        let (a, b) = full.as_bytes().split_at(20);
        let mut pieces = Vec::new();
        for p in parser.push(a) {
            pieces.extend(acc.ingest(&p));
        }
        for p in parser.push(b) {
            pieces.extend(acc.ingest(&p));
        }
        assert_eq!(pieces, vec![StreamPiece::Content("Hi".into())]);
    }

    #[test]
    fn ignores_comment_and_garbage_payloads() {
        let sse = concat!(
            ": keep-alive\n\n",
            "data: not-json\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let (pieces, turn) = drive(sse.as_bytes());
        assert_eq!(contents(&pieces), vec!["ok"]);
        assert_eq!(turn.content, "ok");
    }

    #[test]
    fn parse_model_ids_handles_data_wrapper_and_bare_array() {
        let wrapped = serde_json::json!({"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]});
        assert_eq!(
            parse_model_ids(&wrapped).unwrap(),
            vec!["gpt-4o", "gpt-4o-mini"]
        );
        let bare = serde_json::json!(["b-model", "a-model"]);
        assert_eq!(parse_model_ids(&bare).unwrap(), vec!["a-model", "b-model"]);
    }
}
