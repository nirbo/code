//! Streaming event handling for the Anthropic Messages API.

use crate::types::*;
use crate::Result;
use bytes::Bytes;
use code_core::ResponseEvent;
use code_core::protocol::TokenUsage;
use futures::Stream;
use futures::StreamExt;
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::trace;

/// Anthropic-specific stream processor configuration.
#[derive(Debug, Clone)]
pub struct StreamProcessorConfig {
    /// Idle timeout for the stream.
    pub idle_timeout: Duration,
    /// Maximum number of retries for connection issues.
    pub max_retries: u32,
}

impl Default for StreamProcessorConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(300),
            max_retries: 5,
        }
    }
}

/// Process a raw SSE stream from Anthropic and convert to ResponseEvent.
pub async fn process_anthropic_stream<S>(
    stream: S,
    tx_event: mpsc::Sender<Result<ResponseEvent>>,
    config: StreamProcessorConfig,
) -> Result<MessagesResponseMetadata>
where
    S: Stream<Item = reqwest::Result<Bytes>> + Unpin,
{
    let mut lines = stream
        .map(|item| item.map_err(|e| crate::AnthropicError::Stream(e.to_string())));

    let mut metadata = MessagesResponseMetadata::default();
    let mut current_index: Option<u32> = None;
    let mut current_text = String::new();
    let mut current_thinking = String::new();
    let mut tool_id_map: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut tool_name_map: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut current_tool_json = String::new();

    // Send initial created event
    let _ = tx_event
        .send(Ok(ResponseEvent::Created))
        .await;

    while let Some(item) = lines.next().await {
        let bytes = item?;
        let data = String::from_utf8_lossy(&bytes);

        for line in data.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            // Parse SSE format: "data: {...}"
            if !line.starts_with("data: ") {
                trace!("Skipping non-data line: {}", line);
                continue;
            }

            let json_str = &line[6..]; // Skip "data: "

            // Handle "[DONE]" sentinel
            if json_str == "[DONE]" {
                debug!("Received [DONE] sentinel");
                // Send completion event
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        response_id: metadata.id.clone(),
                        token_usage: metadata.usage.clone(),
                    }))
                    .await;
                return Ok(metadata);
            }

            // Parse the JSON event
            match serde_json::from_str::<StreamingEvent>(json_str) {
                Ok(event) => {
                    trace!("Received event: {:?}", event);

                    match event {
                        StreamingEvent::MessageStart => {
                            debug!("Message started");
                        }
                        StreamingEvent::MessageDelta => {
                            // Message delta - could contain usage info
                        }
                        StreamingEvent::MessageStop => {
                            debug!("Message stopped");
                            // Send completion event if we haven't already
                            let _ = tx_event
                                .send(Ok(ResponseEvent::Completed {
                                    response_id: metadata.id.clone(),
                                    token_usage: metadata.usage.clone(),
                                }))
                                .await;
                            return Ok(metadata);
                        }
                        StreamingEvent::ContentBlockStart { index, content_block } => {
                            current_index = Some(index);

                            if let Some(block) = content_block {
                                match block {
                                    ContentBlock::ToolUse { id, name, .. } => {
                                        tool_id_map.insert(index, id.clone());
                                        tool_name_map.insert(index, name.clone());
                                        trace!("Tool use started: {} ({})", name, id);
                                    }
                                    ContentBlock::Thinking { .. } => {
                                        trace!("Extended thinking started for block {}", index);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        StreamingEvent::ContentBlockDelta { index, delta } => {
                            current_index = Some(index);

                            match delta {
                                Delta::TextDelta { text } => {
                                    if !text.is_empty() {
                                        current_text.push_str(&text);

                                        let _ = tx_event
                                            .send(Ok(ResponseEvent::OutputTextDelta {
                                                delta: text,
                                                item_id: None,
                                                sequence_number: None,
                                                output_index: Some(index),
                                            }))
                                            .await;
                                    }
                                }
                                Delta::ThinkingDelta { thinking } => {
                                    if !thinking.is_empty() {
                                        current_thinking.push_str(&thinking);

                                        let _ = tx_event
                                            .send(Ok(ResponseEvent::ReasoningContentDelta {
                                                delta: thinking,
                                                item_id: None,
                                                sequence_number: None,
                                                output_index: Some(index),
                                                content_index: Some(0),
                                            }))
                                            .await;
                                    }
                                }
                                Delta::InputJsonDelta { partial_json } => {
                                    current_tool_json.push_str(&partial_json);
                                    trace!(
                                        "Tool JSON delta for block {}: {}",
                                        index,
                                        partial_json
                                    );
                                }
                                Delta::Unknown => {
                                    trace!("Unknown delta type for block {}", index);
                                }
                            }
                        }
                        StreamingEvent::ContentBlockStop { index } => {
                            // Flush any accumulated content for this block

                            // Check if this was a tool use block
                            if let Some(tool_id) = tool_id_map.get(&index) {
                                if let Some(tool_name) = tool_name_map.get(&index) {
                                    trace!(
                                        "Tool use block stopped: {} ({})",
                                        tool_name,
                                        tool_id
                                    );

                                    // Try to parse the accumulated JSON as the tool input
                                    let input =
                                        serde_json::from_str::<serde_json::Value>(
                                            &current_tool_json,
                                        )
                                        .unwrap_or_else(|_| {
                                            json!({ "raw_input": current_tool_json.clone() })
                                        });

                                    // Send the tool call event
                                    // Note: We'd need to add a tool call event type or handle this differently
                                    trace!("Tool call: {} with input: {:?}", tool_name, input);
                                }
                            }

                            // Reset per-block state
                            if current_index == Some(index) {
                                current_index = None;
                                current_text.clear();
                                current_tool_json.clear();
                            }
                        }
                        StreamingEvent::Ping => {
                            trace!("Received ping");
                        }
                        StreamingEvent::Unknown => {
                            trace!("Received unknown event type");
                        }
                    }
                }
                Err(e) => {
                    trace!("Failed to parse SSE event: {} from JSON: {}", e, json_str);
                    // Continue processing other events
                }
            }
        }
    }

    // Stream ended without explicit stop
    debug!("Stream ended");
    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id: metadata.id.clone(),
            token_usage: metadata.usage.clone(),
        }))
        .await;

    Ok(metadata)
}

/// Metadata collected during streaming.
#[derive(Debug, Clone, Default)]
pub struct MessagesResponseMetadata {
    pub id: String,
    pub model: String,
    pub usage: Option<TokenUsage>,
}

/// Create a streaming channel for processing Anthropic events.
pub fn create_streaming_channel(
    _config: StreamProcessorConfig,
) -> (mpsc::Sender<Result<ResponseEvent>>, mpsc::Receiver<Result<ResponseEvent>>) {
    mpsc::channel(1600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_processor_config_default() {
        let config = StreamProcessorConfig::default();
        assert_eq!(config.idle_timeout, Duration::from_secs(300));
        assert_eq!(config.max_retries, 5);
    }

    #[test]
    fn test_messages_response_metadata_default() {
        let metadata = MessagesResponseMetadata::default();
        assert!(metadata.id.is_empty());
        assert!(metadata.model.is_empty());
        assert!(metadata.usage.is_none());
    }
}
