//! Claude Pro/Max OAuth — port of Oh My Pi `registry/oauth/anthropic.ts`.
//!
//! Browser PKCE against claude.ai. Tokens go to `vendor-auth.json`.
//! Inference uses Bearer + `anthropic-beta: oauth-2025-04-20`, never `x-api-key`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rand::RngCore;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use super::callback::{error_html, read_http_request, success_html, write_html};
use super::pkce::generate_pkce;
use super::{OAuthPending, parse_authorization_input};
use crate::compat::auth_store::VendorAuthStore;
use crate::compat::probe::VendorLoginError;

pub(super) const PROVIDER_ID: &str = "anthropic-claude";
/// Claude Code public client (`atob("OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl")`).
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// OMP uses api.anthropic.com (not platform.claude.com) so the grant includes
/// `user:inference`. Console tokens from platform.claude.com cannot call Messages.
const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "http://localhost:54545/callback";
const CALLBACK_PATH: &str = "/callback";
const BIND_ADDR: &str = "127.0.0.1:54545";
const SCOPE: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const CLAUDE_CLI_UA: &str = "claude-cli/2.1.220 (external, claude-desktop)";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
const REFRESH_SKEW_SECS: i64 = 5 * 60;

struct InFlight {
    verifier: String,
    state: String,
    manual_tx: mpsc::Sender<String>,
    manual_rx: Mutex<Option<mpsc::Receiver<String>>>,
    cancel_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    cancel_rx: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    listener: Mutex<Option<TcpListener>>,
}

static INFLIGHT: OnceLock<Mutex<HashMap<String, InFlight>>> = OnceLock::new();
static REFRESH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn inflight_map() -> std::sync::MutexGuard<'static, HashMap<String, InFlight>> {
    INFLIGHT
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub(super) fn login_label() -> &'static str {
    "Sign in with Claude Pro/Max"
}

pub(super) async fn begin() -> Result<OAuthPending, VendorLoginError> {
    cancel(PROVIDER_ID);

    let pkce = generate_pkce();
    let state = random_state();
    let listener = match TcpListener::bind(BIND_ADDR).await {
        Ok(listener) => Some(listener),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "Claude OAuth port 54545 is busy; paste the redirect URL after sign-in"
            );
            None
        }
    };
    let listening = listener.is_some();

    let mut authorize =
        reqwest::Url::parse(AUTHORIZE_URL).map_err(|e| VendorLoginError::Probe(e.to_string()))?;
    authorize
        .query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);

    let (manual_tx, manual_rx) = mpsc::channel::<String>(1);
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    inflight_map().insert(
        PROVIDER_ID.to_owned(),
        InFlight {
            verifier: pkce.verifier,
            state,
            manual_tx,
            manual_rx: Mutex::new(Some(manual_rx)),
            cancel_tx: Mutex::new(Some(cancel_tx)),
            cancel_rx: Mutex::new(Some(cancel_rx)),
            listener: Mutex::new(listener),
        },
    );

    let authorize_url = authorize.to_string();
    let _ = webbrowser::open(&authorize_url);

    let instructions = if listening {
        "Complete Claude sign-in in the browser. If it opened on another machine, paste the localhost:54545 redirect URL."
    } else {
        "Port 54545 is in use (often Claude Code). Sign in in the browser, then paste the localhost:54545 redirect URL."
    };

    Ok(OAuthPending {
        provider_id: PROVIDER_ID.to_owned(),
        authorize_url,
        instructions: instructions.into(),
    })
}

pub(super) fn submit_manual(provider_id: &str, input: String) -> Result<(), VendorLoginError> {
    let map = inflight_map();
    let Some(session) = map.get(provider_id) else {
        return Err(VendorLoginError::Probe("no in-flight OAuth login".into()));
    };
    session
        .manual_tx
        .try_send(input)
        .map_err(|_| VendorLoginError::Probe("OAuth login is no longer waiting".into()))?;
    Ok(())
}

pub(super) fn cancel(provider_id: &str) {
    let mut map = inflight_map();
    if let Some(session) = map.remove(provider_id)
        && let Ok(mut tx) = session.cancel_tx.lock()
        && let Some(tx) = tx.take()
    {
        let _ = tx.send(());
    }
}

pub(super) async fn wait_completion(provider_id: &str) -> Result<(), VendorLoginError> {
    if provider_id != PROVIDER_ID {
        return Err(VendorLoginError::UnknownProvider(provider_id.to_owned()));
    }

    let (verifier, expected_state, listener, mut manual_rx, mut cancel_rx) = {
        let map = inflight_map();
        let session = map
            .get(provider_id)
            .ok_or_else(|| VendorLoginError::Probe("no in-flight OAuth login".into()))?;
        let listener = session
            .listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let manual_rx = session
            .manual_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| VendorLoginError::Probe("OAuth callback already running".into()))?;
        let cancel_rx = session
            .cancel_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .ok_or_else(|| VendorLoginError::Probe("OAuth callback already running".into()))?;
        (
            session.verifier.clone(),
            session.state.clone(),
            listener,
            manual_rx,
            cancel_rx,
        )
    };

    let outcome = tokio::select! {
        biased;
        _ = &mut cancel_rx => Err(VendorLoginError::Probe("OAuth login cancelled".into())),
        _ = tokio::time::sleep(LOGIN_TIMEOUT) => {
            Err(VendorLoginError::Probe("Claude OAuth login timed out".into()))
        }
        manual = manual_rx.recv() => {
            let Some(input) = manual else {
                return finish_err(provider_id, VendorLoginError::Probe("OAuth login cancelled".into()));
            };
            match parse_claude_input(&input) {
                Ok((code, state)) => {
                    if let Some(state) = state
                        && state != expected_state
                    {
                        return finish_err(
                            provider_id,
                            VendorLoginError::Probe("OAuth state mismatch".into()),
                        );
                    }
                    exchange_and_store(&verifier, &code, &expected_state).await
                }
                Err(msg) => finish_err(provider_id, VendorLoginError::Probe(msg)),
            }
        }
        accepted = async {
            match listener.as_ref() {
                Some(listener) => Some(listener.accept().await),
                None => std::future::pending().await,
            }
        } => {
            match accepted {
                Some(Ok((mut stream, _))) => {
                    let req = read_http_request(&mut stream).await.unwrap_or_default();
                    match callback_code(&req, &expected_state) {
                        Ok(code) => {
                            match exchange_and_store(&verifier, &code, &expected_state).await {
                                Ok(()) => {
                                    let _ = write_html(
                                        &mut stream,
                                        200,
                                        success_html("Signed in to Claude Pro/Max"),
                                    )
                                    .await;
                                    Ok(())
                                }
                                Err(e) => {
                                    let _ = write_html(
                                        &mut stream,
                                        502,
                                        error_html("Claude token exchange failed.", &e.to_string()),
                                    )
                                    .await;
                                    Err(e)
                                }
                            }
                        }
                        Err(msg) => {
                            let _ = write_html(&mut stream, 400, error_html(&msg, "")).await;
                            Err(VendorLoginError::Probe(msg))
                        }
                    }
                }
                Some(Err(e)) => Err(VendorLoginError::Probe(format!("OAuth callback accept failed: {e}"))),
                None => Err(VendorLoginError::Probe("OAuth callback is not listening".into())),
            }
        }
    };

    inflight_map().remove(provider_id);
    outcome
}

fn finish_err(provider_id: &str, err: VendorLoginError) -> Result<(), VendorLoginError> {
    inflight_map().remove(provider_id);
    Err(err)
}

async fn exchange_and_store(
    verifier: &str,
    code: &str,
    state: &str,
) -> Result<(), VendorLoginError> {
    let token = exchange_authorization_code(code, state, verifier).await?;
    persist_token(token)
}

struct ClaudeToken {
    access: String,
    refresh: String,
    expires: i64,
    account_id: Option<String>,
}

async fn exchange_authorization_code(
    code: &str,
    state: &str,
    verifier: &str,
) -> Result<ClaudeToken, VendorLoginError> {
    let client = reqwest::Client::builder()
        .timeout(TOKEN_TIMEOUT)
        .build()
        .map_err(|e| VendorLoginError::Probe(e.to_string()))?;
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": CLIENT_ID,
            "code": code,
            "state": state,
            "redirect_uri": REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .send()
        .await
        .map_err(|e| VendorLoginError::Probe(format!("Claude token exchange: {e}")))?;
    read_token_response(resp, "exchange").await
}

async fn read_token_response(
    resp: reqwest::Response,
    operation: &str,
) -> Result<ClaudeToken, VendorLoginError> {
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(token_http_error(operation, status, &body));
    }
    token_from_json(&body, operation)
}

fn token_http_error(
    operation: &str,
    status: reqwest::StatusCode,
    body: &Value,
) -> VendorLoginError {
    let detail = body
        .get("error_description")
        .or_else(|| body.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    VendorLoginError::Probe(format!(
        "Claude token {operation} failed (HTTP {status}){extra}",
        extra = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

fn token_from_json(body: &Value, operation: &str) -> Result<ClaudeToken, VendorLoginError> {
    let access = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            VendorLoginError::Probe(format!(
                "Claude token {operation} response missing access_token"
            ))
        })?;
    let refresh = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            VendorLoginError::Probe(format!(
                "Claude token {operation} response missing refresh_token"
            ))
        })?;
    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|n| n as i64)))
        .unwrap_or(3600);
    let account_id = body
        .get("account")
        .and_then(|v| v.get("uuid"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(ClaudeToken {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires: now.saturating_add(expires_in.max(0)),
        account_id,
    })
}

fn persist_token(token: ClaudeToken) -> Result<(), VendorLoginError> {
    let mut store = VendorAuthStore::default_store()?;
    store.set_oauth(
        PROVIDER_ID,
        token.access,
        Some(token.refresh),
        Some(token.expires),
        token.account_id,
    )?;
    Ok(())
}

pub(super) fn refresh_if_needed() -> Result<(), VendorLoginError> {
    let _guard = REFRESH_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let store = VendorAuthStore::default_store()?;
    let Some(cred) = store.credential(PROVIDER_ID).cloned() else {
        return Ok(());
    };
    if cred.kind != "oauth" || !cred.expires_soon(REFRESH_SKEW_SECS) {
        return Ok(());
    }
    let Some(refresh) = cred.refresh.filter(|s| !s.trim().is_empty()) else {
        return Err(VendorLoginError::Probe(
            "Claude Pro/Max session expired; run /provider-login anthropic-claude".into(),
        ));
    };
    let token = refresh_access_token_blocking(&refresh)?;
    persist_token(token)
}

fn refresh_access_token_blocking(refresh: &str) -> Result<ClaudeToken, VendorLoginError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(TOKEN_TIMEOUT)
        .build()
        .map_err(|e| VendorLoginError::Probe(e.to_string()))?;
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("anthropic-beta", OAUTH_BETA)
        .header(
            "User-Agent",
            "anthropic-sdk-typescript/0.94.0 userOAuthProvider",
        )
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": refresh,
        }))
        .send()
        .map_err(|e| VendorLoginError::Probe(format!("Claude token refresh: {e}")))?;
    let status = resp.status();
    let body: Value = resp.json().unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(token_http_error("refresh", status, &body));
    }
    token_from_json(&body, "refresh")
}

/// OAuth Messages requests must not send `x-api-key`.
pub(super) fn inject_request_headers(headers: &mut indexmap::IndexMap<String, String>) {
    headers
        .entry("anthropic-version".into())
        .or_insert_with(|| "2023-06-01".into());
    headers.entry("anthropic-beta".into()).or_insert_with(|| {
        format!("{OAUTH_BETA},claude-code-20250219,interleaved-thinking-2025-05-14")
    });
    headers
        .entry("User-Agent".into())
        .or_insert_with(|| CLAUDE_CLI_UA.to_owned());
    headers
        .entry("anthropic-dangerous-direct-browser-access".into())
        .or_insert_with(|| "true".into());
    headers
        .entry("x-app".into())
        .or_insert_with(|| "cli".into());
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
        out
    })
}

fn parse_claude_input(input: &str) -> Result<(String, Option<String>), String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("Missing authorization code".into());
    }
    if let Ok(url) = reqwest::Url::parse(value) {
        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Claude returned no authorization code.".to_string())?;
        let mut state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned());
        if state.is_none()
            && let Some((_, frag)) = code.split_once('#')
        {
            state = Some(frag.to_owned());
        }
        let code = code.split('#').next().unwrap_or(&code).to_owned();
        return Ok((code, state));
    }
    if let Some((code, state)) = value.split_once('#')
        && !code.is_empty()
    {
        return Ok((code.to_owned(), Some(state.to_owned())));
    }
    parse_authorization_input(value)
        .filter(|s| !s.is_empty())
        .map(|c| (c, None))
        .ok_or_else(|| "Missing authorization code".to_string())
}

fn callback_code(request: &str, expected_state: &str) -> Result<String, String> {
    let first = request
        .lines()
        .next()
        .ok_or_else(|| "empty OAuth callback".to_string())?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if method != "GET" {
        return Err("OAuth callback route not found.".into());
    }
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| "OAuth callback route not found.".to_string())?;
    if url.path() != CALLBACK_PATH {
        return Err("OAuth callback route not found.".into());
    }
    if let Some(err) = url.query_pairs().find(|(k, _)| k == "error") {
        let desc = url
            .query_pairs()
            .find(|(k, _)| k == "error_description")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_else(|| err.1.into_owned());
        return Err(format!("Claude authorization failed: {desc}"));
    }
    if let Some(state) = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
        && state != expected_state
    {
        return Err("OAuth state mismatch".into());
    }
    let raw = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.into_owned())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Claude returned no authorization code.".to_string())?;
    Ok(raw.split('#').next().unwrap_or(&raw).to_owned())
}

#[cfg(test)]
mod tests {
    use super::{CLIENT_ID, parse_claude_input};

    #[test]
    fn client_id_is_claude_code_public_app() {
        assert_eq!(CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    #[test]
    fn parse_code_hash_state() {
        let (code, state) = parse_claude_input("abc#xyz").unwrap();
        assert_eq!(code, "abc");
        assert_eq!(state.as_deref(), Some("xyz"));
    }
}
