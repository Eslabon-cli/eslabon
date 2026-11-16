use super::*;
use crate::requests::chat::ChatToolTarget;
use assert_matches::assert_matches;
use futures::TryStreamExt;
use serde_json::json;
use tokio_util::io::ReaderStream;

fn body(events: &[Value]) -> String {
    let mut out = String::new();
    for event in events {
        out.push_str(&format!("data: {event}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

async fn collect(body: String, tool_map: ChatToolMap) -> Vec<ResponseEvent> {
    let reader = ReaderStream::new(std::io::Cursor::new(body))
        .map_err(|err| TransportError::Network(err.to_string()));
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
    tokio::spawn(process_chat_sse(
        reader,
        tx,
        Duration::from_secs(1),
        None,
        tool_map,
    ));
    let mut out = Vec::new();
    while let Some(event) = rx.recv().await {
        out.push(event.expect("stream error"));
    }
    out
}

#[tokio::test]
async fn text_reasoning_and_usage_with_llama_cache_hits() {
    let events = collect(
        body(&[
            json!({"id": "chatcmpl-1", "choices": [{"delta": {"reasoning_content": "thinking"}}]}),
            json!({"choices": [{"delta": {"content": "Hel"}}]}),
            json!({"choices": [{"delta": {"content": "lo"}, "finish_reason": "stop"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 1000, "completion_tokens": 20,
                   "total_tokens": 1020}, "timings": {"cache_n": 900, "prompt_n": 100}}),
        ]),
        ChatToolMap::new(),
    )
    .await;
    assert_matches!(
        &events[..],
        [
            ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. }),
            ResponseEvent::ReasoningContentDelta { .. },
            ResponseEvent::OutputItemAdded(ResponseItem::Message { .. }),
            ResponseEvent::OutputTextDelta(_),
            ResponseEvent::OutputTextDelta(_),
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning { .. }),
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }),
            ResponseEvent::Completed { response_id, token_usage: Some(usage), end_turn: Some(true), .. },
        ] if response_id == "chatcmpl-1"
            && usage.input_tokens == 1000
            && usage.cached_input_tokens == 900
            && usage.output_tokens == 20
            && matches!(&content[..], [ContentItem::OutputText { text, .. }] if text == "Hello")
    );
}

#[tokio::test]
async fn tool_calls_are_emitted_even_when_finish_reason_is_stop() {
    let events = collect(
        body(&[
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call_a", "function": {"name": "shell", "arguments": "{\"cmd\":"}}
            ]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "\"ls\"}"}}
            ]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        ]),
        ChatToolMap::new(),
    )
    .await;
    assert_matches!(
        &events[..],
        [
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name, arguments, call_id, .. }),
            ResponseEvent::Completed { end_turn: None, .. },
        ] if name == "shell" && arguments == "{\"cmd\":\"ls\"}" && call_id == "call_a"
    );
}

#[tokio::test]
async fn maps_flattened_and_custom_tools_back() {
    let mut map = ChatToolMap::new();
    map.insert(
        "apply_patch".to_string(),
        ChatToolTarget {
            name: "apply_patch".to_string(),
            namespace: None,
            kind: ChatToolKind::Custom,
        },
    );
    map.insert(
        "mcp__linear__list_issues".to_string(),
        ChatToolTarget {
            name: "list_issues".to_string(),
            namespace: Some("mcp__linear".to_string()),
            kind: ChatToolKind::Function,
        },
    );
    let events = collect(
        body(&[
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "apply_patch",
                 "arguments": "{\"input\":\"*** Begin Patch\"}"}},
                {"index": 1, "id": "c2", "function": {"name": "mcp__linear__list_issues",
                 "arguments": "{}"}}
            ]}, "finish_reason": "tool_calls"}]}),
        ]),
        map,
    )
    .await;
    assert_matches!(
        &events[..],
        [
            ResponseEvent::OutputItemDone(ResponseItem::CustomToolCall { name, input, .. }),
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name: fname, namespace: Some(ns), .. }),
            ResponseEvent::Completed { .. },
        ] if name == "apply_patch" && input == "*** Begin Patch"
            && fname == "list_issues" && ns == "mcp__linear"
    );
}

#[tokio::test]
async fn error_chunk_becomes_stream_error() {
    let reader = ReaderStream::new(std::io::Cursor::new(
        "data: {\"error\":{\"message\":\"model not found\"}}\n\n".to_string(),
    ))
    .map_err(|err| TransportError::Network(err.to_string()));
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    tokio::spawn(process_chat_sse(
        reader,
        tx,
        Duration::from_secs(1),
        None,
        ChatToolMap::new(),
    ));
    assert_matches!(rx.recv().await, Some(Err(ApiError::Stream(msg))) if msg == "model not found");
}

#[tokio::test]
async fn deepseek_cache_hit_field_is_read() {
    let events = collect(
        body(&[
            json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 500, "completion_tokens": 1,
                   "prompt_cache_hit_tokens": 450}}),
        ]),
        ChatToolMap::new(),
    )
    .await;
    assert_matches!(
        events.last(),
        Some(ResponseEvent::Completed { token_usage: Some(usage), .. })
            if usage.cached_input_tokens == 450 && usage.total_tokens == 501
    );
}

async fn collect_results(body: String) -> Vec<Result<ResponseEvent, ApiError>> {
    let reader = ReaderStream::new(std::io::Cursor::new(body))
        .map_err(|err| TransportError::Network(err.to_string()));
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(64);
    tokio::spawn(process_chat_sse(
        reader,
        tx,
        Duration::from_secs(1),
        None,
        ChatToolMap::new(),
    ));
    let mut out = Vec::new();
    while let Some(event) = rx.recv().await {
        out.push(event);
    }
    out
}

#[tokio::test]
async fn cut_stream_without_finish_reason_is_an_error() {
    let events = collect_results(format!(
        "data: {}\n\n",
        json!({"choices": [{"delta": {"content": "partial"}}]})
    ))
    .await;
    assert_matches!(events.last(), Some(Err(ApiError::Stream(_))));
    assert!(!events.iter().any(|e| matches!(e, Ok(ResponseEvent::Completed { .. }))));
}

#[tokio::test]
async fn clean_close_after_finish_reason_completes() {
    let events = collect_results(format!(
        "data: {}\n\n",
        json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]})
    ))
    .await;
    assert_matches!(events.last(), Some(Ok(ResponseEvent::Completed { .. })));
}

#[tokio::test]
async fn length_and_content_filter_are_errors() {
    let length = collect_results(body(&[
        json!({"choices": [{"delta": {"content": "trunc"}, "finish_reason": "length"}]}),
    ]))
    .await;
    assert_matches!(length.last(), Some(Err(ApiError::Stream(msg))) if msg.contains("max_output_tokens"));
    let filtered = collect_results(body(&[
        json!({"choices": [{"delta": {}, "finish_reason": "content_filter"}]}),
    ]))
    .await;
    assert_matches!(filtered.last(), Some(Err(ApiError::ContentFilter)));
}

#[tokio::test]
async fn float_usage_counts_are_accepted() {
    let events = collect(
        body(&[
            json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 1000.0, "completion_tokens": 20.0,
                   "total_tokens": 1020.0}}),
        ]),
        ChatToolMap::new(),
    )
    .await;
    assert_matches!(
        events.last(),
        Some(ResponseEvent::Completed { token_usage: Some(usage), .. })
            if usage.input_tokens == 1000 && usage.total_tokens == 1020
    );
}

#[tokio::test]
async fn stops_reading_when_the_receiver_is_dropped() {
    // An endless stream: the parser must return once nobody listens.
    let chunk = format!(
        "data: {}\n\n",
        json!({"choices": [{"delta": {"content": "x"}}]})
    );
    let endless = futures::stream::repeat_with(move || {
        Ok::<_, TransportError>(bytes::Bytes::from(chunk.clone()))
    });
    let (tx, rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1);
    drop(rx);
    let handle = tokio::spawn(process_chat_sse(
        Box::pin(endless),
        tx,
        Duration::from_secs(1),
        None,
        ChatToolMap::new(),
    ));
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("parser kept reading after the receiver was dropped")
        .expect("task");
}

#[tokio::test]
async fn done_without_finish_reason_completes() {
    let events = collect_results(body(&[json!({"choices": [{"delta": {"content": "ok"}}]})])).await;
    assert_matches!(events.last(), Some(Ok(ResponseEvent::Completed { .. })));
}

#[tokio::test]
async fn remembers_gemini_thought_signature() {
    let _ = collect(
        body(&[json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call-sig", "function": {"name": "shell", "arguments": "{}"},
             "extra_content": {"google": {"thought_signature": "sig-xyz"}}}
        ]}, "finish_reason": "tool_calls"}]})]),
        ChatToolMap::new(),
    )
    .await;
    let req = crate::requests::chat::build_chat_request(&crate::common::ResponsesApiRequest {
        model: "m".to_string(),
        stream: true,
        service_tier: None,
        input: vec![ResponseItem::FunctionCall {
            id: None,
            name: "shell".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            encrypted_function_args: None,
            encrypted_content: None,
            status: None,
            call_id: "call-sig".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }],
        tools: None,
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream_options: None,
        include: Vec::new(),
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
        access_programs: None,
    })
    .expect("build");
    assert_eq!(
        req.body["messages"][0]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "sig-xyz"
    );
}

#[tokio::test]
async fn tool_search_call_becomes_a_client_tool_search() {
    let mut map = ChatToolMap::new();
    map.insert(
        "tool_search".to_string(),
        ChatToolTarget {
            name: "tool_search".to_string(),
            namespace: None,
            kind: ChatToolKind::ToolSearch,
        },
    );
    let events = collect(
        body(&[json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "ts1", "function": {"name": "tool_search",
             "arguments": "{\"query\":\"cuentas por pagar\"}"}}
        ]}, "finish_reason": "tool_calls"}]})]),
        map,
    )
    .await;
    assert_matches!(
        &events[..],
        [
            ResponseEvent::OutputItemDone(ResponseItem::ToolSearchCall { call_id, execution, arguments, .. }),
            ResponseEvent::Completed { .. },
        ] if call_id.as_deref() == Some("ts1")
            && execution == "client"
            && arguments == &json!({"query": "cuentas por pagar"})
    );
}
