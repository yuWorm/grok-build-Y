//! Third-party provider login overlay.
//!
//! Chrome, close button, footer shortcuts, and click-outside come from
//! [`super::modal_window`]. Provider picking is a hoverable row list; the
//! API-key field uses [`LineEditor`] with a Settings-style caret.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthStr;

use crate::input::line_editor::{LineEditOutcome, LineEditor};
use crate::render::line_utils::truncate_str;
use crate::theme::Theme;
use crate::views::modal_window::{
    ModalSizing, ModalWindowConfig, ModalWindowOutcome, ModalWindowState, Shortcut,
    handle_modal_key, handle_modal_mouse, render_modal_window,
};

const SHORTCUT_SUBMIT: usize = 1;
const SHORTCUT_CANCEL: usize = 2;
const SHORTCUT_SELECT_ALL: usize = 3;
const SHORTCUT_SELECT_NONE: usize = 4;
const SHORTCUT_SEARCH: usize = 5;
const ADD_CUSTOM_ID: &str = "__add_custom__";
const PROTOCOLS: &[&str] = &["chat_completions", "responses", "messages"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomField {
    Name,
    BaseUrl,
    Protocol,
    Key,
}

#[derive(Debug)]
struct CustomDraft {
    /// Existing custom-provider id when editing; `None` when adding.
    id: Option<String>,
    name: LineEditor,
    base_url: LineEditor,
    key: LineEditor,
    protocol: usize,
    field: CustomField,
    models: Vec<CustomModelPick>,
    selected: usize,
    scroll: usize,
    search: LineEditor,
    search_focused: bool,
}

#[derive(Debug, Clone)]
struct CustomModelPick {
    api_model: String,
    name: String,
    context_window: u64,
    enabled: bool,
}

impl CustomDraft {
    fn new() -> Self {
        let mut base_url = LineEditor::default();
        base_url.set_text("https://");
        Self {
            id: None,
            name: LineEditor::default(),
            base_url,
            key: LineEditor::default(),
            protocol: 0,
            field: CustomField::Name,
            models: Vec::new(),
            selected: 0,
            scroll: 0,
            search: LineEditor::default(),
            search_focused: false,
        }
    }

    fn from_provider(provider: xai_grok_shell::compat::custom::CustomProvider, key: &str) -> Self {
        let mut name = LineEditor::default();
        name.set_text(&provider.name);
        let mut base_url = LineEditor::default();
        base_url.set_text(&provider.base_url);
        let mut key_ed = LineEditor::default();
        if !key.is_empty() {
            key_ed.set_text(key);
        }
        let protocol = PROTOCOLS
            .iter()
            .position(|p| *p == provider.api_backend.as_str())
            .unwrap_or(0);
        let models = provider
            .models
            .iter()
            .map(|m| CustomModelPick {
                api_model: m.api_model.clone(),
                name: m.name.clone(),
                context_window: m.context_window,
                enabled: m.enabled,
            })
            .collect();
        Self {
            id: Some(provider.id),
            name,
            base_url,
            key: key_ed,
            protocol,
            field: CustomField::Name,
            models,
            selected: 0,
            scroll: 0,
            search: LineEditor::default(),
            search_focused: false,
        }
    }

    fn active_editor_mut(&mut self) -> Option<&mut LineEditor> {
        match self.field {
            CustomField::Name => Some(&mut self.name),
            CustomField::BaseUrl => Some(&mut self.base_url),
            CustomField::Key => Some(&mut self.key),
            CustomField::Protocol => None,
        }
    }

    fn cycle_field(&mut self, forward: bool) {
        self.field = match (self.field, forward) {
            (CustomField::Name, true) | (CustomField::Key, false) => CustomField::BaseUrl,
            (CustomField::BaseUrl, true) | (CustomField::Name, false) => CustomField::Protocol,
            (CustomField::Protocol, true) | (CustomField::BaseUrl, false) => CustomField::Key,
            (CustomField::Key, true) | (CustomField::Protocol, false) => CustomField::Name,
        };
    }

    fn filtered_indices(&self) -> Vec<usize> {
        let q = self.search.text().trim().to_ascii_lowercase();
        self.models
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                if q.is_empty() {
                    return true;
                }
                m.api_model.to_ascii_lowercase().contains(&q)
                    || m.name.to_ascii_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn clamp_selection_to_filter(&mut self) {
        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            return;
        }
        if !filtered.contains(&self.selected) {
            self.selected = filtered[0];
            self.scroll = 0;
        }
    }

    fn set_filtered_enabled(&mut self, enabled: bool) {
        let filtered = self.filtered_indices();
        for i in filtered {
            if let Some(row) = self.models.get_mut(i) {
                row.enabled = enabled;
            }
        }
    }

    fn move_selection(&mut self, delta: isize) -> bool {
        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            return false;
        }
        let pos = filtered
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        let next = pos as isize + delta;
        if next < 0 || next >= filtered.len() as isize {
            return false;
        }
        self.selected = filtered[next as usize];
        true
    }
}

#[derive(Debug)]
enum VendorLoginStep {
    Pick {
        selected: usize,
        scroll: usize,
    },
    Method {
        provider_id: String,
        provider_name: String,
        selected: usize,
        from_picker: bool,
    },
    Key {
        provider_id: String,
        provider_name: String,
        requires_auth: bool,
        from_picker: bool,
    },
    OAuthWait {
        provider_id: String,
        provider_name: String,
        authorize_url: String,
        instructions: String,
        from_picker: bool,
    },
    CustomForm,
    CustomModels,
}

#[derive(Debug)]
pub(crate) struct VendorLoginState {
    pub window: ModalWindowState,
    step: VendorLoginStep,
    pub(crate) editor: LineEditor,
    pub error: Option<String>,
    pub probing: bool,
    content_area: Option<Rect>,
    /// Screen Y → provider index, rebuilt each pick-step frame.
    row_map: Vec<(u16, usize)>,
    custom: Option<CustomDraft>,
}

impl VendorLoginState {
    pub(crate) fn picker() -> Self {
        Self {
            window: ModalWindowState::new(),
            step: VendorLoginStep::Pick {
                selected: 0,
                scroll: 0,
            },
            editor: LineEditor::default(),
            error: None,
            probing: false,
            content_area: None,
            row_map: Vec::new(),
            custom: None,
        }
    }

    pub(crate) fn for_provider(provider_id: String, provider_name: String) -> Self {
        let mut state = Self::picker();
        state.enter_provider(provider_id, provider_name, false);
        state
    }

    pub(crate) fn oauth_provider_id(&self) -> Option<&str> {
        match &self.step {
            VendorLoginStep::OAuthWait { provider_id, .. } => Some(provider_id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn enter_oauth_wait(&mut self, authorize_url: String, instructions: String) {
        let (provider_id, provider_name, from_picker) = match &self.step {
            VendorLoginStep::Method {
                provider_id,
                provider_name,
                from_picker,
                ..
            }
            | VendorLoginStep::OAuthWait {
                provider_id,
                provider_name,
                from_picker,
                ..
            }
            | VendorLoginStep::Key {
                provider_id,
                provider_name,
                from_picker,
                ..
            } => (provider_id.clone(), provider_name.clone(), *from_picker),
            VendorLoginStep::Pick { .. }
            | VendorLoginStep::CustomForm
            | VendorLoginStep::CustomModels => return,
        };
        self.step = VendorLoginStep::OAuthWait {
            provider_id,
            provider_name,
            authorize_url,
            instructions,
            from_picker,
        };
        self.editor.reset();
        self.error = None;
        self.probing = false;
    }

    pub(crate) fn apply_custom_models(
        &mut self,
        models: Vec<(String, String, u64)>,
        error: Option<String>,
    ) {
        self.probing = false;
        if let Some(err) = error {
            self.error = Some(err);
            self.step = VendorLoginStep::CustomForm;
            return;
        }
        self.error = None;
        self.step = VendorLoginStep::CustomModels;
        let Some(draft) = self.custom.as_mut() else {
            return;
        };
        let previous: Vec<(String, bool)> = draft
            .models
            .iter()
            .map(|m| (m.api_model.clone(), m.enabled))
            .collect();
        let had_prev = !previous.is_empty();
        draft.models = models
            .into_iter()
            .map(|(api_model, name, context_window)| {
                let enabled = previous
                    .iter()
                    .find(|(id, _)| id == &api_model)
                    .map(|(_, enabled)| *enabled)
                    .unwrap_or(!had_prev);
                CustomModelPick {
                    api_model,
                    name,
                    context_window,
                    enabled,
                }
            })
            .collect();
        draft.selected = 0;
        draft.scroll = 0;
        draft.search_focused = false;
        draft.clamp_selection_to_filter();
    }

    pub(crate) fn custom_form() -> Self {
        let mut state = Self::picker();
        state.enter_custom_form();
        state
    }

    pub(crate) fn edit_custom(provider_id: String) -> Self {
        let mut state = Self::picker();
        state.enter_custom_edit(&provider_id);
        state
    }

    fn enter_custom_form(&mut self) {
        self.custom = Some(CustomDraft::new());
        self.step = VendorLoginStep::CustomForm;
        self.error = None;
        self.probing = false;
        self.row_map.clear();
    }

    fn enter_custom_edit(&mut self, provider_id: &str) {
        let Some(provider) = xai_grok_shell::compat::custom::get_provider(provider_id) else {
            self.enter_custom_form();
            return;
        };
        let key = xai_grok_shell::compat::custom::stored_secret(provider_id).unwrap_or_default();
        let has_models = !provider.models.is_empty();
        self.custom = Some(CustomDraft::from_provider(provider, &key));
        self.step = if has_models {
            VendorLoginStep::CustomModels
        } else {
            VendorLoginStep::CustomForm
        };
        self.error = None;
        self.probing = false;
        self.row_map.clear();
    }

    fn enter_provider(&mut self, provider_id: String, provider_name: String, from_picker: bool) {
        if xai_grok_shell::compat::oauth::has_flow(&provider_id) {
            self.step = VendorLoginStep::Method {
                provider_id,
                provider_name,
                selected: 0,
                from_picker,
            };
            self.editor.reset();
            self.error = None;
            self.probing = false;
            self.row_map.clear();
            return;
        }
        self.enter_key_form(provider_id, provider_name, from_picker);
    }

    fn enter_key_form(&mut self, provider_id: String, provider_name: String, from_picker: bool) {
        let requires_auth = xai_grok_shell::compat::provider_by_id(&provider_id)
            .map(|p| p.requires_auth)
            .unwrap_or(true);
        self.step = VendorLoginStep::Key {
            provider_id,
            provider_name,
            requires_auth,
            from_picker,
        };
        self.editor.reset();
        self.error = None;
        self.probing = false;
        self.row_map.clear();
    }

    fn back_to_picker(&mut self) {
        self.step = VendorLoginStep::Pick {
            selected: 0,
            scroll: 0,
        };
        self.editor.reset();
        self.error = None;
        self.probing = false;
    }

    pub(crate) fn back_from_oauth(&mut self) {
        let (provider_id, provider_name, from_picker) = match &self.step {
            VendorLoginStep::OAuthWait {
                provider_id,
                provider_name,
                from_picker,
                ..
            } => (provider_id.clone(), provider_name.clone(), *from_picker),
            _ => return,
        };
        self.enter_provider(provider_id, provider_name, from_picker);
    }
}

#[derive(Debug)]
pub(crate) enum VendorLoginOutcome {
    Unchanged,
    Changed,
    Submit {
        provider_id: String,
        key: String,
    },
    StartOAuth {
        provider_id: String,
    },
    SubmitOAuthCode {
        provider_id: String,
        code: String,
    },
    SyncCustom {
        name: String,
        base_url: String,
        api_backend: String,
        auth_scheme: String,
        key: String,
    },
    SaveCustom {
        provider_id: Option<String>,
        name: String,
        base_url: String,
        api_backend: String,
        auth_scheme: String,
        key: String,
        models: Vec<(String, String, u64, bool)>,
    },
    Close,
}

struct ProviderRow {
    id: String,
    name: String,
    status: String,
}

fn provider_rows() -> Vec<ProviderRow> {
    let mut rows: Vec<ProviderRow> = xai_grok_shell::compat::arg_items()
        .into_iter()
        .map(|(id, name, desc)| {
            let status = desc.rsplit(" · ").next().unwrap_or("").to_owned();
            ProviderRow { id, name, status }
        })
        .collect();
    rows.push(ProviderRow {
        id: ADD_CUSTOM_ID.into(),
        name: "+ Add custom provider".into(),
        status: "name · URL · protocol · key".into(),
    });
    rows
}

fn modal_config<'a>(
    title: &'a str,
    shortcuts: &'a [Shortcut<'a>],
    compact: bool,
) -> ModalWindowConfig<'a> {
    ModalWindowConfig {
        title,
        tabs: None,
        shortcuts,
        sizing: ModalSizing::medium().with_compact(compact),
        fold_info: None,
    }
}

fn pick_shortcuts() -> [Shortcut<'static>; 2] {
    [
        Shortcut {
            label: "Enter select",
            clickable: true,
            id: SHORTCUT_SUBMIT,
        },
        Shortcut {
            label: "Esc cancel",
            clickable: true,
            id: SHORTCUT_CANCEL,
        },
    ]
}

fn key_shortcuts(from_picker: bool, probing: bool, requires_auth: bool) -> [Shortcut<'static>; 2] {
    let submit = if probing {
        "…"
    } else if requires_auth {
        "Enter verify"
    } else {
        "Enter connect"
    };
    let cancel = if from_picker && !probing {
        "Esc back"
    } else {
        "Esc cancel"
    };
    [
        Shortcut {
            label: submit,
            clickable: !probing,
            id: SHORTCUT_SUBMIT,
        },
        Shortcut {
            label: cancel,
            clickable: true,
            id: SHORTCUT_CANCEL,
        },
    ]
}

fn step_shortcuts(state: &VendorLoginState) -> Vec<Shortcut<'static>> {
    match &state.step {
        VendorLoginStep::Key {
            from_picker,
            requires_auth,
            ..
        } => {
            let [submit, cancel] = key_shortcuts(*from_picker, state.probing, *requires_auth);
            vec![submit, cancel]
        }
        VendorLoginStep::CustomForm => vec![
            Shortcut {
                label: if state.probing { "…" } else { "Enter sync" },
                clickable: !state.probing,
                id: SHORTCUT_SUBMIT,
            },
            Shortcut {
                label: "Esc back",
                clickable: true,
                id: SHORTCUT_CANCEL,
            },
        ],
        VendorLoginStep::CustomModels => vec![
            Shortcut {
                label: if state.probing { "…" } else { "Enter save" },
                clickable: !state.probing,
                id: SHORTCUT_SUBMIT,
            },
            Shortcut {
                label: "^A all",
                clickable: !state.probing,
                id: SHORTCUT_SELECT_ALL,
            },
            Shortcut {
                label: "^N none",
                clickable: !state.probing,
                id: SHORTCUT_SELECT_NONE,
            },
            Shortcut {
                label: "/ find",
                clickable: !state.probing,
                id: SHORTCUT_SEARCH,
            },
            Shortcut {
                label: "Esc back",
                clickable: true,
                id: SHORTCUT_CANCEL,
            },
        ],
        _ => {
            let [submit, cancel] = pick_shortcuts();
            vec![submit, cancel]
        }
    }
}

pub(crate) fn handle_vendor_login_key(
    state: &mut VendorLoginState,
    key: &KeyEvent,
) -> VendorLoginOutcome {
    if key.kind == KeyEventKind::Release {
        return VendorLoginOutcome::Unchanged;
    }

    if state.probing {
        return if matches!(key.code, KeyCode::Esc) {
            VendorLoginOutcome::Close
        } else {
            VendorLoginOutcome::Unchanged
        };
    }

    if matches!(key.code, KeyCode::Esc) {
        match &state.step {
            VendorLoginStep::Key {
                from_picker: true, ..
            }
            | VendorLoginStep::Method {
                from_picker: true, ..
            } => {
                state.back_to_picker();
                return VendorLoginOutcome::Changed;
            }
            VendorLoginStep::OAuthWait { .. } => {
                state.back_from_oauth();
                return VendorLoginOutcome::Changed;
            }
            VendorLoginStep::CustomModels => {
                if let Some(draft) = state.custom.as_mut()
                    && draft.search_focused
                {
                    if !draft.search.text().is_empty() {
                        draft.search.reset();
                        draft.clamp_selection_to_filter();
                    } else {
                        draft.search_focused = false;
                    }
                    return VendorLoginOutcome::Changed;
                }
                state.step = VendorLoginStep::CustomForm;
                state.error = None;
                return VendorLoginOutcome::Changed;
            }
            VendorLoginStep::CustomForm => {
                state.custom = None;
                state.back_to_picker();
                return VendorLoginOutcome::Changed;
            }
            VendorLoginStep::Method {
                from_picker: false, ..
            } => {
                return VendorLoginOutcome::Close;
            }
            _ => {}
        }
    }

    let title = title_for(state);
    let shortcuts = step_shortcuts(state);
    let config = modal_config(&title, &shortcuts, false);
    match handle_modal_key(&mut state.window, key, &config) {
        ModalWindowOutcome::CloseRequested => return VendorLoginOutcome::Close,
        ModalWindowOutcome::Handled => return VendorLoginOutcome::Changed,
        _ => {}
    }

    match &state.step {
        VendorLoginStep::Pick { .. } => handle_pick_key(state, key),
        VendorLoginStep::Method { .. } => handle_method_key(state, key),
        VendorLoginStep::OAuthWait { .. } => handle_oauth_wait_key(state, key),
        VendorLoginStep::Key { .. } => handle_key_form_key(state, key),
        VendorLoginStep::CustomForm => handle_custom_form_key(state, key),
        VendorLoginStep::CustomModels => handle_custom_models_key(state, key),
    }
}

fn method_rows(provider_id: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if let Some(label) = xai_grok_shell::compat::oauth::login_label(provider_id) {
        rows.push(("oauth".into(), label.to_owned()));
    }
    rows.push(("api_key".into(), "API key".into()));
    rows
}

fn handle_method_key(state: &mut VendorLoginState, key: &KeyEvent) -> VendorLoginOutcome {
    if matches!(key.code, KeyCode::Enter) {
        return confirm_method(state);
    }
    let VendorLoginStep::Method {
        provider_id,
        selected,
        ..
    } = &mut state.step
    else {
        return VendorLoginOutcome::Unchanged;
    };
    let last = method_rows(provider_id).len().saturating_sub(1);
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if *selected > 0 {
                *selected -= 1;
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if *selected < last {
                *selected += 1;
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        _ => VendorLoginOutcome::Unchanged,
    }
}

fn handle_oauth_wait_key(state: &mut VendorLoginState, key: &KeyEvent) -> VendorLoginOutcome {
    if matches!(key.code, KeyCode::Enter) {
        return submit_oauth_code(state);
    }
    match state.editor.handle_key(key) {
        LineEditOutcome::Unhandled | LineEditOutcome::HandledNoChange => {
            VendorLoginOutcome::Unchanged
        }
        _ => VendorLoginOutcome::Changed,
    }
}

fn handle_pick_key(state: &mut VendorLoginState, key: &KeyEvent) -> VendorLoginOutcome {
    if matches!(key.code, KeyCode::Enter) {
        return confirm_pick(state);
    }
    let VendorLoginStep::Pick { selected, scroll } = &mut state.step else {
        return VendorLoginOutcome::Unchanged;
    };
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if *selected > 0 {
                *selected -= 1;
                if *selected < *scroll {
                    *scroll = *selected;
                }
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let last = provider_rows().len().saturating_sub(1);
            if *selected < last {
                *selected += 1;
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        _ => VendorLoginOutcome::Unchanged,
    }
}

fn handle_key_form_key(state: &mut VendorLoginState, key: &KeyEvent) -> VendorLoginOutcome {
    if matches!(key.code, KeyCode::Enter) {
        return submit_key(state);
    }
    match state.editor.handle_key(key) {
        LineEditOutcome::Unhandled | LineEditOutcome::HandledNoChange => {
            VendorLoginOutcome::Unchanged
        }
        _ => {
            state.error = None;
            VendorLoginOutcome::Changed
        }
    }
}

pub(crate) fn handle_vendor_login_paste(
    state: &mut VendorLoginState,
    text: &str,
) -> VendorLoginOutcome {
    if state.probing
        || matches!(
            state.step,
            VendorLoginStep::Pick { .. } | VendorLoginStep::Method { .. }
        )
    {
        return VendorLoginOutcome::Unchanged;
    }
    if matches!(state.step, VendorLoginStep::CustomModels) {
        let pasted = state.custom.as_mut().and_then(|draft| {
            if !draft.search_focused {
                return None;
            }
            Some(draft.search.insert_paste(text))
        });
        if pasted.is_some() {
            if let Some(draft) = state.custom.as_mut() {
                draft.clamp_selection_to_filter();
            }
            return VendorLoginOutcome::Changed;
        }
        return VendorLoginOutcome::Unchanged;
    }
    if matches!(state.step, VendorLoginStep::CustomForm) {
        let pasted = state
            .custom
            .as_mut()
            .and_then(|d| d.active_editor_mut())
            .map(|editor| editor.insert_paste(text));
        if pasted.is_some() {
            state.error = None;
            return VendorLoginOutcome::Changed;
        }
        return VendorLoginOutcome::Unchanged;
    }
    match state.editor.insert_paste(text) {
        LineEditOutcome::Unhandled | LineEditOutcome::HandledNoChange => {
            VendorLoginOutcome::Unchanged
        }
        _ => {
            state.error = None;
            VendorLoginOutcome::Changed
        }
    }
}

pub(crate) fn handle_vendor_login_mouse(
    state: &mut VendorLoginState,
    kind: MouseEventKind,
    column: u16,
    row: u16,
) -> VendorLoginOutcome {
    let chrome = handle_modal_mouse(&mut state.window, kind, column, row);
    match chrome {
        ModalWindowOutcome::CloseRequested => return VendorLoginOutcome::Close,
        ModalWindowOutcome::ShortcutActivated(SHORTCUT_CANCEL) => {
            if !state.probing {
                match &state.step {
                    VendorLoginStep::Key {
                        from_picker: true, ..
                    }
                    | VendorLoginStep::Method {
                        from_picker: true, ..
                    } => {
                        state.back_to_picker();
                        return VendorLoginOutcome::Changed;
                    }
                    VendorLoginStep::OAuthWait { .. } => {
                        state.back_from_oauth();
                        return VendorLoginOutcome::Changed;
                    }
                    VendorLoginStep::CustomModels => {
                        if let Some(draft) = state.custom.as_mut()
                            && draft.search_focused
                        {
                            if !draft.search.text().is_empty() {
                                draft.search.reset();
                                draft.clamp_selection_to_filter();
                            } else {
                                draft.search_focused = false;
                            }
                            return VendorLoginOutcome::Changed;
                        }
                        state.step = VendorLoginStep::CustomForm;
                        return VendorLoginOutcome::Changed;
                    }
                    VendorLoginStep::CustomForm => {
                        state.custom = None;
                        state.back_to_picker();
                        return VendorLoginOutcome::Changed;
                    }
                    _ => {}
                }
            }
            return VendorLoginOutcome::Close;
        }
        ModalWindowOutcome::ShortcutActivated(SHORTCUT_SUBMIT) => {
            if state.probing {
                return VendorLoginOutcome::Unchanged;
            }
            return match &state.step {
                VendorLoginStep::Pick { .. } => confirm_pick(state),
                VendorLoginStep::Method { .. } => confirm_method(state),
                VendorLoginStep::Key { .. } => submit_key(state),
                VendorLoginStep::OAuthWait { .. } => submit_oauth_code(state),
                VendorLoginStep::CustomForm => submit_custom_sync(state),
                VendorLoginStep::CustomModels => submit_custom_save(state),
            };
        }
        ModalWindowOutcome::ShortcutActivated(SHORTCUT_SELECT_ALL) => {
            if state.probing {
                return VendorLoginOutcome::Unchanged;
            }
            if let Some(draft) = state.custom.as_mut() {
                draft.set_filtered_enabled(true);
                return VendorLoginOutcome::Changed;
            }
            return VendorLoginOutcome::Unchanged;
        }
        ModalWindowOutcome::ShortcutActivated(SHORTCUT_SELECT_NONE) => {
            if state.probing {
                return VendorLoginOutcome::Unchanged;
            }
            if let Some(draft) = state.custom.as_mut() {
                draft.set_filtered_enabled(false);
                return VendorLoginOutcome::Changed;
            }
            return VendorLoginOutcome::Unchanged;
        }
        ModalWindowOutcome::ShortcutActivated(SHORTCUT_SEARCH) => {
            if state.probing {
                return VendorLoginOutcome::Unchanged;
            }
            if let Some(draft) = state.custom.as_mut() {
                draft.search_focused = true;
                return VendorLoginOutcome::Changed;
            }
            return VendorLoginOutcome::Unchanged;
        }
        ModalWindowOutcome::Handled => return VendorLoginOutcome::Changed,
        _ => {}
    }

    if state.probing {
        return VendorLoginOutcome::Unchanged;
    }

    let Some(area) = state.content_area else {
        return VendorLoginOutcome::Unchanged;
    };
    let in_content = column >= area.x
        && column < area.x + area.width
        && row >= area.y
        && row < area.y + area.height;

    if matches!(kind, MouseEventKind::Down(MouseButton::Left)) && in_content {
        if matches!(state.step, VendorLoginStep::CustomModels) && row == area.y {
            if let Some(draft) = state.custom.as_mut() {
                draft.search_focused = true;
                return VendorLoginOutcome::Changed;
            }
        }
        if let Some(&(_, idx)) = state.row_map.iter().find(|(y, _)| *y == row) {
            if matches!(state.step, VendorLoginStep::Pick { .. }) {
                if let VendorLoginStep::Pick { selected, .. } = &mut state.step {
                    *selected = idx;
                }
                return confirm_pick(state);
            }
            if matches!(state.step, VendorLoginStep::Method { .. }) {
                if let VendorLoginStep::Method { selected, .. } = &mut state.step {
                    *selected = idx;
                }
                return confirm_method(state);
            }
            if matches!(state.step, VendorLoginStep::CustomModels) {
                if let Some(draft) = state.custom.as_mut() {
                    draft.search_focused = false;
                    draft.selected = idx;
                    if let Some(row) = draft.models.get_mut(idx) {
                        row.enabled = !row.enabled;
                    }
                    return VendorLoginOutcome::Changed;
                }
            }
        }
        if matches!(state.step, VendorLoginStep::CustomForm)
            && let Some(draft) = state.custom.as_mut()
        {
            let idx = row.saturating_sub(area.y) / 2;
            draft.field = match idx {
                0 => CustomField::Name,
                1 => CustomField::BaseUrl,
                2 => CustomField::Protocol,
                3 => CustomField::Key,
                _ => return VendorLoginOutcome::Unchanged,
            };
            if draft.field == CustomField::Protocol {
                draft.protocol = (draft.protocol + 1) % PROTOCOLS.len();
            }
            return VendorLoginOutcome::Changed;
        }
        return VendorLoginOutcome::Unchanged;
    }

    if let VendorLoginStep::Method { selected, .. } = &mut state.step {
        if matches!(kind, MouseEventKind::Moved) && in_content {
            if let Some(&(_, idx)) = state.row_map.iter().find(|(y, _)| *y == row) {
                if *selected != idx {
                    *selected = idx;
                    return VendorLoginOutcome::Changed;
                }
            }
        }
        return VendorLoginOutcome::Unchanged;
    }

    if matches!(state.step, VendorLoginStep::CustomModels) {
        if matches!(kind, MouseEventKind::Moved) && in_content {
            if let Some(&(_, idx)) = state.row_map.iter().find(|(y, _)| *y == row)
                && let Some(draft) = state.custom.as_mut()
                && draft.selected != idx
            {
                draft.selected = idx;
                return VendorLoginOutcome::Changed;
            }
        }
        return VendorLoginOutcome::Unchanged;
    }

    let VendorLoginStep::Pick { selected, scroll } = &mut state.step else {
        return VendorLoginOutcome::Unchanged;
    };
    match kind {
        MouseEventKind::Moved if in_content => {
            if let Some(&(_, idx)) = state.row_map.iter().find(|(y, _)| *y == row) {
                if *selected != idx {
                    *selected = idx;
                    return VendorLoginOutcome::Changed;
                }
            }
            VendorLoginOutcome::Unchanged
        }
        MouseEventKind::ScrollUp if in_content => {
            if *selected > 0 {
                *selected -= 1;
                if *selected < *scroll {
                    *scroll = *selected;
                }
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        MouseEventKind::ScrollDown if in_content => {
            let last = provider_rows().len().saturating_sub(1);
            if *selected < last {
                *selected += 1;
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        _ => VendorLoginOutcome::Unchanged,
    }
}

fn confirm_pick(state: &mut VendorLoginState) -> VendorLoginOutcome {
    let selected = match &state.step {
        VendorLoginStep::Pick { selected, .. } => *selected,
        _ => return VendorLoginOutcome::Unchanged,
    };
    let rows = provider_rows();
    let Some(row) = rows.get(selected) else {
        return VendorLoginOutcome::Unchanged;
    };
    let id = row.id.clone();
    let name = row.name.clone();
    if id == ADD_CUSTOM_ID {
        state.enter_custom_form();
        return VendorLoginOutcome::Changed;
    }
    if xai_grok_shell::compat::custom::get_provider(&id).is_some() {
        state.enter_custom_edit(&id);
        return VendorLoginOutcome::Changed;
    }
    state.enter_provider(id, name, true);
    VendorLoginOutcome::Changed
}

fn handle_custom_form_key(state: &mut VendorLoginState, key: &KeyEvent) -> VendorLoginOutcome {
    if matches!(key.code, KeyCode::Enter) {
        return submit_custom_sync(state);
    }
    let outcome = {
        let Some(draft) = state.custom.as_mut() else {
            return VendorLoginOutcome::Unchanged;
        };
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                draft.cycle_field(true);
                VendorLoginOutcome::Changed
            }
            KeyCode::BackTab | KeyCode::Up => {
                draft.cycle_field(false);
                VendorLoginOutcome::Changed
            }
            KeyCode::Left if draft.field == CustomField::Protocol => {
                if draft.protocol > 0 {
                    draft.protocol -= 1;
                } else {
                    draft.protocol = PROTOCOLS.len() - 1;
                }
                VendorLoginOutcome::Changed
            }
            KeyCode::Right if draft.field == CustomField::Protocol => {
                draft.protocol = (draft.protocol + 1) % PROTOCOLS.len();
                VendorLoginOutcome::Changed
            }
            _ => {
                if let Some(editor) = draft.active_editor_mut() {
                    match editor.handle_key(key) {
                        LineEditOutcome::Unhandled | LineEditOutcome::HandledNoChange => {
                            VendorLoginOutcome::Unchanged
                        }
                        _ => VendorLoginOutcome::Changed,
                    }
                } else {
                    VendorLoginOutcome::Unchanged
                }
            }
        }
    };
    if matches!(outcome, VendorLoginOutcome::Changed) {
        state.error = None;
    }
    outcome
}

fn handle_custom_models_key(state: &mut VendorLoginState, key: &KeyEvent) -> VendorLoginOutcome {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') => {
                if let Some(draft) = state.custom.as_mut() {
                    draft.set_filtered_enabled(true);
                    return VendorLoginOutcome::Changed;
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                if let Some(draft) = state.custom.as_mut() {
                    draft.set_filtered_enabled(false);
                    return VendorLoginOutcome::Changed;
                }
            }
            _ => {}
        }
    }

    let search_focused = state.custom.as_ref().is_some_and(|d| d.search_focused);

    if search_focused {
        match key.code {
            KeyCode::Enter | KeyCode::Down | KeyCode::Tab => {
                if let Some(draft) = state.custom.as_mut() {
                    draft.search_focused = false;
                    draft.clamp_selection_to_filter();
                }
                return VendorLoginOutcome::Changed;
            }
            _ => {
                let outcome = {
                    let Some(draft) = state.custom.as_mut() else {
                        return VendorLoginOutcome::Unchanged;
                    };
                    match draft.search.handle_key(key) {
                        LineEditOutcome::Unhandled | LineEditOutcome::HandledNoChange => {
                            VendorLoginOutcome::Unchanged
                        }
                        _ => {
                            draft.clamp_selection_to_filter();
                            VendorLoginOutcome::Changed
                        }
                    }
                };
                return outcome;
            }
        }
    }

    if matches!(key.code, KeyCode::Enter) {
        return submit_custom_save(state);
    }
    let Some(draft) = state.custom.as_mut() else {
        return VendorLoginOutcome::Unchanged;
    };
    match key.code {
        KeyCode::Char('/') | KeyCode::Tab => {
            draft.search_focused = true;
            VendorLoginOutcome::Changed
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if draft.move_selection(-1) {
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if draft.move_selection(1) {
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        KeyCode::Char(' ') => {
            if let Some(row) = draft.models.get_mut(draft.selected) {
                row.enabled = !row.enabled;
                VendorLoginOutcome::Changed
            } else {
                VendorLoginOutcome::Unchanged
            }
        }
        KeyCode::Char('a') => {
            draft.set_filtered_enabled(true);
            VendorLoginOutcome::Changed
        }
        KeyCode::Char('n') => {
            draft.set_filtered_enabled(false);
            VendorLoginOutcome::Changed
        }
        _ => VendorLoginOutcome::Unchanged,
    }
}

fn submit_custom_sync(state: &mut VendorLoginState) -> VendorLoginOutcome {
    let Some(draft) = state.custom.as_ref() else {
        return VendorLoginOutcome::Unchanged;
    };
    let name = draft.name.text().trim().to_owned();
    let base_url = draft.base_url.text().trim().to_owned();
    let mut key = draft.key.text().trim().to_owned();
    if key.is_empty()
        && let Some(id) = draft.id.as_deref()
        && let Some(stored) = xai_grok_shell::compat::custom::stored_secret(id)
    {
        key = stored;
    }
    if name.is_empty() || base_url.len() < 8 {
        state.error = Some("Name and base URL are required".into());
        return VendorLoginOutcome::Changed;
    }
    let api_backend = PROTOCOLS[draft.protocol].to_owned();
    let auth_scheme = if api_backend == "messages" {
        "x-api-key".to_owned()
    } else {
        "bearer".to_owned()
    };
    state.probing = true;
    state.error = None;
    VendorLoginOutcome::SyncCustom {
        name,
        base_url,
        api_backend,
        auth_scheme,
        key,
    }
}

fn submit_custom_save(state: &mut VendorLoginState) -> VendorLoginOutcome {
    let Some(draft) = state.custom.as_ref() else {
        return VendorLoginOutcome::Unchanged;
    };
    if !draft.models.iter().any(|m| m.enabled) {
        state.error = Some("Enable at least one model".into());
        return VendorLoginOutcome::Changed;
    }
    let name = draft.name.text().trim().to_owned();
    let base_url = draft.base_url.text().trim().to_owned();
    let key = draft.key.text().trim().to_owned();
    let api_backend = PROTOCOLS[draft.protocol].to_owned();
    let auth_scheme = if api_backend == "messages" {
        "x-api-key".to_owned()
    } else {
        "bearer".to_owned()
    };
    let models = draft
        .models
        .iter()
        .map(|m| {
            (
                m.api_model.clone(),
                m.name.clone(),
                m.context_window,
                m.enabled,
            )
        })
        .collect();
    state.probing = true;
    VendorLoginOutcome::SaveCustom {
        provider_id: draft.id.clone(),
        name,
        base_url,
        api_backend,
        auth_scheme,
        key,
        models,
    }
}

fn confirm_method(state: &mut VendorLoginState) -> VendorLoginOutcome {
    let (provider_id, provider_name, selected, from_picker) = match &state.step {
        VendorLoginStep::Method {
            provider_id,
            provider_name,
            selected,
            from_picker,
        } => (
            provider_id.clone(),
            provider_name.clone(),
            *selected,
            *from_picker,
        ),
        _ => return VendorLoginOutcome::Unchanged,
    };
    let rows = method_rows(&provider_id);
    let Some((id, _)) = rows.get(selected) else {
        return VendorLoginOutcome::Unchanged;
    };
    if id == "oauth" {
        state.probing = true;
        state.error = None;
        return VendorLoginOutcome::StartOAuth { provider_id };
    }
    state.enter_key_form(provider_id, provider_name, from_picker);
    VendorLoginOutcome::Changed
}

fn submit_oauth_code(state: &mut VendorLoginState) -> VendorLoginOutcome {
    let VendorLoginStep::OAuthWait { provider_id, .. } = &state.step else {
        return VendorLoginOutcome::Unchanged;
    };
    let code = state.editor.text().trim().to_owned();
    if code.is_empty() {
        state.error = Some("Paste the redirect URL or authorization code".into());
        return VendorLoginOutcome::Changed;
    }
    VendorLoginOutcome::SubmitOAuthCode {
        provider_id: provider_id.clone(),
        code,
    }
}

fn submit_key(state: &mut VendorLoginState) -> VendorLoginOutcome {
    let (provider_id, requires_auth) = match &state.step {
        VendorLoginStep::Key {
            provider_id,
            requires_auth,
            ..
        } => (provider_id.clone(), *requires_auth),
        _ => return VendorLoginOutcome::Unchanged,
    };
    let key = state.editor.text().trim().to_owned();
    if key.is_empty()
        && requires_auth
        && xai_grok_shell::compat::provider_by_id(&provider_id).is_some()
    {
        state.error = Some("Paste an API key first".into());
        return VendorLoginOutcome::Changed;
    }
    state.probing = true;
    state.error = None;
    VendorLoginOutcome::Submit { provider_id, key }
}

fn title_for(state: &VendorLoginState) -> String {
    match &state.step {
        VendorLoginStep::Pick { .. } => "Sign in · Providers".into(),
        VendorLoginStep::CustomForm => {
            if state.custom.as_ref().is_some_and(|d| d.id.is_some()) {
                "Edit custom provider".into()
            } else {
                "Add custom provider".into()
            }
        }
        VendorLoginStep::CustomModels => {
            let name = state
                .custom
                .as_ref()
                .map(|d| d.name.text().trim().to_owned())
                .filter(|s| !s.is_empty());
            match name {
                Some(name) => format!("Select models · {name}"),
                None => "Select models".into(),
            }
        }
        VendorLoginStep::Method { provider_name, .. }
        | VendorLoginStep::Key { provider_name, .. }
        | VendorLoginStep::OAuthWait { provider_name, .. } => {
            format!("Sign in · {provider_name}")
        }
    }
}

pub(crate) fn render_vendor_login_modal(
    buf: &mut Buffer,
    area: Rect,
    state: &mut VendorLoginState,
    compact: bool,
) {
    let theme = Theme::current();
    let title = title_for(state);
    let shortcuts = step_shortcuts(state);
    let config = modal_config(&title, &shortcuts, compact);
    let Some(areas) = render_modal_window(buf, area, &mut state.window, &config, &theme) else {
        state.content_area = None;
        state.row_map.clear();
        return;
    };
    state.content_area = Some(areas.content);
    match &state.step {
        VendorLoginStep::Pick { .. } => render_pick_list(buf, areas.content, state, &theme),
        VendorLoginStep::Method { .. } => render_method_list(buf, areas.content, state, &theme),
        VendorLoginStep::Key { .. } => render_key_form(buf, areas.content, state, &theme),
        VendorLoginStep::OAuthWait { .. } => render_oauth_wait(buf, areas.content, state, &theme),
        VendorLoginStep::CustomForm => render_custom_form(buf, areas.content, state, &theme),
        VendorLoginStep::CustomModels => render_custom_models(buf, areas.content, state, &theme),
    }
}

fn render_method_list(buf: &mut Buffer, area: Rect, state: &mut VendorLoginState, theme: &Theme) {
    let VendorLoginStep::Method {
        provider_id,
        selected,
        ..
    } = &state.step
    else {
        return;
    };
    let rows = method_rows(provider_id);
    let selected = *selected;
    state.row_map.clear();
    for (i, (_, label)) in rows.iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let y = area.y + i as u16;
        state.row_map.push((y, i));
        let focused = i == selected;
        if focused {
            let hover = Style::default().bg(theme.bg_highlight);
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(hover);
                }
            }
        }
        let style = if focused {
            Style::default()
                .fg(theme.text_primary)
                .bg(theme.bg_highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_primary)
        };
        Paragraph::new(Line::from(Span::styled(label.clone(), style))).render(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            buf,
        );
    }
}

fn render_oauth_wait(buf: &mut Buffer, area: Rect, state: &VendorLoginState, theme: &Theme) {
    let VendorLoginStep::OAuthWait {
        authorize_url,
        instructions,
        ..
    } = &state.step
    else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let mut y = area.y;
    Paragraph::new(Line::from(Span::styled(
        truncate_str(instructions, area.width as usize),
        Style::default().fg(theme.text_secondary),
    )))
    .render(
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        },
        buf,
    );
    y = y.saturating_add(2);
    if y < area.y + area.height {
        Paragraph::new(Line::from(Span::styled(
            truncate_str(authorize_url, area.width as usize),
            Style::default()
                .fg(theme.accent_user)
                .add_modifier(Modifier::UNDERLINED),
        )))
        .render(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            buf,
        );
        y = y.saturating_add(2);
    }
    if y >= area.y + area.height {
        return;
    }
    let input_bg = theme.bg_visual;
    let input_rect = Rect {
        x: area.x,
        y,
        width: area.width,
        height: 1,
    };
    buf.set_style(input_rect, Style::default().bg(input_bg));
    let room = area.width as usize;
    if state.editor.text().is_empty() {
        let ph = truncate_str("<paste redirect URL or code>", room.saturating_sub(1));
        let ph_w = ph.width() as u16;
        buf.set_span(
            area.x,
            y,
            &Span::styled(ph, Style::default().fg(theme.gray_dim).bg(input_bg)),
            ph_w,
        );
        buf.set_span(
            area.x,
            y,
            &Span::styled(
                crate::glyphs::selection_bar(),
                Style::default().fg(theme.accent_user).bg(input_bg),
            ),
            1,
        );
    } else {
        let viewport = state.editor.viewport(room);
        let visible = &state.editor.text()[viewport.visible_byte_range];
        let w = (visible.width() as u16).min(area.width);
        buf.set_span(
            area.x,
            y,
            &Span::styled(
                visible,
                Style::default().fg(theme.text_primary).bg(input_bg),
            ),
            w,
        );
        let cursor_x =
            area.x + (viewport.cursor_display_column as u16).min(area.width.saturating_sub(1));
        buf.set_span(
            cursor_x,
            y,
            &Span::styled(
                crate::glyphs::selection_bar(),
                Style::default().fg(theme.accent_user).bg(input_bg),
            ),
            1,
        );
    }
    if let Some(err) = &state.error {
        let err_y = y.saturating_add(2);
        if err_y < area.y + area.height {
            let text = truncate_str(err, area.width as usize);
            let tw = text.width() as u16;
            buf.set_span(
                area.x,
                err_y,
                &Span::styled(text, Style::default().fg(theme.accent_error)),
                tw,
            );
        }
    }
}

fn render_labeled_input(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    label: &str,
    editor: &LineEditor,
    focused: bool,
    mask: bool,
    theme: &Theme,
) {
    let bg = if focused {
        theme.bg_visual
    } else {
        theme.bg_base
    };
    let row = Rect {
        x: area.x,
        y,
        width: area.width,
        height: 1,
    };
    buf.set_style(row, Style::default().bg(bg));
    let label_s = format!("{label}: ");
    let label_w = label_s.width() as u16;
    buf.set_span(
        area.x,
        y,
        &Span::styled(label_s, Style::default().fg(theme.gray_bright).bg(bg)),
        label_w.min(area.width),
    );
    if area.width <= label_w {
        return;
    }
    let input_x = area.x + label_w;
    let room = area.width.saturating_sub(label_w) as usize;
    let text = editor.text();
    let display = if mask && !text.is_empty() {
        "•".repeat(text.chars().count())
    } else {
        text.to_owned()
    };
    if display.is_empty() && focused {
        buf.set_span(
            input_x,
            y,
            &Span::styled(
                crate::glyphs::selection_bar(),
                Style::default().fg(theme.accent_user).bg(bg),
            ),
            1,
        );
        return;
    }
    let shown = truncate_str(&display, room.saturating_sub(1));
    let sw = shown.width() as u16;
    buf.set_span(
        input_x,
        y,
        &Span::styled(shown, Style::default().fg(theme.text_primary).bg(bg)),
        sw,
    );
    if focused {
        let cursor_x = input_x + sw.min(area.width.saturating_sub(label_w + 1));
        buf.set_span(
            cursor_x,
            y,
            &Span::styled(
                crate::glyphs::selection_bar(),
                Style::default().fg(theme.accent_user).bg(bg),
            ),
            1,
        );
    }
}

fn render_custom_form(buf: &mut Buffer, area: Rect, state: &VendorLoginState, theme: &Theme) {
    let Some(draft) = state.custom.as_ref() else {
        return;
    };
    if area.height == 0 {
        return;
    }
    let fields = [
        (CustomField::Name, "Name", false),
        (CustomField::BaseUrl, "Base URL", false),
        (CustomField::Protocol, "Protocol", false),
        (CustomField::Key, "API key", true),
    ];
    for (i, (field, label, mask)) in fields.iter().enumerate() {
        let y = area.y + i as u16 * 2;
        if y >= area.y + area.height {
            break;
        }
        let focused = draft.field == *field;
        if *field == CustomField::Protocol {
            let bg = if focused {
                theme.bg_visual
            } else {
                theme.bg_base
            };
            let row = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };
            buf.set_style(row, Style::default().bg(bg));
            let value = PROTOCOLS[draft.protocol];
            let line = format!("Protocol: {value}   ←/→");
            buf.set_span(
                area.x,
                y,
                &Span::styled(
                    truncate_str(&line, area.width as usize),
                    Style::default().fg(theme.text_primary).bg(bg),
                ),
                area.width,
            );
        } else {
            let editor = match field {
                CustomField::Name => &draft.name,
                CustomField::BaseUrl => &draft.base_url,
                CustomField::Key => &draft.key,
                CustomField::Protocol => continue,
            };
            render_labeled_input(buf, area, y, label, editor, focused, *mask, theme);
        }
    }
    let hint_y = area.y + 8;
    if hint_y < area.y + area.height {
        let hint = if state.probing {
            "Fetching /models…"
        } else if let Some(err) = &state.error {
            err.as_str()
        } else {
            "Tab fields · Enter sync models"
        };
        let style = if state.error.is_some() {
            Style::default().fg(theme.accent_error)
        } else if state.probing {
            Style::default().fg(theme.accent_model)
        } else {
            Style::default().fg(theme.gray_bright)
        };
        let text = truncate_str(hint, area.width as usize);
        let tw = text.width() as u16;
        buf.set_span(area.x, hint_y, &Span::styled(text, style), tw);
    }
}

fn render_custom_models(buf: &mut Buffer, area: Rect, state: &mut VendorLoginState, theme: &Theme) {
    if area.height == 0 {
        return;
    }
    let probing = state.probing;
    let error = state.error.clone();
    let search_y = area.y;
    let list_top = area.y.saturating_add(1);
    let list_bottom = area.y + area.height.saturating_sub(1);
    let list_height = list_bottom.saturating_sub(list_top);
    let Some(draft) = state.custom.as_mut() else {
        return;
    };
    draft.clamp_selection_to_filter();
    let filtered = draft.filtered_indices();
    let search_focused = draft.search_focused;
    let query = draft.search.text().to_owned();
    let enabled_n = draft.models.iter().filter(|m| m.enabled).count();
    let total_n = draft.models.len();

    let search_bg = if search_focused {
        theme.bg_visual
    } else {
        theme.bg_base
    };
    let search_rect = Rect {
        x: area.x,
        y: search_y,
        width: area.width,
        height: 1,
    };
    buf.set_style(search_rect, Style::default().bg(search_bg));
    let prefix = "/ ";
    let prefix_w = prefix.width() as u16;
    buf.set_span(
        area.x,
        search_y,
        &Span::styled(prefix, Style::default().fg(theme.gray_bright).bg(search_bg)),
        prefix_w.min(area.width),
    );
    if area.width > prefix_w {
        let room = area.width.saturating_sub(prefix_w) as usize;
        if query.is_empty() {
            let ph = truncate_str("find models", room);
            let ph_w = ph.width() as u16;
            buf.set_span(
                area.x + prefix_w,
                search_y,
                &Span::styled(ph, Style::default().fg(theme.gray_dim).bg(search_bg)),
                ph_w,
            );
            if search_focused {
                buf.set_span(
                    area.x + prefix_w,
                    search_y,
                    &Span::styled(
                        crate::glyphs::selection_bar(),
                        Style::default().fg(theme.accent_user).bg(search_bg),
                    ),
                    1,
                );
            }
        } else {
            let shown = truncate_str(&query, room.saturating_sub(1));
            let sw = shown.width() as u16;
            buf.set_span(
                area.x + prefix_w,
                search_y,
                &Span::styled(shown, Style::default().fg(theme.text_primary).bg(search_bg)),
                sw,
            );
            if search_focused {
                buf.set_span(
                    area.x + prefix_w + sw.min(area.width.saturating_sub(prefix_w + 1)),
                    search_y,
                    &Span::styled(
                        crate::glyphs::selection_bar(),
                        Style::default().fg(theme.accent_user).bg(search_bg),
                    ),
                    1,
                );
            }
        }
    }

    let visible = list_height.max(1) as usize;
    let pos = filtered
        .iter()
        .position(|&i| i == draft.selected)
        .unwrap_or(0);
    if pos < draft.scroll {
        draft.scroll = pos;
    } else if pos >= draft.scroll + visible {
        draft.scroll = pos.saturating_add(1).saturating_sub(visible);
    }
    let selected = draft.selected;
    let scroll = draft.scroll;
    let models = draft.models.clone();
    state.row_map.clear();
    for (row_idx, &i) in filtered.iter().skip(scroll).take(visible).enumerate() {
        let y = list_top + row_idx as u16;
        if y >= list_bottom {
            break;
        }
        state.row_map.push((y, i));
        let Some(row) = models.get(i) else {
            continue;
        };
        let focused = i == selected && !search_focused;
        if focused {
            let hover = Style::default().bg(theme.bg_highlight);
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(hover);
                }
            }
        }
        let mark = if row.enabled { "[x]" } else { "[ ]" };
        let label = if row.name.is_empty() {
            format!("{mark} {}", row.api_model)
        } else {
            format!("{mark} {}  {}", row.api_model, row.name)
        };
        let style = if focused {
            Style::default()
                .fg(theme.text_primary)
                .bg(theme.bg_highlight)
        } else {
            Style::default().fg(theme.text_primary)
        };
        Paragraph::new(Line::from(Span::styled(
            truncate_str(&label, area.width as usize),
            style,
        )))
        .render(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            buf,
        );
    }

    let status_y = area.y + area.height.saturating_sub(1);
    if status_y > search_y {
        let hint = if probing {
            "Saving provider…".to_owned()
        } else if let Some(err) = error.as_deref() {
            err.to_owned()
        } else if !query.trim().is_empty() {
            format!("{enabled_n}/{total_n} enabled · {} shown", filtered.len())
        } else {
            format!("{enabled_n}/{total_n} enabled · Space toggle")
        };
        let style = if error.is_some() {
            Style::default().fg(theme.accent_error)
        } else if probing {
            Style::default().fg(theme.accent_model)
        } else {
            Style::default().fg(theme.gray_bright)
        };
        let text = truncate_str(&hint, area.width as usize);
        let tw = text.width() as u16;
        buf.set_span(area.x, status_y, &Span::styled(text, style), tw);
    }
}

fn render_pick_list(buf: &mut Buffer, area: Rect, state: &mut VendorLoginState, theme: &Theme) {
    let rows = provider_rows();
    let VendorLoginStep::Pick { selected, scroll } = &mut state.step else {
        return;
    };
    if rows.is_empty() {
        state.row_map.clear();
        Paragraph::new(Line::from(Span::styled(
            "No providers in catalog",
            Style::default().fg(theme.gray_bright),
        )))
        .render(area, buf);
        return;
    }
    if *selected >= rows.len() {
        *selected = rows.len() - 1;
    }
    let visible = area.height.max(1) as usize;
    if *selected < *scroll {
        *scroll = *selected;
    } else if *selected >= *scroll + visible {
        *scroll = (*selected).saturating_add(1).saturating_sub(visible);
    }

    state.row_map.clear();
    for (row_idx, (i, row)) in rows
        .iter()
        .enumerate()
        .skip(*scroll)
        .take(visible)
        .enumerate()
    {
        let y = area.y + row_idx as u16;
        state.row_map.push((y, i));
        let focused = i == *selected;
        if focused {
            let hover = Style::default().bg(theme.bg_highlight);
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(hover);
                }
            }
        }
        let name_style = if focused {
            Style::default()
                .fg(theme.text_primary)
                .bg(theme.bg_highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text_primary)
        };
        let status_style = if focused {
            Style::default()
                .fg(theme.gray_bright)
                .bg(theme.bg_highlight)
        } else {
            Style::default().fg(theme.gray_bright)
        };
        let status_w = row.status.width() as u16;
        let gap = 2u16;
        let name_budget = area
            .width
            .saturating_sub(status_w.saturating_add(gap))
            .max(1) as usize;
        let name = truncate_str(&row.name, name_budget);
        let mut spans = vec![Span::styled(name, name_style)];
        if area.width > status_w + 1 {
            let used = spans[0].content.width() as u16;
            let pad = area.width.saturating_sub(used).saturating_sub(status_w);
            if pad > 0 {
                spans.push(Span::styled(
                    " ".repeat(pad as usize),
                    if focused {
                        Style::default().bg(theme.bg_highlight)
                    } else {
                        Style::default()
                    },
                ));
            }
            spans.push(Span::styled(row.status.clone(), status_style));
        }
        Paragraph::new(Line::from(spans)).render(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            buf,
        );
    }
}

fn render_key_form(buf: &mut Buffer, area: Rect, state: &VendorLoginState, theme: &Theme) {
    let VendorLoginStep::Key {
        provider_name,
        requires_auth,
        ..
    } = &state.step
    else {
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }

    let prompt = if *requires_auth {
        format!("Paste your {provider_name} API key")
    } else {
        format!("Connect to local {provider_name} (no API key)")
    };
    Paragraph::new(Line::from(Span::styled(
        truncate_str(&prompt, area.width as usize),
        Style::default().fg(theme.text_secondary),
    )))
    .render(
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
        buf,
    );

    if area.height < 3 {
        return;
    }
    let input_y = area.y + 2;
    let input_bg = theme.bg_visual;
    let has_error = state.error.is_some();
    let input_fg = if has_error {
        theme.accent_error
    } else {
        theme.text_primary
    };
    let input_style = Style::default().fg(input_fg).bg(input_bg);
    let cursor_style = Style::default().fg(theme.accent_user).bg(input_bg);
    let input_rect = Rect {
        x: area.x,
        y: input_y,
        width: area.width,
        height: 1,
    };
    buf.set_style(input_rect, Style::default().bg(input_bg));

    let room = area.width as usize;
    if room == 0 {
        return;
    }
    let cursor_reserve = 1usize;
    let visible_w = room.saturating_sub(cursor_reserve);

    if state.editor.text().is_empty() {
        let placeholder = if *requires_auth {
            "<paste or type>"
        } else {
            "<Enter to connect>"
        };
        if visible_w > 0 && !state.probing {
            let ph = truncate_str(placeholder, visible_w);
            let ph_w = ph.width() as u16;
            buf.set_span(
                area.x,
                input_y,
                &Span::styled(ph, Style::default().fg(theme.gray_dim).bg(input_bg)),
                ph_w,
            );
        }
        buf.set_span(
            area.x,
            input_y,
            &Span::styled(crate::glyphs::selection_bar(), cursor_style),
            1,
        );
    } else {
        let viewport = state.editor.viewport(room);
        let visible = &state.editor.text()[viewport.visible_byte_range];
        let display: String = "•".repeat(visible.chars().count());
        let display_w = (display.width() as u16).min(area.width);
        buf.set_span(
            area.x,
            input_y,
            &Span::styled(&display, input_style),
            display_w,
        );
        let cursor_x =
            area.x + (viewport.cursor_display_column as u16).min(area.width.saturating_sub(1));
        buf.set_span(
            cursor_x,
            input_y,
            &Span::styled(crate::glyphs::selection_bar(), cursor_style),
            1,
        );
    }

    if area.height < 5 {
        return;
    }
    let status_y = input_y + 2;
    let status = if state.probing {
        if *requires_auth {
            "Checking key…"
        } else {
            "Connecting…"
        }
    } else if let Some(err) = &state.error {
        err.as_str()
    } else {
        ""
    };
    if !status.is_empty() {
        let style = if state.error.is_some() {
            Style::default().fg(theme.accent_error)
        } else {
            Style::default().fg(theme.accent_model)
        };
        let text = truncate_str(status, area.width as usize);
        let text_w = text.width() as u16;
        buf.set_span(area.x, status_y, &Span::styled(text, style), text_w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn picker_enter_opens_key_form() {
        let mut state = VendorLoginState::picker();
        assert!(matches!(state.step, VendorLoginStep::Pick { .. }));
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Enter));
        assert!(matches!(out, VendorLoginOutcome::Changed));
        assert!(matches!(
            state.step,
            VendorLoginStep::Key {
                from_picker: true,
                ..
            }
        ));
    }

    #[test]
    fn esc_from_key_form_returns_to_picker() {
        let mut state = VendorLoginState::picker();
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Enter));
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Esc));
        assert!(matches!(out, VendorLoginOutcome::Changed));
        assert!(matches!(state.step, VendorLoginStep::Pick { .. }));
    }

    #[test]
    fn direct_provider_esc_closes() {
        let mut state = VendorLoginState::for_provider("openai".into(), "OpenAI".into());
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Esc));
        assert!(matches!(out, VendorLoginOutcome::Close));
    }

    #[test]
    fn empty_key_refuses_submit() {
        let mut state = VendorLoginState::for_provider("openai".into(), "OpenAI".into());
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Enter));
        assert!(matches!(out, VendorLoginOutcome::Changed));
        assert_eq!(state.error.as_deref(), Some("Paste an API key first"));
        assert!(!state.probing);
    }

    #[test]
    fn openrouter_opens_on_method_choice() {
        let mut state = VendorLoginState::for_provider("openrouter".into(), "OpenRouter".into());
        assert!(matches!(state.step, VendorLoginStep::Method { .. }));
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Down));
        assert!(matches!(out, VendorLoginOutcome::Changed));
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Enter));
        match out {
            VendorLoginOutcome::StartOAuth { provider_id } => {
                assert_eq!(provider_id, "openrouter");
            }
            other => panic!("expected StartOAuth, got {other:?}"),
        }
    }

    #[test]
    fn line_editor_arrow_keys_move_cursor() {
        let mut state = VendorLoginState::for_provider("openai".into(), "OpenAI".into());
        assert!(matches!(
            handle_vendor_login_paste(&mut state, "sk-test"),
            VendorLoginOutcome::Changed
        ));
        let end = state.editor.cursor_byte();
        assert!(end > 0);
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Left));
        assert!(matches!(out, VendorLoginOutcome::Changed));
        assert!(state.editor.cursor_byte() < end);
    }

    #[test]
    fn add_custom_row_opens_form() {
        let mut state = VendorLoginState::picker();
        let idx = provider_rows()
            .iter()
            .position(|r| r.id == ADD_CUSTOM_ID)
            .expect("add custom row");
        if let VendorLoginStep::Pick { selected, .. } = &mut state.step {
            *selected = idx;
        }
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Enter));
        assert!(matches!(out, VendorLoginOutcome::Changed));
        assert!(matches!(state.step, VendorLoginStep::CustomForm));
    }

    #[test]
    fn custom_form_requires_name_and_url() {
        let mut state = VendorLoginState::custom_form();
        let out = handle_vendor_login_key(&mut state, &key(KeyCode::Enter));
        assert!(matches!(out, VendorLoginOutcome::Changed));
        assert_eq!(
            state.error.as_deref(),
            Some("Name and base URL are required")
        );
        assert!(!state.probing);
    }

    #[test]
    fn custom_form_tab_and_protocol_cycle() {
        let mut state = VendorLoginState::custom_form();
        assert_eq!(state.custom.as_ref().unwrap().field, CustomField::Name);
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Tab));
        assert_eq!(state.custom.as_ref().unwrap().field, CustomField::BaseUrl);
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Tab));
        assert_eq!(state.custom.as_ref().unwrap().field, CustomField::Protocol);
        let start = state.custom.as_ref().unwrap().protocol;
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Right));
        assert_eq!(
            state.custom.as_ref().unwrap().protocol,
            (start + 1) % PROTOCOLS.len()
        );
        assert_eq!(PROTOCOLS[1], "responses");
    }

    #[test]
    fn custom_form_sync_and_toggle_models() {
        let mut state = VendorLoginState::custom_form();
        let _ = handle_vendor_login_paste(&mut state, "Acme");
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Tab));
        let _ = handle_vendor_login_paste(&mut state, "acme.example/v1");
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Tab));
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Tab));
        let _ = handle_vendor_login_paste(&mut state, "sk-test");
        match handle_vendor_login_key(&mut state, &key(KeyCode::Enter)) {
            VendorLoginOutcome::SyncCustom {
                name,
                base_url,
                api_backend,
                key,
                ..
            } => {
                assert_eq!(name, "Acme");
                assert_eq!(base_url, "https://acme.example/v1");
                assert_eq!(api_backend, "chat_completions");
                assert_eq!(key, "sk-test");
            }
            other => panic!("expected SyncCustom, got {other:?}"),
        }
        assert!(state.probing);
        state.apply_custom_models(
            vec![
                ("acme-large".into(), "Large".into(), 64_000),
                ("acme-small".into(), "Small".into(), 32_000),
            ],
            None,
        );
        assert!(matches!(state.step, VendorLoginStep::CustomModels));
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Char(' ')));
        assert!(!state.custom.as_ref().unwrap().models[0].enabled);
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Down));
        match handle_vendor_login_key(&mut state, &key(KeyCode::Enter)) {
            VendorLoginOutcome::SaveCustom {
                provider_id,
                models,
                ..
            } => {
                assert!(provider_id.is_none());
                assert!(!models[0].3);
                assert!(models[1].3);
            }
            other => panic!("expected SaveCustom, got {other:?}"),
        }
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn custom_models_search_and_select_visible() {
        let mut state = VendorLoginState::custom_form();
        state.apply_custom_models(
            vec![
                ("gpt-4o".into(), "GPT-4o".into(), 128_000),
                ("claude-sonnet".into(), "Sonnet".into(), 200_000),
                ("gpt-4.1".into(), "GPT-4.1".into(), 128_000),
            ],
            None,
        );
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Char('n')));
        assert!(
            state
                .custom
                .as_ref()
                .unwrap()
                .models
                .iter()
                .all(|m| !m.enabled)
        );
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Char('/')));
        assert!(state.custom.as_ref().unwrap().search_focused);
        let _ = handle_vendor_login_paste(&mut state, "gpt");
        assert_eq!(state.custom.as_ref().unwrap().filtered_indices().len(), 2);
        let _ = handle_vendor_login_key(&mut state, &ctrl(KeyCode::Char('a')));
        let draft = state.custom.as_ref().unwrap();
        assert!(
            draft
                .models
                .iter()
                .filter(|m| m.api_model.starts_with("gpt"))
                .all(|m| m.enabled)
        );
        assert!(
            !draft
                .models
                .iter()
                .find(|m| m.api_model == "claude-sonnet")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn custom_resync_preserves_enabled_and_id() {
        let mut state = VendorLoginState::custom_form();
        if let Some(draft) = state.custom.as_mut() {
            draft.id = Some("acme".into());
            draft.name.set_text("Acme");
        }
        state.apply_custom_models(
            vec![
                ("keep".into(), "Keep".into(), 32_000),
                ("drop".into(), "Drop".into(), 32_000),
            ],
            None,
        );
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Down));
        let _ = handle_vendor_login_key(&mut state, &key(KeyCode::Char(' ')));
        state.apply_custom_models(
            vec![
                ("keep".into(), "Keep".into(), 32_000),
                ("drop".into(), "Drop".into(), 32_000),
                ("new".into(), "New".into(), 32_000),
            ],
            None,
        );
        let draft = state.custom.as_ref().unwrap();
        assert!(
            draft
                .models
                .iter()
                .find(|m| m.api_model == "keep")
                .unwrap()
                .enabled
        );
        assert!(
            !draft
                .models
                .iter()
                .find(|m| m.api_model == "drop")
                .unwrap()
                .enabled
        );
        assert!(
            !draft
                .models
                .iter()
                .find(|m| m.api_model == "new")
                .unwrap()
                .enabled
        );
        match handle_vendor_login_key(&mut state, &key(KeyCode::Enter)) {
            VendorLoginOutcome::SaveCustom { provider_id, .. } => {
                assert_eq!(provider_id.as_deref(), Some("acme"));
            }
            other => panic!("expected SaveCustom, got {other:?}"),
        }
    }
}
