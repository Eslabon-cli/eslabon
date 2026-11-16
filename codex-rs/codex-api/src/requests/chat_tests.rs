use super::*;
use crate::common::ResponsesApiTools;
use codex_protocol::models::FunctionCallOutputPayload;
use pretty_assertions::assert_eq;
use serde_json::value::RawValue;
use std::sync::Arc;

fn msg(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![if role == "assistant" {
            ContentItem::OutputText {
                text: text.to_string(),
                annotations: None,
                logprobs: None,
            }
        } else {
            ContentItem::InputText {
                text: text.to_string(),
            }
        }],
        phase: None,
        encrypted_content: None,
        status: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn call(name: &str, namespace: Option<&str>, args: &str, call_id: &str) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: None,
        name: name.to_string(),
        namespace: namespace.map(str::to_string),
        arguments: args.to_string(),
        encrypted_function_args: None,
        encrypted_content: None,
        status: None,
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn output(call_id: &str, text: &str) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some(call_id.to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(text.to_string()),
            success: Some(true),
        },
        internal_chat_message_metadata_passthrough: None,
    }
}

fn reasoning(text: &str) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: text.to_string(),
        }]),
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn request(input: Vec<ResponseItem>, tools: Option<Value>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "qwen3.6-35b-heretic".to_string(),
        stream: true,
        service_tier: None,
        input,
        tools: tools.map(|value| {
            let raw: Arc<RawValue> =
                Arc::from(RawValue::from_string(value.to_string()).expect("raw tools"));
            ResponsesApiTools::from(raw)
        }),
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
    }
}

fn messages(req: &ChatRequest) -> &Vec<Value> {
    req.body["messages"].as_array().expect("messages")
}

#[test]
fn leading_developer_messages_fold_into_one_system_message() {
    let req = build_chat_request(&request(
        vec![
            msg("developer", "base instructions"),
            msg("developer", "permissions"),
            msg("user", "env context"),
            msg("user", "hello"),
        ],
        None,
    ))
    .expect("build");
    assert_eq!(
        messages(&req),
        &vec![
            json!({"role": "system", "content": "base instructions\n\npermissions"}),
            json!({"role": "user", "content": "env context\n\nhello"}),
        ]
    );
    assert_eq!(req.body["stream_options"], json!({"include_usage": true}));
    assert!(req.body.get("tools").is_none());
}

#[test]
fn late_developer_message_becomes_user_text() {
    let req = build_chat_request(&request(
        vec![
            msg("developer", "base"),
            msg("user", "hi"),
            msg("assistant", "hello"),
            msg("developer", "model switched"),
        ],
        None,
    ))
    .expect("build");
    let last = messages(&req).last().expect("last");
    assert_eq!(last["role"], "user");
    assert_eq!(
        last["content"],
        "<developer>\nmodel switched\n</developer>"
    );
}

#[test]
fn groups_tool_calls_and_attaches_reasoning_to_its_step() {
    let req = build_chat_request(&request(
        vec![
            msg("user", "old question"),
            reasoning("old thinking"),
            msg("assistant", "old answer"),
            msg("user", "read a and b"),
            reasoning("need both files"),
            call("read", None, r#"{"p":"a"}"#, "c1"),
            call("read", None, r#"{"p":"b"}"#, "c2"),
            output("c1", "A"),
            output("c2", "B"),
        ],
        None,
    ))
    .expect("build");
    let msgs = messages(&req);
    // user, assistant(old), user, assistant(tool_calls), tool, tool
    assert_eq!(msgs.len(), 6);
    assert_eq!(msgs[1]["reasoning_content"], "old thinking");
    assert_eq!(msgs[3]["reasoning_content"], "need both files");
    assert_eq!(msgs[3]["content"], Value::Null);
    let calls = msgs[3]["tool_calls"].as_array().expect("tool_calls");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1]["id"], "c2");
    assert_eq!(msgs[4], json!({"role": "tool", "tool_call_id": "c1", "content": "A"}));
    assert_eq!(msgs[5]["tool_call_id"], "c2");
}

#[test]
fn prefix_with_reasoning_is_stable_across_turns() {
    let turn1 = vec![
        msg("developer", "base"),
        msg("user", "q1"),
        reasoning("think 1"),
        call("read", None, "{}", "c1"),
        output("c1", "A"),
        reasoning("think 2"),
        msg("assistant", "answer 1"),
    ];
    let mut turn2 = turn1.clone();
    turn2.push(msg("user", "q2"));
    turn2.push(reasoning("think 3"));
    let first = build_chat_request(&request(turn1, None)).expect("build");
    let second = build_chat_request(&request(turn2, None)).expect("build");
    let first = messages(&first);
    let second = messages(&second);
    assert_eq!(&second[..first.len()], &first[..]);
}

#[test]
fn outputs_without_a_rendered_call_become_user_text() {
    let req = build_chat_request(&request(
        vec![
            msg("user", "go"),
            ResponseItem::FunctionCallOutput {
                id: None,
                call_id: Some("external-1".to_string()),
                name: Some("browser".to_string()),
                namespace: None,
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::Text("page loaded".to_string()),
                    success: Some(true),
                },
                internal_chat_message_metadata_passthrough: None,
            },
        ],
        None,
    ))
    .expect("build");
    let msgs = messages(&req);
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"], "go\n\n[browser output]\npage loaded");
    assert!(msgs.iter().all(|m| m["role"] != "tool"));
}

#[test]
fn unrepresentable_images_become_placeholders() {
    let req = build_chat_request(&request(
        vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputImage {
                image: ImageReference::File {
                    file_id: "file-1".to_string(),
                },
                detail: None,
            }],
            phase: None,
            encrypted_content: None,
            status: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        None,
    ))
    .expect("build");
    assert_eq!(
        messages(&req)[0]["content"],
        json!([{"type": "text", "text": "[image not representable]"}])
    );
}

#[test]
fn assistant_text_then_tool_call_share_one_message() {
    let req = build_chat_request(&request(
        vec![
            msg("user", "go"),
            msg("assistant", "Reading the file."),
            call("read", None, "{}", "c1"),
            output("c1", "data"),
        ],
        None,
    ))
    .expect("build");
    let msgs = messages(&req);
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["content"], "Reading the file.");
    assert_eq!(msgs[1]["tool_calls"][0]["id"], "c1");
}

#[test]
fn adapts_custom_and_namespaced_tools_and_drops_hosted_ones() {
    // The client-executed tool_search stays (as a plain function); hosted tools go.
    let tools = json!([
        {"type": "function", "name": "shell", "description": "run", "strict": true,
         "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}}}},
        {"type": "custom", "name": "apply_patch", "description": "edit files",
         "format": {"type": "grammar", "syntax": "lark", "definition": "start: PATCH"}},
        {"type": "namespace", "name": "mcp__linear", "description": "Linear",
         "tools": [{"type": "function", "name": "list_issues", "description": "list",
                    "strict": false, "parameters": {"type": "object", "properties": {}}}]},
        {"type": "web_search"},
        {"type": "tool_search", "execution": "client", "description": "x", "parameters": {}}
    ]);
    let req = build_chat_request(&request(vec![msg("user", "hi")], Some(tools))).expect("build");
    let specs = req.body["tools"].as_array().expect("tools");
    let names: Vec<&str> = specs
        .iter()
        .map(|t| t["function"]["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec!["shell", "apply_patch", "mcp__linear__list_issues", "tool_search"]
    );
    assert_eq!(req.tool_map["tool_search"].kind, ChatToolKind::ToolSearch);
    assert!(specs[0]["function"].get("strict").is_none());
    assert_eq!(specs[1]["function"]["parameters"]["required"], json!(["input"]));
    assert!(
        specs[1]["function"]["description"]
            .as_str()
            .expect("desc")
            .contains("start: PATCH")
    );
    assert_eq!(
        req.tool_map["apply_patch"],
        ChatToolTarget {
            name: "apply_patch".to_string(),
            namespace: None,
            kind: ChatToolKind::Custom,
        }
    );
    assert_eq!(
        req.tool_map["mcp__linear__list_issues"],
        ChatToolTarget {
            name: "list_issues".to_string(),
            namespace: Some("mcp__linear".to_string()),
            kind: ChatToolKind::Function,
        }
    );
}

#[test]
fn custom_tool_call_history_round_trips_as_input_argument() {
    let req = build_chat_request(&request(
        vec![
            msg("user", "patch it"),
            ResponseItem::CustomToolCall {
                id: None,
                status: None,
                call_id: "c1".to_string(),
                name: "apply_patch".to_string(),
                namespace: None,
                input: "*** Begin Patch".to_string(),
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
            },
        ],
        None,
    ))
    .expect("build");
    let args = messages(&req)[1]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .expect("args")
        .to_string();
    assert_eq!(
        serde_json::from_str::<Value>(&args).expect("json"),
        json!({"input": "*** Begin Patch"})
    );
}

#[test]
fn prefix_is_stable_when_the_conversation_grows() {
    let turn1 = vec![
        msg("developer", "base"),
        msg("user", "q1"),
        call("read", None, "{}", "c1"),
        output("c1", "A"),
        msg("assistant", "answer 1"),
    ];
    let mut turn2 = turn1.clone();
    turn2.push(msg("user", "q2"));
    let first = build_chat_request(&request(turn1, None)).expect("build");
    let second = build_chat_request(&request(turn2, None)).expect("build");
    let first = messages(&first);
    let second = messages(&second);
    assert_eq!(&second[..first.len()], &first[..]);
}

#[test]
fn long_tool_names_are_truncated_deterministically() {
    let long = "x".repeat(80);
    let a = chat_tool_name(Some("mcp__server"), &long);
    let b = chat_tool_name(Some("mcp__server"), &long);
    assert_eq!(a, b);
    assert_eq!(a.len(), MAX_TOOL_NAME_LEN);
    assert_eq!(chat_tool_name(Some("functions"), "shell"), "shell");
    assert_eq!(chat_tool_name(Some("mcp.fs"), "read"), "mcp_fs__read");
}

#[test]
fn reasoning_without_a_following_step_is_never_rendered() {
    // Turn N ends in reasoning (aborted); turn N+1 starts with a user message.
    let turn_n = vec![msg("user", "q1"), reasoning("half thought")];
    let mut turn_n1 = turn_n.clone();
    turn_n1.push(msg("user", "q2"));
    let first = build_chat_request(&request(turn_n, None)).expect("build");
    let second = build_chat_request(&request(turn_n1, None)).expect("build");
    assert_eq!(messages(&first), &vec![json!({"role": "user", "content": "q1"})]);
    assert_eq!(messages(&second), &vec![json!({"role": "user", "content": "q1\n\nq2"})]);
}

#[test]
fn echoes_provider_extra_content_on_tool_calls() {
    let signature = json!({"google": {"thought_signature": "sig-abc"}});
    remember_tool_call_extra("call-gemini-1", signature.clone());
    let req = build_chat_request(&request(
        vec![
            msg("user", "go"),
            call("shell", None, "{}", "call-gemini-1"),
            output("call-gemini-1", "ok"),
            call("shell", None, "{}", "call-other"),
            output("call-other", "ok"),
        ],
        None,
    ))
    .expect("build");
    let msgs = messages(&req);
    assert_eq!(msgs[1]["tool_calls"][0]["extra_content"], signature);
    // Calls without recorded data stay plain (llama.cpp/Novita never see the field).
    assert!(!msgs[1]["tool_calls"].as_array().expect("calls").is_empty());
    let other = msgs
        .iter()
        .flat_map(|m| m["tool_calls"].as_array().cloned().unwrap_or_default())
        .find(|c| c["id"] == "call-other")
        .expect("other call");
    assert!(other.get("extra_content").is_none());
}

#[test]
fn tool_search_round_trip_loads_deferred_tools_into_later_requests() {
    let tools = json!([
        {"type": "function", "name": "shell", "description": "run",
         "parameters": {"type": "object", "properties": {}}},
        {"type": "tool_search", "execution": "client", "description": "find tools",
         "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}}
    ]);
    let loaded = json!({"type": "namespace", "name": "mcp__chainaccounts", "description": "CA",
        "tools": [{"type": "function", "name": "aging_suplidores", "description": "AP aging",
                   "parameters": {"type": "object", "properties": {}}}]});
    let input = vec![
        msg("user", "balance de cuentas por pagar"),
        ResponseItem::ToolSearchCall {
            id: None,
            call_id: Some("ts-1".to_string()),
            status: None,
            execution: "client".to_string(),
            arguments: json!({"query": "cuentas por pagar"}),
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::ToolSearchOutput {
            id: None,
            call_id: Some("ts-1".to_string()),
            status: "completed".to_string(),
            execution: "client".to_string(),
            tools: vec![loaded],
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let req = build_chat_request(&request(input, Some(tools))).expect("build");

    let names: Vec<&str> = req.body["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|t| t["function"]["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec!["shell", "tool_search", "mcp__chainaccounts__aging_suplidores"]
    );
    assert_eq!(
        req.tool_map["mcp__chainaccounts__aging_suplidores"],
        ChatToolTarget {
            name: "aging_suplidores".to_string(),
            namespace: Some("mcp__chainaccounts".to_string()),
            kind: ChatToolKind::Function,
        }
    );

    let msgs = messages(&req);
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "tool_search");
    assert_eq!(
        msgs[1]["tool_calls"][0]["function"]["arguments"],
        json!({"query": "cuentas por pagar"}).to_string()
    );
    assert_eq!(msgs[2]["role"], "tool");
    assert_eq!(msgs[2]["tool_call_id"], "ts-1");
    assert_eq!(
        msgs[2]["content"],
        "Loaded tools, now callable: mcp__chainaccounts__aging_suplidores"
    );
}

#[test]
fn extra_body_merges_except_adapter_owned_keys() {
    let mut body = json!({"model": "bonsai", "messages": [], "stream": true,
                          "temperature": 0.7, "tools": []});
    let extra = json!({
        "model": "otro", "messages": "x", "stream": false, "stream_options": {}, "tools": "x",
        "temperature": 0.2,
        "chat_template_kwargs": {"enable_thinking": false, "nested": {"a": [1, 2]}}
    });
    merge_extra_body(&mut body, extra.as_object().expect("object"));
    assert_eq!(
        body,
        json!({"model": "bonsai", "messages": [], "stream": true, "tools": [],
               "temperature": 0.2,
               "chat_template_kwargs": {"enable_thinking": false, "nested": {"a": [1, 2]}}})
    );
}
