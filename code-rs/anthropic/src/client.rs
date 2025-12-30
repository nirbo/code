//! Anthropic Messages API client with streaming support.

use super::messages::*;
use super::streaming::*;
use super::tools::*;
use super::types::*;
use super::*;
use code_core::config::Config;
use code_core::ResponseEvent;
use futures::Stream;
use reqwest::header::{CONTENT_TYPE, USER_AGENT};
use serde_json::json;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc;
use tracing::debug;
use tracing::error;

/// Anthropic Messages API client.
pub struct AnthropicClient {
    http_client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: AnthropicModel,
}

impl AnthropicClient {
    /// Create a new Anthropic client using the provided API key.
    pub fn with_api_key(api_key: impl Into<String>, model: AnthropicModel) -> Self {
        Self {
            http_client: reqwest::Client::builder()
                .build()
                .expect("Failed to build HTTP client"),
            api_key: api_key.into(),
            base_url: ANTHROPIC_BASE_URL.to_string(),
            model,
        }
    }

    /// Create a new Anthropic client using authentication from code_home.
    pub async fn from_auth(code_home: &std::path::Path, model: AnthropicModel) -> Result<Self> {
        ensure_token(code_home).await?;
        let api_key = get_token()?;
        Ok(Self::with_api_key(api_key, model))
    }

    /// Create a client from the Codex config.
    pub async fn from_config(config: &Config, model: AnthropicModel) -> Result<Self> {
        Self::from_auth(&config.code_home, model).await
    }

    /// Set a custom base URL (useful for testing/proxy).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Get the current API key.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Send a streaming request to the Messages API.
    pub async fn stream_messages(
        &self,
        request: MessagesRequest,
    ) -> Result<mpsc::Receiver<Result<ResponseEvent>>> {
        let url = format!("{}{}", self.base_url, MESSAGES_API_PATH);

        let (tx_event, rx_event) = mpsc::channel(1600);

        // Build the request payload
        let payload = self.build_request_payload(&request)?;

        debug!(
            "POST to {}: {}",
            url,
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );

        // Build the HTTP request
        // NOTE: The anthropic-beta header is required for OAuth subscription tokens to work!
        // oauth-2025-04-20 enables Bearer token auth for Pro/Max subscriptions
        let req_builder = self
            .http_client
            .post(&url)
            .header("anthropic-version", API_VERSION_HEADER)
            .header(
                "anthropic-beta",
                "oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14",
            )
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, "codex-cli/1.0")
            .bearer_auth(&self.api_key)
            .json(&payload);

        // Send the request
        let response = req_builder.send().await.map_err(|e| {
            error!("Failed to send request: {}", e);
            AnthropicError::Request(e)
        })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!("Request failed with status {}: {}", status, body);

            // Try to parse as Anthropic error format
            if let Ok(err_resp) = serde_json::from_str::<ApiErrorResponse>(&body) {
                return Err(AnthropicError::UnexpectedResponse(format!(
                    "{}: {}",
                    err_resp.error.error_type, err_resp.error.message
                )));
            }

            return Err(AnthropicError::UnexpectedResponse(format!(
                "Status {}: {}",
                status, body
            )));
        }

        // Process the streaming response
        let stream = response.bytes_stream();

        let config = StreamProcessorConfig::default();

        tokio::spawn(async move {
            match process_anthropic_stream(stream, tx_event.clone(), config).await {
                Ok(_) => debug!("Stream processing completed successfully"),
                Err(e) => {
                    error!("Stream processing error: {:?}", e);
                    let _ = tx_event
                        .send(Err(e))
                        .await;
                }
            }
        });

        Ok(rx_event)
    }

    /// Build the request payload for the Messages API.
    fn build_request_payload(&self, request: &MessagesRequest) -> Result<serde_json::Value> {
        // Convert messages
        let (anthropic_messages, _system) = to_anthropic_messages(&request.input, &request.system)?;

        // Convert tools
        let anthropic_tools = openai_to_anthropic_tools(&request.tools)?;

        let mut payload = json!({
            "model": self.model.as_id(),
            "messages": anthropic_messages,
            "max_tokens": request.config.max_tokens,
            "stream": true,
        });

        // Add system prompt
        if !_system.is_empty() {
            payload["system"] = json!(_system);
        }

        // Add tools if present
        if !anthropic_tools.is_empty() {
            payload["tools"] = json!(anthropic_tools);
            // Anthropic defaults to auto tool use
            payload["tool_choice"] = json!({ "type": "auto" });
        }

        // Add temperature if specified
        if let Some(temp) = request.config.temperature {
            payload["temperature"] = json!(temp);
        }

        // Add top_p if specified
        if let Some(top_p) = request.config.top_p {
            payload["top_p"] = json!(top_p);
        }

        // Add top_k if specified
        if let Some(top_k) = request.config.top_k {
            payload["top_k"] = json!(top_k);
        }

        // Add extended thinking if enabled
        if request.config.extended_thinking {
            let thinking_config = request
                .config
                .extended_thinking_config
                .as_ref()
                .cloned()
                .unwrap_or_default();

            payload["thinking"] = json!({
                "type": "extended",
                "max_tokens": thinking_config.max_tokens.unwrap_or(200_000),
                "emit_thinking": thinking_config.emit_thinking.unwrap_or(false)
            });
        }

        Ok(payload)
    }

    /// Get the model being used by this client.
    pub fn model(&self) -> AnthropicModel {
        self.model
    }

    /// Set a different model for this client.
    pub fn with_model(mut self, model: AnthropicModel) -> Self {
        self.model = model;
        self
    }
}

/// Request for the Messages API.
#[derive(Debug, Clone)]
pub struct MessagesRequest {
    /// The system prompt.
    pub system: String,
    /// Conversation history in Codex protocol format.
    pub input: Vec<code_protocol::models::ResponseItem>,
    /// Tools available to the model (OpenAI format).
    pub tools: Vec<serde_json::Value>,
    /// Streaming configuration.
    pub config: StreamConfig,
}

impl MessagesRequest {
    /// Create a new request.
    pub fn new(
        system: impl Into<String>,
        input: Vec<code_protocol::models::ResponseItem>,
        tools: Vec<serde_json::Value>,
    ) -> Self {
        Self {
            system: system.into(),
            input,
            tools,
            config: StreamConfig::default(),
        }
    }

    /// Set the max tokens for the response.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.config.max_tokens = max_tokens;
        self
    }

    /// Set the temperature.
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.config.temperature = Some(temperature);
        self
    }

    /// Enable extended thinking.
    pub fn with_extended_thinking(mut self, enabled: bool) -> Self {
        self.config.extended_thinking = enabled;
        self
    }

    /// Set extended thinking configuration.
    pub fn with_extended_thinking_config(mut self, config: ExtendedThinkingConfig) -> Self {
        self.config.extended_thinking = true;
        self.config.extended_thinking_config = Some(config);
        self
    }
}

impl Default for MessagesRequest {
    fn default() -> Self {
        Self {
            system: String::new(),
            input: Vec::new(),
            tools: Vec::new(),
            config: StreamConfig::default(),
        }
    }
}

/// Adapter type for compatibility with existing streaming infrastructure.
pub struct AnthropicStream {
    pub(crate) rx_event: mpsc::Receiver<Result<ResponseEvent>>,
}

impl Stream for AnthropicStream {
    type Item = Result<ResponseEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}

impl From<mpsc::Receiver<Result<ResponseEvent>>> for AnthropicStream {
    fn from(rx: mpsc::Receiver<Result<ResponseEvent>>) -> Self {
        Self { rx_event: rx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_model_ids() {
        assert_eq!(AnthropicModel::Opus.as_id(), "claude-opus-4.5");
        assert_eq!(AnthropicModel::Sonnet.as_id(), "claude-sonnet-4.5");
        assert_eq!(AnthropicModel::Haiku.as_id(), "claude-haiku-4.5");
    }

    #[test]
    fn test_messages_request_default() {
        let request = MessagesRequest::default();
        assert_eq!(request.system, "");
        assert!(request.input.is_empty());
        assert!(request.tools.is_empty());
        assert_eq!(request.config.max_tokens, 8192);
        assert!(!request.config.extended_thinking);
    }

    #[test]
    fn test_messages_request_builder() {
        let request = MessagesRequest::new(
            "You are a helpful assistant.",
            vec![],
            vec![],
        )
        .with_max_tokens(4096)
        .with_temperature(0.7)
        .with_extended_thinking(true);

        assert_eq!(request.system, "You are a helpful assistant.");
        assert_eq!(request.config.max_tokens, 4096);
        assert_eq!(request.config.temperature, Some(0.7));
        assert!(request.config.extended_thinking);
    }

    #[test]
    fn test_extended_thinking_config_default() {
        let config = ExtendedThinkingConfig::default();
        assert_eq!(config.max_tokens, Some(200_000));
        assert_eq!(config.emit_thinking, Some(false));
    }
}
