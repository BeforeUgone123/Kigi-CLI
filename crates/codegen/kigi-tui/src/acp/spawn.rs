//! Agent spawning — creates the agent and ACP channels.
//!
//! The default is KigiShell (in-process); `--external-agent` swaps in a
//! child-process ACP agent (e.g. `devin acp`) over newline-delimited stdio.

use std::rc::Rc;
use std::thread;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use kigi_acp_lib::{
    AcpAgentChannel, AcpClientChannel, AcpClientTx, AcpGatewayReceiver, AcpGatewaySender,
    acp_channels,
};
use kigi_shell::{
    agent::{MvpAgent, config::Config as AgentConfig, models::RefreshStrategy},
    auth::AuthManager,
    util::kigi_home::kigi_home,
};

/// Result of spawning a child agent.
pub struct SpawnedAgent {
    /// Kept alive so the thread isn't detached. Will be used for graceful shutdown.
    pub _thread_handle: thread::JoinHandle<Result<()>>,
    pub channel: AcpClientChannel,
    pub cancel: CancellationToken,
    /// The agent's `AuthManager`, shared so pager-side consumers resolve the
    /// same refreshing bearer as chat traffic.
    pub auth_manager: std::sync::Arc<AuthManager>,
}

/// Pager-side prerequisites shared by every agent transport: the `AuthManager`
/// pager consumers resolve bearers through, plus the bootstrap sequence
/// (managed policy, bundled skills, model catalog) local features depend on.
async fn bootstrap_pager_side(
    agent_config: &AgentConfig,
) -> Result<(
    std::sync::Arc<AuthManager>,
    AgentConfig,
    kigi_shell::agent::models::ModelsManager,
)> {
    let auth_manager = std::sync::Arc::new(AuthManager::new(
        &kigi_home(),
        agent_config.kimi_code_config.clone(),
    ));
    auth_manager.configure_refresher();
    // Pause token refreshes across system sleep so an OIDC refresh can't
    // straddle a suspend (which can revoke the refresh token and force
    // re-login). No-op where the OS listener is unavailable.
    auth_manager.start_system_power_listener();

    // Best-effort refresh of managed policy before bootstrap reads it (repairs a wrong-identity/missing
    // cache). Never errors — the OS-protected system/MDM layers still apply.
    kigi_shell::managed_config::ensure_managed_policy_present(&auth_manager).await;

    // Run the full bootstrap sequence: config resolution, process-level
    // singletons (including `extract_bundled_files` which writes compiled-in
    // skills to ~/.kigi/skills/), and model catalog construction.
    let (agent_config, models_manager) =
        kigi_shell::agent::init::bootstrap(agent_config, &auth_manager, None)
            .map_err(|e| anyhow::anyhow!(e))?;
    models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;
    Ok((auth_manager, agent_config, models_manager))
}

/// Spawn a KigiShell agent in a background thread.
pub async fn spawn_kigi_shell(
    agent_config: AgentConfig,
    cancel: &CancellationToken,
    memory_config: Option<kigi_shell::config::MemoryConfig>,
) -> Result<SpawnedAgent> {
    let (auth_manager, agent_config, models_manager) = bootstrap_pager_side(&agent_config).await?;

    let agent_cancel = cancel.child_token();
    let (acp_client, acp_agent) = acp_channels();

    // Clone before `auth_manager` is moved into the agent closure below, so the
    // pager can share the same refreshing bearer.
    let auth_manager_for_pager = auth_manager.clone();

    let spawn_fn: Box<dyn FnOnce(AcpClientTx) -> Result<Rc<MvpAgent>> + Send + 'static> = {
        Box::new(move |client_tx| {
            let gateway = AcpGatewaySender::new(client_tx);

            let mut agent =
                MvpAgent::with_models(gateway, &agent_config, auth_manager, models_manager);
            if let Some(mc) = memory_config {
                agent.set_memory_config(mc);
            }
            Ok(Rc::new(agent))
        })
    };

    let handle = spawn_agent_thread_direct(spawn_fn, acp_agent, agent_cancel.clone())?;

    Ok(SpawnedAgent {
        _thread_handle: handle,
        channel: acp_client,
        cancel: agent_cancel,
        auth_manager: auth_manager_for_pager,
    })
}

/// Spawn an external ACP agent command (e.g. `devin acp`) as a child process.
///
/// The child speaks ACP JSON-RPC over newline-delimited stdio — the same wire
/// format as `kigi agent`. Pager-side services still get the kigi
/// `AuthManager`/bootstrap so local features keep working; the external agent
/// manages its own credentials and model catalog.
pub async fn spawn_external_agent(
    command: &str,
    agent_config: AgentConfig,
    cancel: &CancellationToken,
) -> Result<SpawnedAgent> {
    let (auth_manager, _agent_config, _models_manager) =
        bootstrap_pager_side(&agent_config).await?;
    let agent_cancel = cancel.child_token();
    let bridge = super::external::spawn_external(command, agent_cancel.clone())?;
    Ok(SpawnedAgent {
        _thread_handle: bridge.thread_handle,
        channel: bridge.channel,
        cancel: agent_cancel,
        auth_manager,
    })
}

/// Spawn an agent in a dedicated thread with direct RPC dispatch.
///
/// The agent runs on a single-threaded tokio LocalSet runtime.
/// RPC requests go directly to the agent via Rc, bypassing simplex pipes.
fn spawn_agent_thread_direct(
    spawn_agent: Box<dyn FnOnce(AcpClientTx) -> Result<Rc<MvpAgent>> + Send + 'static>,
    channel: AcpAgentChannel,
    cancel: CancellationToken,
) -> Result<thread::JoinHandle<Result<()>>> {
    Ok(thread::Builder::new()
        .name("acp-agent-worker".into())
        .spawn(move || -> Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let client_tx = channel.tx.clone();
                let agent_rc = spawn_agent(client_tx)?;

                let gw_rx = AcpGatewayReceiver::new(channel.rx, agent_rc).with_tracing(true);
                tokio::task::spawn_local(gw_rx.run());
                tokio::task::yield_now().await;

                cancel.cancelled().await;
                anyhow::Result::Ok(())
            })
        })?)
}
