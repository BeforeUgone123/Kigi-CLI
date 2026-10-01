//! `/fusion` — pick a Devin fusion pair (lead + sidekick).
//!
//! Fusion pairs arrive as `fusion-*` values inside the agent's Model-category
//! configOption (`Fusion (<lead> + <sidekick>)`); `models_from_config_options`
//! stamps each synthesized `ModelInfo.meta["fusion"]` with the parsed
//! `{lead, sidekick}`. Switching rides the same `Action::SwitchModel` →
//! `session/set_config_option` path as `/model` — the command only filters the
//! catalog down to fusion entries and accepts a `lead` or `lead + sidekick`
//! token, matching devin-tui's `/fusion` picker.

use agent_client_protocol as acp;

use crate::acp::model_state::ModelState;
use crate::app::actions::Action;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

/// Switch to a fusion lead+sidekick pair (devin acp).
pub struct FusionCommand;

impl SlashCommand for FusionCommand {
    fn name(&self) -> &str {
        "fusion"
    }

    fn description(&self) -> &str {
        "Switch to a fusion lead+sidekick pair"
    }

    fn session_scoped(&self) -> bool {
        true
    }

    fn usage(&self) -> &str {
        "/fusion <lead[+sidekick]>"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        true
    }

    fn arg_placeholder(&self) -> Option<&str> {
        Some("<lead> [sidekick]")
    }

    fn visible(&self, ctx: &AppCtx) -> bool {
        fusion_entries(ctx.models).next().is_some()
    }

    fn suggest_args(&self, ctx: &AppCtx, _args_query: &str) -> Option<Vec<ArgItem>> {
        let current_id = ctx.models.current.as_ref();
        let items: Vec<ArgItem> = fusion_entries(ctx.models)
            .map(|(id, info, lead, sidekick)| ArgItem {
                display: if current_id == Some(id) {
                    format!("{} (current)", info.name)
                } else {
                    info.name.clone()
                },
                match_text: format!("{lead} {sidekick} {}", id.0),
                insert_text: info.name.clone(),
                description: format!("{lead} + {sidekick}"),
            })
            .collect();
        if items.is_empty() { None } else { Some(items) }
    }

    fn run(&self, ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            let pairs: Vec<String> = fusion_entries(ctx.models)
                .map(|(_, info, _, _)| info.name.clone())
                .collect();
            if pairs.is_empty() {
                return CommandResult::Error("No fusion pairs on this agent".into());
            }
            return CommandResult::Error(format!(
                "Usage: /fusion <pair> — available: {}",
                pairs.join(", ")
            ));
        }
        match resolve_fusion(ctx.models, trimmed) {
            Some(id) => CommandResult::Action(Action::SwitchModel {
                model_id: id,
                effort: None,
            }),
            None => {
                let pairs: Vec<String> = fusion_entries(ctx.models)
                    .map(|(_, info, _, _)| info.name.clone())
                    .collect();
                if pairs.is_empty() {
                    CommandResult::Error("No fusion pairs on this agent".into())
                } else {
                    CommandResult::Error(format!(
                        "Unknown fusion pair '{trimmed}' — available: {}",
                        pairs.join(", ")
                    ))
                }
            }
        }
    }
}

/// The fusion models in the catalog — `(model_id, info, lead, sidekick)`.
fn fusion_entries(
    models: &ModelState,
) -> impl Iterator<Item = (&acp::ModelId, &acp::ModelInfo, &str, &str)> {
    models.available.iter().filter_map(|(id, info)| {
        let f = info.meta.as_ref()?.get("fusion")?;
        Some((
            id,
            info,
            f.get("lead")?.as_str()?,
            f.get("sidekick")?.as_str()?,
        ))
    })
}

/// Match `arg` to a fusion pair: exact value id, full name (`Fusion (L + S)`),
/// or a `lead`/`lead sidekick`/`lead + sidekick` token (case-insensitive).
/// `lead`-only resolves to that lead's first pair, mirroring devin-tui's
/// default-sidekick rule.
fn resolve_fusion(models: &ModelState, arg: &str) -> Option<acp::ModelId> {
    let normalized: Vec<(acp::ModelId, String, String, String)> = fusion_entries(models)
        .map(|(id, info, lead, sidekick)| {
            (
                id.clone(),
                info.name.to_lowercase(),
                lead.to_lowercase(),
                sidekick.to_lowercase(),
            )
        })
        .collect();
    let a = arg.to_lowercase();
    // Exact value id (`fusion-…`).
    if let Some((id, _, _, _)) = normalized.iter().find(|(id, _, _, _)| id.0.as_ref() == arg) {
        return Some(id.clone());
    }
    // Full display name.
    if let Some((id, _, _, _)) = normalized.iter().find(|(_, n, _, _)| *n == a) {
        return Some(id.clone());
    }
    // `lead sidekick` / `lead + sidekick` — compare against the name core.
    let core = a
        .strip_prefix("fusion (")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(&a)
        .replace(" + ", " ");
    let mut lead_only: Option<&acp::ModelId> = None;
    for (id, _, lead, sidekick) in &normalized {
        if *lead == core {
            lead_only = lead_only.or(Some(id));
            continue;
        }
        if format!("{lead} {sidekick}") == core {
            return Some(id.clone());
        }
    }
    lead_only.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fusion_model(value: &str, lead: &str, sidekick: &str) -> (acp::ModelId, acp::ModelInfo) {
        let id = acp::ModelId::new(Arc::from(value));
        let info = acp::ModelInfo::new(id.clone(), format!("Fusion ({lead} + {sidekick})")).meta(
            serde_json::json!({"fusion": {"lead": lead, "sidekick": sidekick}})
                .as_object()
                .cloned(),
        );
        (id, info)
    }

    fn fusion_models() -> ModelState {
        let mut state = ModelState::default();
        for (id, info) in [
            fusion_model("fusion-a-b", "A", "B"),
            fusion_model("fusion-a-c", "A", "C"),
            fusion_model("fusion-d-b", "D", "B"),
        ] {
            state.available.insert(id, info);
        }
        // A non-fusion entry must be ignored.
        let plain = acp::ModelId::new(Arc::from("swe-2"));
        state.available.insert(
            plain.clone(),
            acp::ModelInfo::new(plain, "SWE-2".to_string()),
        );
        state
    }

    static EMPTY_BUNDLE: crate::app::bundle::BundleState = crate::app::bundle::BundleState {
        has_cache: false,
        version: String::new(),
        personas: Vec::new(),
        roles: Vec::new(),
        agents: Vec::new(),
        skills: Vec::new(),
        persona_details: Vec::new(),
        role_details: Vec::new(),
    };

    fn dummy_exec_ctx(models: &ModelState) -> CommandExecCtx<'_> {
        CommandExecCtx {
            models,
            session_id: None,
            bundle_state: &EMPTY_BUNDLE,
            screen_mode: crate::app::ScreenMode::Inline,
            pager_state: crate::settings::PagerLocalSnapshot::default(),
        }
    }

    #[test]
    fn resolves_by_value_id() {
        let models = fusion_models();
        assert_eq!(
            resolve_fusion(&models, "fusion-a-c")
                .map(|id| id.0.to_string())
                .as_deref(),
            Some("fusion-a-c")
        );
    }

    #[test]
    fn resolves_by_display_name_and_lead_sidekick() {
        let models = fusion_models();
        for q in ["Fusion (A + C)", "a + c", "a c"] {
            assert_eq!(
                resolve_fusion(&models, q)
                    .map(|id| id.0.to_string())
                    .as_deref(),
                Some("fusion-a-c"),
                "query: {q}"
            );
        }
    }

    #[test]
    fn lead_only_picks_first_pair() {
        let models = fusion_models();
        assert_eq!(
            resolve_fusion(&models, "d")
                .map(|id| id.0.to_string())
                .as_deref(),
            Some("fusion-d-b")
        );
    }

    #[test]
    fn ignores_non_fusion_and_unknown() {
        let models = fusion_models();
        assert!(resolve_fusion(&models, "swe-2").is_none());
        assert!(resolve_fusion(&models, "zzz").is_none());
    }

    #[test]
    fn run_emits_switch_model() {
        let models = fusion_models();
        let mut ctx = dummy_exec_ctx(&models);
        let cmd = FusionCommand;
        match cmd.run(&mut ctx, "a + c") {
            CommandResult::Action(Action::SwitchModel { model_id, effort }) => {
                assert_eq!(model_id.0.as_ref(), "fusion-a-c");
                assert!(effort.is_none());
            }
            other => panic!("expected SwitchModel, got {other:?}"),
        }
    }

    #[test]
    fn hidden_without_fusion_pairs() {
        let mut models = ModelState::default();
        let plain = acp::ModelId::new(Arc::from("swe-2"));
        models.available.insert(
            plain.clone(),
            acp::ModelInfo::new(plain, "SWE-2".to_string()),
        );
        let cwd = std::path::PathBuf::from("/tmp");
        let ctx = AppCtx {
            models: &models,
            cwd: &cwd,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        assert!(!FusionCommand.visible(&ctx));
    }
}
