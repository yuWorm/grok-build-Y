//! Pi-shaped vendor OAuth (login / refresh / to_auth).
//!
//! Flows live here, not in `AuthManager`. Tokens go to `vendor-auth.json`.

mod callback;
mod openai_codex;
mod openrouter;
mod pkce;

pub use openrouter::{begin as begin_openrouter, login_label as openrouter_login_label};

use crate::compat::probe::VendorLoginError;

/// UI-facing start of an OAuth login (authorize URL + copy).
#[derive(Debug, Clone)]
pub struct OAuthPending {
    pub provider_id: String,
    pub authorize_url: String,
    pub instructions: String,
}

pub fn has_flow(provider_id: &str) -> bool {
    matches!(provider_id, "openrouter" | "openai-codex")
}

pub fn login_label(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "openrouter" => Some(openrouter_login_label()),
        "openai-codex" => Some(openai_codex::login_label()),
        _ => None,
    }
}

pub async fn begin(provider_id: &str) -> Result<OAuthPending, VendorLoginError> {
    match provider_id {
        "openrouter" => begin_openrouter().await,
        "openai-codex" => openai_codex::begin().await,
        other => Err(VendorLoginError::Probe(format!(
            "OAuth is not wired for '{other}' yet"
        ))),
    }
}

pub async fn wait_completion(provider_id: &str) -> Result<(), VendorLoginError> {
    match provider_id {
        "openrouter" => openrouter::wait_completion(provider_id).await,
        "openai-codex" => openai_codex::wait_completion(provider_id).await,
        other => Err(VendorLoginError::UnknownProvider(other.to_owned())),
    }
}

pub fn submit_manual(provider_id: &str, input: &str) -> Result<(), VendorLoginError> {
    match provider_id {
        "openrouter" => openrouter::submit_manual(provider_id, input.to_owned()),
        "openai-codex" => openai_codex::submit_manual(provider_id, input.to_owned()),
        other => Err(VendorLoginError::UnknownProvider(other.to_owned())),
    }
}

pub fn cancel(provider_id: &str) {
    match provider_id {
        "openrouter" => openrouter::cancel(provider_id),
        "openai-codex" => openai_codex::cancel(provider_id),
        _ => {}
    }
}

/// Refresh expiring OAuth access tokens before a live request.
pub fn ensure_fresh(provider_id: &str) {
    if provider_id == openai_codex::PROVIDER_ID {
        if let Err(error) = openai_codex::refresh_if_needed() {
            tracing::warn!(provider_id, error = %error, "vendor OAuth refresh failed");
        }
    }
}

/// Headers that must ride on every Codex (and similar) request.
pub fn inject_request_headers(provider_id: &str, headers: &mut indexmap::IndexMap<String, String>) {
    if provider_id == openai_codex::PROVIDER_ID {
        openai_codex::inject_request_headers(headers);
    }
}

/// Pull an authorization code out of a pasted redirect URL, `code=` query, or raw code.
pub fn parse_authorization_input(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(url) = reqwest::Url::parse(value)
        && let Some(code) = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
        && !code.is_empty()
    {
        return Some(code);
    }
    if value.contains("code=") {
        let stripped = value.trim_start_matches('?');
        if let Some(code) = url::form_urlencoded::parse(stripped.as_bytes())
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
            && !code.is_empty()
        {
            return Some(code);
        }
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{has_flow, login_label, parse_authorization_input};

    #[test]
    fn parse_raw_code() {
        assert_eq!(
            parse_authorization_input("  abc123  ").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn parse_redirect_url() {
        assert_eq!(
            parse_authorization_input("http://127.0.0.1:9/oauth/callback/x?code=xyz&foo=1")
                .as_deref(),
            Some("xyz")
        );
    }

    #[test]
    fn openai_codex_has_oauth_flow() {
        assert!(has_flow("openai-codex"));
        assert_eq!(
            login_label("openai-codex"),
            Some("Sign in with ChatGPT Plus/Pro (Codex)")
        );
        assert!(!has_flow("openai"));
    }
}
