//! Token management for Anthropic authentication.

use code_core::token_data::TokenData;
use std::sync::LazyLock;
use std::sync::RwLock;

static ANTHROPIC_TOKEN: LazyLock<RwLock<Option<TokenData>>> = LazyLock::new(|| RwLock::new(None));

/// Get the current Anthropic token data.
pub fn get_anthropic_token_data() -> Option<TokenData> {
    ANTHROPIC_TOKEN.read().ok()?.clone()
}

/// Set the Anthropic token data.
pub fn set_anthropic_token_data(value: TokenData) {
    if let Ok(mut guard) = ANTHROPIC_TOKEN.write() {
        *guard = Some(value);
    }
}

/// Clear the Anthropic token data.
pub fn clear_anthropic_token_data() {
    if let Ok(mut guard) = ANTHROPIC_TOKEN.write() {
        *guard = None;
    }
}
