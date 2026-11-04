//! Attach-only recovery registration. A single worker serializes updates and explicit cleanup.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Context;
use clap::ValueEnum;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::sync::watch;

use crate::cli::Cli;
use crate::session_state::ThreadSessionState;

const DEADLINE: Duration = Duration::from_millis(500);
static WORKER: OnceLock<Worker> = OnceLock::new();

struct Worker {
    tx: watch::Sender<Update>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone, Default, PartialEq)]
enum Update {
    #[default]
    Pending,
    Session {
        id: String,
        cwd: String,
    },
    Quit,
}

pub(crate) fn start(cli: &Cli, remote: Option<&crate::RemoteAppServerEndpoint>) {
    // Remote thread IDs and cwd paths cannot be resumed by this local recipe.
    if remote.is_some() {
        return;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return;
    }
    let environment = ["HC_TERMINAL_SOCKET", "HC_TERMINAL_ID", "HC_SCOPE"].map(std::env::var);
    let [Ok(socket), Ok(terminal), Ok(scope)] = environment else {
        return;
    };
    if socket.is_empty() || terminal.is_empty() || scope.is_empty() {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    WORKER.get_or_init(|| {
        let argv = launch_arguments(cli, &executable);
        let (tx, rx) = watch::channel(Update::Pending);
        let task = tokio::spawn(run(socket, terminal, scope, argv, rx));
        Worker {
            tx,
            task: Mutex::new(Some(task)),
        }
    });
}

pub(crate) fn set_primary(session: Option<&ThreadSessionState>) {
    if let Some(worker) = WORKER.get() {
        let next = session
            .filter(|session| session.rollout_path.is_some())
            .map_or(Update::Pending, |session| Update::Session {
                id: session.thread_id.to_string(),
                cwd: session.cwd.to_string_lossy().into_owned(),
            });
        worker.tx.send_if_modified(|current| {
            if *current == Update::Quit || *current == next {
                return false;
            }
            *current = next;
            true
        });
    }
}

// Event dispatch returns explicit user exits, including logout/archive/delete and offline quit.
// EOF and signals exit through the terminal event path; fatal app events retain recovery too.
pub(crate) async fn after_app_event(result: &color_eyre::Result<crate::app::AppRunControl>) {
    use crate::app::AppRunControl;
    use crate::app::ExitReason;
    if matches!(
        result,
        Ok(AppRunControl::Exit(
            ExitReason::UserRequested
                | ExitReason::Archived(_)
                | ExitReason::TurnInterrupted
                | ExitReason::ThreadRemoved
        ))
    ) {
        deliberate_exit().await;
    }
}

async fn deliberate_exit() {
    if let Some(worker) = WORKER.get() {
        worker.tx.send_replace(Update::Quit);
        let task = worker
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(DEADLINE * 6, task).await;
        }
    }
}

fn launch_arguments(cli: &Cli, executable: &Path) -> Vec<String> {
    let mut argv = vec![executable.to_string_lossy().into_owned()];
    for (flag, enabled) in [
        ("--no-daemon", cli.no_daemon),
        ("--strict-config", cli.strict_config),
        ("--oss", cli.oss),
        ("--approve-for-me", cli.auto_review),
        (
            "--dangerously-bypass-approvals-and-sandbox",
            cli.dangerously_bypass_approvals_and_sandbox,
        ),
        ("--dangerously-bypass-hook-trust", cli.bypass_hook_trust),
        ("--search", cli.web_search),
        ("--auto-account", cli.auto_account),
        ("--no-alt-screen", cli.no_alt_screen),
    ] {
        if enabled {
            argv.push(flag.into());
        }
    }
    for (flag, value) in [
        ("--model", cli.model.clone()),
        ("--local-provider", cli.oss_provider.clone()),
        (
            "--profile",
            cli.config_profile_v2.as_ref().map(ToString::to_string),
        ),
        (
            "--sandbox",
            cli.sandbox_mode
                .and_then(|mode| mode.to_possible_value())
                .map(|value| value.get_name().to_owned()),
        ),
        (
            "--ask-for-approval",
            cli.approval_policy
                .and_then(|mode| mode.to_possible_value())
                .map(|value| value.get_name().to_owned()),
        ),
    ] {
        if let Some(value) = value {
            argv.extend([flag.into(), value]);
        }
    }
    for value in &cli.config_overrides.raw_overrides {
        argv.extend(["--config".into(), value.clone()]);
    }
    let launch_cwd = std::env::current_dir().unwrap_or_default();
    let workspace = cli
        .cwd
        .as_ref()
        .map_or_else(|| launch_cwd.clone(), |cwd| launch_cwd.join(cwd));
    for directory in &cli.add_dir {
        argv.extend([
            "--add-dir".into(),
            workspace.join(directory).to_string_lossy().into_owned(),
        ]);
    }
    // Prompt, images, picker/fork switches, --worktree and the original --cd are launch-only.
    argv
}

async fn run(
    socket: String,
    terminal: String,
    scope: String,
    argv: Vec<String>,
    mut rx: watch::Receiver<Update>,
) {
    let mut owned = false;
    while rx.changed().await.is_ok() {
        let update = rx.borrow_and_update().clone();
        let quit = update == Update::Quit;
        if matches!(update, Update::Quit | Update::Pending) {
            if owned {
                let result = clear_registration(&socket, &terminal, &scope).await;
                if result.is_ok() {
                    owned = false;
                } else {
                    tracing::debug!(?result, "Herdr recovery cleanup unavailable");
                }
            }
            if quit {
                break;
            }
            continue;
        }
        let result = tokio::time::timeout(DEADLINE, async {
            match update {
                Update::Session { id, cwd } => {
                    let record =
                        rpc(&socket, "terminal.get", json!({"terminal": terminal})).await?;
                    if record["keep"] != true || record["state"] != "live" {
                        return Ok(());
                    }
                    // Stock core ignores unknown fields: discover before sending a private request.
                    let schema = rpc(&socket, "rpc.discover", json!({})).await?;
                    let supported = schema["methods"].as_array().is_some_and(|methods| {
                        methods.iter().any(|method| {
                            method["name"] == "terminal.keep"
                                && ["recovery", "expected_scope"].iter().all(|name| {
                                    method["params"].as_array().is_some_and(|params| {
                                        params.iter().any(|param| param["name"] == *name)
                                    })
                                })
                        })
                    });
                    if !supported {
                        return Ok(());
                    }
                    let mut command = argv.clone();
                    command.extend(["resume".into(), id, "--cd".into(), cwd.clone()]);
                    // Managed Codex launchers register once before launching us. Taking over with
                    // the actual main ID/cwd preserves recovery across /new and cwd changes.
                    owned = true;
                    rpc(
                        &socket,
                        "terminal.keep",
                        json!({"terminal": terminal, "expected_scope": scope,
                        "keep": true, "recovery": {"argv": command, "cwd": cwd}}),
                    )
                    .await?;
                    crate::terminal_program_status::refresh_recovery_binding();
                }
                Update::Quit | Update::Pending => {}
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::debug!(?result, "Herdr recovery registration unavailable");
        }
        if quit {
            break;
        }
    }
}

// Clearing and restoration are separate API calls. Always attempt restoration, even when
// the clear acknowledgement times out after its write, using fresh bounded request budgets.
async fn clear_registration(socket: &str, terminal: &str, scope: &str) -> anyhow::Result<()> {
    let record = bounded_rpc(socket, "terminal.get", json!({"terminal": terminal})).await?;
    if record["keep"] != true {
        return Ok(());
    }
    let cleared = bounded_rpc(
        socket,
        "terminal.keep",
        json!({"terminal": terminal, "expected_scope": scope, "keep": false}),
    )
    .await;
    let params = json!({"terminal": terminal, "expected_scope": scope, "keep": true});
    let mut restored = bounded_rpc(socket, "terminal.keep", params.clone()).await;
    if restored.is_err() {
        restored = bounded_rpc(socket, "terminal.keep", params).await;
    }
    cleared?;
    restored?;
    Ok(())
}

async fn bounded_rpc(socket: &str, method: &str, params: Value) -> anyhow::Result<Value> {
    tokio::time::timeout(DEADLINE, rpc(socket, method, params))
        .await
        .context("Herdr request timed out")?
}

async fn rpc(socket: &str, method: &str, params: Value) -> anyhow::Result<Value> {
    #[cfg(windows)]
    let stream = tokio::net::windows::named_pipe::ClientOptions::new().open(socket)?;
    #[cfg(unix)]
    let stream = tokio::net::UnixStream::connect(socket).await?;
    exchange(stream, method, params).await
}

async fn exchange(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    let mut stream = BufReader::new(stream);
    let mut request = serde_json::to_vec(
        &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
    )?;
    request.push(b'\n');
    stream.write_all(&request).await?;
    stream.flush().await?;
    let mut line = String::new();
    stream.read_line(&mut line).await?;
    let mut reply: Value = serde_json::from_str(&line)?;
    anyhow::ensure!(
        reply["id"] == 1 && reply.get("error").is_none(),
        "Herdr rejected request"
    );
    reply
        .as_object_mut()
        .and_then(|reply| reply.remove("result"))
        .context("missing Herdr reply")
}

#[cfg(test)]
#[path = "herdr_recovery_tests.rs"]
mod tests;
