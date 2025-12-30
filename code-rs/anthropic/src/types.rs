//! Core types for the Anthropic Messages API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Anthropic model identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicModel {
    /// Claude 4.5 Opus (most capable)
    Opus,
    /// Claude 4.5 Sonnet (balanced)
    Sonnet,
    /// Claude 4.5 Haiku (fastest)
    Haiku,
}

impl AnthropicModel {
    /// Returns the model ID string for the API.
    pub fn as_id(&self) -> &'static str {
        match self {
            Self::Opus => "claude-opus-4.5",
            Self::Sonnet => "claude-sonnet-4.5",
            Self::Haiku => "claude-haiku-4.5",
        }
    }

    /// Returns the display name.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Opus => "Opus",
            Self::Sonnet => "Sonnet",
            Self::Haiku => "Haiku",
        }
    }

    /// Parse a model string to an AnthropicModel.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            s if s.contains("opus") => Some(Self::Opus),
            s if s.contains("sonnet") => Some(Self::Sonnet),
            s if s.contains("haiku") => Some(Self::Haiku),
            _ => None,
        }
    }
}

/// Role in a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Content block for messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    Thinking {
        thinking: String,
    },
    ThinkingDelta {
        delta: String,
    },
}

impl ContentBlock {
    /// Create a text content block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
        }
    }

    /// Create a tool use content block.
    pub fn tool_use(id: String, name: String, input: Value) -> Self {
        Self::ToolUse { id, name, input }
    }

    /// Create a tool result content block.
    pub fn tool_result(tool_use_id: String, content: Vec<ContentBlock>) -> Self {
        Self::ToolResult {
            tool_use_id,
            content,
            is_error: None,
        }
    }
}

/// Image source for image content blocks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub media_type: MediaType,
    pub data: String,
}

/// Media type for images.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    ImagePng,
    ImageJpeg,
    ImageGif,
    ImageWebp,
}

impl MediaType {
    /// Returns the MIME type string.
    pub fn as_mime(&self) -> &'static str {
        match self {
            Self::ImagePng => "image/png",
            Self::ImageJpeg => "image/jpeg",
            Self::ImageGif => "image/gif",
            Self::ImageWebp => "image/webp",
        }
    }

    /// Create from a MIME type string.
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime.to_lowercase().as_str() {
            "image/png" => Some(Self::ImagePng),
            "image/jpeg" | "image/jpg" => Some(Self::ImageJpeg),
            "image/gif" => Some(Self::ImageGif),
            "image/webp" => Some(Self::ImageWebp),
            _ => None,
        }
    }
}

/// A message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

/// Content of a message (can be a single block or multiple blocks).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Single(String),
    Multiple(Vec<ContentBlock>),
}

impl MessageContent {
    /// Create content from a single string.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self::Single(text.into())
    }

    /// Create content from multiple blocks.
    pub fn from_blocks(blocks: Vec<ContentBlock>) -> Self {
        Self::Multiple(blocks)
    }

    /// Returns whether this is multi-block content.
    pub fn is_multiple(&self) -> bool {
        matches!(self, Self::Multiple(_))
    }
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        Self::Single(s)
    }
}

impl From<Vec<ContentBlock>> for MessageContent {
    fn from(blocks: Vec<ContentBlock>) -> Self {
        Self::Multiple(blocks)
    }
}

/// Tool definition for the Anthropic API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Streaming response event types from Anthropic.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamingEvent {
    MessageStart,
    MessageDelta,
    MessageStop,
    ContentBlockStart {
        index: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_block: Option<ContentBlock>,
    },
    ContentBlockDelta {
        index: u32,
        delta: Delta,
    },
    ContentBlockStop {
        index: u32,
    },
    /// Ping event for connection keep-alive.
    Ping,
    #[serde(other)]
    Unknown,
}

/// Delta content for streaming updates.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Unknown,
}

/// Usage information for the API response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Response metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMetadata {
    pub id: String,
    pub model: String,
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Reason the model stopped generating.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
}

/// API error response.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorResponse {
    pub error: ApiError,
}

/// Error detail from the API.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

/// Request configuration for streaming.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// Temperature for sampling.
    pub temperature: Option<f32>,
    /// Top-p sampling.
    pub top_p: Option<f32>,
    /// Top-k sampling.
    pub top_k: Option<u32>,
    /// Enable extended thinking.
    pub extended_thinking: bool,
    /// Extended thinking configuration.
    pub extended_thinking_config: Option<ExtendedThinkingConfig>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            max_tokens: 8192,
            temperature: None,
            top_p: None,
            top_k: None,
            extended_thinking: false,
            extended_thinking_config: None,
        }
    }
}

/// Configuration for extended thinking.
#[derive(Debug, Clone, Serialize)]
pub struct ExtendedThinkingConfig {
    /// Maximum number of tokens for extended thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Whether to emit the thinking content in the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emit_thinking: Option<bool>,
}

impl Default for ExtendedThinkingConfig {
    fn default() -> Self {
        Self {
            max_tokens: Some(200_000),
            emit_thinking: Some(false),
        }
    }
}

/// Complete API response (non-streaming).
#[derive(Debug, Clone, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    pub model: String,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_model_from_str() {
        assert_eq!(AnthropicModel::from_str("opus"), Some(AnthropicModel::Opus));
        assert_eq!(AnthropicModel::from_str("sonnet"), Some(AnthropicModel::Sonnet));
        assert_eq!(AnthropicModel::from_str("haiku"), Some(AnthropicModel::Haiku));
        assert_eq!(AnthropicModel::from_str("unknown"), None);
    }

    #[test]
    fn test_media_type_from_mime() {
        assert_eq!(MediaType::from_mime("image/png"), Some(MediaType::ImagePng));
        assert_eq!(MediaType::from_mime("image/jpeg"), Some(MediaType::ImageJpeg));
        assert_eq!(MediaType::from_mime("image/jpg"), Some(MediaType::ImageJpeg));
        assert_eq!(MediaType::from_mime("image/gif"), Some(MediaType::ImageGif));
        assert_eq!(MediaType::from_mime("image/webp"), Some(MediaType::ImageWebp));
        assert_eq!(MediaType::from_mime("unknown"), None);
    }
}
