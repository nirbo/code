//! Anthropic Claude API client for Codex.
//!
//! This crate provides streaming support for the Anthropic Messages API,
//! including tool use, extended thinking, and proper event mapping to the
//! internal `ResponseEvent` format.

mod anthropic_token;
mod client;
mod messages;
mod streaming;
mod tools;
mod types;

pub use client::AnthropicClient;
pub use messages::*;
pub use streaming::*;
pub use tools::*;
pub use types::*;

pub use anthropic_token::*;

use code_core::auth::init_anthropic_token_from_auth;

/// Anthropic API base URL.
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Anthropic API version header.
pub const API_VERSION_HEADER: &str = "2023-06-01";

/// Anthropic Messages API path.
pub const MESSAGES_API_PATH: &str = "/v1/messages";

/// Errors that can occur when using the Anthropic client.
pub type Result<T> = std::result::Result<T, AnthropicError>;

#[derive(Debug, thiserror::Error)]
pub enum AnthropicError {
    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("API request failed: {0}")]
    Request(#[from] reqwest::Error),

    #[error("JSON serialization/deserialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("unexpected API response: {0}")]
    UnexpectedResponse(String),

    #[error("stream processing error: {0}")]
    Stream(String),

    #[error("tool translation error: {0}")]
    ToolTranslation(String),
}

/// Initialize the Anthropic token from stored authentication.
/// This loads the token and caches it for subsequent API calls.
pub async fn ensure_token(code_home: &std::path::Path) -> Result<()> {
    if let Some(token_data) = init_anthropic_token_from_auth(code_home, "codex-cli")
        .await
        .map_err(|e| AnthropicError::Auth(e.to_string()))?
    {
        set_anthropic_token_data(token_data);
    }
    Ok(())
}

/// Get the current Anthropic API token from cache.
pub fn get_token() -> Result<String> {
    get_anthropic_token_data()
        .map(|t| t.access_token.clone())
        .ok_or_else(|| AnthropicError::Auth("No Anthropic token available".to_string()))
}
