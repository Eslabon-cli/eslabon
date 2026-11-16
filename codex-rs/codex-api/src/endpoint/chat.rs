//! eslabon: Chat Completions client with the same contract as [`ResponsesClient`].
//!
//! [`ResponsesClient`]: crate::endpoint::responses::ResponsesClient

use crate::auth::SharedAuthProvider;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::responses::ResponsesOptions;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::chat::CHAT_EXTRA_BODY_HEADER;
use crate::requests::chat::build_chat_request;
use crate::requests::chat::merge_extra_body;
use crate::requests::headers::build_session_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::sse::chat::spawn_chat_stream;
use crate::telemetry::SseTelemetry;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde_json::Map;
use serde_json::Value;
use std::sync::Arc;
use tracing::instrument;

pub struct ChatClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
    /// Fields merged into every request body (see [`CHAT_EXTRA_BODY_HEADER`]).
    extra_body: Option<Map<String, Value>>,
}

impl<T: HttpTransport> ChatClient<T> {
    pub fn new(transport: T, mut provider: Provider, auth: SharedAuthProvider) -> Self {
        let extra_body = take_extra_body(&mut provider.headers);
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
            extra_body,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
            extra_body: self.extra_body,
        }
    }

    /// Streams a turn built for the Responses API through `/chat/completions`.
    ///
    /// Compression and turn-state options are ignored: Chat Completions servers
    /// rarely accept zstd bodies and have no turn-state header.
    #[instrument(
        name = "chat.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "chat_http",
            http.method = "POST",
            api.path = "/chat/completions"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ResponsesOptions {
            session_id,
            thread_id,
            session_source,
            extra_headers,
            ..
        } = options;
        let mut chat = build_chat_request(&request)?;
        if let Some(extra) = &self.extra_body {
            merge_extra_body(&mut chat.body, extra);
        }
        let body = EncodedJsonBody::encode(&chat.body)
            .map_err(|e| ApiError::Stream(format!("failed to encode chat request: {e}")))?;

        let mut headers = extra_headers;
        if let Some(ref thread_id) = thread_id {
            insert_header(&mut headers, "x-client-request-id", thread_id);
        }
        headers.extend(build_session_headers(session_id, thread_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        let stream_response = self
            .session
            .stream_encoded_json_with(
                Method::POST,
                "/chat/completions",
                headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                },
            )
            .await?;

        Ok(spawn_chat_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            chat.tool_map,
        ))
    }
}

/// Removes the extra-body header so it never reaches the server, and parses it.
/// A value that is not a JSON object is ignored with a warning.
fn take_extra_body(headers: &mut HeaderMap) -> Option<Map<String, Value>> {
    let value = headers.remove(CHAT_EXTRA_BODY_HEADER)?;
    match value
        .to_str()
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
    {
        Some(Value::Object(object)) => Some(object),
        _ => {
            tracing::warn!("ignoring `{CHAT_EXTRA_BODY_HEADER}`: its value is not a JSON object");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn extra_body_header_is_removed_and_parsed() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CHAT_EXTRA_BODY_HEADER,
            HeaderValue::from_static(r#"{"chat_template_kwargs":{"enable_thinking":false}}"#),
        );
        headers.insert("x-other", HeaderValue::from_static("kept"));
        let extra = take_extra_body(&mut headers).expect("object");
        assert_eq!(
            Value::Object(extra),
            json!({"chat_template_kwargs": {"enable_thinking": false}})
        );
        assert!(headers.get(CHAT_EXTRA_BODY_HEADER).is_none());
        assert!(headers.get("x-other").is_some());

        let mut bad = HeaderMap::new();
        bad.insert(CHAT_EXTRA_BODY_HEADER, HeaderValue::from_static("[1,2]"));
        assert_eq!(take_extra_body(&mut bad), None);
        assert!(bad.get(CHAT_EXTRA_BODY_HEADER).is_none());
    }
}
