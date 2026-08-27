//! Third-party provider login / logout (GROK_COMPAT). Does not use xAI `/login`.

use super::ctx::{get_active_agent_mut, with_active_agent};
use crate::acp::model_state::ModelState;
use crate::app::actions::Effect;
use crate::app::app_view::AppView;
use crate::views::vendor_login_modal::VendorLoginState;
use xai_grok_shell::sampling::types::{
    REASONING_EFFORT_META_KEY, REASONING_EFFORTS_META_KEY, SUPPORTS_REASONING_EFFORT_META_KEY,
    parse_reasoning_effort_meta, reasoning_effort_meta_value, reasoning_efforts_meta_value,
};

pub(super) fn dispatch_open_vendor_login(
    app: &mut AppView,
    provider_id: Option<String>,
) -> Vec<Effect> {
    let state = match provider_id {
        Some(id) if id == "custom" || id == "add" => VendorLoginState::custom_form(),
        Some(id) => {
            if let Some(spec) = xai_grok_shell::compat::provider_by_id(&id) {
                VendorLoginState::for_provider(spec.id.to_owned(), spec.name.to_owned())
            } else if xai_grok_shell::compat::custom::get_provider(&id).is_some() {
                VendorLoginState::edit_custom(id)
            } else if xai_grok_shell::compat::is_known_vendor(&id) {
                VendorLoginState::for_provider(id.clone(), id)
            } else {
                app.show_toast(&format!("Unknown provider '{id}'"));
                return vec![];
            }
        }
        None => VendorLoginState::picker(),
    };
    let Some(agent) = get_active_agent_mut(app) else {
        app.show_toast("Open a session, then run /provider-login");
        return vec![];
    };
    agent.vendor_login = Some(state);
    vec![]
}

pub(super) fn dispatch_close_vendor_login(app: &mut AppView) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        if let Some(id) = agent
            .vendor_login
            .as_ref()
            .and_then(|s| s.oauth_provider_id().map(str::to_owned))
        {
            xai_grok_shell::compat::oauth::cancel(&id);
        }
        agent.vendor_login = None;
    });
    vec![]
}

pub(super) fn dispatch_start_vendor_oauth(app: &mut AppView, provider_id: String) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        if let Some(state) = agent.vendor_login.as_mut() {
            state.probing = true;
            state.error = None;
        }
    });
    vec![Effect::VendorOAuthStart { provider_id }]
}

pub(super) fn dispatch_submit_vendor_oauth_code(
    app: &mut AppView,
    provider_id: String,
    code: String,
) -> Vec<Effect> {
    if let Err(error) = xai_grok_shell::compat::oauth::submit_manual(&provider_id, &code) {
        if let Some(agent) = get_active_agent_mut(app)
            && let Some(state) = agent.vendor_login.as_mut()
        {
            state.probing = false;
            state.error = Some(error.to_string());
        } else {
            app.show_toast(&error.to_string());
        }
        return vec![];
    }
    with_active_agent(app, |agent| {
        if let Some(state) = agent.vendor_login.as_mut() {
            state.probing = true;
            state.error = None;
        }
    });
    vec![]
}

pub(super) fn dispatch_sync_vendor_custom(
    app: &mut AppView,
    base_url: String,
    api_backend: String,
    auth_scheme: String,
    key: String,
) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        if let Some(state) = agent.vendor_login.as_mut() {
            state.probing = true;
            state.error = None;
        }
    });
    vec![Effect::VendorCustomSync {
        base_url,
        api_backend,
        auth_scheme,
        key,
    }]
}

pub(super) fn dispatch_save_vendor_custom(
    app: &mut AppView,
    provider_id: Option<String>,
    name: String,
    base_url: String,
    api_backend: String,
    auth_scheme: String,
    key: String,
    models: Vec<(String, String, u64, bool)>,
) -> Vec<Effect> {
    with_active_agent(app, |agent| {
        if let Some(state) = agent.vendor_login.as_mut() {
            state.probing = true;
            state.error = None;
        }
    });
    vec![Effect::VendorCustomSave {
        provider_id,
        name,
        base_url,
        api_backend,
        auth_scheme,
        key,
        models,
    }]
}

pub(super) fn handle_vendor_custom_synced(
    app: &mut AppView,
    models: Vec<(String, String, u64)>,
    error: Option<String>,
) -> Vec<Effect> {
    if let Some(agent) = get_active_agent_mut(app)
        && let Some(state) = agent.vendor_login.as_mut()
    {
        state.apply_custom_models(models, error);
    } else if let Some(err) = error {
        app.show_toast(&err);
    }
    vec![]
}

pub(super) fn handle_vendor_oauth_pending(
    app: &mut AppView,
    provider_id: String,
    authorize_url: String,
    instructions: String,
) -> Vec<Effect> {
    if let Some(agent) = get_active_agent_mut(app)
        && let Some(state) = agent.vendor_login.as_mut()
    {
        state.enter_oauth_wait(authorize_url, instructions);
        return vec![Effect::VendorOAuthWait { provider_id }];
    }
    xai_grok_shell::compat::oauth::cancel(&provider_id);
    vec![]
}

pub(super) fn dispatch_submit_vendor_key(
    _app: &mut AppView,
    provider_id: String,
    key: String,
) -> Vec<Effect> {
    vec![Effect::VendorProbe { provider_id, key }]
}

pub(super) fn dispatch_vendor_logout(app: &mut AppView, provider_id: String) -> Vec<Effect> {
    let _ = app;
    vec![Effect::VendorLogoutPersist { provider_id }]
}

pub(super) fn dispatch_sync_models_dev(app: &mut AppView) -> Vec<Effect> {
    app.show_toast("Syncing model metadata from models.dev…");
    vec![Effect::SyncModelsDev]
}

pub(super) fn handle_models_dev_synced(
    app: &mut AppView,
    count: usize,
    error: Option<String>,
) -> Vec<Effect> {
    if let Some(err) = error {
        app.show_toast(&format!("models.dev sync failed: {err}"));
        return vec![];
    }
    patch_model_state_reasoning(&mut app.models);
    for agent in app.agents.values_mut() {
        patch_model_state_reasoning(&mut agent.session.models);
    }
    app.show_toast(&format!("Synced {count} models from models.dev"));
    vec![]
}

fn patch_model_state_reasoning(models: &mut ModelState) {
    for (id, info) in models.available.iter_mut() {
        let key = id.0.as_ref();
        if !key.contains('/') {
            continue;
        }
        let Some(meta) = xai_grok_shell::compat::reasoning::lookup_meta(key) else {
            continue;
        };
        let mut map = info.meta.clone().unwrap_or_default();
        if let Some(menu) = meta.reasoning {
            map.insert(
                SUPPORTS_REASONING_EFFORT_META_KEY.to_string(),
                serde_json::Value::Bool(true),
            );
            map.insert(
                REASONING_EFFORTS_META_KEY.to_string(),
                reasoning_efforts_meta_value(&menu.options),
            );
            if parse_reasoning_effort_meta(Some(&map)).is_none() {
                map.insert(
                    REASONING_EFFORT_META_KEY.to_string(),
                    reasoning_effort_meta_value(menu.default),
                );
            }
        }
        if let Some(ctx) = meta.context_window {
            map.insert(
                "totalContextTokens".to_string(),
                serde_json::Value::Number(ctx.into()),
            );
        }
        info.meta = Some(map);
    }
}

pub(super) fn handle_vendor_login_complete(
    app: &mut AppView,
    provider_id: String,
    error: Option<String>,
) -> Vec<Effect> {
    if let Some(err) = error {
        let cancelled = err.to_lowercase().contains("cancel");
        if let Some(agent) = get_active_agent_mut(app)
            && let Some(state) = agent.vendor_login.as_mut()
        {
            state.probing = false;
            if cancelled {
                state.back_from_oauth();
            } else {
                state.error = Some(err);
            }
        } else if !cancelled {
            app.show_toast(&format!("Sign-in failed: {err}"));
        }
        return vec![];
    }

    let extras = xai_grok_shell::compat::acp_models_for_provider(&provider_id);
    let prefix = format!("{provider_id}/");
    app.models
        .available
        .retain(|id, _| !id.0.starts_with(&prefix));
    app.models.available.extend(extras.clone());
    for agent in app.agents.values_mut() {
        agent
            .session
            .models
            .available
            .retain(|id, _| !id.0.starts_with(&prefix));
        agent.session.models.available.extend(extras.clone());
        agent.vendor_login = None;
    }
    let name = xai_grok_shell::compat::provider_display_name(&provider_id);
    app.show_toast(&format!("Signed in to {name}. Use /model to switch."));
    vec![]
}

pub(super) fn handle_vendor_logout_complete(
    app: &mut AppView,
    provider_id: String,
    removed: bool,
    error: Option<String>,
) -> Vec<Effect> {
    if let Some(err) = error {
        app.show_toast(&format!("Logout failed: {err}"));
        return vec![];
    }
    let prefix = format!("{provider_id}/");
    app.models
        .available
        .retain(|id, _| !id.0.starts_with(&prefix));
    for agent in app.agents.values_mut() {
        agent
            .session
            .models
            .available
            .retain(|id, _| !id.0.starts_with(&prefix));
    }
    let name = xai_grok_shell::compat::provider_display_name(&provider_id);
    if removed {
        app.show_toast(&format!("Signed out of {name}"));
    } else {
        app.show_toast(&format!("No stored key for {name}"));
    }
    vec![]
}
