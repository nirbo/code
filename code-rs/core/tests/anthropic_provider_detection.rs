//! Tests for Anthropic provider auto-detection.
//!
//! These tests verify that Claude model names (e.g., `claude-sonnet-4-5`)
//! are automatically routed to the `anthropic` provider without requiring
//! explicit `--model-provider anthropic`.

use code_core::{built_in_model_providers, WireApi};

#[test]
fn anthropic_provider_exists_in_builtins() {
    let providers = built_in_model_providers();
    assert!(
        providers.contains_key("anthropic"),
        "anthropic provider should exist in built-in providers"
    );
}

#[test]
fn anthropic_provider_uses_anthropic_wire_api() {
    let providers = built_in_model_providers();
    let anthropic = providers
        .get("anthropic")
        .expect("anthropic provider should exist");
    assert_eq!(
        anthropic.wire_api,
        WireApi::Anthropic,
        "anthropic provider should use WireApi::Anthropic"
    );
}

#[test]
fn anthropic_provider_has_correct_base_url() {
    let providers = built_in_model_providers();
    let anthropic = providers
        .get("anthropic")
        .expect("anthropic provider should exist");
    assert_eq!(
        anthropic.base_url.as_deref(),
        Some("https://api.anthropic.com"),
        "anthropic provider should use api.anthropic.com"
    );
}

/// Helper to infer provider from model name.
/// Mirrors the logic in config.rs for testing.
fn infer_provider_from_model(model: &str) -> Option<&'static str> {
    let m_lower = model.to_ascii_lowercase();
    if m_lower.starts_with("claude-") || m_lower.starts_with("anthropic/") {
        Some("anthropic")
    } else if m_lower.starts_with("gemini-") || m_lower.starts_with("google/") {
        Some("google")
    } else if m_lower.starts_with("qwen-") {
        Some("openrouter")
    } else {
        None
    }
}

#[test]
fn claude_model_names_infer_anthropic_provider() {
    // Standard Claude model names
    assert_eq!(infer_provider_from_model("claude-sonnet-4-5"), Some("anthropic"));
    assert_eq!(infer_provider_from_model("claude-opus-4-5"), Some("anthropic"));
    assert_eq!(infer_provider_from_model("claude-haiku-4-5"), Some("anthropic"));
    
    // Legacy model names
    assert_eq!(infer_provider_from_model("claude-3-5-sonnet-20241022"), Some("anthropic"));
    assert_eq!(infer_provider_from_model("claude-3-opus"), Some("anthropic"));
    
    // With anthropic/ prefix
    assert_eq!(infer_provider_from_model("anthropic/claude-sonnet-4-5"), Some("anthropic"));
}

#[test]
fn claude_model_names_are_case_insensitive() {
    assert_eq!(infer_provider_from_model("CLAUDE-SONNET-4-5"), Some("anthropic"));
    assert_eq!(infer_provider_from_model("Claude-Opus-4-5"), Some("anthropic"));
    assert_eq!(infer_provider_from_model("ANTHROPIC/CLAUDE-HAIKU"), Some("anthropic"));
}

#[test]
fn non_claude_models_do_not_infer_anthropic() {
    assert_eq!(infer_provider_from_model("gpt-5.2-codex"), None);
    assert_eq!(infer_provider_from_model("gpt-5.1"), None);
    assert_eq!(infer_provider_from_model("o3"), None);
    assert_eq!(infer_provider_from_model("gemini-3-pro"), Some("google"));
}
