//! eslabon: parse a Chat Completions SSE stream into Responses events.
//!
//! Differences from the parser Codex removed in #10157:
//! * `Completed` waits for the trailing usage chunk (`stream_options.include_usage`)
//!   so token counts and cache hits reach the UI.
//! * `reasoning_content` (llama.cpp, DeepSeek, GLM, K2) is read alongside `reasoning`.
//! * Tool calls are emitted whatever the finish reason; several servers report
//!   `stop` even when the step ended in tool calls.
//! * Flattened / freeform tools are mapped back through [`ChatToolMap`].

use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::requests::chat::ChatToolKind;
use crate::requests::chat::ChatToolMap;
use crate::telemetry::SseTelemetry;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::Stream;
use futures::StreamExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;

const REQUEST_ID_HEADER: &str = "x-request-id";

pub(crate) fn spawn_chat_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    tool_map: ChatToolMap,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        process_chat_sse(
            stream_response.bytes,
            tx_event,
            idle_timeout,
            telemetry,
            tool_map,
        )
        .await;
    });
    ResponseStream {
        routing_affinity: None,
        rx_event,
        upstream_request_id,
        interrupt: None,
    }
}

#[derive(Default, Debug)]
struct ToolCallState {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    /// Opaque provider data to echo back (Gemini's thought signature).
    extra_content: Option<Value>,
}

#[derive(Default)]
struct ChatStreamState {
    response_id: Option<String>,
    reasoning: Option<String>,
    text: Option<String>,
    /// Keyed by the server's `index`; BTreeMap keeps emission order stable.
    tool_calls: BTreeMap<usize, ToolCallState>,
    last_tool_index: Option<usize>,
    finish_reason: Option<String>,
    usage: Option<TokenUsage>,
    /// llama.cpp reports prompt cache hits in `timings.cache_n`.
    llama_cache_n: Option<i64>,
}

type Tx = mpsc::Sender<Result<ResponseEvent, ApiError>>;

pub(crate) async fn process_chat_sse<S>(
    stream: S,
    tx_event: Tx,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    tool_map: ChatToolMap,
) where
    S: Stream<Item = Result<bytes::Bytes, TransportError>> + Unpin,
{
    let mut stream = stream.eventsource();
    let mut state = ChatStreamState::default();
    // Every send checks for a closed receiver: when the core drops the stream
    // (Esc, turn end) we stop reading, which drops the HTTP body and lets the
    // server (e.g. a llama.cpp slot) stop generating.

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                // A clean close without `[DONE]` is only trusted when the server
                // already reported why it stopped; otherwise the body was cut.
                if state.finish_reason.is_some() {
                    finish(&tx_event, state, &tool_map).await;
                } else {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(
                            "chat stream closed before completion".into(),
                        )))
                        .await;
                }
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        trace!("chat SSE event: {}", sse.data);
        let data = sse.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" || data == "DONE" {
            finish(&tx_event, state, &tool_map).await;
            return;
        }

        let chunk: Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(err) => {
                debug!("failed to parse chat SSE chunk: {err}, data: {data}");
                continue;
            }
        };

        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| error.to_string());
            let _ = tx_event.send(Err(ApiError::Stream(message))).await;
            return;
        }

        if state.response_id.is_none()
            && let Some(id) = chunk.get("id").and_then(Value::as_str)
        {
            state.response_id = Some(id.to_string());
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            state.usage = Some(parse_usage(usage));
        }
        if let Some(cache_n) = chunk
            .get("timings")
            .and_then(|t| t.get("cache_n"))
            .and_then(Value::as_i64)
        {
            state.llama_cache_n = Some(cache_n);
        }

        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            continue;
        };
        // Codex never requests n > 1; only the first choice is meaningful.
        let Some(choice) = choices.first() else {
            continue;
        };
        if let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) {
            if let Some(text) = reasoning_delta(delta)
                && !append_reasoning(&tx_event, &mut state, text).await
            {
                return;
            }
            let texts: Vec<String> = match delta.get("content") {
                Some(Value::String(text)) if !text.is_empty() => vec![text.clone()],
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            for text in texts {
                if !append_text(&tx_event, &mut state, text).await {
                    return;
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    accumulate_tool_call(&mut state, call);
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            state.finish_reason = Some(reason.to_string());
        }
    }
}

fn reasoning_delta(delta: &Value) -> Option<String> {
    for key in ["reasoning_content", "reasoning"] {
        match delta.get(key) {
            Some(Value::String(text)) if !text.is_empty() => return Some(text.clone()),
            Some(Value::Object(obj)) => {
                if let Some(text) = obj
                    .get("text")
                    .or_else(|| obj.get("content"))
                    .and_then(Value::as_str)
                    && !text.is_empty()
                {
                    return Some(text.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn accumulate_tool_call(state: &mut ChatStreamState, call: &Value) {
    let id = call.get("id").and_then(Value::as_str).filter(|s| !s.is_empty());
    let index = call
        .get("index")
        .and_then(Value::as_u64)
        .map(|i| i as usize)
        .or_else(|| {
            // Some servers omit `index`; fall back to matching by id, then to the
            // call being streamed, then to a fresh slot.
            id.and_then(|id| {
                state
                    .tool_calls
                    .iter()
                    .find(|(_, s)| s.id.as_deref() == Some(id))
                    .map(|(i, _)| *i)
            })
            .or(if id.is_none() { state.last_tool_index } else { None })
        })
        .unwrap_or_else(|| {
            state
                .tool_calls
                .keys()
                .next_back()
                .map_or(0, |last| last + 1)
        });
    let slot = state.tool_calls.entry(index).or_default();
    if let Some(id) = id {
        slot.id.get_or_insert_with(|| id.to_string());
    }
    if let Some(extra) = call.get("extra_content").filter(|extra| !extra.is_null()) {
        slot.extra_content = Some(extra.clone());
    }
    if let Some(function) = call.get("function") {
        if let Some(name) = function.get("name").and_then(Value::as_str)
            && !name.is_empty()
        {
            slot.name.get_or_insert_with(|| name.to_string());
        }
        match function.get("arguments") {
            Some(Value::String(args)) => slot.arguments.push_str(args),
            // A few servers send already-parsed JSON arguments.
            Some(other @ Value::Object(_)) => slot.arguments.push_str(&other.to_string()),
            _ => {}
        }
    }
    state.last_tool_index = Some(index);
}

/// Returns `false` once the receiver is gone.
async fn send(tx: &Tx, event: ResponseEvent) -> bool {
    tx.send(Ok(event)).await.is_ok()
}

async fn append_text(tx: &Tx, state: &mut ChatStreamState, text: String) -> bool {
    if state.text.is_none() {
        state.text = Some(String::new());
        if !send(tx, ResponseEvent::OutputItemAdded(assistant_message(String::new()))).await {
            return false;
        }
    }
    if let Some(buffer) = state.text.as_mut() {
        buffer.push_str(&text);
    }
    send(tx, ResponseEvent::OutputTextDelta(text)).await
}

async fn append_reasoning(tx: &Tx, state: &mut ChatStreamState, text: String) -> bool {
    if state.reasoning.is_none() {
        state.reasoning = Some(String::new());
        if !send(tx, ResponseEvent::OutputItemAdded(reasoning_item(String::new()))).await {
            return false;
        }
    }
    if let Some(buffer) = state.reasoning.as_mut() {
        buffer.push_str(&text);
    }
    send(
        tx,
        ResponseEvent::ReasoningContentDelta {
            delta: text,
            content_index: 0,
        },
    )
    .await
}

async fn finish(tx: &Tx, state: ChatStreamState, tool_map: &ChatToolMap) {
    let ChatStreamState {
        response_id,
        reasoning,
        text,
        tool_calls,
        finish_reason,
        usage,
        llama_cache_n,
        ..
    } = state;

    // Mirror how the Responses parser treats `response.incomplete`.
    match finish_reason.as_deref() {
        Some("content_filter") => {
            let _ = tx.send(Err(ApiError::ContentFilter)).await;
            return;
        }
        Some("length") => {
            let _ = tx
                .send(Err(ApiError::Stream(
                    "Incomplete response returned, reason: max_output_tokens".into(),
                )))
                .await;
            return;
        }
        _ => {}
    }

    if let Some(reasoning) = reasoning
        && !send(tx, ResponseEvent::OutputItemDone(reasoning_item(reasoning))).await
    {
        return;
    }
    if let Some(text) = text
        && !send(tx, ResponseEvent::OutputItemDone(assistant_message(text))).await
    {
        return;
    }
    let mut emitted_tool_call = false;
    for (index, call) in tool_calls {
        let Some(name) = call.name else {
            debug!("skipping chat tool call at index {index}: missing name");
            continue;
        };
        let call_id = call.id.unwrap_or_else(|| format!("call_{index}"));
        if let Some(extra) = call.extra_content {
            crate::requests::chat::remember_tool_call_extra(&call_id, extra);
        }
        let item = tool_call_item(name, call.arguments, call_id, tool_map);
        if !send(tx, ResponseEvent::OutputItemDone(item)).await {
            return;
        }
        emitted_tool_call = true;
    }

    let token_usage = usage.map(|mut usage| {
        if usage.cached_input_tokens == 0
            && let Some(cache_n) = llama_cache_n
        {
            usage.cached_input_tokens = cache_n;
        }
        usage
    });
    let end_turn = (!emitted_tool_call && finish_reason.as_deref() == Some("stop")).then_some(true);
    let _ = tx
        .send(Ok(ResponseEvent::Completed {
            response_id: response_id.unwrap_or_default(),
            token_usage,
            usage_metadata: None,
            end_turn,
        }))
        .await;
}

fn tool_call_item(
    wire_name: String,
    arguments: String,
    call_id: String,
    tool_map: &ChatToolMap,
) -> ResponseItem {
    match tool_map.get(&wire_name) {
        Some(target) if target.kind == ChatToolKind::ToolSearch => ResponseItem::ToolSearchCall {
            id: None,
            call_id: Some(call_id),
            status: None,
            execution: "client".to_string(),
            encrypted_content: None,
            // Unparseable arguments still reach the handler, which reports the error.
            arguments: serde_json::from_str::<Value>(&arguments)
                .unwrap_or(Value::String(arguments)),
            internal_chat_message_metadata_passthrough: None,
        },
        Some(target) if target.kind == ChatToolKind::Custom => {
            let input = serde_json::from_str::<Value>(&arguments)
                .ok()
                .and_then(|v| v.get("input").and_then(Value::as_str).map(str::to_string))
                .unwrap_or(arguments);
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id,
                name: target.name.clone(),
                namespace: target.namespace.clone(),
                input,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
            }
        }
        Some(target) => ResponseItem::FunctionCall {
            id: None,
            name: target.name.clone(),
            namespace: target.namespace.clone(),
            arguments,
            encrypted_function_args: None,
            encrypted_content: None,
            status: None,
            call_id,
            internal_chat_message_metadata_passthrough: None,
        },
        // Unknown tool: pass it through so core reports the error back to the model.
        None => ResponseItem::FunctionCall {
            id: None,
            name: wire_name,
            namespace: None,
            arguments,
            encrypted_function_args: None,
            encrypted_content: None,
            status: None,
            call_id,
            internal_chat_message_metadata_passthrough: None,
        },
    }
}

fn assistant_message(text: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentItem::OutputText {
                text,
                annotations: None,
                logprobs: None,
            }]
        },
        phase: None,
        encrypted_content: None,
        status: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn reasoning_item(text: String) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: Some(if text.is_empty() {
            Vec::new()
        } else {
            vec![ReasoningItemContent::ReasoningText { text }]
        }),
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn parse_usage(usage: &Value) -> TokenUsage {
    // Some Python servers serialize counts as floats (`1020.0`).
    let num = |v: &Value| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64));
    let int = |v: Option<&Value>| v.and_then(num).unwrap_or(0);
    let input_tokens = int(usage.get("prompt_tokens"));
    let output_tokens = int(usage.get("completion_tokens"));
    let cached_input_tokens = usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(num)
        // DeepSeek reports cache hits separately.
        .or_else(|| usage.get("prompt_cache_hit_tokens").and_then(num))
        .unwrap_or(0);
    let reasoning_output_tokens = int(
        usage
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens")),
    );
    let total_tokens = usage
        .get("total_tokens")
        .and_then(num)
        .unwrap_or(input_tokens + output_tokens);
    TokenUsage {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_output_tokens,
        total_tokens,
        ..TokenUsage::default()
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
