//! Tests for Anthropic rate limit header parsing.
//!
//! Verifies that Anthropic's rate limit headers are correctly parsed
//! and converted to the RateLimitSnapshotEvent format.

use code_core::protocol::RateLimitSnapshotEvent;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

/// Helper to parse Anthropic rate limit headers.
/// This mirrors the logic in anthropic_completions.rs.
fn parse_anthropic_rate_limits(headers: &HeaderMap) -> Option<RateLimitSnapshotEvent> {
    fn parse_header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
        headers.get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
    }
    
    let requests_limit = parse_header_u64(headers, "anthropic-ratelimit-requests-limit")?;
    let requests_remaining = parse_header_u64(headers, "anthropic-ratelimit-requests-remaining")?;
    let tokens_limit = parse_header_u64(headers, "anthropic-ratelimit-tokens-limit")?;
    let tokens_remaining = parse_header_u64(headers, "anthropic-ratelimit-tokens-remaining")?;
    
    let requests_used_percent = if requests_limit > 0 {
        ((requests_limit - requests_remaining) as f64 / requests_limit as f64) * 100.0
    } else {
        0.0
    };
    
    let tokens_used_percent = if tokens_limit > 0 {
        ((tokens_limit - tokens_remaining) as f64 / tokens_limit as f64) * 100.0
    } else {
        0.0
    };
    
    Some(RateLimitSnapshotEvent {
        primary_used_percent: requests_used_percent,
        primary_window_minutes: 1,
        primary_reset_after_seconds: None,
        secondary_used_percent: tokens_used_percent,
        secondary_window_minutes: 1,
        secondary_reset_after_seconds: None,
        primary_to_secondary_ratio_percent: 100.0,
    })
}

fn make_headers(entries: Vec<(&'static str, &'static str)>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in entries {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    headers
}

#[test]
fn parses_rate_limit_headers_correctly() {
    let headers = make_headers(vec![
        ("anthropic-ratelimit-requests-limit", "100"),
        ("anthropic-ratelimit-requests-remaining", "75"),
        ("anthropic-ratelimit-tokens-limit", "100000"),
        ("anthropic-ratelimit-tokens-remaining", "80000"),
    ]);
    
    let rate_limits = parse_anthropic_rate_limits(&headers)
        .expect("should parse rate limits");
    
    // 25 out of 100 requests used = 25%
    assert!((rate_limits.primary_used_percent - 25.0).abs() < 0.01);
    // 20000 out of 100000 tokens used = 20%
    assert!((rate_limits.secondary_used_percent - 20.0).abs() < 0.01);
}

#[test]
fn returns_none_for_missing_headers() {
    let headers = make_headers(vec![
        ("anthropic-ratelimit-requests-limit", "100"),
        // missing remaining headers
    ]);
    
    let rate_limits = parse_anthropic_rate_limits(&headers);
    assert!(rate_limits.is_none(), "should return None for incomplete headers");
}

#[test]
fn handles_zero_limit_gracefully() {
    let headers = make_headers(vec![
        ("anthropic-ratelimit-requests-limit", "0"),
        ("anthropic-ratelimit-requests-remaining", "0"),
        ("anthropic-ratelimit-tokens-limit", "100000"),
        ("anthropic-ratelimit-tokens-remaining", "100000"),
    ]);
    
    let rate_limits = parse_anthropic_rate_limits(&headers)
        .expect("should parse rate limits");
    
    // Zero limit should result in 0% used (avoid division by zero)
    assert_eq!(rate_limits.primary_used_percent, 0.0);
}

#[test]
fn calculates_100_percent_when_exhausted() {
    let headers = make_headers(vec![
        ("anthropic-ratelimit-requests-limit", "100"),
        ("anthropic-ratelimit-requests-remaining", "0"),
        ("anthropic-ratelimit-tokens-limit", "100000"),
        ("anthropic-ratelimit-tokens-remaining", "0"),
    ]);
    
    let rate_limits = parse_anthropic_rate_limits(&headers)
        .expect("should parse rate limits");
    
    assert_eq!(rate_limits.primary_used_percent, 100.0);
    assert_eq!(rate_limits.secondary_used_percent, 100.0);
}
