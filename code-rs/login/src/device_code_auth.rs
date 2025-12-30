use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Deserializer;
use serde::de::{self};
use std::time::Duration;
use std::time::Instant;

use crate::pkce::PkceCodes;
use crate::server::{OAuthProvider, persist_tokens_async, exchange_code_for_tokens, ServerOptions};
use code_browser::global as browser_global;
use code_core::default_client;
use std::io::Write;
use std::io::{self};

#[derive(Deserialize)]
struct UserCodeResp {
    device_auth_id: String,
    #[serde(alias = "user_code", alias = "usercode")]
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_interval")]
    interval: u64,
}

#[derive(Serialize)]
struct UserCodeReq {
    client_id: String,
}

#[derive(Serialize)]
struct TokenPollReq {
    device_auth_id: String,
    user_code: String,
}

fn deserialize_interval<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    s.trim()
        .parse::<u64>()
        .map_err(|e| de::Error::custom(format!("invalid u64 string: {e}")))
}

#[derive(Deserialize)]
struct CodeSuccessResp {
    authorization_code: String,
    code_challenge: String,
    code_verifier: String,
}

// ===== RFC 8628 Device Authorization Grant =====

/// RFC 8628 Device Authorization Response
#[derive(Deserialize)]
struct RfcDeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: String,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_expires_in() -> u64 { 900 } // 15 minutes
fn default_interval() -> u64 { 5 }

/// RFC 8628 Token Request for Device Code Grant
#[derive(Serialize)]
struct RfcDeviceTokenRequest {
    grant_type: String,
    device_code: String,
    client_id: String,
}

/// RFC 8628 Token Response
#[derive(Deserialize)]
struct RfcDeviceTokenResponse {
    access_token: String,
    #[serde(default)]
    id_token: Option<String>,
    refresh_token: String,
}

/// RFC 8628 Token Error Response
#[derive(Deserialize)]
struct RfcDeviceTokenError {
    error: String,
    #[serde(default)]
    error_description: String,
}

/// Request the user code and polling interval.
async fn request_user_code(
    client: &reqwest::Client,
    auth_base_url: &str,
    base_url: &str,
    client_id: &str,
) -> std::io::Result<UserCodeResp> {
    let url = format!("{auth_base_url}/deviceauth/usercode");
    let body = serde_json::to_string(&UserCodeReq {
        client_id: client_id.to_string(),
    })
    .map_err(std::io::Error::other)?;
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(std::io::Error::other)?;

    let status = resp.status();
    let headers = resp.headers().clone();
    let body_text = resp.text().await.map_err(std::io::Error::other)?;

    if !status.is_success() {
        if status == StatusCode::NOT_FOUND {
            return Err(std::io::Error::other(
                "device code login is not enabled for this Codex server. Use the browser login or verify the server URL.",
            ));
        }

        if looks_like_cloudflare_challenge(status, &headers, &body_text) {
            if let Ok(via_browser) = request_user_code_via_browser(base_url, client_id).await {
                return Ok(via_browser);
            }
        }

        return Err(std::io::Error::other(format!(
            "device code request failed with status {}",
            status
        )));
    }

    serde_json::from_str(&body_text).map_err(std::io::Error::other)
}

/// Poll token endpoint until a code is issued or timeout occurs.
async fn poll_for_token(
    client: &reqwest::Client,
    auth_base_url: &str,
    device_auth_id: &str,
    user_code: &str,
    interval: u64,
) -> std::io::Result<CodeSuccessResp> {
    let url = format!("{auth_base_url}/deviceauth/token");
    let max_wait = Duration::from_secs(15 * 60);
    let start = Instant::now();

    loop {
        let body = serde_json::to_string(&TokenPollReq {
            device_auth_id: device_auth_id.to_string(),
            user_code: user_code.to_string(),
        })
        .map_err(std::io::Error::other)?;
        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(std::io::Error::other)?;

        let status = resp.status();

        if status.is_success() {
            return resp.json().await.map_err(std::io::Error::other);
        }

        if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
            if start.elapsed() >= max_wait {
                return Err(std::io::Error::other(
                    "device auth timed out after 15 minutes",
                ));
            }
            let sleep_for = Duration::from_secs(interval).min(max_wait - start.elapsed());
            tokio::time::sleep(sleep_for).await;
            continue;
        }

        return Err(std::io::Error::other(format!(
            "device auth failed with status {}",
            resp.status()
        )));
    }
}

// Helper to print colored text if terminal supports ANSI
fn print_colored_warning_device_code() {
    // ANSI escape code for bright yellow
    const YELLOW: &str = "\x1b[93m";
    const RESET: &str = "\x1b[0m";
    let warning = "WARN!!! device code authentication has potential risks and\n\
        should be used with caution only in cases where browser support \n\
        is missing. This is prone to attacks.\n\
        \n\
        - This code is valid for 15 minutes.\n\
        - Do not share this code with anyone.\n\
        ";
    let mut stdout = io::stdout().lock();
    let _ = write!(stdout, "{YELLOW}{warning}{RESET}");
    let _ = stdout.flush();
}

/// Full device code login flow.
pub async fn run_device_code_login(opts: ServerOptions) -> std::io::Result<()> {
    print_colored_warning_device_code();
    println!("⏳ Generating a new 9-digit device code for authentication...\n");
    let session = DeviceCodeSession::start(opts).await?;

    println!(
        "To authenticate, visit: {} and enter code: {}",
        session.authorize_url(),
        session.user_code()
    );

    session
        .wait_for_tokens()
        .await
        .map_err(|err| std::io::Error::other(format!("device code exchange failed: {err}")))
}

pub struct DeviceCodeSession {
    client: reqwest::Client,
    opts: ServerOptions,
    api_base_url: String,
    base_url: String,
    device_auth_id: String,
    user_code: String,
    interval: u64,
}

impl DeviceCodeSession {
    pub async fn start(opts: ServerOptions) -> std::io::Result<Self> {
        let client = default_client::create_client(&opts.originator);
        let base_url = opts.issuer.trim_end_matches('/').to_string();
        let api_base_url = format!("{}/api/accounts", base_url);
        let uc = request_user_code(&client, &api_base_url, &base_url, &opts.client_id).await?;

        Ok(Self {
            client,
            api_base_url,
            base_url,
            device_auth_id: uc.device_auth_id,
            user_code: uc.user_code,
            interval: uc.interval,
            opts,
        })
    }

    pub fn authorize_url(&self) -> String {
        format!("{}/deviceauth/authorize", self.api_base_url)
    }

    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    pub async fn wait_for_tokens(self) -> std::io::Result<()> {
        let code_resp = poll_for_token(
            &self.client,
            &self.api_base_url,
            &self.device_auth_id,
            &self.user_code,
            self.interval,
        )
        .await?;

        let pkce = PkceCodes {
            code_verifier: code_resp.code_verifier,
            code_challenge: code_resp.code_challenge,
        };
        let redirect_uri = format!("{}/deviceauth/callback", self.base_url);

        let tokens = exchange_code_for_tokens(
            &self.base_url,
            &self.opts.client_id,
            &redirect_uri,
            &pkce,
            &code_resp.authorization_code,
            self.opts.provider,
        )
        .await
        .map_err(|err| std::io::Error::other(format!("device code exchange failed: {err}")))?;

        persist_tokens_async(
            &self.opts.code_home,
            None,
            tokens.id_token,
            tokens.access_token,
            tokens.refresh_token,
        )
        .await
    }
}

fn looks_like_cloudflare_challenge(
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
) -> bool {
    if status != StatusCode::FORBIDDEN {
        return false;
    }

    let lower = body.to_lowercase();
    if lower.contains("cloudflare")
        || lower.contains("cf-ray")
        || lower.contains("_cf_chl_opt")
        || lower.contains("challenge-platform")
        || lower.contains("just a moment")
        || lower.contains("enable javascript and cookies")
    {
        return true;
    }

    headers.get("cf-ray").is_some()
        || headers.get("cf-mitigated").is_some()
        || headers
            .get("server-timing")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_lowercase().contains("chlray"))
            .unwrap_or(false)
        || headers
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|cookie| cookie.contains("__cf_bm="))
            .unwrap_or(false)
}

async fn request_user_code_via_browser(
    base_url: &str,
    client_id: &str,
) -> std::io::Result<UserCodeResp> {
    let issuer = base_url.trim_end_matches('/');
    let authorize_page = format!("{issuer}/codex/device");
    let manager = browser_global::get_or_create_browser_manager().await;

    tokio::time::timeout(Duration::from_secs(30), manager.goto(&authorize_page))
        .await
        .map_err(|_| std::io::Error::other("browser navigation timed out"))?
        .map_err(|err| std::io::Error::other(format!("browser navigation failed: {err}")))?;

    tokio::time::sleep(Duration::from_secs(4)).await;

    let api_url = format!("{issuer}/api/accounts/deviceauth/usercode");
    let payload_literal = serde_json::to_string(&serde_json::json!({ "client_id": client_id }))
        .map_err(std::io::Error::other)?;
    let script = format!(
        r#"(async () => {{
            try {{
                const resp = await fetch("{api_url}", {{
                    method: "POST",
                    credentials: "include",
                    headers: {{ "Content-Type": "application/json" }},
                    body: {payload_literal}
                }});
                const text = await resp.text();
                return {{ ok: resp.ok, status: resp.status, body: text }};
            }} catch (err) {{
                return {{ ok: false, status: 0, body: String(err) }};
            }}
        }})()"#
    );

    for _ in 0..3 {
        let value = tokio::time::timeout(Duration::from_secs(15), manager.execute_javascript(&script))
            .await
            .map_err(|_| std::io::Error::other("browser fetch timed out"))?
            .map_err(|err| std::io::Error::other(format!("browser execution failed: {err}")))?;

        let status = value
            .get("status")
            .and_then(|v| v.as_i64())
            .unwrap_or_default();
        let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let body = value.get("body").and_then(|v| v.as_str()).unwrap_or("");

        if ok {
            return serde_json::from_str(body).map_err(std::io::Error::other);
        }

        if status == 403 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        return Err(std::io::Error::other(format!(
            "device code request failed with status {} while using browser fallback",
            status
        )));
    }

    Err(std::io::Error::other(
        "device code request failed after browser fallback retries",
    ))
}

// ===== RFC 8628 Device Authorization Grant Implementation =====

/// RFC 8628: Request device code from the authorization server
async fn rfc_request_device_code(
    client: &reqwest::Client,
    _issuer: &str,
    client_id: &str,
    provider: OAuthProvider,
) -> std::io::Result<RfcDeviceCodeResponse> {
    // Use device authorization base for Anthropic
    let base = provider.device_authorization_base();
    let device_auth_path = provider.device_authorization_path();
    let url = format!("{base}{device_auth_path}");

    let body = format!(
        "client_id={}",
        urlencoding::encode(client_id)
    );

    let resp = client
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(std::io::Error::other)?;

    let status = resp.status();
    let body_text = resp.text().await.map_err(std::io::Error::other)?;

    if !status.is_success() {
        return Err(std::io::Error::other(format!(
            "device code request failed with status {}: {}",
            status, body_text
        )));
    }

    serde_json::from_str(&body_text).map_err(std::io::Error::other)
}

/// RFC 8628: Poll for token until authorization completes or timeout
async fn rfc_poll_for_token(
    client: &reqwest::Client,
    _issuer: &str,
    device_code: &str,
    client_id: &str,
    interval: u64,
    expires_in: u64,
    provider: OAuthProvider,
) -> std::io::Result<RfcDeviceTokenResponse> {
    // Use device authorization base for Anthropic token polling too
    let base = provider.device_authorization_base();
    let token_path = provider.token_path();
    let url = format!("{base}{token_path}");
    let max_wait = Duration::from_secs(expires_in);
    let start = Instant::now();

    loop {
        if start.elapsed() >= max_wait {
            return Err(std::io::Error::other(
                "device authorization timed out",
            ));
        }

        let body = serde_json::to_string(&RfcDeviceTokenRequest {
            grant_type: "urn:ietf:params:oauth:grant-type:device_code".to_string(),
            device_code: device_code.to_string(),
            client_id: client_id.to_string(),
        })
        .map_err(std::io::Error::other)?;

        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(std::io::Error::other)?;

        let status = resp.status();
        let body_text = resp.text().await.map_err(std::io::Error::other)?;

        // Check for token error response
        if let Ok(err_resp) = serde_json::from_str::<RfcDeviceTokenError>(&body_text) {
            match err_resp.error.as_str() {
                "authorization_pending" => {
                    // User hasn't authorized yet, continue polling
                    let sleep_for = Duration::from_secs(interval).min(max_wait - start.elapsed());
                    tokio::time::sleep(sleep_for).await;
                    continue;
                }
                "slow_down" => {
                    // Poll too fast, increase interval
                    let sleep_for = Duration::from_secs(interval + 5).min(max_wait - start.elapsed());
                    tokio::time::sleep(sleep_for).await;
                    continue;
                }
                "access_denied" | "expired_token" => {
                    return Err(std::io::Error::other(format!(
                        "device authorization failed: {}",
                        err_resp.error
                    )));
                }
                _ => {
                    // Unknown error, might be transient
                    if status.is_client_error() {
                        return Err(std::io::Error::other(format!(
                            "device authorization failed: {} - {}",
                            err_resp.error, err_resp.error_description
                        )));
                    }
                    // For server errors, retry after delay
                    let sleep_for = Duration::from_secs(interval).min(max_wait - start.elapsed());
                    tokio::time::sleep(sleep_for).await;
                    continue;
                }
            }
        }

        if status.is_success() {
            return serde_json::from_str(&body_text).map_err(std::io::Error::other);
        }

        return Err(std::io::Error::other(format!(
            "token request failed with status {}",
            status
        )));
    }
}

/// RFC 8628 Device Code Session for Anthropic
pub struct RfcDeviceCodeSession {
    client: reqwest::Client,
    opts: ServerOptions,
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    interval: u64,
    expires_in: u64,
}

impl RfcDeviceCodeSession {
    pub async fn start(opts: ServerOptions) -> std::io::Result<Self> {
        let client = default_client::create_client(&opts.originator);
        let issuer = opts.issuer.trim_end_matches('/');

        let resp = rfc_request_device_code(&client, issuer, &opts.client_id, opts.provider).await?;

        // Use provider's verification URI if the response doesn't provide one
        let verification_uri = if resp.verification_uri.is_empty() {
            opts.provider.device_verification_uri().to_string()
        } else {
            resp.verification_uri
        };

        Ok(Self {
            client,
            opts,
            device_code: resp.device_code,
            user_code: resp.user_code,
            verification_uri,
            verification_uri_complete: resp.verification_uri_complete,
            interval: resp.interval,
            expires_in: resp.expires_in,
        })
    }

    pub fn verification_uri(&self) -> &str {
        &self.verification_uri
    }

    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    pub fn verification_uri_complete(&self) -> &str {
        &self.verification_uri_complete
    }

    pub async fn wait_for_tokens(self) -> std::io::Result<()> {
        // Use device authorization base for token polling
        let base = self.opts.provider.device_authorization_base();

        let tokens = rfc_poll_for_token(
            &self.client,
            base,
            &self.device_code,
            &self.opts.client_id,
            self.interval,
            self.expires_in,
            self.opts.provider,
        )
        .await?;

        // For Anthropic, id_token may be included or may need to be derived
        let id_token = tokens.id_token.unwrap_or_else(|| tokens.access_token.clone());

        persist_tokens_async(
            &self.opts.code_home,
            None, // Anthropic doesn't do the API key exchange
            id_token,
            tokens.access_token,
            tokens.refresh_token,
        )
        .await
    }
}

/// Run RFC 8628 device code login flow
pub async fn run_rfc_device_code_login(opts: ServerOptions) -> std::io::Result<()> {
    print_colored_warning_device_code();
    println!("⏳ Generating a device code for authentication...\n");
    let session = RfcDeviceCodeSession::start(opts).await?;

    println!("To authenticate, visit:");
    println!("  {}", session.verification_uri());
    println!("\nEnter the code:");
    println!("  {}\n", session.user_code());

    // If there's a complete URI (with code pre-filled), also show it
    if !session.verification_uri_complete().is_empty() {
        println!("Or use this direct link:");
        println!("  {}\n", session.verification_uri_complete());
    }

    println!("Waiting for authorization... (expires in {} minutes)", session.expires_in / 60);

    session
        .wait_for_tokens()
        .await
        .map_err(|err| std::io::Error::other(format!("device code exchange failed: {err}")))
}
