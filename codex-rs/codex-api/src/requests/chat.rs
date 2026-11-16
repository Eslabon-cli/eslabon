//! eslabon: translate a Responses API request into a Chat Completions request.
//!
//! The rest of Codex keeps speaking Responses items; this module is the adapter
//! seam for providers that only expose `/chat/completions`. Two rules shape it:
//!
//! * The output must be a pure function of the input history, so the message
//!   prefix stays byte-identical across turns and server-side prefix caches
//!   (llama.cpp slots, provider prompt caches) keep hitting.
//! * Tools that Chat Completions cannot express are adapted, not dropped:
//!   freeform (`custom`) tools become functions with a single `input` string,
//!   and namespaced tools are flattened. [`ChatToolMap`] records the mapping so
//!   the stream parser can rebuild the original call shape.

use crate::common::ResponsesApiRequest;
use crate::error::ApiError;
use codex_protocol::DEFAULT_FUNCTION_NAMESPACE;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

/// Chat Completions limits function names to 64 characters.
const MAX_TOOL_NAME_LEN: usize = 64;

/// Wire name of Codex's client-executed tool search.
pub(crate) const TOOL_SEARCH_WIRE_NAME: &str = "tool_search";

/// Provider-specific data attached to tool calls that must be echoed back verbatim,
/// keyed by call id. Gemini 3 (Vertex/AI Studio OpenAI-compatible APIs) returns
/// `extra_content.google.thought_signature` on each tool call and rejects the next
/// request with 400 if it is missing. Codex history has no field for it, so the
/// adapter keeps it here for the life of the process.
static TOOL_CALL_EXTRA: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Value>>> =
    std::sync::LazyLock::new(Default::default);
const TOOL_CALL_EXTRA_CAP: usize = 20_000;

pub(crate) fn remember_tool_call_extra(call_id: &str, extra: Value) {
    let mut map = TOOL_CALL_EXTRA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() >= TOOL_CALL_EXTRA_CAP {
        map.clear();
    }
    map.insert(call_id.to_string(), extra);
}

fn tool_call_extra(call_id: &str) -> Option<Value> {
    TOOL_CALL_EXTRA
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(call_id)
        .cloned()
}

fn with_extra(mut tool_call: Value, call_id: &str) -> Value {
    if let Some(extra) = tool_call_extra(call_id)
        && let Some(object) = tool_call.as_object_mut()
    {
        object.insert("extra_content".to_string(), extra);
    }
    tool_call
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatToolKind {
    Function,
    /// A freeform tool exposed as a function whose only argument is `input`.
    Custom,
    /// Codex's client-executed `tool_search`, exposed as a plain function.
    ToolSearch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatToolTarget {
    pub(crate) name: String,
    pub(crate) namespace: Option<String>,
    pub(crate) kind: ChatToolKind,
}

/// Wire function name -> original tool identity.
pub(crate) type ChatToolMap = HashMap<String, ChatToolTarget>;

pub(crate) struct ChatRequest {
    pub(crate) body: Value,
    pub(crate) tool_map: ChatToolMap,
}

pub(crate) fn build_chat_request(request: &ResponsesApiRequest) -> Result<ChatRequest, ApiError> {
    let mut tools = ChatTools::default();
    if let Some(raw) = &request.tools {
        let values: Vec<Value> = serde_json::from_str(raw.as_raw_value().get())
            .map_err(|err| ApiError::Stream(format!("failed to decode tools for chat: {err}")))?;
        for value in &values {
            tools.add_tool_value(value, None);
        }
    }
    for item in &request.input {
        if let ResponseItem::AdditionalTools { tools: extra, .. } = item {
            for value in extra.iter() {
                tools.add_tool_value(value, None);
            }
        }
    }
    // Deferred tools that `tool_search` loaded earlier in the conversation become
    // regular tools from then on, in history order so the list stays deterministic.
    for item in &request.input {
        if let ResponseItem::ToolSearchOutput { tools: loaded, .. } = item {
            for value in loaded.iter() {
                tools.add_tool_value(value, None);
            }
        }
    }

    let messages = build_messages(&request.input);

    let mut body = Map::new();
    body.insert("model".to_string(), json!(request.model));
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("stream".to_string(), json!(true));
    body.insert("stream_options".to_string(), json!({"include_usage": true}));
    if !tools.specs.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools.specs));
        body.insert("tool_choice".to_string(), json!("auto"));
        body.insert(
            "parallel_tool_calls".to_string(),
            json!(request.parallel_tool_calls),
        );
    }
    if let Some(effort) = request
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.effort.as_ref())
        .and_then(chat_reasoning_effort)
    {
        body.insert("reasoning_effort".to_string(), json!(effort));
    }

    Ok(ChatRequest {
        body: Value::Object(body),
        tool_map: tools.map,
    })
}

/// Provider header whose JSON object value is merged into the chat request body, for
/// server-specific fields (e.g. `chat_template_kwargs`). It never reaches the server.
pub(crate) const CHAT_EXTRA_BODY_HEADER: &str = "x-eslabon-chat-extra-body";

/// Body fields the adapter owns; an extra body cannot override them.
const PROTECTED_BODY_KEYS: [&str; 5] = ["model", "messages", "stream", "stream_options", "tools"];

/// Merges the provider's extra body into the request, overwriting existing keys
/// except the ones the adapter owns.
pub(crate) fn merge_extra_body(body: &mut Value, extra: &Map<String, Value>) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    for (key, value) in extra {
        if !PROTECTED_BODY_KEYS.contains(&key.as_str()) {
            object.insert(key.clone(), value.clone());
        }
    }
}

/// Only the three effort levels that the Chat Completions ecosystem broadly accepts are
/// forwarded; anything else is omitted rather than risking a 400 from a strict server.
fn chat_reasoning_effort(effort: &ReasoningEffort) -> Option<&'static str> {
    match effort {
        ReasoningEffort::Low => Some("low"),
        ReasoningEffort::Medium => Some("medium"),
        ReasoningEffort::High => Some("high"),
        _ => None,
    }
}

/// Deterministic wire name for a (possibly namespaced) tool.
pub(crate) fn chat_tool_name(namespace: Option<&str>, name: &str) -> String {
    let raw = match namespace {
        Some(namespace) if namespace != DEFAULT_FUNCTION_NAMESPACE && !namespace.is_empty() => {
            if name.starts_with(namespace) {
                name.to_string()
            } else {
                format!("{namespace}__{name}")
            }
        }
        _ => name.to_string(),
    };
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.len() <= MAX_TOOL_NAME_LEN {
        return sanitized;
    }
    // DefaultHasher::new() uses fixed keys, so this is stable across runs.
    let mut hasher = DefaultHasher::new();
    raw.hash(&mut hasher);
    let suffix = format!("{:08x}", hasher.finish() as u32);
    format!("{}_{suffix}", &sanitized[..MAX_TOOL_NAME_LEN - suffix.len() - 1])
}

#[derive(Default)]
struct ChatTools {
    specs: Vec<Value>,
    map: ChatToolMap,
    seen: HashSet<String>,
}

impl ChatTools {
    fn add_tool_value(&mut self, value: &Value, namespace: Option<&str>) {
        match value.get("type").and_then(Value::as_str) {
            Some("function") => {
                let Some(name) = value.get("name").and_then(Value::as_str) else {
                    return;
                };
                let description = value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let parameters = value
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                self.push(namespace, name, description.to_string(), parameters, ChatToolKind::Function);
            }
            Some("custom") => {
                let Some(name) = value.get("name").and_then(Value::as_str) else {
                    return;
                };
                let mut description = value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                description.push_str(
                    "\n\nPass the raw tool input, unmodified, as the `input` string argument.",
                );
                if let Some(format) = value.get("format")
                    && let Some(definition) = format.get("definition").and_then(Value::as_str)
                {
                    let syntax = format
                        .get("syntax")
                        .and_then(Value::as_str)
                        .unwrap_or("grammar");
                    description.push_str(&format!(
                        " The input must match this {syntax} grammar:\n{definition}"
                    ));
                }
                let parameters = json!({
                    "type": "object",
                    "properties": {
                        "input": {"type": "string", "description": "Raw tool input."}
                    },
                    "required": ["input"],
                });
                self.push(namespace, name, description, parameters, ChatToolKind::Custom);
            }
            Some("namespace") => {
                let Some(ns) = value.get("name").and_then(Value::as_str) else {
                    return;
                };
                if let Some(inner) = value.get("tools").and_then(Value::as_array) {
                    for tool in inner {
                        self.add_tool_value(tool, Some(ns));
                    }
                }
            }
            // Codex runs this search itself, so it works on any wire: the model sees a
            // plain function and the tools it loads are added to later requests.
            Some("tool_search")
                if value.get("execution").and_then(Value::as_str) == Some("client") =>
            {
                let description = value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let parameters = value
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                self.push(
                    None,
                    TOOL_SEARCH_WIRE_NAME,
                    description,
                    parameters,
                    ChatToolKind::ToolSearch,
                );
            }
            // Hosted tools (web_search, image_generation, server-side tool_search, ...)
            // have no Chat Completions equivalent.
            _ => {}
        }
    }

    fn push(
        &mut self,
        namespace: Option<&str>,
        name: &str,
        description: String,
        parameters: Value,
        kind: ChatToolKind,
    ) {
        let wire = chat_tool_name(namespace, name);
        if !self.seen.insert(wire.clone()) {
            if self.map.get(&wire).is_some_and(|existing| {
                existing.name != name || existing.namespace.as_deref() != namespace
            }) {
                tracing::warn!(
                    "chat tool name `{wire}` for {namespace:?}/{name} collides with another tool; dropping it"
                );
            }
            return;
        }
        self.specs.push(json!({
            "type": "function",
            "function": {
                "name": wire,
                "description": description,
                "parameters": parameters,
            }
        }));
        self.map.insert(
            wire,
            ChatToolTarget {
                name: name.to_string(),
                namespace: namespace
                    .filter(|ns| *ns != DEFAULT_FUNCTION_NAMESPACE)
                    .map(str::to_string),
                kind,
            },
        );
    }
}

fn build_messages(input: &[ResponseItem]) -> Vec<Value> {
    let mut out = MessageBuilder::default();

    // Leading system/developer messages become the single system message. Many
    // local chat templates reject a system message anywhere but first.
    let mut system_parts: Vec<String> = Vec::new();
    let mut start = 0;
    for item in input {
        match item {
            ResponseItem::Message { role, content, .. } if role == "system" || role == "developer" => {
                let text = content_text(content);
                if !text.is_empty() {
                    system_parts.push(text);
                }
                start += 1;
            }
            ResponseItem::AdditionalTools { .. } | ResponseItem::ConfigurationUpdate { .. } => {
                start += 1;
            }
            _ => break,
        }
    }
    if !system_parts.is_empty() {
        out.messages
            .push(json!({"role": "system", "content": system_parts.join("\n\n")}));
    }

    // Reasoning is always attached to the step that produced it, whatever turn it
    // belongs to. Dropping it once a new user message arrives would rewrite an
    // earlier assistant message and break the cached prefix; chat templates that
    // discard old reasoning still do so on the server side.
    let mut pending_reasoning: Option<String> = None;

    for item in &input[start..] {
        match item {
            ResponseItem::Message { role, content, .. } => match role.as_str() {
                "assistant" => {
                    let text = content_text(content);
                    let reasoning = pending_reasoning.take();
                    out.push_assistant_text(text, reasoning);
                }
                "user" => {
                    pending_reasoning = None;
                    out.push_user(user_content(content));
                }
                _ => {
                    // Late system/developer messages are delivered as user text so the
                    // conversation stays valid for templates that only allow one system
                    // message at the top.
                    let text = content_text(content);
                    if !text.is_empty() {
                        out.push_user(Value::String(format!("<{role}>\n{text}\n</{role}>")));
                    }
                }
            },
            ResponseItem::AgentMessage { author, content, .. } => {
                let text: Vec<&str> = content
                    .iter()
                    .filter_map(|c| match c {
                        AgentMessageInputContent::InputText { text } => Some(text.as_str()),
                        AgentMessageInputContent::EncryptedContent { .. } => None,
                    })
                    .collect();
                if !text.is_empty() {
                    out.push_user(Value::String(format!(
                        "[message from {author}]\n{}",
                        text.join("\n")
                    )));
                }
            }
            ResponseItem::Reasoning {
                summary, content, ..
            } => {
                let text = reasoning_text(summary, content.as_deref());
                if !text.trim().is_empty() {
                    match &mut pending_reasoning {
                        Some(existing) => {
                            existing.push('\n');
                            existing.push_str(&text);
                        }
                        None => pending_reasoning = Some(text),
                    }
                }
            }
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                let reasoning = pending_reasoning.take();
                out.push_tool_call(
                    with_extra(
                        json!({
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": chat_tool_name(namespace.as_deref(), name),
                                "arguments": arguments,
                            }
                        }),
                        call_id,
                    ),
                    reasoning,
                );
            }
            ResponseItem::CustomToolCall {
                name,
                namespace,
                input,
                call_id,
                ..
            } => {
                let reasoning = pending_reasoning.take();
                out.push_tool_call(
                    with_extra(
                        json!({
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": chat_tool_name(namespace.as_deref(), name),
                                "arguments": json!({"input": input}).to_string(),
                            }
                        }),
                        call_id,
                    ),
                    reasoning,
                );
            }
            ResponseItem::FunctionCallOutput {
                call_id, name, output, ..
            } => {
                out.push_tool_output(call_id.as_deref(), name.as_deref(), output);
            }
            ResponseItem::CustomToolCallOutput {
                call_id, name, output, ..
            } => {
                out.push_tool_output(Some(call_id), name.as_deref(), output);
            }
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                execution,
                arguments,
                ..
            } if execution == "client" => {
                let reasoning = pending_reasoning.take();
                out.push_tool_call(
                    with_extra(
                        json!({
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": TOOL_SEARCH_WIRE_NAME,
                                "arguments": arguments.to_string(),
                            }
                        }),
                        call_id,
                    ),
                    reasoning,
                );
            }
            ResponseItem::ToolSearchOutput {
                call_id,
                execution,
                tools,
                ..
            } if execution == "client" => {
                let names = loaded_tool_names(tools);
                let text = if names.is_empty() {
                    "No matching tools found.".to_string()
                } else {
                    format!("Loaded tools, now callable: {}", names.join(", "))
                };
                out.push_tool_output(
                    call_id.as_deref(),
                    Some(TOOL_SEARCH_WIRE_NAME),
                    &FunctionCallOutputPayload::from_text(text),
                );
            }
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::ConfigurationUpdate { .. }
            | ResponseItem::CompactionTrigger {}
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => {}
        }
    }
    out.flush_pending_images();
    out.messages
}

/// Wire names of the tools a `tool_search` result loaded, as the model will call them.
fn loaded_tool_names(tools: &[Value]) -> Vec<String> {
    let mut names = Vec::new();
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("namespace") => {
                let namespace = tool.get("name").and_then(Value::as_str);
                for inner in tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    if let Some(name) = inner
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                    {
                        names.push(chat_tool_name(namespace, name));
                    }
                }
            }
            _ => {
                if let Some(name) = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                {
                    names.push(chat_tool_name(None, name));
                }
            }
        }
    }
    names
}

#[derive(Default)]
struct MessageBuilder {
    messages: Vec<Value>,
    /// Call ids rendered so far; a `tool` message for any other id is rejected by
    /// strict servers (e.g. outputs of external tool events, or of skipped calls).
    emitted_call_ids: HashSet<String>,
    /// Images returned by tools. Chat `tool` messages are text-only on most servers,
    /// so they follow the tool batch as a user message.
    pending_images: Vec<Value>,
}

impl MessageBuilder {
    fn push_user(&mut self, content: Value) {
        self.flush_pending_images();
        if let Some(Value::Object(last)) = self.messages.last_mut()
            && last.get("role").and_then(Value::as_str) == Some("user")
            && let Some(existing) = last.get_mut("content")
        {
            // Merge consecutive user messages: several chat templates require
            // strict user/assistant alternation.
            match (existing, content) {
                (Value::String(a), Value::String(b)) => {
                    a.push_str("\n\n");
                    a.push_str(&b);
                }
                (existing, content) => {
                    let mut parts = into_parts(existing.take());
                    parts.extend(into_parts(content));
                    *existing = Value::Array(parts);
                }
            }
            return;
        }
        self.messages
            .push(json!({"role": "user", "content": content}));
    }

    fn push_assistant_text(&mut self, text: String, reasoning: Option<String>) {
        self.flush_pending_images();
        let mut msg = json!({"role": "assistant", "content": text});
        if let Some(reasoning) = reasoning
            && let Some(obj) = msg.as_object_mut()
        {
            obj.insert("reasoning_content".to_string(), json!(reasoning));
        }
        self.messages.push(msg);
    }

    /// Chat Completions requires the calls of one model step to live in a single
    /// assistant message (`tool_calls: [...]`), followed by the `tool` responses.
    fn push_tool_call(&mut self, tool_call: Value, reasoning: Option<String>) {
        self.flush_pending_images();
        if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
            self.emitted_call_ids.insert(id.to_string());
        }
        if let Some(Value::Object(last)) = self.messages.last_mut()
            && last.get("role").and_then(Value::as_str) == Some("assistant")
        {
            match last.get_mut("tool_calls").and_then(Value::as_array_mut) {
                Some(calls) => calls.push(tool_call),
                None => {
                    last.insert("tool_calls".to_string(), json!([tool_call]));
                }
            }
            if let Some(reasoning) = reasoning {
                match last.get_mut("reasoning_content") {
                    Some(Value::String(existing)) => {
                        existing.push('\n');
                        existing.push_str(&reasoning);
                    }
                    _ => {
                        last.insert("reasoning_content".to_string(), json!(reasoning));
                    }
                }
            }
            return;
        }
        let mut msg = json!({"role": "assistant", "content": null, "tool_calls": [tool_call]});
        if let Some(reasoning) = reasoning
            && let Some(obj) = msg.as_object_mut()
        {
            obj.insert("reasoning_content".to_string(), json!(reasoning));
        }
        self.messages.push(msg);
    }

    fn push_tool_output(
        &mut self,
        call_id: Option<&str>,
        name: Option<&str>,
        output: &FunctionCallOutputPayload,
    ) {
        let call_id = call_id
            .filter(|id| !id.is_empty() && self.emitted_call_ids.contains(*id))
            .map(str::to_string);
        let content = match &output.body {
            FunctionCallOutputBody::Text(text) => text.clone(),
            FunctionCallOutputBody::ContentItems(items) => {
                let mut texts = Vec::new();
                for item in items {
                    match item {
                        FunctionCallOutputContentItem::InputText { text } => {
                            texts.push(text.clone())
                        }
                        FunctionCallOutputContentItem::InputImage { image, .. } => {
                            if let Some(part) = image_part(image) {
                                self.pending_images.push(part);
                                texts.push("[image attached below]".to_string());
                            }
                        }
                        FunctionCallOutputContentItem::InputAudio { .. }
                        | FunctionCallOutputContentItem::EncryptedContent { .. } => {}
                    }
                }
                texts.join("\n")
            }
        };
        match call_id {
            Some(call_id) => self.messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content,
            })),
            None => {
                let label = name.unwrap_or("tool");
                self.push_user(Value::String(format!("[{label} output]\n{content}")));
            }
        }
    }

    fn flush_pending_images(&mut self) {
        if self.pending_images.is_empty() {
            return;
        }
        // Only flush once the tool batch is over; tool messages must directly
        // follow their assistant message.
        if self
            .messages
            .last()
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            != Some("tool")
        {
            return;
        }
        let mut parts = vec![json!({"type": "text", "text": "Images returned by the tool calls above:"})];
        parts.append(&mut self.pending_images);
        self.messages
            .push(json!({"role": "user", "content": parts}));
    }
}

fn into_parts(content: Value) -> Vec<Value> {
    match content {
        Value::Array(parts) => parts,
        Value::String(text) => vec![json!({"type": "text", "text": text})],
        Value::Null => Vec::new(),
        other => vec![json!({"type": "text", "text": other.to_string()})],
    }
}

fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn user_content(content: &[ContentItem]) -> Value {
    let has_image = content
        .iter()
        .any(|c| matches!(c, ContentItem::InputImage { .. }));
    if !has_image {
        return Value::String(content_text(content));
    }
    let parts = content
        .iter()
        .map(|c| match c {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                json!({"type": "text", "text": text})
            }
            ContentItem::InputImage { image, .. } => image_part(image)
                .unwrap_or_else(|| json!({"type": "text", "text": "[image not representable]"})),
            ContentItem::InputAudio { .. } => {
                json!({"type": "text", "text": "[audio not representable]"})
            }
        })
        .collect();
    Value::Array(parts)
}

fn image_part(image: &ImageReference) -> Option<Value> {
    match image {
        ImageReference::Inline { image_url } => {
            Some(json!({"type": "image_url", "image_url": {"url": image_url}}))
        }
        ImageReference::File { .. } => None,
    }
}

fn reasoning_text(
    summary: &[codex_protocol::models::ReasoningItemReasoningSummary],
    content: Option<&[ReasoningItemContent]>,
) -> String {
    let from_content: String = content
        .unwrap_or_default()
        .iter()
        .map(|c| match c {
            ReasoningItemContent::ReasoningText { text } | ReasoningItemContent::Text { text } => {
                text.as_str()
            }
        })
        .collect();
    if !from_content.trim().is_empty() {
        return from_content;
    }
    summary
        .iter()
        .map(|s| match s {
            codex_protocol::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                text.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
