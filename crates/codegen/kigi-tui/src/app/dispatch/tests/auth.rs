//! Tests for login, logout, account switching, and auth-code dispatchers.

use super::*;

/// A bash command typed while a turn is RUNNING takes the
/// server-authoritative immediate path (Effect + optimistic echo, no local
/// queue entry).
#[test]
fn bash_while_running_is_server_authoritative() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().session.state = AgentState::TurnRunning;

    let effects = dispatch(Action::SendBashCommand("ls -la".into()), &mut app);
    let pid = match &effects[0] {
        Effect::SendBashCommand {
            command, prompt_id, ..
        } => {
            assert_eq!(command, "ls -la");
            prompt_id.clone()
        }
        other => panic!("expected immediate SendBashCommand, got {other:?}"),
    };
    assert_eq!(app.agents[&id].session.queue_len(), 0);
    let q = app
        .shared_prompt_queue("test-session")
        .expect("echo present");
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].id, pid);
    assert_eq!(q[0].kind, "bash");
    assert_eq!(q[0].text, "ls -la");
}

#[test]
fn auth_complete_triggers_bundle_status_fetch() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    assert!(matches!(app.auth_state, AuthState::Done));
    // Pager only refreshes the on-disk catalog snapshot; the bundle
    // download runs inside the shell post-auth.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchBundleStatus))
    );
}

#[test]
fn auth_complete_with_deferred_load_also_fetches_status() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    app.deferred_startup.session =
        Some(crate::app::session_startup::DeferredSessionStartup::Load {
            session_id: "test-session".into(),
            session_cwd: None,
            chat_kind: false,
        });

    let effects = dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: None,
        }),
        &mut app,
    );

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::FetchBundleStatus))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadSession { .. }))
    );
    assert!(app.deferred_startup.session.is_none());
}

/// The Moonshot API-key entry flow: picking a row opens the paste box,
/// submitting a key dispatches ONE effect that persists the key and then
/// authenticates with the platform's method id, and the visual flips to the
/// connecting state under the same request seq.
#[test]
fn submit_platform_api_key_dispatches_persist_then_authenticate() {
    use crate::app::app_view::PlatformLogin;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };

    let effects = dispatch(
        Action::BeginPlatformKeyEntry(PlatformLogin(kigi_shell::models::PlatformId::MoonshotCn)),
        &mut app,
    );
    assert!(effects.is_empty(), "entering key entry is UI-only");
    let seq = match &app.auth_state {
        AuthState::Authenticating {
            request_seq,
            mode: AuthMode::ApiKeyEntry(PlatformLogin(kigi_shell::models::PlatformId::MoonshotCn)),
            ..
        } => *request_seq,
        other => panic!("expected ApiKeyEntry(MoonshotCn), got {other:?}"),
    };

    let effects = dispatch(Action::SubmitPlatformApiKey("sk-test-key".into()), &mut app);
    match effects.as_slice() {
        [
            Effect::PersistPlatformApiKeyAndAuthenticate {
                request_seq,
                target,
                key,
            },
        ] => {
            assert_eq!(*request_seq, seq);
            assert_eq!(
                *target,
                PlatformLogin(kigi_shell::models::PlatformId::MoonshotCn)
            );
            assert_eq!(key, "sk-test-key");
            assert_eq!(
                target.method_id().0.as_ref(),
                "moonshot-cn",
                "authenticate must use the shell's moonshot-cn method id"
            );
        }
        other => panic!("expected exactly the persist+authenticate effect, got {other:?}"),
    }
    // Same seq, connecting visual: this attempt's AuthComplete/AuthFailed
    // still matches.
    assert!(matches!(
        app.auth_state,
        AuthState::Authenticating {
            request_seq,
            mode: AuthMode::Pending,
            ..
        } if request_seq == seq
    ));

    // Failed validation lands back on the picker with the error line.
    dispatch(
        Action::TaskComplete(TaskResult::AuthFailed {
            request_seq: seq,
            error: "Invalid API key for moonshot-cn".into(),
        }),
        &mut app,
    );
    assert!(matches!(
        &app.auth_state,
        AuthState::Pending { error: Some(e) } if e == "Invalid API key for moonshot-cn"
    ));
}

/// Esc in the paste box returns to the picker (no error), clears the typed
/// key, and invalidates the seq so stale auth results are dropped.
#[test]
fn cancel_platform_key_entry_returns_to_picker() {
    use crate::app::app_view::PlatformLogin;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    dispatch(
        Action::BeginPlatformKeyEntry(PlatformLogin(kigi_shell::models::PlatformId::MoonshotAi)),
        &mut app,
    );
    app.auth_code_input = "sk-half-typed".into();
    let seq_before = app.next_auth_request_seq;

    let effects = dispatch(Action::CancelPlatformKeyEntry, &mut app);
    assert!(effects.is_empty());
    assert!(matches!(app.auth_state, AuthState::Pending { error: None }));
    assert!(app.auth_code_input.is_empty(), "typed key must be cleared");
    assert!(app.next_auth_request_seq > seq_before);
}

/// A submitted empty/whitespace key is a no-op (stays in the paste box).
#[test]
fn submit_platform_api_key_ignores_blank_key() {
    use crate::app::app_view::PlatformLogin;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    dispatch(
        Action::BeginPlatformKeyEntry(PlatformLogin(kigi_shell::models::PlatformId::MoonshotCn)),
        &mut app,
    );
    let effects = dispatch(Action::SubmitPlatformApiKey("   ".into()), &mut app);
    assert!(effects.is_empty());
    assert!(matches!(
        app.auth_state,
        AuthState::Authenticating {
            mode: AuthMode::ApiKeyEntry(PlatformLogin(kigi_shell::models::PlatformId::MoonshotCn)),
            ..
        }
    ));
}

/// `/login` from the welcome screen (startup / logged-out) must NOT
/// stash a return view — the normal login-then-load flow is preserved.
#[test]
fn login_from_welcome_does_not_stash_return_view() {
    let mut app = test_app();
    assert_eq!(app.active_view, ActiveView::Welcome);

    dispatch(Action::Login, &mut app);

    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, None);
}

/// A second auth-failed turn with no rewindable prompt
/// (`in_flight_prompt == None`) must not clobber the stash from an
/// earlier 401.
#[test]
fn second_auth_failure_does_not_clobber_reauth_stash() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.reauth_stashed_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "first prompt".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            chip_elements: Vec::new(),
        });
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
        agent.session.state = AgentState::TurnRunning;
        agent.turn_started_at = Some(std::time::Instant::now());
        agent.session.in_flight_prompt = None;
    }

    dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("Unauthorized (401)".to_string()),
            http_status: Some(401),
            prompt_id: None,
        }),
        &mut app,
    );

    assert_eq!(
        app.agents[&id]
            .reauth_stashed_prompt
            .as_ref()
            .map(|prompt| prompt.text.as_str()),
        Some("first prompt"),
        "a None in_flight_prompt must not wipe an earlier stash"
    );
}

/// Cancelling a mid-session re-auth drops the stashed prompt so it is
/// not silently resubmitted on a later, unrelated login.
#[test]
fn cancel_login_drops_reauth_stashed_prompt() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents.get_mut(&id).unwrap().reauth_stashed_prompt =
        Some(crate::app::agent::InFlightPrompt {
            text: "stale".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            chip_elements: Vec::new(),
        });

    dispatch(Action::Login, &mut app);
    dispatch(Action::CancelLogin, &mut app);

    assert!(
        app.agents[&id].reauth_stashed_prompt.is_none(),
        "cancelling re-auth must drop the stashed prompt"
    );
}

/// Cancelling a mid-session re-auth strips the stale `ReAuthRequired`
/// prompt from scrollback so a later `PromptResponse` cannot re-detect
/// it and re-stash the prompt for silent resubmission.
#[test]
fn cancel_login_strips_reauth_prompt_from_scrollback() {
    use crate::scrollback::block::RenderBlock;
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.reauth_stashed_prompt = Some(crate::app::agent::InFlightPrompt {
            text: "stale".into(),
            images: Vec::new(),
            scrollback_entry: crate::scrollback::EntryId::new(0),
            chip_elements: Vec::new(),
        });
        agent
            .scrollback
            .push_block(RenderBlock::session_event(SessionEvent::ReAuthRequired));
    }

    dispatch(Action::Login, &mut app);
    dispatch(Action::CancelLogin, &mut app);

    let sb = &app.agents[&id].scrollback;
    let has_reauth = (0..sb.len()).any(|i| {
        matches!(
            sb.entry(i).map(|e| &e.block),
            Some(RenderBlock::SessionEvent(ev)) if matches!(ev.event, SessionEvent::ReAuthRequired)
        )
    });
    assert!(
        !has_reauth,
        "cancelling re-auth must strip the stale re-auth prompt from scrollback"
    );
}

/// Empty `auth_methods` (preferred_method pin unavailable) must not invent
/// `kimi-code` or start an OIDC flow the agent did not advertise.
#[test]
fn login_with_empty_auth_methods_fails_closed() {
    let mut app = test_app_with_agent();
    app.auth_methods.clear();
    app.login_method_id = None;

    let effects = dispatch(Action::Login, &mut app);

    assert!(
        effects.is_empty(),
        "must not start Authenticate without an advertised method"
    );
    assert_eq!(
        app.active_view,
        ActiveView::Agent(AgentId(0)),
        "must stay on the session view"
    );
    assert!(
        matches!(
            &app.auth_state,
            AuthState::Pending { error: Some(msg) }
                if msg.contains("No login method available")
        ),
        "must surface no-login-method error, got {:?}",
        app.auth_state
    );
    assert!(app.login_method_id.is_none());
}

/// Cancelling a mid-session login returns to the session rather than
/// quitting the app, and clears the stashed view + auth state.
#[test]
fn cancel_login_restores_view() {
    let mut app = test_app_with_agent();
    dispatch(Action::Login, &mut app);
    assert_eq!(app.active_view, ActiveView::Welcome);

    let effects = dispatch(Action::CancelLogin, &mut app);

    assert!(effects.is_empty(), "cancel is pure state, no effects");
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));
    assert_eq!(app.auth_return_view, None);
    assert!(matches!(app.auth_state, AuthState::Done));
}

/// `CancelLogin` outside a mid-session login is a no-op (must not move
/// off the welcome screen or panic).
#[test]
fn cancel_login_noop_without_stashed_view() {
    let mut app = test_app();
    let effects = dispatch(Action::CancelLogin, &mut app);
    assert!(effects.is_empty());
    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, None);
}

#[test]
fn auth_complete_extracts_show_resolved_model_from_meta() {
    let mut app = test_app();
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };
    assert!(app.show_resolved_model);

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: Some(serde_json::json!({ "show_resolved_model": false })),
        }),
        &mut app,
    );

    assert!(!app.show_resolved_model);
}

#[test]
fn auth_complete_preserves_show_resolved_model_when_absent() {
    let mut app = test_app();
    app.show_resolved_model = false;
    app.auth_state = AuthState::Authenticating {
        request_seq: 1,
        handle: None,
        auth_url: None,
        mode: AuthMode::Pending,
    };

    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: 1,
            meta: Some(serde_json::to_value(kigi_shell::auth::AuthMeta::default()).unwrap()),
        }),
        &mut app,
    );

    assert!(!app.show_resolved_model);
}

/// `LoginWith` must authenticate with EXACTLY the chosen method and adopt
/// its label — even when a different method was resolved earlier.
/// Regression: the picker collapsed every row to the id-less `Login`,
/// which resolved to the first interactive method (kimi-code), so the
/// Grok/Claude/Copilot/Codex rows all opened the Kimi device flow.
#[test]
fn login_with_routes_to_the_chosen_method() {
    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    app.auth_methods = kigi_shell::agent::auth_method::build_auth_methods(
        kigi_shell::agent::auth_method::AuthMethodsBuildInputs {
            has_external_api_key: false,
            has_cached_token: false,
            login_label: None,
        },
    )
    .methods;
    // A previous login resolved kimi-code; the explicit choice must win.
    app.login_method_id = Some(acp::AuthMethodId::new("kimi-code"));
    app.login_label = Some("Kimi Code".into());

    let effects = dispatch(
        Action::LoginWith(acp::AuthMethodId::new("claude-pro-max")),
        &mut app,
    );

    let authenticated_with = effects.iter().find_map(|e| match e {
        Effect::Authenticate { method_id, .. } => Some(method_id.0.to_string()),
        _ => None,
    });
    assert_eq!(
        authenticated_with.as_deref(),
        Some("claude-pro-max"),
        "the chosen provider's own method must ride the Authenticate effect"
    );
    assert_eq!(
        app.login_method_id.as_ref().map(|id| id.0.as_ref()),
        Some("claude-pro-max")
    );
    assert_eq!(
        app.login_label.as_deref(),
        Some("Claude Pro/Max (subscription)"),
        "the picker label must follow the chosen method"
    );
    assert!(matches!(app.auth_state, AuthState::Authenticating { .. }));
}

/// A `LoginWith` id that is not among the shell-advertised methods fails
/// closed with an error on the picker — silently falling back to the
/// first method is exactly the routing bug this guards against.
#[test]
fn login_with_unknown_method_fails_closed() {
    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };

    let effects = dispatch(
        Action::LoginWith(acp::AuthMethodId::new("no-such-provider")),
        &mut app,
    );

    assert!(effects.is_empty(), "must not start any auth flow");
    assert!(
        matches!(
            &app.auth_state,
            AuthState::Pending { error: Some(e) } if e.contains("no-such-provider")
        ),
        "must surface the unknown method, got {:?}",
        app.auth_state
    );
}

/// `/login` opens the provider picker (welcome `Pending`) instead of
/// auto-starting a flow — and from inside a session it stashes the view
/// (so Esc/q/Cancel return to it) without emitting any auth effect.
#[test]
fn open_login_picker_shows_picker_and_stashes_view() {
    let mut app = test_app_with_agent();
    assert_eq!(app.active_view, ActiveView::Agent(AgentId(0)));

    let effects = dispatch(Action::OpenLoginPicker, &mut app);

    assert!(effects.is_empty(), "the picker itself starts nothing");
    assert_eq!(app.active_view, ActiveView::Welcome);
    assert_eq!(app.auth_return_view, Some(ActiveView::Agent(AgentId(0))));
    assert!(matches!(app.auth_state, AuthState::Pending { error: None }));
}

/// A successful login stamps `_meta.connected` on the method that just
/// authenticated, so a later `/login` picker shows its green badge without
/// re-initializing the shell.
#[test]
fn auth_complete_marks_the_authenticated_method_connected() {
    use crate::app::app_view::pending_menu_items;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    app.auth_methods = kigi_shell::agent::auth_method::build_auth_methods(
        kigi_shell::agent::auth_method::AuthMethodsBuildInputs {
            has_external_api_key: false,
            has_cached_token: false,
            login_label: None,
        },
    )
    .methods;

    dispatch(
        Action::LoginWith(acp::AuthMethodId::new("xai-grok")),
        &mut app,
    );
    let seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        other => panic!("expected Authenticating, got {other:?}"),
    };
    dispatch(
        Action::TaskComplete(TaskResult::AuthComplete {
            request_seq: seq,
            meta: None,
        }),
        &mut app,
    );

    let items = pending_menu_items(&app.auth_methods, None);
    let grok = items
        .iter()
        .find(|i| i.label().starts_with("xAI Grok"))
        .expect("grok row");
    assert!(
        grok.connected(&app.auth_methods),
        "the just-authenticated method must show as connected"
    );
    let kimi = items
        .iter()
        .find(|i| i.label().starts_with("Kimi Code"))
        .expect("kimi row");
    assert!(
        !kimi.connected(&app.auth_methods),
        "other methods must stay unmarked"
    );
}

fn custom_field(app: &AppView) -> crate::app::custom_entry::CustomEntryField {
    match &app.auth_state {
        AuthState::Authenticating {
            mode: AuthMode::CustomProviderEntry(step),
            ..
        } => step.field,
        other => panic!("expected a custom provider step, got {other:?}"),
    }
}

/// URL, name, key; the last Enter opens the selection screen and fetches.
#[test]
fn custom_provider_steps_end_in_the_model_selection() {
    use crate::app::custom_entry::CustomEntryField;
    use kigi_shell::models::custom::CustomApi;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };

    let effects = dispatch(
        Action::BeginCustomProviderEntry(CustomApi::Anthropic),
        &mut app,
    );
    assert!(effects.is_empty());
    assert_eq!(custom_field(&app), CustomEntryField::BaseUrl);
    let seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        _ => unreachable!(),
    };

    dispatch(
        Action::SubmitCustomProviderInput("https://api.gw.example/v1/".into()),
        &mut app,
    );
    assert_eq!(custom_field(&app), CustomEntryField::Name);
    assert_eq!(
        app.auth_code_input, "gw-example",
        "the name is prefilled from the host"
    );

    dispatch(Action::SubmitCustomProviderInput("gw".into()), &mut app);
    assert_eq!(custom_field(&app), CustomEntryField::Key);
    assert!(app.auth_code_input.is_empty(), "the key box starts empty");

    let effects = dispatch(Action::SubmitCustomProviderInput("sk-gw".into()), &mut app);
    match effects.as_slice() {
        [
            Effect::FetchCustomProviderModels {
                request_seq,
                provider,
                key,
            },
        ] => {
            assert_eq!(*request_seq, seq);
            assert_eq!(provider.name, "gw");
            assert_eq!(provider.api, CustomApi::Anthropic);
            assert_eq!(provider.base_url, "https://api.gw.example/v1");
            assert_eq!(key, "sk-gw");
        }
        other => panic!("expected exactly the fetch effect, got {other:?}"),
    }
    assert!(matches!(
        app.auth_state,
        AuthState::Authenticating {
            request_seq,
            mode: AuthMode::CustomProviderSelect,
            ..
        } if request_seq == seq
    ));
    assert!(app.custom_select.is_some());
    assert!(app.auth_code_input.is_empty());
}

/// The listing fills rows; the selection persists only the chosen models.
#[test]
fn custom_provider_selection_persists_the_chosen_models() {
    use kigi_shell::models::custom::{CustomApi, CustomProvider};

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    dispatch(
        Action::BeginCustomProviderEntry(CustomApi::OpenAi),
        &mut app,
    );
    dispatch(
        Action::SubmitCustomProviderInput("https://h.example".into()),
        &mut app,
    );
    dispatch(Action::SubmitCustomProviderInput("gw".into()), &mut app);
    dispatch(Action::SubmitCustomProviderInput("sk-gw".into()), &mut app);
    let seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        _ => unreachable!(),
    };

    // The listing lands: rows appear, the probed base is adopted.
    let adopted = CustomProvider::new("gw", CustomApi::OpenAi, "https://h.example/v1").unwrap();
    let listed = kigi_shell::agent::custom_providers::LoginFetch {
        provider: adopted,
        model_ids: vec!["m1".to_owned(), "m2".to_owned()],
    };
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CustomProviderModelsListed {
            request_seq: seq,
            result: Ok(listed),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    let state = app.custom_select.as_ref().expect("selection lives");
    assert_eq!(state.provider.base_url, "https://h.example/v1");
    assert_eq!(state.rows.len(), 2);
    assert!(!state.rows[0].selected);

    // An empty selection refuses to finish once the listing resolved.
    assert!(dispatch(Action::SubmitCustomProviderSelection, &mut app).is_empty());
    assert!(
        app.custom_select
            .as_ref()
            .and_then(|s| s.error.as_deref())
            .is_some()
    );

    // A late manual id typed before the fetch survives the fill.
    let state = app.custom_select.as_mut().expect("selection lives");
    state.add_manual("claude-opus-5-5[1m]");
    assert_eq!(state.rows.len(), 3);
    assert!(state.rows[2].manual && state.rows[2].selected);

    // Select m2 as well, then finish.
    let state = app.custom_select.as_mut().expect("selection lives");
    state.highlight = 1;
    state.toggle_highlighted();
    let effects = dispatch(Action::SubmitCustomProviderSelection, &mut app);
    match effects.as_slice() {
        [
            Effect::PersistCustomProviderAndAuthenticate {
                request_seq,
                provider,
                key,
                models,
            },
        ] => {
            assert_eq!(*request_seq, seq);
            assert_eq!(provider.base_url, "https://h.example/v1");
            assert_eq!(key, "sk-gw");
            assert_eq!(models, &["m2".to_owned(), "claude-opus-5-5[1m]".to_owned()]);
        }
        other => panic!("expected the persist effect, got {other:?}"),
    }
    assert!(app.custom_select.is_none());
    assert!(matches!(
        app.auth_state,
        AuthState::Authenticating {
            mode: AuthMode::Pending,
            ..
        }
    ));
    assert_eq!(
        app.auth_in_flight_method.as_ref().map(|m| m.0.as_ref()),
        Some("custom:gw")
    );
}

/// Finishing while the listing is in flight completes when it lands.
#[test]
fn custom_provider_selection_finish_waits_for_the_fetch() {
    use kigi_shell::models::custom::{CustomApi, CustomProvider};

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    dispatch(
        Action::BeginCustomProviderEntry(CustomApi::OpenAi),
        &mut app,
    );
    dispatch(
        Action::SubmitCustomProviderInput("https://h.example".into()),
        &mut app,
    );
    dispatch(Action::SubmitCustomProviderInput("gw".into()), &mut app);
    dispatch(Action::SubmitCustomProviderInput("sk-gw".into()), &mut app);
    let seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        _ => unreachable!(),
    };

    let state = app.custom_select.as_mut().expect("selection lives");
    state.add_manual("m1");
    // Enter with an empty box while the fetch runs: no persist yet.
    assert!(dispatch(Action::SubmitCustomProviderSelection, &mut app).is_empty());
    assert!(app.custom_select.as_ref().expect("lives").finish_requested);
    assert!(matches!(
        app.auth_state,
        AuthState::Authenticating {
            mode: AuthMode::CustomProviderSelect,
            ..
        }
    ));

    // The listing lands with an adopted /v1 base; the finish fires now.
    let adopted = CustomProvider::new("gw", CustomApi::OpenAi, "https://h.example/v1").unwrap();
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CustomProviderModelsListed {
            request_seq: seq,
            result: Ok(kigi_shell::agent::custom_providers::LoginFetch {
                provider: adopted,
                model_ids: vec!["m2".to_owned()],
            }),
        }),
        &mut app,
    );
    match effects.as_slice() {
        [
            Effect::PersistCustomProviderAndAuthenticate {
                provider, models, ..
            },
        ] => {
            assert_eq!(provider.base_url, "https://h.example/v1");
            assert_eq!(models, &["m1".to_owned()]);
        }
        other => panic!("expected the persist effect, got {other:?}"),
    }
    assert!(app.custom_select.is_none());
}

/// A failed listing keeps the manual input usable and shows the reason.
#[test]
fn custom_provider_selection_survives_a_failed_fetch() {
    use kigi_shell::models::custom::CustomApi;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    dispatch(
        Action::BeginCustomProviderEntry(CustomApi::OpenAi),
        &mut app,
    );
    dispatch(
        Action::SubmitCustomProviderInput("https://h.example/v1".into()),
        &mut app,
    );
    dispatch(Action::SubmitCustomProviderInput("gw".into()), &mut app);
    dispatch(Action::SubmitCustomProviderInput("sk-gw".into()), &mut app);
    let seq = match &app.auth_state {
        AuthState::Authenticating { request_seq, .. } => *request_seq,
        _ => unreachable!(),
    };

    let effects = dispatch(
        Action::TaskComplete(TaskResult::CustomProviderModelsListed {
            request_seq: seq,
            result: Err("https://h.example/v1/models answered HTTP 404".to_owned()),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    let state = app.custom_select.as_ref().expect("selection lives");
    assert!(matches!(
        &state.fetch,
        crate::app::custom_entry::SelectFetch::Failed(reason) if reason.contains("404")
    ));

    // A stale result for an older request never clobbers the screen.
    let effects = dispatch(
        Action::TaskComplete(TaskResult::CustomProviderModelsListed {
            request_seq: seq + 99,
            result: Ok(kigi_shell::agent::custom_providers::LoginFetch {
                provider: state.provider.clone(),
                model_ids: vec!["stale".to_owned()],
            }),
        }),
        &mut app,
    );
    assert!(effects.is_empty());
    assert!(
        app.custom_select
            .as_ref()
            .expect("selection lives")
            .rows
            .is_empty()
    );

    // Manual add still finishes the flow.
    let state = app.custom_select.as_mut().expect("selection lives");
    state.add_manual("m1");
    let effects = dispatch(Action::SubmitCustomProviderSelection, &mut app);
    assert!(matches!(
        effects.as_slice(),
        [Effect::PersistCustomProviderAndAuthenticate { models, .. }] if models == &["m1".to_owned()]
    ));
}

/// A refused field keeps the user on it.
#[test]
fn custom_provider_refusals_stay_on_the_field() {
    use crate::app::custom_entry::CustomEntryField;
    use kigi_shell::models::custom::CustomApi;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    dispatch(
        Action::BeginCustomProviderEntry(CustomApi::OpenAi),
        &mut app,
    );

    assert!(
        dispatch(
            Action::SubmitCustomProviderInput("host.example".into()),
            &mut app
        )
        .is_empty()
    );
    assert_eq!(custom_field(&app), CustomEntryField::BaseUrl);
    assert!(app.custom_entry.error.is_some());

    dispatch(
        Action::SubmitCustomProviderInput("https://h.example/v1".into()),
        &mut app,
    );
    assert!(
        dispatch(
            Action::SubmitCustomProviderInput("anthropic".into()),
            &mut app
        )
        .is_empty()
    );
    assert_eq!(custom_field(&app), CustomEntryField::Name);
    assert!(app.custom_entry.error.is_some());
}

/// Esc returns to the picker and drops the draft.
#[test]
fn cancel_custom_provider_entry_returns_to_picker() {
    use kigi_shell::models::custom::CustomApi;

    let mut app = test_app();
    app.auth_state = AuthState::Pending { error: None };
    dispatch(
        Action::BeginCustomProviderEntry(CustomApi::OpenAi),
        &mut app,
    );
    dispatch(Action::SubmitCustomProviderInput("nope".into()), &mut app);
    assert!(app.custom_entry.error.is_some());
    let seq_before = app.next_auth_request_seq;

    let effects = dispatch(Action::CancelPlatformKeyEntry, &mut app);

    assert!(effects.is_empty());
    assert!(matches!(app.auth_state, AuthState::Pending { error: None }));
    assert!(app.custom_entry.error.is_none());
    assert!(app.next_auth_request_seq > seq_before);
}
