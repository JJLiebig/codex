//! The result-metric hook must preserve terminal exit codes and pipe-failure behavior.

use super::*;
use crate::ipc_framed::ExitPayload;
use crate::ipc_framed::write_frame;
use pretty_assertions::assert_eq;
use std::io::Seek;

#[test]
fn runner_result_reporting_preserves_exit_and_closed_pipe_results() -> Result<()> {
    for exit_code in [Some(0), Some(23), None] {
        let mut frames = tempfile::tempfile()?;
        if let Some(exit_code) = exit_code {
            write_frame(
                &mut frames,
                &FramedMessage {
                    version: IPC_PROTOCOL_VERSION,
                    message: Message::Exit {
                        payload: ExitPayload {
                            exit_code,
                            timed_out: false,
                        },
                    },
                },
            )?;
        }
        frames.rewind()?;
        let (stdout_tx, _stdout_rx) = broadcast::channel(1);
        let (exit_tx, exit_rx) = oneshot::channel();
        start_runner_stdout_reader(
            frames, stdout_tx, /*stderr_tx*/ None, /*direct_stderr_tx*/ None, exit_tx,
        );
        assert_eq!(exit_rx.blocking_recv()?, exit_code.unwrap_or(-1));
    }
    Ok(())
}

use crate::ipc_framed as ipc;

fn assert_control_failure(mut pipe: std::fs::File, expected_prefix: &str) {
    std::io::Seek::rewind(&mut pipe).unwrap();
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(1);
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
    start_runner_stdout_reader(
        pipe,
        tokio::sync::broadcast::channel(1).0,
        None,
        Some(output_tx),
        exit_tx,
    );
    let diagnostic = output_rx.blocking_recv().unwrap();
    assert!(String::from_utf8_lossy(&diagnostic).starts_with(expected_prefix));
    assert_eq!(exit_rx.blocking_recv().unwrap(), -1);
}

#[test]
fn runner_control_failures_are_emitted_on_stderr_before_exit() {
    let mut runner_error = tempfile::tempfile().unwrap();
    ipc::write_frame(
        &mut runner_error,
        &ipc::FramedMessage {
            version: ipc::IPC_PROTOCOL_VERSION,
            message: ipc::Message::Error {
                payload: ipc::ErrorPayload {
                    message: "child failed".to_string(),
                    stage: ipc::ErrorStage::SpawnChild,
                    windows_error_code: None,
                },
            },
        },
    )
    .unwrap();
    assert_control_failure(runner_error, "runner error: child failed\n");
    assert_control_failure(
        tempfile::tempfile().unwrap(),
        "runner error: runner pipe closed before exit\n",
    );
    let mut malformed = tempfile::tempfile().unwrap();
    std::io::Write::write_all(&mut malformed, &1_u32.to_le_bytes()).unwrap();
    std::io::Write::write_all(&mut malformed, b"{").unwrap();
    assert_control_failure(malformed, "runner error: runner read failed:");
}
