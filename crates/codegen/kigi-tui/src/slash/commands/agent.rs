//! `/agent [name]` — open a new agent tab bound to a secondary agent
//! backend.
//!
//! `<name>` resolves through the agent-provider registry (`acp::provider`):
//! builtin presets (`kigi`, `local-devin`, `devin`) first, then
//! `[agent_providers.<name>] command = "..."` entries in config.toml.
//! With no argument the dispatcher lists the known provider names instead
//! of connecting.

use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

/// Open an agent tab on a secondary agent backend (e.g. `devin acp`).
pub struct AgentCommand;

impl SlashCommand for AgentCommand {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "Open an agent tab on another agent provider"
    }

    fn usage(&self) -> &str {
        "/agent [name]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("name")
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        CommandResult::Action(Action::ConnectAgentProvider {
            name: if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::model_state::ModelState;
    use crate::app::bundle::BundleState;

    fn ctx<'a>(models: &'a ModelState, bundle: &'a BundleState) -> CommandExecCtx<'a> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: bundle,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot {
                multiline_mode: false,
                yolo_mode: false,
                ..crate::settings::PagerLocalSnapshot::default()
            },
        }
    }

    #[test]
    fn no_args_lists_providers() {
        let (models, bundle) = (ModelState::default(), BundleState::default());
        let mut c = ctx(&models, &bundle);
        match AgentCommand.run(&mut c, "  ") {
            CommandResult::Action(Action::ConnectAgentProvider { name }) => {
                assert!(name.is_none());
            }
            other => panic!("expected ConnectAgentProvider, got {other:?}"),
        }
    }

    #[test]
    fn name_arg_connects_provider() {
        let (models, bundle) = (ModelState::default(), BundleState::default());
        let mut c = ctx(&models, &bundle);
        match AgentCommand.run(&mut c, "  local-devin  ") {
            CommandResult::Action(Action::ConnectAgentProvider { name }) => {
                assert_eq!(name.as_deref(), Some("local-devin"));
            }
            other => panic!("expected ConnectAgentProvider, got {other:?}"),
        }
    }

    #[test]
    fn metadata() {
        let cmd = AgentCommand;
        assert_eq!(cmd.name(), "agent");
        assert!(cmd.takes_args());
        assert_eq!(cmd.arg_placeholder(), Some("name"));
        assert!(!cmd.description().is_empty());
        assert!(!cmd.usage().is_empty());
    }
}
