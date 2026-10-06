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

use crate::acp::model_state::{ModelState, native_fusion};
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

    fn suggest_args(&self, ctx: &AppCtx, args_query: &str) -> Option<Vec<ArgItem>> {
        let models = ctx.models;
        let current_id = models.current.as_ref();

        if let Some(lead_model) = detect_native_fusion_phase(models, args_query) {
            let items: Vec<ArgItem> = models
                .available
                .iter()
                .filter_map(|(id, info)| native_fusion(info).map(|f| (id, f)))
                .filter(|(_, f)| f.lead_model == lead_model)
                .map(|(id, f)| ArgItem {
                    display: if current_id == Some(id) {
                        format!("{} (current)", f.sidekick)
                    } else {
                        f.sidekick.clone()
                    },
                    match_text: format!(
                        "{} devin/{} devin/{} {} {}",
                        f.lead, f.lead_model, f.lead, f.sidekick, id.0
                    ),
                    insert_text: id.0.to_string(),
                    description: format!("{} + {}", f.lead, f.sidekick),
                })
                .collect();
            return if items.is_empty() { None } else { Some(items) };
        }

        let mut items: Vec<ArgItem> = Vec::new();
        let mut seen_leads: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (id, info) in &models.available {
            if let Some(f) = native_fusion(info) {
                if !seen_leads.insert(f.lead_model.clone()) {
                    continue;
                }
                let any_current = models.available.iter().any(|(mid, minfo)| {
                    current_id == Some(mid)
                        && native_fusion(minfo).is_some_and(|g| g.lead_model == f.lead_model)
                });
                items.push(ArgItem {
                    display: if any_current {
                        format!("{} (current)", f.lead)
                    } else {
                        f.lead.clone()
                    },
                    match_text: format!("{} devin/{}", f.lead, f.lead_model),
                    insert_text: format!("devin/{} ", f.lead_model),
                    description: "Devin · fusion sidekicks".to_string(),
                });
                continue;
            }
            if let Some(f_meta) = info.meta.as_ref().and_then(|m| m.get("fusion")) {
                let lead = f_meta.get("lead").and_then(|v| v.as_str());
                let sidekick = f_meta.get("sidekick").and_then(|v| v.as_str());
                if let (Some(lead), Some(sidekick)) = (lead, sidekick) {
                    items.push(ArgItem {
                        display: if current_id == Some(id) {
                            format!("{} (current)", info.name)
                        } else {
                            info.name.clone()
                        },
                        match_text: format!("{lead} {sidekick} {}", id.0),
                        insert_text: info.name.clone(),
                        description: format!("{lead} + {sidekick}"),
                    });
                }
            }
        }
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

fn detect_native_fusion_phase(models: &ModelState, args_query: &str) -> Option<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut best: Option<(usize, String)> = None;
    for info in models.available.values() {
        let Some(f) = native_fusion(info) else {
            continue;
        };
        if !seen.insert(f.lead_model.clone()) {
            continue;
        }
        for token in [f.lead.clone(), format!("devin/{}", f.lead_model)] {
            if args_query.len() > token.len()
                && args_query.is_char_boundary(token.len())
                && args_query[..token.len()].eq_ignore_ascii_case(&token)
                && args_query[token.len()..].starts_with(char::is_whitespace)
                && best.as_ref().is_none_or(|(len, _)| token.len() > *len)
            {
                best = Some((token.len(), f.lead_model.clone()));
            }
        }
    }
    best.map(|(_, lead_model)| lead_model)
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
    for (id, info) in &models.available {
        if let Some(f) = native_fusion(info)
            && format!("devin/{}", f.lead_model).eq_ignore_ascii_case(arg)
        {
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

    fn native_pair(
        pair_uid: &str,
        lead: &str,
        sidekick: &str,
        lead_model: &str,
        sidekick_model: &str,
    ) -> (acp::ModelId, acp::ModelInfo) {
        let id = acp::ModelId::new(Arc::from(format!("devin/{pair_uid}")));
        let info = acp::ModelInfo::new(id.clone(), format!("Fusion ({lead} + {sidekick})")).meta(
            serde_json::json!({
                "fusion": {
                    "lead": lead, "sidekick": sidekick,
                    "leadModel": lead_model, "sidekickModel": sidekick_model,
                },
            })
            .as_object()
            .cloned(),
        );
        (id, info)
    }

    fn native_fusion_models() -> ModelState {
        let mut state = fusion_models();
        for (id, info) in [
            native_pair("fusion-a-sidekick-b", "LeadA", "HelperB", "a", "b"),
            native_pair("fusion-a-sidekick-c", "LeadA", "HelperC", "a", "c"),
            native_pair("fusion-d-sidekick-b", "LeadD", "HelperB", "d", "b"),
        ] {
            state.available.insert(id, info);
        }
        state
    }

    #[test]
    fn native_fusion_root_rows_one_per_lead_with_trailing_space() {
        let models = native_fusion_models();
        let cwd = std::path::PathBuf::from("/tmp");
        let ctx = AppCtx {
            models: &models,
            cwd: &cwd,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        let items = FusionCommand.suggest_args(&ctx, "").expect("items");
        let native_rows: Vec<&ArgItem> = items
            .iter()
            .filter(|i| i.insert_text.ends_with(' '))
            .collect();
        assert_eq!(native_rows.len(), 2, "one root row per lead model");
        assert_eq!(native_rows[0].display, "LeadA");
        assert_eq!(native_rows[0].insert_text, "devin/a ");
        assert_eq!(native_rows[1].display, "LeadD");
        assert_eq!(native_rows[1].insert_text, "devin/d ");
        assert!(
            items.iter().any(|i| i.insert_text == "Fusion (A + B)"),
            "foreign single rows remain"
        );
    }

    #[test]
    fn native_fusion_child_phase_lists_pair_uids() {
        let models = native_fusion_models();
        let cwd = std::path::PathBuf::from("/tmp");
        let ctx = AppCtx {
            models: &models,
            cwd: &cwd,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        for prefix in ["devin/a ", "Leada "] {
            let items = FusionCommand.suggest_args(&ctx, prefix).expect("children");
            let inserts: Vec<&str> = items.iter().map(|i| i.insert_text.as_str()).collect();
            assert_eq!(
                inserts,
                vec!["devin/fusion-a-sidekick-b", "devin/fusion-a-sidekick-c"],
                "prefix {prefix:?} lists the actual pair uids"
            );
            assert_eq!(items[0].display, "HelperB");
            assert!(
                items[0].match_text.contains("devin/a")
                    && items[0].match_text.contains("HelperB")
                    && items[0].match_text.contains("LeadA"),
                "match_text carries parent aliases + helper: {}",
                items[0].match_text
            );
        }
    }

    #[test]
    fn native_fusion_resolves_exact_lead_alias_and_full_name() {
        let models = native_fusion_models();
        for (q, want) in [
            ("devin/fusion-a-sidekick-c", "devin/fusion-a-sidekick-c"),
            ("Fusion (LeadA + HelperC)", "devin/fusion-a-sidekick-c"),
            ("leada + helperc", "devin/fusion-a-sidekick-c"),
            ("leada helperc", "devin/fusion-a-sidekick-c"),
            ("devin/a", "devin/fusion-a-sidekick-b"),
            ("leada", "devin/fusion-a-sidekick-b"),
            ("devin/d", "devin/fusion-d-sidekick-b"),
        ] {
            assert_eq!(
                resolve_fusion(&models, q)
                    .map(|id| id.0.to_string())
                    .as_deref(),
                Some(want),
                "query: {q}"
            );
        }
    }

    #[test]
    fn native_fusion_phase_longest_overlapping_lead_prefix_and_case() {
        let mut models = fusion_models();
        for (id, info) in [
            native_pair("fusion-lead-sidekick-h1", "Lead", "H1", "lead", "h1"),
            native_pair(
                "fusion-lead-pro-sidekick-h1",
                "Lead Pro",
                "H1",
                "lead-pro",
                "h1",
            ),
            native_pair(
                "fusion-lead-pro-sidekick-h9",
                "Lead Pro",
                "H9",
                "lead-pro",
                "h9",
            ),
        ] {
            models.available.insert(id, info);
        }
        let cwd = std::path::PathBuf::from("/tmp");
        let ctx = AppCtx {
            models: &models,
            cwd: &cwd,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        let items = FusionCommand
            .suggest_args(&ctx, "lead pro ")
            .expect("children");
        let inserts: Vec<&str> = items.iter().map(|i| i.insert_text.as_str()).collect();
        assert_eq!(
            inserts,
            vec![
                "devin/fusion-lead-pro-sidekick-h1",
                "devin/fusion-lead-pro-sidekick-h9"
            ],
            "longest lead alias wins: `lead pro ` beats the `lead` prefix"
        );

        let items = FusionCommand
            .suggest_args(&ctx, "DEVIN/LEAD-PRO ")
            .expect("children");
        let inserts: Vec<&str> = items.iter().map(|i| i.insert_text.as_str()).collect();
        assert_eq!(
            inserts.len(),
            2,
            "devin/<lead_uid> alias is case-insensitive"
        );
    }

    #[test]
    fn native_fusion_current_pair_marks_root_and_helper() {
        let mut models = fusion_models();
        let current = acp::ModelId::new(Arc::from("devin/fusion-a-sidekick-b"));
        for (id, info) in [
            native_pair("fusion-a-sidekick-b", "LeadA", "HelperB", "a", "b"),
            native_pair("fusion-a-sidekick-c", "LeadA", "HelperC", "a", "c"),
            native_pair("fusion-d-sidekick-b", "LeadD", "HelperB", "d", "b"),
        ] {
            models.available.insert(id, info);
        }
        models.current = Some(current);
        let cwd = std::path::PathBuf::from("/tmp");
        let ctx = AppCtx {
            models: &models,
            cwd: &cwd,
            screen_mode: crate::app::ScreenMode::Inline,
        };
        let items = FusionCommand.suggest_args(&ctx, "").expect("items");
        let lead_a = items.iter().find(|i| i.insert_text == "devin/a ").unwrap();
        assert_eq!(
            lead_a.display, "LeadA (current)",
            "any active pair marks its lead"
        );
        let lead_d = items.iter().find(|i| i.insert_text == "devin/d ").unwrap();
        assert_eq!(lead_d.display, "LeadD");

        let items = FusionCommand
            .suggest_args(&ctx, "devin/a ")
            .expect("children");
        let helper_b = items
            .iter()
            .find(|i| i.insert_text == "devin/fusion-a-sidekick-b")
            .unwrap();
        assert_eq!(helper_b.display, "HelperB (current)");
        let helper_c = items
            .iter()
            .find(|i| i.insert_text == "devin/fusion-a-sidekick-c")
            .unwrap();
        assert_eq!(helper_c.display, "HelperC");
    }

    #[test]
    fn native_fusion_run_emits_switch_model() {
        let mut models = fusion_models();
        let (id, info) = native_pair("fusion-a-sidekick-b", "LeadA", "HelperB", "a", "b");
        models.available.insert(id, info);
        let mut ctx = dummy_exec_ctx(&models);
        let out = FusionCommand.run(&mut ctx, "devin/fusion-a-sidekick-b");
        let model_id = match out {
            CommandResult::Action(Action::SwitchModel { model_id, .. }) => model_id,
            other => panic!("expected SwitchModel, got {other:?}"),
        };
        assert_eq!(model_id.0.as_ref(), "devin/fusion-a-sidekick-b");

        let out = FusionCommand.run(&mut ctx, "devin/a");
        assert_eq!(
            match out {
                CommandResult::Action(Action::SwitchModel { model_id, .. }) => {
                    model_id.0.to_string()
                }
                other => panic!("expected SwitchModel, got {other:?}"),
            },
            "devin/fusion-a-sidekick-b"
        );
    }
}
