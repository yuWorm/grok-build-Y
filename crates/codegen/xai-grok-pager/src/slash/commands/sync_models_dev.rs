//! `/sync-models-dev` — refresh vendor model lists and models.dev metadata.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct SyncModelsDevCommand;

impl SlashCommand for SyncModelsDevCommand {
    fn name(&self) -> &str {
        "sync-models-dev"
    }

    fn aliases(&self) -> &[&str] {
        &["sync-models"]
    }

    fn description(&self) -> &str {
        "Refresh vendor model lists, reasoning menus, and context windows"
    }

    fn usage(&self) -> &str {
        "/sync-models-dev"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::SyncModelsDev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_models_dev_metadata() {
        let cmd = SyncModelsDevCommand;
        assert_eq!(cmd.name(), "sync-models-dev");
        assert_eq!(cmd.aliases(), &["sync-models"]);
        assert!(!cmd.takes_args());
    }
}
