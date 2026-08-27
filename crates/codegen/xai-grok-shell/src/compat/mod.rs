//! Multi-provider catalog, API-key store, and credential merge.
//!
//! GROK_COMPAT: first-party xAI auth (`AuthManager` / `auth.json`) is
//! untouched. Vendor keys live in `~/.grok/vendor-auth.json`.

mod auth_store;
mod catalog;
pub mod custom;
pub mod oauth;
mod probe;
pub mod reasoning;

pub use auth_store::{VendorAuthStore, VendorCredential};
pub use catalog::{
    ProviderSpec, acp_models_for_provider, arg_items, builtin_providers, merge_vendor_catalog,
    provider_by_id, provider_display_name,
};
pub use probe::{VendorLoginError, login_with_api_key, logout_provider, probe_api_key};

use crate::agent::config::ModelEntry;

/// Look up a vendor API key for a catalog model (`model_family` = provider id).
pub fn vendor_key_for_model(model: &ModelEntry) -> Option<String> {
    let family = model.info.model_family.as_deref()?;
    if !is_known_vendor(family) {
        return None;
    }
    oauth::ensure_fresh(family);
    VendorAuthStore::default_store()
        .ok()?
        .api_key(family)
        .filter(|k| !k.trim().is_empty())
}

pub fn is_known_vendor(id: &str) -> bool {
    provider_by_id(id).is_some() || custom::is_custom_provider(id)
}

pub fn is_vendor_catalog_model(model: &ModelEntry) -> bool {
    model
        .info
        .model_family
        .as_deref()
        .is_some_and(is_known_vendor)
}

/// Builtin or custom provider whose `base_url` owns this sampling URL.
pub fn vendor_id_for_base_url(base_url: &str) -> Option<String> {
    catalog::provider_id_for_base_url(base_url)
        .map(str::to_owned)
        .or_else(|| custom::provider_id_for_base_url(base_url))
}

/// Refresh-on-read vendor bearer for a live request. `None` if the URL is
/// not a vendor endpoint or the sidecar has no usable secret.
pub fn live_vendor_key_for_url(base_url: &str) -> Option<String> {
    let id = vendor_id_for_base_url(base_url)?;
    oauth::ensure_fresh(&id);
    VendorAuthStore::default_store()
        .ok()?
        .api_key(&id)
        .filter(|k| !k.trim().is_empty())
}

pub fn vendor_auth_user_message(provider_id: &str) -> String {
    let name = provider_display_name(provider_id);
    format!(
        "Authentication required: {name} rejected this session. Run /provider-login {provider_id}, then resend your message."
    )
}

#[cfg(test)]
mod tests {
    use super::{vendor_auth_user_message, vendor_id_for_base_url};

    #[test]
    fn vendor_id_for_codex_url() {
        assert_eq!(
            vendor_id_for_base_url("https://chatgpt.com/backend-api/codex").as_deref(),
            Some("openai-codex")
        );
        assert_eq!(vendor_id_for_base_url("https://api.x.ai/v1"), None);
    }

    #[test]
    fn vendor_auth_message_points_at_provider_login() {
        let msg = vendor_auth_user_message("openai-codex");
        assert!(msg.contains("/provider-login openai-codex"), "{msg}");
        assert!(!msg.contains("/login "), "{msg}");
    }
}
