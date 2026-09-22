use super::windows_common as runner;
use crate::ipc_framed as ipc;
use crate::runner_client as client;
use codex_utils_pty as pty;
use tokio::sync;
fn direct_output_channel(
    mut file: std::fs::File,
) -> (
    sync::mpsc::Sender<Vec<u8>>,
    sync::mpsc::Receiver<Vec<u8>>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = sync::mpsc::channel(256);
    let reader_tx = tx.clone();
    let reader_handle = tokio::task::spawn_blocking(move || {
        let mut buffer = [0; 8192];
        while let Ok(count) = std::io::Read::read(&mut file, &mut buffer) {
            if count == 0 || reader_tx.blocking_send(buffer[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    (tx, rx, reader_handle)
}
pub async fn spawn_current_user_runner_session(
    codex_home: &std::path::Path,
    command: Vec<String>,
    cwd: &std::path::Path,
    env: std::collections::HashMap<String, String>,
    stdin_open: bool,
) -> anyhow::Result<pty::SpawnedProcess> {
    let codex_home = codex_home.to_path_buf();
    let cwd = cwd.to_path_buf();
    let request = ipc::SpawnRequest {
        command,
        cwd: cwd.clone(),
        env,
        execution_mode: ipc::RunnerExecutionMode::CurrentUser,
        child_console_mode: ipc::ChildConsoleMode::NoWindow,
        permission_profile: codex_protocol::models::PermissionProfile::Disabled,
        workspace_roots: Vec::new(),
        codex_home: codex_home.clone(),
        real_codex_home: codex_home.clone(),
        cap_sids: Vec::new(),
        network_proxy_restricting_sid: None,
        timeout_ms: None,
        tty: false,
        stdin_open,
        private_desktop_name: None,
    };
    let transport = tokio::task::spawn_blocking(move || {
        client::spawn_runner_transport(
            &codex_home,
            &cwd,
            client::RunnerLaunch::CurrentUser,
            /*log_dir*/ None,
            request,
            /*desktop_policy*/ None,
        )
    })
    .await
    .map_err(|err| anyhow::anyhow!("runner handshake task failed: {err}"))??;
    let (pipe_write, pipe_read, stdout_file, stderr_file) = transport.into_files_with_output()?;
    let (writer_tx, writer_rx) = sync::mpsc::channel::<Vec<u8>>(128);
    let (exit_tx, exit_rx) = sync::oneshot::channel::<i32>();
    let (stdout_tx, stdout_rx, stdout_reader_handle) = direct_output_channel(stdout_file);
    let (stderr_tx, stderr_rx, stderr_reader_handle) = direct_output_channel(stderr_file);
    drop(stdout_tx);
    let outbound_tx = runner::start_runner_pipe_writer(pipe_write);
    let writer_handle =
        runner::start_runner_stdin_writer(writer_rx, outbound_tx.clone(), false, stdin_open);
    runner::start_runner_stdout_reader(
        pipe_read,
        sync::broadcast::channel(1).0,
        None,
        Some(stderr_tx),
        exit_tx,
    );
    let spawned = pty::spawn_from_direct_driver(pty::DirectProcessDriver {
        writer_tx,
        stdout_rx,
        stderr_rx,
        exit_rx,
        stdout_reader_handle,
        stderr_reader_handle,
        writer_handle: Some(writer_handle),
        terminator: Some(Box::new(move || {
            let _ = outbound_tx.send(ipc::FramedMessage {
                version: ipc::IPC_PROTOCOL_VERSION,
                message: ipc::Message::Terminate {
                    payload: ipc::EmptyPayload::default(),
                },
            });
        })),
    });
    if !stdin_open {
        spawned.session.close_stdin();
    }
    Ok(spawned)
}
