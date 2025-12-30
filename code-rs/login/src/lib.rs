mod device_code_auth;
mod pkce;
mod server;

pub use device_code_auth::{run_device_code_login, run_rfc_device_code_login, DeviceCodeSession, RfcDeviceCodeSession};
pub use pkce::{generate_pkce, PkceCodes};
pub use server::LoginServer;
pub use server::OAuthProvider;
pub use server::ServerOptions;
pub use server::ShutdownHandle;
pub use server::run_login_server;
pub use manual_auth::{build_manual_auth_url, exchange_manual_auth_code, generate_state};

mod manual_auth {
    use std::io::{self};
    use std::path::Path;

    use super::OAuthProvider;
    use super::pkce::PkceCodes;

    /// Generate a random state parameter for OAuth
    pub fn generate_state() -> String {
        use base64::Engine;
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Build an authorization URL for manual copy-paste flow (headless authentication)
    pub fn build_manual_auth_url(
        provider: OAuthProvider,
        pkce: &PkceCodes,
        state: &str,
        originator: &str,
    ) -> String {
        let client_id = provider.client_id();
        let scopes = provider.scopes();
        let auth_path = provider.authorize_path();
        // Use provider-specific redirect URI
        let redirect_uri = provider.manual_code_redirect_uri();

        let issuer = provider.issuer();

        let mut query = vec![
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("scope", scopes),
            ("code_challenge", &pkce.code_challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
        ];

        // Add originator for ChatGPT
        if matches!(provider, OAuthProvider::ChatGPT) {
            query.push(("originator", originator));
        }

        let qs = query
            .into_iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        format!("{}{}?{}", issuer, auth_path, qs)
    }

    /// Exchange an authorization code (obtained via manual copy-paste) for tokens
    pub async fn exchange_manual_auth_code(
        provider: OAuthProvider,
        code_home: &Path,
        code: &str,
        pkce: &PkceCodes,
        state: &str,
    ) -> io::Result<()> {
        let redirect_uri = provider.manual_code_redirect_uri();

        match provider {
            OAuthProvider::AnthropicSubscription => {
                // Anthropic subscription flow uses different token endpoint and format
                exchange_anthropic_subscription_code(code_home, code, pkce, state).await
            }
            _ => {
                // Use existing token exchange function for other providers
                let tokens = super::server::exchange_code_for_tokens(
                    provider.issuer(),
                    provider.client_id(),
                    redirect_uri,
                    pkce,
                    code,
                    provider,
                )
                .await?;

                // Persist tokens
                super::server::persist_tokens_async(
                    code_home,
                    None, // No API key for manual flow
                    tokens.id_token,
                    tokens.access_token,
                    tokens.refresh_token,
                )
                .await
            }
        }
    }

    /// Exchange an authorization code for Anthropic subscription (Pro/Max) tokens
    ///
    /// Anthropic's subscription flow:
    /// - Code format: `code#state` (state is appended after #)
    /// - Token endpoint: https://console.anthropic.com/v1/oauth/token
    /// - Response includes: access_token, refresh_token, expires_in (no id_token)
    async fn exchange_anthropic_subscription_code(
        code_home: &Path,
        code: &str,
        pkce: &PkceCodes,
        _state: &str,
    ) -> io::Result<()> {
        use chrono::Utc;
        use code_core::auth::write_auth_json;
        use code_core::auth::AuthDotJson;
        use code_core::auth::get_auth_file;
        use code_core::token_data::TokenData;
        use serde::Deserialize;

        #[derive(Deserialize)]
        struct AnthropicTokenResponse {
            access_token: String,
            refresh_token: String,
            expires_in: u64,
        }

        // Parse code#state format
        let splits: Vec<&str> = code.split('#').collect();
        let auth_code = splits.first().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Authorization code is empty")
        })?;
        // Note: state is included in the code but we don't need to validate it separately
        // since the PKCE verifier is already bound to the original request

        let client = code_core::http_client::build_http_client();
        let resp = client
            .post("https://console.anthropic.com/v1/oauth/token")
            .header("Content-Type", "application/json")
            .body(serde_json::json!({
                "code": auth_code,
                "grant_type": "authorization_code",
                "client_id": OAuthProvider::AnthropicSubscription.client_id(),
                "redirect_uri": OAuthProvider::AnthropicSubscription.manual_code_redirect_uri(),
                "code_verifier": pkce.code_verifier,
            }).to_string())
            .send()
            .await
            .map_err(io::Error::other)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_text = resp.text().await.unwrap_or_else(|_| "Unable to read error".to_string());
            return Err(io::Error::other(
                format!("Anthropic token endpoint returned status {}: {}", status, error_text)
            ));
        }

        let token_resp: AnthropicTokenResponse = resp
            .json()
            .await
            .map_err(io::Error::other)?;

        // Create a minimal id_token for compatibility (Anthropic doesn't provide one)
        // We'll use a JWT-like format with the account email
        let _expires_at = Utc::now() + chrono::Duration::seconds(token_resp.expires_in as i64);
        use base64::Engine;
        let id_token = format!("anthropic_subscription_{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&token_resp.access_token));

        let tokens = TokenData {
            id_token: code_core::token_data::parse_id_token(&id_token).map_err(io::Error::other)?,
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            account_id: None,
        };

        // Persist to auth.json
        let auth_file = get_auth_file(code_home);
        if let Some(parent) = auth_file.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(io::Error::other)?;
            }
        }

        let auth = AuthDotJson {
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: Some(Utc::now()),
        };
        write_auth_json(&auth_file, &auth)?;

        Ok(())
    }
}

// Re-export commonly used auth types and helpers from codex-core for compatibility
pub use code_app_server_protocol::AuthMode;
pub use code_core::AuthManager;
pub use code_core::CodexAuth;
pub use code_core::auth::AuthDotJson;
pub use code_core::auth::CLIENT_ID;
pub use code_core::auth::CODEX_API_KEY_ENV_VAR;
pub use code_core::auth::OPENAI_API_KEY_ENV_VAR;
pub use code_core::auth::get_auth_file;
pub use code_core::auth::login_with_api_key;
pub use code_core::auth::logout;
pub use code_core::auth::try_read_auth_json;
pub use code_core::auth::write_auth_json;
pub use code_core::token_data::TokenData;
