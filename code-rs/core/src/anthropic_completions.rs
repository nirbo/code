//! Anthropic Messages API streaming integration.
//!
//! This module provides the bridge between Codex's internal streaming
//! infrastructure and the Anthropic Messages API.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use code_otel::otel_event_manager::OtelEventManager;
use eventsource_stream::Eventsource;
use futures::Stream;
use futures::StreamExt;
use futures::TryStreamExt;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::trace;

use crate::auth::AuthManager;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::client_common::ResponseStream;
use crate::debug_logger::DebugLogger;
use crate::error::CodexErr;
use crate::error::Result;
use crate::model_family::ModelFamily;
use crate::model_provider_info::ModelProviderInfo;
use crate::openai_tools::create_tools_json_for_chat_completions_api;
use crate::models::ContentItem;
use crate::models::ResponseItem;
use crate::util::backoff;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text { text: String },
    Image {
        source: AnthropicImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<AnthropicContentBlock>,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
struct AnthropicImageSource {
    #[serde(rename = "type")]
    media_type: String,
    data: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

/// Stream a response from the Anthropic Messages API.
pub(crate) async fn stream_anthropic_messages(
    prompt: &Prompt,
    model_family: &ModelFamily,
    model_slug: &str,
    client: &reqwest::Client,
    provider: &ModelProviderInfo,
    debug_logger: &Arc<std::sync::Mutex<DebugLogger>>,
    auth_manager: Option<Arc<AuthManager>>,
    _otel_event_manager: Option<OtelEventManager>,
    log_tag: Option<&str>,
) -> Result<ResponseStream> {
    if prompt.output_schema.is_some() {
        return Err(CodexErr::UnsupportedOperation(
            "output_schema is not supported for Anthropic Messages API".to_string(),
        ));
    }

    // Get the full system prompt
    let full_instructions = prompt.get_full_instructions(model_family);

    // Get the formatted input (conversation history)
    let input = prompt.get_formatted_input();

    // Convert to Anthropic Messages API format
    let (anthropic_messages, system_prompt) = to_anthropic_messages(&input, &full_instructions);

    // Build tools in Anthropic format
    let tools_json = create_tools_json_for_chat_completions_api(&prompt.tools)?;
    let anthropic_tools = to_anthropic_tools(&tools_json);

    // Build the request payload
    let mut payload = json!({
        "model": model_slug,
        "messages": anthropic_messages,
        "max_tokens": 8192,
        "stream": true,
    });

    // Build system prompt as an array of text blocks
    // CRITICAL: For subscription OAuth tokens, Anthropic validates that the FIRST text block
    // contains the Claude Code identity. The identity MUST be in its own separate block!
    const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
    
    let system_blocks = if system_prompt.is_empty() {
        // Only the identity block
        json!([{"type": "text", "text": CLAUDE_CODE_IDENTITY}])
    } else {
        // Identity as first block, then additional instructions in a second block
        json!([
            {"type": "text", "text": CLAUDE_CODE_IDENTITY},
            {"type": "text", "text": system_prompt}
        ])
    };
    payload["system"] = system_blocks;

    // Add tools if present
    if !anthropic_tools.is_empty() {
        payload["tools"] = json!(anthropic_tools);
        payload["tool_choice"] = json!({ "type": "auto" });
    }

    // Build the endpoint URL (get_full_url already appends /v1/messages for WireApi::Anthropic)
    let url = provider.get_full_url(&auth_manager.as_ref().and_then(|a| a.auth()));

    debug!(
        "POST to {}: {}",
        url,
        serde_json::to_string_pretty(&payload).unwrap_or_default()
    );

    let mut attempt = 0;
    let max_retries = provider.request_max_retries();
    let mut request_id = String::new();
    loop {
        attempt += 1;

        // Build the request with auth
        let mut req_builder = client
            .post(&url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::USER_AGENT, "claude-code/20250219")
            .header("anthropic-version", "2023-06-01")
            // NOTE: anthropic-beta header with oauth-2025-04-20 is required for OAuth subscription tokens!
            // See opencode-anthropic-auth plugin for reference implementation
            .header(
                "anthropic-beta",
                "oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14",
            )
            .json(&payload);

        // Add OAuth token as Authorization: Bearer header (NOT X-API-Key)
        // Reference: opencode-anthropic-auth plugin uses Bearer token for OAuth subscription tokens
        let auth = auth_manager.as_ref().and_then(|m| m.auth());
        if let Some(auth) = auth.as_ref() {
            debug!("Anthropic auth mode: {:?}", auth.mode);
            match auth.get_token().await {
                Ok(token) => {
                    debug!("Anthropic token first 30 chars: {}...", &token[..30.min(token.len())]);
                    req_builder = req_builder.bearer_auth(&token);
                }
                Err(e) => {
                    debug!("Failed to get Anthropic token: {:?}", e);
                }
            }
        } else {
            debug!("No auth manager or auth available for Anthropic request");
        }

        // Add any provider-specific headers
        if let Some(headers) = provider.http_headers.as_ref() {
            for (key, value) in headers {
                req_builder = req_builder.header(key, value);
            }
        }

        // Log the request
        if request_id.is_empty() {
            if let Ok(logger) = debug_logger.lock() {
                request_id = logger
                    .start_request_log(
                        &url,
                        &payload,
                        None,
                        log_tag,
                    )
                    .unwrap_or_default();
            }
        }

        let res = req_builder.send().await;

        match res {
            Ok(resp) if resp.status().is_success() => {
                debug!("Anthropic stream initiated successfully");

                let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent>>(1600);
                let stream = resp.bytes_stream().map_err(CodexErr::Reqwest);

                tokio::spawn(process_anthropic_sse(
                    stream,
                    tx_event,
                    provider.stream_idle_timeout(),
                    debug_logger.clone(),
                    request_id.clone(),
                ));

                return Ok(ResponseStream { rx_event });
            }
            Ok(res) => {
                let status = res.status();
                if !(status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()) {
                    let body = res.text().await.unwrap_or_default();
                    return Err(CodexErr::UnexpectedStatus(crate::error::UnexpectedResponseError {
                        status,
                        body,
                        request_id: None,
                    }));
                }

                if attempt > max_retries {
                    return Err(CodexErr::RetryLimit(crate::error::RetryLimitReachedError {
                        status,
                        request_id: None,
                        retryable: status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS,
                    }));
                }

                let delay = backoff(attempt);
                tokio::time::sleep(delay).await;
            }
            Err(e) => {
                let is_connectivity = e.is_connect() || e.is_timeout() || e.is_request();
                if attempt > max_retries {
                    if is_connectivity {
                        let req_id = (!request_id.is_empty()).then(|| request_id.clone());
                        return Err(CodexErr::Stream(
                            format!("[transport] network unavailable: {e}"),
                            None,
                            req_id,
                        ));
                    }
                    return Err(e.into());
                }
                let delay = backoff(attempt);
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Convert Codex protocol items to Anthropic Messages format.
fn to_anthropic_messages(
    items: &[ResponseItem],
    system_prompt: &str,
) -> (Vec<serde_json::Value>, String) {
    let mut messages = Vec::new();
    let mut current_user_content: Vec<AnthropicContentBlock> = Vec::new();
    let mut current_assistant_content: Vec<AnthropicContentBlock> = Vec::new();
    let system_message = system_prompt.to_string();

    for item in items {
        match item {
            ResponseItem::Message { role, content, .. } => {
                match role.as_str() {
                    "user" => {
                        if !current_assistant_content.is_empty() {
                            messages.push(json!({
                                "role": "assistant",
                                "content": serialize_content_blocks(&current_assistant_content)
                            }));
                            current_assistant_content.clear();
                        }

                        for block in content {
                            match block {
                                ContentItem::InputText { text } => {
                                    current_user_content.push(AnthropicContentBlock::Text {
                                        text: text.clone(),
                                    });
                                }
                                ContentItem::InputImage { image_url } => {
                                    if let Some(img) = parse_data_url(image_url) {
                                        current_user_content.push(img);
                                    }
                                }
                                ContentItem::OutputText { .. } => {}
                            }
                        }
                    }
                    "assistant" => {
                        if !current_user_content.is_empty() {
                            messages.push(json!({
                                "role": "user",
                                "content": serialize_content_blocks(&current_user_content)
                            }));
                            current_user_content.clear();
                        }

                        for block in content {
                            if let ContentItem::OutputText { text } = block {
                                current_assistant_content.push(AnthropicContentBlock::Text {
                                    text: text.clone(),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                if !current_user_content.is_empty() {
                    messages.push(json!({
                        "role": "user",
                        "content": serialize_content_blocks(&current_user_content)
                    }));
                    current_user_content.clear();
                }

                let input = serde_json::from_str(arguments).unwrap_or_else(|_| {
                    json!({ "arguments": arguments })
                });

                current_assistant_content.push(AnthropicContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: name.clone(),
                    input,
                });
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                if !current_assistant_content.is_empty() {
                    messages.push(json!({
                        "role": "assistant",
                        "content": serialize_content_blocks(&current_assistant_content)
                    }));
                    current_assistant_content.clear();
                }

                current_user_content.push(AnthropicContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content: vec![AnthropicContentBlock::Text {
                        text: output.content.clone(),
                    }],
                });
            }
            _ => {}
        }
    }

    // Flush remaining content
    if !current_user_content.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": serialize_content_blocks(&current_user_content)
        }));
    }
    if !current_assistant_content.is_empty() {
        messages.push(json!({
            "role": "assistant",
            "content": serialize_content_blocks(&current_assistant_content)
        }));
    }

    (messages, system_message)
}

fn serialize_content_blocks(blocks: &[AnthropicContentBlock]) -> serde_json::Value {
    if blocks.len() == 1 {
        match &blocks[0] {
            AnthropicContentBlock::Text { text } => json!(text),
            _ => json!(blocks),
        }
    } else {
        json!(blocks)
    }
}

fn parse_data_url(url: &str) -> Option<AnthropicContentBlock> {
    if !url.starts_with("data:") {
        return None;
    }

    let parts: Vec<&str> = url.splitn(3, ':').collect();
    if parts.len() < 2 {
        return None;
    }

    let after_scheme = parts[1];
    let mut parts2 = after_scheme.splitn(2, ';');
    let mime_type = parts2.next()?;
    let encoding = parts2.next()?;

    if !encoding.starts_with("base64,") {
        return None;
    }

    let data = encoding[7..].to_string();

    let (media_type, data) = match mime_type {
        "image/png" => ("image/png".to_string(), data),
        "image/jpeg" | "image/jpg" => ("image/jpeg".to_string(), data),
        "image/gif" => ("image/gif".to_string(), data),
        "image/webp" => ("image/webp".to_string(), data),
        _ => return None,
    };

    Some(AnthropicContentBlock::Image {
        source: AnthropicImageSource {
            media_type,
            data,
        },
    })
}

/// Convert OpenAI tools to Anthropic format.
fn to_anthropic_tools(openai_tools: &[serde_json::Value]) -> Vec<AnthropicTool> {
    let mut anthropic_tools = Vec::new();

    for tool in openai_tools {
        let function = tool.get("function").unwrap();
        let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let description = function.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let input_schema = function.get("parameters").cloned().unwrap_or_else(|| {
            json!({ "type": "object", "properties": {} })
        });

        anthropic_tools.push(AnthropicTool {
            name: name.to_string(),
            description: description.to_string(),
            input_schema,
        });
    }

    anthropic_tools
}

/// Process SSE events from Anthropic and convert to ResponseEvent.
async fn process_anthropic_sse<S>(
    stream: S,
    tx_event: mpsc::Sender<Result<ResponseEvent>>,
    _idle_timeout: Duration,
    _debug_logger: Arc<std::sync::Mutex<DebugLogger>>,
    request_id: String,
) where
    S: Stream<Item = Result<Bytes>> + Unpin,
{
    let mut stream = stream.eventsource();

    // Send initial created event
    let _ = tx_event.send(Ok(ResponseEvent::Created)).await;

    let mut current_index: Option<u32> = None;

    loop {
        let sse = match stream.next().await {
            Some(Ok(ev)) => ev,
            Some(Err(e)) => {
                let _ = tx_event
                    .send(Err(CodexErr::Stream(
                        format!("[transport] {e}"),
                        None,
                        Some(request_id.clone()),
                    )))
                    .await;
                return;
            }
            None => {
                // Stream closed by server
                debug!("Anthropic SSE stream closed");
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        response_id: request_id.clone(),
                        token_usage: None,
                    }))
                    .await;
                return;
            }
        };

        // Anthropic Messages API sends individual events (not wrapped in data: prefix)
        // The eventsource extension already parsed them
        let data = sse.data.trim();

        if data.is_empty() {
            continue;
        }

        trace!("SSE data: {}", data);

        // Check for [DONE] sentinel
        if data == "[DONE]" {
            debug!("Received [DONE] from Anthropic");
            let _ = tx_event
                .send(Ok(ResponseEvent::Completed {
                    response_id: request_id.clone(),
                    token_usage: None,
                }))
                .await;
            return;
        }

        if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(event_type) = event.get("type").and_then(|v| v.as_str()) {
                match event_type {
                    "message_start" => {
                        debug!("Message started");
                    }
                    "content_block_start" => {
                        if let Some(index) = event.get("index").and_then(|v| v.as_u64()) {
                            current_index = Some(index as u32);
                            trace!("Content block {} started", index);
                        }
                    }
                    "content_block_delta" => {
                        if let Some(delta) = event.get("delta") {
                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                let _ = tx_event
                                    .send(Ok(ResponseEvent::OutputTextDelta {
                                        delta: text.to_string(),
                                        item_id: None,
                                        sequence_number: None,
                                        output_index: current_index,
                                    }))
                                    .await;
                            }
                        }
                    }
                    "content_block_stop" => {
                        current_index = None;
                    }
                    "message_stop" => {
                        debug!("Message stopped");
                        let _ = tx_event
                            .send(Ok(ResponseEvent::Completed {
                                response_id: request_id.clone(),
                                token_usage: None,
                            }))
                            .await;
                        return;
                    }
                    "ping" => {
                        trace!("Ping received");
                    }
                    _ => {
                        trace!("Unknown event type: {}", event_type);
                    }
                }
            }
        }
    }
}
