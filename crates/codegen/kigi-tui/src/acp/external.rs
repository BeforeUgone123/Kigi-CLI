//! External ACP agent transport.
//!
//! Spawns an arbitrary ACP agent command (e.g. `devin acp`, `kimi --acp`)
//! as a child process and bridges its newline-delimited JSON-RPC stdio into
//! the typed `AcpClientChannel` the pager expects, reusing the leader
//! bridge's `ClientSideConnection` codec.

use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use kigi_shell::leader::ReconnectPolicy;

use super::leader_bridge::{LeaderBridge, bridge_channels};

/// Spawn `command` as a child process and bridge its stdio into an
/// `AcpClientChannel`.
///
/// The command string is shell-split; argv[0] resolves via PATH. The child
/// must speak ACP JSON-RPC over newline-delimited JSON on stdout/stdin —
/// the same wire format `kigi agent` and `devin acp` use on stdio.
///
/// Cancelling the token kills the child; the child exiting (stdout EOF)
/// takes the bridge down through the usual disconnect path.
pub fn spawn_external(command: &str, cancel: CancellationToken) -> Result<LeaderBridge> {
    let argv = shlex::split(command)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("invalid external agent command: {command:?}"))?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to spawn external agent: {command:?}"))?;

    let mut child_stdin = child.stdin.take().expect("stdin is piped");
    let child_stdout = child.stdout.take().expect("stdout is piped");
    let child_stderr = child.stderr.take().expect("stderr is piped");

    // The bridge's "leader" endpoints: its outbound lines go to child stdin,
    // inbound lines come from child stdout.
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<String>();
    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<String>();

    // Child stderr → debug log. Agents commonly log there (e.g. devin prints
    // a startup banner); it must never reach the stdout JSON stream.
    tokio::spawn(async move {
        let mut lines = BufReader::new(child_stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!("[external-agent stderr] {line}");
        }
    });

    // I/O pump + child supervisor on the caller's runtime.
    let pump_cancel = cancel.clone();
    tokio::spawn(async move {
        let mut stdout_lines = BufReader::new(child_stdout).lines();
        loop {
            tokio::select! {
                biased;
                _ = pump_cancel.cancelled() => break,
                line = stdout_lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if inbound_tx.send(line).is_err() {
                            break;
                        }
                    }
                    _ => break,
                },
                line = outbound_rx.recv() => match line {
                    Some(line) => {
                        if child_stdin.write_all(line.as_bytes()).await.is_err()
                            || child_stdin.write_all(b"\n").await.is_err()
                            || child_stdin.flush().await.is_err()
                        {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
        let _ = child.kill().await;
    });

    bridge_channels(
        outbound_tx,
        inbound_rx,
        cancel,
        None,
        ReconnectPolicy::bounded(),
    )
}
