//! `/provider-login` — sign in to a third-party model provider.

use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

pub struct ProviderLoginCommand;

impl SlashCommand for ProviderLoginCommand {
    fn name(&self) -> &str {
        "provider-login"
    }

    fn aliases(&self) -> &[&str] {
        &["plogin"]
    }

    fn description(&self) -> &str {
        "Sign in to a provider, or add/edit a custom one"
    }

    fn usage(&self) -> &str {
        "/provider-login [provider]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<provider>")
    }

    fn suggest_args(&self, _ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        let mut items: Vec<ArgItem> = xai_grok_shell::compat::arg_items()
            .into_iter()
            .map(|(id, name, desc)| ArgItem {
                display: name.clone(),
                match_text: format!("{id} {name}"),
                insert_text: id,
                description: desc,
            })
            .collect();
        items.push(ArgItem {
            display: "Add custom provider".into(),
            match_text: "custom add provider".into(),
            insert_text: "custom".into(),
            description: "Name, base URL, protocol, API key, then sync models".into(),
        });
        Some(items)
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let id = args.trim();
        if id.is_empty() {
            return CommandResult::Action(Action::OpenVendorLogin { provider_id: None });
        }
        if id == "custom" || id == "add" {
            return CommandResult::Action(Action::OpenVendorLogin {
                provider_id: Some("custom".into()),
            });
        }
        let Some(spec) = xai_grok_shell::compat::provider_by_id(id) else {
            if xai_grok_shell::compat::is_known_vendor(id) {
                return CommandResult::Action(Action::OpenVendorLogin {
                    provider_id: Some(id.to_owned()),
                });
            }
            return CommandResult::Error(format!(
                "Unknown provider '{id}'. Try openai, openai-codex, anthropic, openrouter, deepseek, ollama, or custom."
            ));
        };
        CommandResult::Action(Action::OpenVendorLogin {
            provider_id: Some(spec.id.to_owned()),
        })
    }
}
