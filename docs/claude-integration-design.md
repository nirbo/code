# Claude Integration Design Document

## Project: Native Anthropic Claude Driver for Codex-RS

**Status**: Implementation-Ready Specification
**Last Updated**: 2025-12-30
**Goal**: Enable Claude Opus/Sonnet/Haiku 4.5 as main driver via Anthropic subscription (Pro/Max)

---

## Table of Contents
1. [Executive Summary](#executive-summary)
2. [Deep Analysis Findings](#deep-analysis-findings)
3. [OAuth Authentication](#oauth-authentication)
4. [API Translation Specifications](#api-translation-specifications)
5. [Implementation Tasks](#implementation-tasks)
6. [Auto-Drive Integration](#auto-drive-integration)

---

## Executive Summary

**Verdict**: HIGHLY FEASIBLE with moderate engineering effort (4-6 weeks).

The existing architecture is well-suited for multi-provider support. The primary work is:
1. OAuth authentication flow (subscription-based, not API keys)
2. Anthropic Messages API client crate
3. Translation layer between OpenAI and Anthropic formats
4. Provider integration into existing ModelProviderInfo system

**Key Insight**: The codebase already has:
- Provider abstraction via `ModelProviderInfo`
- Event-driven streaming via `ResponseEvent` enum
- Tool routing via `ToolRouter` (MCP tools work transparently!)
- Reasoning infrastructure that can map to Anthropic's `thinking` blocks
- Auto-drive mode that will work with Anthropic once provider is integrated

---

## Deep Analysis Findings

### Existing Rust Type Structures

**Source: `codex-api/src/` and `code-rs/protocol/src/models.rs`**

#### Core Request Type - `Prompt`
```rust
// File: codex-api/src/lib.rs (approximate location)
pub struct Prompt {
    pub instructions: String,              // System prompt
    pub input: Vec<ResponseItem>,          // Conversation history
    pub tools: Vec<Value>,                 // JSON tool definitions
    pub parallel_tool_calls: bool,
    pub output_schema: Option<Value>,
}
```

#### Core Response Type - `ResponseItem`
```rust
// File: code-rs/protocol/src/models.rs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message { id: Option<String>, role: String, content: Vec<ContentItem> },
    Reasoning {
        id: String,
        summary: Vec<ReasoningItemReasoningSummary>,
        content: Option<Vec<ReasoningItemContent>>,
        encrypted_content: Option<String>,
    },
    FunctionCall { id: Option<String>, name: String, arguments: String, call_id: String },
    FunctionCallOutput { call_id: String, output: FunctionCallOutputPayload },
    CustomToolCall { id: Option<String>, status: Option<String>, call_id: String, name: String, input: String },
    CustomToolCallOutput { call_id: String, output: String },
    LocalShellCall { ... },
    WebSearchCall { ... },
    CompactionSummary { encrypted_content: String },
    Other,
}
```

#### Streaming Event Type - `ResponseEvent`
```rust
// File: codex-api/src/lib.rs
#[derive(Debug)]
pub enum ResponseEvent {
    Created,
    OutputItemDone(ResponseItem),
    OutputItemAdded(ResponseItem),
    OutputTextDelta(String),
    ReasoningSummaryDelta { delta: String, summary_index: i64 },
    ReasoningContentDelta { delta: String, content_index: i64 },
    Completed { response_id: String, token_usage: Option<TokenUsage> },
    RateLimits(RateLimitSnapshot),
}
```

#### Error Type - `CodexErr`
```rust
// File: code-rs/core/src/error.rs
#[derive(Debug, Error)]
pub enum CodexErr {
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("unexpected status: {status}: {body}")]
    UnexpectedResponse { status: StatusCode, body: String, request_id: Option<String> },
    #[error("authentication error: {0}")]
    Auth(#[from] RefreshTokenError),
    #[error("usage limit reached (plan: {plan_type:?})")]
    UsageLimitReached(UsageLimitReachedError),
    // ... other variants
}
```

### Provider Selection Flow

**Key Files:**
- `code-rs/core/src/model_provider_info.rs` - Provider definitions
- `code-rs/core/src/config.rs` - Configuration loading (lines 2787-2790 for model selection)
- `code-rs/core/src/client.rs` - Client construction

**Selection Precedence:**
1. CLI `-m model` (highest)
2. Profile `model` setting
3. Global config `model` setting
4. Default model (lowest)

**Wire API Types:**
```rust
// File: code-rs/core/src/model_provider_info.rs
pub enum WireApi {
    Responses,  // OpenAI's /v1/responses API
    Chat,       // Standard /v1/chat/completions API
    Compact,    // For conversation compression
}
// Need to add: Anthropic
```

### Tool Routing Flow

**Key Files:**
- `code-rs/core/src/openai_tools.rs` - Tool definitions (line 671: `get_openai_tools()`)
- `code-rs/core/src/codex.rs` - Main turn processing (lines 7003-7099)
- `code-rs/core/src/tools/router.rs` - Tool dispatch
- `code-rs/core/src/mcp_connection_manager.rs` - MCP tool management

**Tool Discovery:**
- Built-in tools: shell, plan, browser, agent, wait, kill, code_bridge, web_search
- MCP tools: Automatically discovered, names prefixed with `mcp__<server>__<tool_name>`
- All tools converted to OpenAI-compatible JSON format

**MCP Compatibility:** ✅ **Transparent**
- MCP tools use OpenAI function format
- No provider-specific handling needed
- Will work with Anthropic after tool format translation

### Conversation History Building

**Key File:** `code-rs/core/src/codex.rs`
- Function: `turn_input_with_history()` - Builds prompt from history
- Function: `build_initial_context()` - Adds system prompt and environment context

**History Flow:**
```
Timeline Items (baseline + deltas)
    ↓
Filtered History (removes ephemeral, legacy items)
    ↓
User Instructions
    ↓
Environment Context (v2 with timeline approach)
    ↓
Final Prompt (Vec<ResponseItem>)
```

### Reasoning Infrastructure (for Extended Thinking)

**Key Files:**
- `code-rs/protocol/src/models.rs` - `ReasoningItem`, `ReasoningItemContent` types
- `code-rs/tui/src/history_cell/reasoning.rs` - Collapsible UI cells
- `code-rs/core/src/history/state.rs` - `ReasoningState` persistence

**Existing Types:**
```rust
pub enum ReasoningItemContent {
    ReasoningText { text: String },  // Main reasoning content
    Text { text: String },           // Regular text
}

pub enum ReasoningItemReasoningSummary {
    SummaryText { text: String },    // Single-line summary
}
```

**Mapping Strategy:** Anthropic `thinking` blocks → `ReasoningItem::ReasoningText`

---

## OAuth Authentication

### OpenCode Reference Implementation

**Client ID (public):** `9d1c250a-e61b-44d9-88ed-5944d1962f5e`

**OAuth Endpoints:**
- Authorization: `https://claude.ai/oauth/authorize`
- Token Exchange: `https://console.anthropic.com/v1/oauth/token`
- Callback: `https://console.anthropic.com/oauth/callback`

**Scopes:**
- `user:inference` - Make API requests
- `org:create_api_key` - Create API keys
- `user:profile` - Access profile

### Token Storage

**Location:** `~/.code/auth.json`

```json
{
  "anthropic": {
    "access_token": "sk-ant-...",
    "refresh_token": "...",
    "expires_at": 1735689600000,
    "provider": "anthropic"
  }
}
```

### Required Headers for API Requests

> [!TIP]
> **RESOLVED (2025-12-30):** Anthropic subscription OAuth tokens now work correctly!
> 
> **Root Cause:** The `-m claude-*` flag only sets the model name, but the `model_provider_id` 
> defaulted to `"openai"`, causing requests to go to the wrong API with the Anthropic token.
>
> **Fix:** Implemented provider auto-detection in `config.rs` (lines 2633-2656):
> - `claude-*` → `anthropic` provider
> - `gemini-*` → `google` provider
> - `qwen-*` → `openrouter` provider
> 
> **Code Linkage:**
> ```
> model_presets.rs (id: "anthropic", model: "claude-sonnet-4-5")
>         ↓ preset.id matches provider key
> model_provider_info.rs ("anthropic" → wire_api: WireApi::Anthropic)
>         ↓ provider.wire_api routing
> client.rs:430-454 (WireApi::Anthropic → stream_anthropic_messages())
>         ↓ function call
> anthropic_completions.rs (stream_anthropic_messages() - actual API call)
> ```

**For Subscription OAuth (Claude Pro/Max):**
```http
POST https://api.anthropic.com/v1/messages?beta=true
Authorization: Bearer {oauth_access_token}
anthropic-version: 2023-06-01
anthropic-beta: oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14
content-type: application/json
User-Agent: claude-code/20250219
```

**For API Keys (Developer/Console):**
```http
POST https://api.anthropic.com/v1/messages
x-api-key: {api_key}
anthropic-version: 2023-06-01
content-type: application/json
```

### All Discovered Endpoints (from Claude Code source)

| Endpoint | Purpose |
|----------|---------|
| `https://api.anthropic.com/api/oauth/claude` | **Subscription OAuth Messages API** |
| `https://api.anthropic.com/v1/messages` | API Key Messages API (NOT for OAuth!) |
| `https://claude.ai/oauth/authorize` | OAuth Authorization |
| `https://console.anthropic.com/v1/oauth/token` | Token Exchange/Refresh |
| `https://console.anthropic.com/oauth/code/callback` | OAuth Callback |
| `https://api.anthropic.com/api/hello` | Health check |
| `https://api.anthropic.com/api/organization/` | Organization info |

### Token Refresh Flow

```rust
// Before each API request:
if expires_at < now() {
    POST https://console.anthropic.com/v1/oauth/token
    {
        "grant_type": "refresh_token",
        "refresh_token": stored_refresh_token,
        "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
    }
    // Update stored tokens
}
```

---

## API Translation Specifications

### Request Format Translation

**OpenAI → Anthropic:**

| OpenAI Field | Anthropic Field | Notes |
|--------------|-----------------|-------|
| `model` | `model` | Direct mapping |
| `messages[]` | `messages[]` | Same structure |
| `tools[].type` | (removed) | Anthropic doesn't nest |
| `tools[].function.name` | `tools[].name` | Unnest |
| `tools[].function.description` | `tools[].description` | Unnest |
| `tools[].function.parameters` | `tools[].input_schema` | Rename |
| `parallel_tool_calls` | (omitted) | Always parallel for Anthropic |
| (add) | `max_tokens` | Required in Anthropic |
| (add) | `thinking` | Optional extended thinking |

**Tool Schema Translation:**
```rust
// OpenAI format
{
    "type": "function",
    "function": {
        "name": "shell",
        "description": "Execute shell command",
        "parameters": {
            "type": "object",
            "properties": { "command": { "type": "string" } }
        }
    }
}

// Anthropic format
{
    "name": "shell",
    "description": "Execute shell command",
    "input_schema": {
        "type": "object",
        "properties": { "command": { "type": "string" } }
    }
}
```

### Response Format Translation

**Anthropic → OpenAI (internal format):**

```rust
// Anthropic response
{
    "id": "msg_123",
    "role": "assistant",
    "content": [
        { "type": "text", "text": "Hello" },
        { "type": "tool_use", "id": "toolu_abc", "name": "shell", "input": {...} }
    ],
    "stop_reason": "end_turn"
}

// Convert to ResponseItem
ResponseItem::Message {
    role: "assistant".to_string(),
    content: vec![
        ContentItem::OutputText { text: "Hello".to_string() },
        // Tool use becomes FunctionCall
    ],
    // Also emit ResponseItem::FunctionCall for tool_use blocks
}
```

**Tool Result Translation:**
```rust
// OpenAI format (internal)
ResponseItem::FunctionCallOutput {
    call_id: "call_abc".to_string(),
    output: FunctionCallOutputPayload { ... }
}

// Anthropic format (external)
{
    "role": "user",
    "content": [
        {
            "type": "tool_result",
            "tool_use_id": "toolu_abc",
            "content": "Result text"
        }
    ]
}
```

### Streaming Event Mapping

**Anthropic SSE Events → ResponseEvent:**

| Anthropic Event | ResponseEvent | Notes |
|-----------------|---------------|-------|
| `message_start` | (internal) | Initialize stream state |
| `content_block_start` (type: `text`) | (internal) | Track content block index |
| `content_block_delta` (type: `text_delta`) | `OutputTextDelta` | Stream text |
| `content_block_start` (type: `thinking`) | (internal) | Start reasoning |
| `content_block_delta` (type: `thinking_delta`) | `ReasoningContentDelta` | Stream thinking |
| `content_block_delta` (type: `signature_delta`) | (internal) | For verification |
| `content_block_start` (type: `tool_use`) | (internal) | Start tool call |
| `content_block_delta` (type: `input_json_delta`) | (accumulate) | Build tool args |
| `content_block_stop` | Emit `OutputItemDone` | Finalize block |
| `message_delta` | (internal) | Track stop reason, usage |
| `message_stop` | `Completed` | Final event |

**Tool Call Accumulation:**
```rust
// Anthropic streams tool args as partial JSON strings
// Need to accumulate and parse when content_block_stop received
let mut tool_args = String::new();
match delta {
    ContentBlockDelta::InputJson { partial_json } => {
        tool_args.push_str(&partial_json);
    }
    ContentBlockStop => {
        let args: Value = serde_json::from_str(&tool_args)?;
        // Emit FunctionCall ResponseItem
    }
}
```

### Extended Thinking Mapping

**Anthropic `thinking` → `ReasoningItem`:**

```rust
// Anthropic response with thinking
{
    "content": [
        {
            "type": "thinking",
            "thinking": "Let me analyze this step by step...",
            "signature": "..."
        },
        { "type": "text", "text": "Based on analysis..." }
    ]
}

// Convert to ResponseItem::Reasoning
ResponseItem::Reasoning {
    id: generate_id(),
    summary: vec![
        ReasoningItemReasoningSummary::SummaryText {
            text: first_line(&thinking_content).to_string()
        }
    ],
    content: Some(vec![
        ReasoningItemContent::ReasoningText {
            text: thinking_content.to_string()
        }
    ]),
    encrypted_content: None,
}
```

**Request Format:**
```rust
// Add thinking to Anthropic request
{
    "thinking": {
        "type": "enabled",
        "budget_tokens": 10000
    }
}
```

### Error Mapping

**Anthropic Errors → CodexErr:**

| Anthropic Error Type | HTTP Status | CodexErr Mapping |
|---------------------|-------------|------------------|
| `authentication_error` | 401 | `CodexErr::Auth(RefreshTokenError::Permanent)` |
| `permission_error` | 403 | `CodexErr::UnexpectedResponse { status: 403, ... }` |
| `not_found_error` | 404 | `CodexErr::UnexpectedResponse { status: 404, ... }` |
| `rate_limit_error` | 429 | `CodexErr::RetryLimitReached { retryable: true, ... }` |
| `invalid_request_error` | 400 | `CodexErr::UnexpectedResponse { status: 400, ... }` |
| `overloaded_error` | 529 | `CodexErr::RetryLimitReached { retryable: true, ... }` |
| `api_error` | 500 | `CodexErr::RetryLimitReached { retryable: true, ... }` |

---

## Implementation Tasks

### TASK-001: Create code-auth Crate for OAuth

**Files:**
- `code-rs/auth/Cargo.toml` (new)
- `code-rs/auth/src/lib.rs` (new)

**Description:** Create a new crate for OAuth authentication flow.

**Dependencies:**
- `serde` / `serde_json` for JSON handling
- `tokio` for async
- `sha2` for PKCE hashing
- `base64` for encoding
- `rand` for code verifier generation

**Implementation:**
```rust
// code-rs/auth/src/lib.rs
pub mod oauth;
pub mod pkce;
pub mod storage;

pub use oauth::OAuthClient;
pub use storage::TokenStorage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64, // Unix timestamp
}

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("token expired")]
    TokenExpired,
    #[error("refresh failed: {0}")]
    RefreshFailed(String),
}
```

**Acceptance Criteria:**
- Crate compiles successfully
- Exports `OAuthClient`, `TokenStorage`, `OAuthTokens`
- `./build-fast.sh` passes (no errors, no warnings)

---

### TASK-002: Implement PKCE Generation

**Files:**
- `code-rs/auth/src/pkce.rs` (new)

**Description:** Implement PKCE (Proof Key for Code Exchange) for secure OAuth flow.

**Specification:**
```rust
use rand::Rng;
use sha2::{Sha256, Digest};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

pub struct Pkce {
    pub code_verifier: String,
    pub code_challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        // Generate random code verifier (128 bytes)
        let verifier: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(128)
            .map(char::from)
            .collect();

        // Create challenge: BASE64URL(SHA256(verifier))
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        Self { code_verifier: verifier, code_challenge: challenge }
    }
}
```

**Acceptance Criteria:**
- `code_verifier` is 128 characters, alphanumeric
- `code_challenge` is valid BASE64URL of SHA256 hash
- Unit tests verify challenge generation

---

### TASK-003: Implement Browser OAuth Flow

**Files:**
- `code-rs/auth/src/oauth.rs` (new)

**Description:** Handle browser-based OAuth flow with Anthropic.

**Constants:**
```rust
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/callback";
const SCOPE: &str = "user:inference org:create_api_key user:profile";
```

**Implementation:**
```rust
use std::process::Command;

pub struct OAuthClient {
    pub client: reqwest::Client,
    pub storage: Arc<TokenStorage>,
}

impl OAuthClient {
    pub fn start_auth_flow(&self) -> Result<OAuthTokens, OAuthError> {
        let pkce = Pkce::new();

        // Build authorization URL
        let url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256",
            AUTH_URL, ANTHROPIC_CLIENT_ID, REDIRECT_URI, SCOPE, pkce.code_challenge
        );

        // Open browser
        if cfg!(target_os = "macos") {
            Command::new("open").arg(&url).spawn()?;
        } else if cfg!(target_os = "linux") {
            Command::new("xdg-open").arg(&url).spawn()?;
        } else {
            Command::new("cmd").args(["/C", "start", "", &url]).spawn()?;
        }

        // Wait for callback (need to spin up a local HTTP server or poll)
        // For now, prompt user to paste the callback URL
        println!("Visit the URL in your browser and paste the callback URL here:");
        let mut callback = String::new();
        std::io::stdin().read_line(&mut callback)?;

        // Extract code from callback URL
        let code = extract_code(&callback)?;

        // Exchange for tokens
        self.exchange_code(code, pkce.code_verifier).await
    }

    async fn exchange_code(&self, code: String, verifier: String) -> Result<OAuthTokens, OAuthError> {
        let body = serde_json::json!({
            "code": code,
            "grant_type": "authorization_code",
            "client_id": ANTHROPIC_CLIENT_ID,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier
        });

        let resp = self.client.post(TOKEN_URL).json(&body).send().await?;
        let tokens: OAuthTokens = resp.json().await?;

        self.storage.save_tokens(&tokens).await?;
        Ok(tokens)
    }
}
```

**Acceptance Criteria:**
- Opens browser to correct OAuth URL
- Extracts code from callback
- Exchanges for tokens
- Saves tokens to storage

---

### TASK-004: Token Storage and Refresh

**Files:**
- `code-rs/auth/src/storage.rs` (new)

**Description:** Store OAuth tokens and handle automatic refresh.

**Implementation:**
```rust
use std::path::PathBuf;
use tokio::fs;

const AUTH_FILE: &str = ".code/auth.json";

pub struct TokenStorage {
    auth_path: PathBuf,
}

impl TokenStorage {
    pub fn new() -> Self {
        let home = std::env::var("HOME").expect("HOME not set");
        let auth_path = PathBuf::from(home).join(AUTH_FILE);
        Self { auth_path }
    }

    pub async fn save_tokens(&self, tokens: &OAuthTokens) -> Result<(), OAuthError> {
        if let Some(parent) = self.auth_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let data = serde_json::json!({
            "anthropic": {
                "access_token": tokens.access_token,
                "refresh_token": tokens.refresh_token,
                "expires_at": tokens.expires_at
            }
        });

        fs::write(&self.auth_path, serde_json::to_string_pretty(&data)?).await?;
        Ok(())
    }

    pub async fn load_tokens(&self) -> Result<OAuthTokens, OAuthError> {
        let content = fs::read_to_string(&self.auth_path).await?;
        let data: serde_json::Value = serde_json::from_str(&content)?;

        let anthropic = &data["anthropic"];
        Ok(OAuthTokens {
            access_token: anthropic["access_token"].as_str().ok_or("missing access_token")?.to_string(),
            refresh_token: anthropic["refresh_token"].as_str().ok_or("missing refresh_token")?.to_string(),
            expires_at: anthropic["expires_at"].as_u64().ok_or("missing expires_at")?,
        })
    }

    pub async fn get_access_token(&self) -> Result<String, OAuthError> {
        let mut tokens = self.load_tokens().await?;

        if tokens.expires_at < now_ms() {
            tokens = self.refresh_tokens(&tokens.refresh_token).await?;
            self.save_tokens(&tokens).await?;
        }

        Ok(tokens.access_token)
    }

    async fn refresh_tokens(&self, refresh_token: &str) -> Result<OAuthTokens, OAuthError> {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": ANTHROPIC_CLIENT_ID
        });

        let resp = client.post(TOKEN_URL).json(&body).send().await?;
        let tokens: OAuthTokens = resp.json().await?;
        Ok(tokens)
    }
}
```

**Acceptance Criteria:**
- Tokens saved to `~/.code/auth.json`
- Auto-refresh on expiry
- Thread-safe for concurrent access

---

### TASK-005: Add CLI Auth Commands

**Files:**
- `code-rs/cli/src/main.rs` (modify)
- `code-rs/cli/src/cmd/auth.rs` (new)

**Description:** Add CLI commands for OAuth authentication.

**Implementation:**
```rust
// code-rs/cli/src/cmd/auth.rs
use clap::Subcommand;

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Login to Claude with OAuth (Pro/Max subscription)
    Login,
    /// Show current authentication status
    Status,
    /// Logout and clear stored credentials
    Logout,
}

pub async fn handle_auth(cmd: AuthCommand) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        AuthCommand::Login => {
            let storage = TokenStorage::new();
            let oauth = OAuthClient::new(storage);
            oauth.start_auth_flow().await?;
            println!("✓ Successfully authenticated with Claude!");
        }
        AuthCommand::Status => {
            let storage = TokenStorage::new();
            match storage.load_tokens().await {
                Ok(tokens) => {
                    let expiry = DateTime::from_timestamp(tokens.expires_at as i64, 0);
                    println!("✓ Authenticated as Claude user");
                    println!("  Token expires: {}", expiry.format("%Y-%m-%d %H:%M"));
                }
                Err(_) => println!("✗ Not authenticated. Run 'code auth login'"),
            }
        }
        AuthCommand::Logout => {
            let storage = TokenStorage::new();
            storage.clear_tokens().await?;
            println!("✓ Logged out");
        }
    }
    Ok(())
}
```

**Acceptance Criteria:**
- `code auth login` initiates OAuth flow
- `code auth status` shows auth state
- `code auth logout` clears credentials

---

### TASK-006: Create code-anthropic Crate

**Files:**
- `code-rs/anthropic/Cargo.toml` (new)
- `code-rs/anthropic/src/lib.rs` (new)

**Cargo.toml:**
```toml
[package]
name = "code-anthropic"
version = "0.1.0"
edition = "2024"

[dependencies]
code-auth = { path = "../auth" }
code-protocol = { path = "../protocol" }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
reqwest = { version = "0.12", features = ["stream", "json"] }
tokio = { version = "1.0", features = ["full"] }
tokio-stream = "0.1"
futures = "0.3"
thiserror = "2.0"
async-stream = "0.3"
```

**Acceptance Criteria:**
- Crate compiles
- Basic module structure in place

---

### TASK-007: Anthropic Request Types

**Files:**
- `code-rs/anthropic/src/types.rs` (new)

**Description:** Define request types for Anthropic Messages API.

**Implementation:**
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
    ToolResult { tool_use_id: String, content: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
}

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Auto { type: String }, // "auto"
    Any { type: String },  // "any"
    None { type: String }, // "none"
    Tool { type: String, name: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Thinking {
    pub r#type: String, // "enabled"
    pub budget_tokens: u32,
}
```

**Acceptance Criteria:**
- All request types defined
- Serializes to correct Anthropic format

---

### TASK-008: Anthropic Response Types

**Files:**
- `code-rs/anthropic/src/types.rs` (append)

**Description:** Define response types for Anthropic Messages API.

**Implementation:**
```rust
#[derive(Debug, Clone, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

// SSE Event Types
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SseEvent {
    MessageStart { message: MessageStart },
    ContentBlockStart { index: usize, content_block: ContentBlock },
    ContentBlockDelta { index: usize, delta: Delta },
    ContentBlockStop { index: usize },
    MessageDelta { delta: MessageDelta, usage: Option<Usage> },
    MessageStop,
    Error { error: ErrorResponse },
    Ping,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageStart {
    pub id: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}
```

**Acceptance Criteria:**
- All response types defined
- Deserializes from Anthropic SSE format

---

### TASK-009: SSE Streaming Parser

**Files:**
- `code-rs/anthropic/src/streaming.rs` (new)

**Description:** Parse Anthropic's SSE format into structured events.

**Implementation:**
```rust
use futures::stream::{Stream, StreamExt};
use reqwest::Response;
use async_stream::stream;

pub struct AnthropicStream {
    inner: Pin<Box<dyn Stream<Item = Result<SseEvent, StreamError>> + Send>>,
}

impl AnthropicStream {
    pub fn new(response: Response) -> Self {
        let stream = response.bytes_stream().map(|result| {
            result.map_err(StreamError::Transport).and_then(|bytes| {
                let text = String::from_utf8(bytes.to_vec())
                    .map_err(StreamError::Utf8)?;
                parse_sse_line(&text)
            })
        });

        Self { inner: Box::pin(stream) }
    }
}

fn parse_sse_line(line: &str) -> Result<SseEvent, StreamError> {
    // Parse SSE format: "event: <type>\ndata: <json>"
    let lines: Vec<&str> = line.lines().collect();

    let event_type = lines.iter()
        .find(|l| l.starts_with("event:"))
        .map(|l| l.trim_start_matches("event:").trim());

    let data = lines.iter()
        .find(|l| l.starts_with("data:"))
        .map(|l| l.trim_start_matches("data:").trim());

    match (event_type, data) {
        (Some(et), Some(d)) => {
            let json: SseEvent = serde_json::from_str(d)
                .map_err(|e| StreamError::Parse(e.to_string()))?;
            Ok(json)
        }
        _ => Err(StreamError::Parse("Invalid SSE format".to_string())),
    }
}
```

**Acceptance Criteria:**
- Correctly parses SSE events
- Handles all event types
- Error recovery on malformed events

---

### TASK-010: HTTP Client with Bearer Auth

**Files:**
- `code-rs/anthropic/src/client.rs` (new)

**Description:** Implement HTTP client with OAuth Bearer token authentication.

**Implementation:**
```rust
use reqwest::Client;
use code_auth::TokenStorage;

const BASE_URL: &str = "https://api.anthropic.com/v1";
const API_VERSION: &str = "2023-06-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";

pub struct AnthropicClient {
    http: Client,
    storage: Arc<TokenStorage>,
}

impl AnthropicClient {
    pub fn new(storage: Arc<TokenStorage>) -> Self {
        Self {
            http: Client::new(),
            storage,
        }
    }

    async fn get_headers(&self) -> Result<HeaderMap, ApiError> {
        let token = self.storage.get_access_token().await?;
        let mut headers = HeaderMap::new();

        headers.insert("anthropic-version", API_VERSION.parse().unwrap());
        headers.insert("anthropic-beta", OAUTH_BETA.parse().unwrap());
        headers.insert("Authorization", format!("Bearer {}", token).parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        Ok(headers)
    }

    pub async fn stream(&self, request: MessagesRequest) -> impl Stream<Item = Result<SseEvent, ApiError>> {
        let headers = self.get_headers().await.unwrap();
        let url = format!("{}/messages", BASE_URL);

        let resp = self.http
            .post(&url)
            .headers(headers)
            .json(&request)
            .send()
            .await
            .unwrap();

        AnthropicStream::new(resp).inner
    }
}
```

**Acceptance Criteria:**
- Includes required headers
- Auto-refreshes tokens
- Handles auth errors

---

### TASK-011: Tool Format Translator

**Files:**
- `code-rs/anthropic/src/translation.rs` (new)

**Description:** Convert between OpenAI and Anthropic tool formats.

**Implementation:**
```rust
// OpenAI → Anthropic
pub fn translate_tools_to_anthropic(openai_tools: &[Value]) -> Vec<Tool> {
    openai_tools.iter().map(|t| {
        let function = &t["function"];
        Tool {
            name: function["name"].as_str().unwrap().to_string(),
            description: function["description"].as_str().unwrap().to_string(),
            input_schema: function["parameters"].clone(),
        }
    }).collect()
}

// Anthropic → OpenAI (for internal processing)
pub fn translate_tool_to_openai(tool_use: &ContentBlock) -> Value {
    if let ContentBlock::ToolUse { id, name, input } = tool_use {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": name,
                "arguments": serde_json::to_string(input).unwrap()
            },
            "id": id
        })
    } else {
        panic!("Not a tool_use block");
    }
}

// OpenAI tool result → Anthropic format
pub fn translate_tool_result_to_anthropic(call_id: &str, output: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: call_id.to_string(),
        content: output.to_string(),
    }
}
```

**Acceptance Criteria:**
- Correctly transforms tool definitions
- Handles nested vs flat structure
- Preserves JSON schemas

---

### TASK-012: Response Format Translator

**Files:**
- `code-rs/anthropic/src/translation.rs` (append)

**Description:** Convert Anthropic responses to internal `ResponseItem` format.

**Implementation:**
```rust
use code_protocol::{ResponseItem, ContentItem};

pub fn translate_anthropic_response(
    response: MessagesResponse,
) -> Vec<ResponseItem> {
    let mut items = Vec::new();

    for block in response.content {
        match block {
            ContentBlock::Text { text } => {
                items.push(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText { text }],
                });
            }
            ContentBlock::ToolUse { id, name, input } => {
                items.push(ResponseItem::FunctionCall {
                    id: None,
                    name,
                    arguments: serde_json::to_string(&input).unwrap(),
                    call_id: id,
                });
            }
            _ => {}
        }
    }

    items
}

// For conversation history: convert ResponseItem back to Anthropic format
pub fn translate_history_to_anthropic(messages: Vec<ResponseItem>) -> Vec<Message> {
    messages.into_iter().map(|item| match item {
        ResponseItem::Message { role, content, .. } => {
            let content_blocks = content.into_iter().map(|c| match c {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    ContentBlock::Text { text }
                }
                ContentItem::InputImage { image_url } => {
                    ContentBlock::Image { source: ImageSource { url: image_url } }
                }
            }).collect();

            Message { role, content: content_blocks }
        }
        ResponseItem::FunctionCallOutput { call_id, output } => {
            Message {
                role: "user".to_string(),
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: call_id,
                        content: output.content,
                    }
                ],
            }
        }
        // ... handle other variants
        _ => unimplemented!()
    }).collect()
}
```

**Acceptance Criteria:**
- Correctly maps content array to message format
- Handles tool results
- Preserves conversation order

---

### TASK-013: Event Mapper for Streaming

**Files:**
- `code-rs/anthropic/src/events.rs` (new)

**Description:** Map Anthropic SSE events to internal `ResponseEvent`.

**Implementation:**
```rust
use codex_api::ResponseEvent;

pub fn map_sse_to_response_event(
    event: SseEvent,
    accumulated: &mut StreamState,
) -> Option<ResponseEvent> {
    match event {
        SseEvent::ContentBlockDelta { index, delta } => {
            match delta {
                Delta::TextDelta { text } => {
                    Some(ResponseEvent::OutputTextDelta(text))
                }
                Delta::InputJsonDelta { partial_json } => {
                    accumulated.add_tool_arg(index, partial_json);
                    None
                }
                Delta::ThinkingDelta { thinking } => {
                    Some(ResponseEvent::ReasoningContentDelta {
                        delta: thinking,
                        content_index: index as i64,
                    })
                }
                _ => None,
            }
        }
        SseEvent::ContentBlockStop { index } => {
            if let Some(args) = accumulated.finish_tool(index) {
                Some(ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                    id: None,
                    name: accumulated.tool_name(index),
                    arguments: args,
                    call_id: accumulated.tool_id(index),
                }))
            } else {
                Some(ResponseEvent::OutputItemDone(ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText {
                        text: accumulated.text(index)
                    }],
                }))
            }
        }
        SseEvent::MessageStop => {
            Some(ResponseEvent::Completed {
                response_id: accumulated.message_id,
                token_usage: accumulated.usage,
            })
        }
        _ => None,
    }
}

struct StreamState {
    message_id: String,
    tool_args: HashMap<usize, String>,
    text_buffers: HashMap<usize, String>,
    tool_ids: HashMap<usize, String>,
    tool_names: HashMap<usize, String>,
    usage: Option<TokenUsage>,
}
```

**Acceptance Criteria:**
- Correctly emits ResponseEvent types
- Accumulates partial JSON for tool args
- Tracks message state across events

---

### TASK-014: Extended Thinking Integration

**Files:**
- `code-rs/anthropic/src/events.rs` (append)

**Description:** Map Anthropic thinking blocks to reasoning infrastructure.

**Implementation:**
```rust
use code_protocol::{ReasoningItem, ReasoningItemContent, ReasoningItemReasoningSummary};

pub fn map_thinking_to_reasoning(
    thinking_content: &str,
    signature: Option<&str>,
) -> ResponseItem {
    // Generate summary from first line
    let summary_text = thinking_content
        .lines()
        .filter(|l| !l.is_empty())
        .next()
        .unwrap_or("Thinking...")
        .to_string();

    ResponseItem::Reasoning {
        id: generate_id(),
        summary: vec![
            ReasoningItemReasoningSummary::SummaryText { text: summary_text }
        ],
        content: Some(vec![
            ReasoningItemContent::ReasoningText {
                text: thinking_content.to_string()
            }
        ]),
        encrypted_content: signature.map(|s| s.to_string()),
    }
}

// For streaming: emit reasoning deltas
pub fn map_thinking_delta(delta: &str, content_index: i64) -> ResponseEvent {
    ResponseEvent::ReasoningContentDelta {
        delta: delta.to_string(),
        content_index,
    }
}
```

**Acceptance Criteria:**
- Thinking blocks display in collapsible UI
- Streaming thinking works
- Signature stored for verification

---

### TASK-015: Add WireApi::Anthropic

**Files:**
- `code-rs/core/src/model_provider_info.rs` (modify)

**Description:** Add Anthropic as a Wire API variant.

**Changes:**
```rust
// Find existing WireApi enum and add:
pub enum WireApi {
    Responses,
    Chat,
    Compact,
    Anthropic,  // NEW
}
```

**Acceptance Criteria:**
- Enum compiles
- All match statements updated

---

### TASK-016: Add Anthropic Provider Definition

**Files:**
- `code-rs/core/src/model_provider_info.rs` (modify)

**Description:** Define Anthropic provider in built-in providers.

**Changes:**
```rust
// In built_in_model_providers() function:
pub fn built_in_model_providers() -> HashMap<String, ModelProviderInfo> {
    let mut providers = HashMap::new();

    // ... existing providers ...

    providers.insert("anthropic".to_string(), ModelProviderInfo {
        name: "Anthropic".to_string(),
        base_url: Some("https://api.anthropic.com/v1".to_string()),
        env_key: None, // Uses OAuth instead
        wire_api: WireApi::Anthropic,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: Some(4),
        stream_max_retries: Some(5),
        stream_idle_timeout_ms: Some(300000),
        requires_openai_auth: false,
        // For OAuth
        oauth_provider: Some(OAuthProvider::Anthropic),
    });

    providers
}
```

**Acceptance Criteria:**
- Provider in built-in list
- Config can reference `[model_providers.anthropic]`

---

### TASK-017: Update ModelClient for Anthropic

**Files:**
- `code-rs/core/src/client.rs` (modify)

**Description:** Route Anthropic wire API to new client.

**Changes:**
```rust
// In ModelClient::stream() method:
match self.provider.wire_api {
    WireApi::Responses => {
        // Existing codex-api client
        codex_api::stream(&self.prompt, ...).await
    }
    WireApi::Chat => {
        // Existing chat completions
        chat_completions::stream(...).await
    }
    WireApi::Anthropic => {
        // NEW: Route to Anthropic client
        let anthropic = AnthropicClient::new(self.auth_manager.clone());
        anthropic.stream(translate_prompt(&self.prompt)).await
    }
    _ => unimplemented!(),
}
```

**Acceptance Criteria:**
- Anthropic requests route correctly
- Translation layer invoked
- Stream integrates with event system

---

### TASK-018: Add Claude Model Definitions

**Files:**
- `code-rs/core/src/model_family.rs` (modify)

**Description:** Define Claude 4.5 model families.

**Changes:**
```rust
// Add to model families:
ModelFamily {
    id: "claude-opus-4-5".to_string(),
    display_name: "Claude Opus 4.5".to_string(),
    supports_parallel_tool_calls: true,
    supports_extended_thinking: true,
    max_output_tokens: 8192,
    context_window: 200000,
    provider: "anthropic".to_string(),
}

ModelFamily {
    id: "claude-sonnet-4-5".to_string(),
    display_name: "Claude Sonnet 4.5".to_string(),
    supports_parallel_tool_calls: true,
    supports_extended_thinking: true,
    max_output_tokens: 8192,
    context_window: 200000,
    provider: "anthropic".to_string(),
}

ModelFamily {
    id: "claude-haiku-4-5".to_string(),
    display_name: "Claude Haiku 4.5".to_string(),
    supports_parallel_tool_calls: true,
    supports_extended_thinking: true,
    max_output_tokens: 8192,
    context_window: 200000,
    provider: "anthropic".to_string(),
}
```

**Acceptance Criteria:**
- Models available via `-m`
- Correct capabilities set

---

### TASK-019: Extended Thinking Configuration

**Files:**
- `code-rs/core/src/config.rs` (modify)

**Description:** Add config options for extended thinking.

**Changes:**
```rust
// In ConfigProfile struct:
pub struct ConfigProfile {
    // ... existing fields ...
    pub model_extended_thinking: Option<bool>,
    pub model_thinking_budget: Option<u32>,
}

// In profile loading:
if let Some(thinking_enabled) = profile.model_extended_thinking {
    request.thinking = Some(Thinking {
        type: "enabled".to_string(),
        budget_tokens: profile.model_thinking_budget.unwrap_or(10000),
    });
}
```

**Acceptance Criteria:**
- `extended_thinking: true` in profile enables thinking
- Budget is configurable
- Default is disabled

---

### TASK-020: Example Profile Configurations

**Files:**
- `~/.code/config.toml` (user config)

**Description:** Document example Claude profiles.

**Example Config:**
```toml
# ~/.code/config.toml

[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
wire_api = "anthropic"

[profiles.claude-opus]
model = "claude-opus-4-5-20251101"
model_provider = "anthropic"
model_extended_thinking = true
model_thinking_budget = 10000

[profiles.claude-sonnet]
model = "claude-sonnet-4-5-20250929"
model_provider = "anthropic"
model_extended_thinking = true
model_thinking_budget = 5000

[profiles.claude-haiku]
model = "claude-haiku-4-5-20251001"
model_provider = "anthropic"
model_extended_thinking = false
```

**Acceptance Criteria:**
- Profiles load correctly
- `-p claude-opus` uses Anthropic
- Thinking settings apply

---

### TASK-021: Auto-Drive Integration

**Files:**
- `code-rs/code-auto-drive-core/` (verify compatibility)

**Description:** Ensure Anthropic provider works with auto-drive mode.

**Key Points:**
- Auto-drive is provider-agnostic - it operates at the `Codex` level
- The main requirement is that streaming events work correctly
- Tool calls must emit `ResponseEvent::OutputItemDone` events
- Approval requests flow through the same event system

**Verification:**
```rust
// Auto-drive only requires:
// 1. Stream emits ResponseEvent events
// 2. Tool calls generate FunctionCall items
// 3. Approval events are emitted
// All of these are handled by the translation layer
```

**Acceptance Criteria:**
- Auto-drive can use `-p claude-opus`
- Tool approvals work
- Stream errors handled correctly

---

### TASK-022: Integration Tests

**Files:**
- `code-rs/anthropic/tests/integration_test.rs` (new)

**Description:** Write comprehensive integration tests.

**Test Cases:**
```rust
#[tokio::test]
async fn test_oauth_flow() {
    // Mock OAuth endpoints
    // Test PKCE generation
    // Test token exchange
}

#[tokio::test]
async fn test_tool_translation() {
    let openai_tools = create_sample_tools();
    let anthropic_tools = translate_tools_to_anthropic(&openai_tools);

    assert_eq!(anthropic_tools[0].name, "shell");
    assert!(anthropic_tools[0].input_schema.is_object());
}

#[tokio::test]
async fn test_response_translation() {
    let anthropic_resp = create_sample_response();
    let response_items = translate_anthropic_response(anthropic_resp);

    assert!(matches!(response_items[0], ResponseItem::Message { .. }));
}

#[tokio::test]
async fn test_thinking_to_reasoning() {
    let thinking = "Let me analyze this...";
    let reasoning = map_thinking_to_reasoning(thinking, None);

    assert!(matches!(reasoning, ResponseItem::Reasoning { .. }));
}
```

**Acceptance Criteria:**
- All tests pass
- Coverage for translation layer
- Mock OAuth flow tested

---

### TASK-023: End-to-End Test

**Description:** Test with real Claude subscription.

**Test Plan:**
1. Run `code auth login` with real subscription
2. Run `code -p claude-opus`
3. Send a simple request
4. Verify response
5. Test tool use (e.g., `shell` tool)
6. Test extended thinking

**Acceptance Criteria:**
- Full request/response cycle works
- Tools execute correctly
- Thinking displays in UI
- No errors or warnings

---

## Auto-Drive Integration

### How Auto-Drive Works

**Reference:** `code-rs/code-auto-drive-core/`

Auto-drive is a **provider-agnostic** automation system that:
1. Monitors `ResponseEvent` stream
2. Detects when tool calls need approval
3. Can automatically approve based on policy
4. Continues execution without user intervention

### Integration Requirements

**Good News:** Auto-drive requires NO provider-specific code!

The integration point is the **event system**, which we already map:

```
Anthropic SSE Events
        ↓
    Event Mapper (TASK-013)
        ↓
    ResponseEvent (internal)
        ↓
    Auto-Drive Controller
        ↓
    Approval / Execution
```

**Required Events:**
- `OutputItemDone` → For completed tool calls
- `OutputTextDelta` → For streaming text
- `Error` → For error handling
- `ExecApprovalRequest` → For tool approval (generated from tool calls)

All of these are already mapped in TASK-013.

### Testing Auto-Drive with Claude

```bash
# Start auto-drive with Claude
code -p claude-opus --auto-drive

# Claude should:
# 1. Receive requests
# 2. Call tools
# 3. Auto-approve based on policy
# 4. Continue until task complete
```

---

## File Changes Summary

### New Files
```
code-rs/
├── auth/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── oauth.rs
│       ├── pkce.rs
│       └── storage.rs
├── anthropic/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── types.rs
│       ├── client.rs
│       ├── streaming.rs
│       ├── translation.rs
│       └── events.rs
└── cli/src/cmd/
    └── auth.rs
```

### Modified Files
```
code-rs/core/src/
├── model_provider_info.rs  (add Anthropic, WireApi::Anthropic)
├── client.rs                (route Anthropic requests)
├── config.rs                (extended thinking config)
└── model_family.rs          (Claude model definitions)
```

---

## References

- [OpenCode GitHub](https://github.com/sst/opencode) - OAuth auth reference
- [Anthropic Messages API](https://docs.anthropic.com/en/api/messages)
- [Extended Thinking](https://docs.anthropic.com/en/build-with-claude/extended-thinking)
- [Claude Agent SDK](https://www.anthropic.com/engineering/building-agents-with-the-claude-agent-sdk)

---

**Document Version:** 2.0
**Status:** Implementation-Ready
**Total Tasks:** 23
**Estimated Effort:** 4-6 weeks
