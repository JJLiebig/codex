use codex_exec_server::WriteStatus;
use codex_sandboxing::SandboxType;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn completion_does_not_wait_for_descendant_output_streams() {
    let process = crate::unified_exec::process_tests::remote_process(
        WriteStatus::Accepted,
        /*terminate_error*/ None,
        SandboxType::None,
    )
    .await;
    assert_eq!(process.completion(), None);
    let state = process.state_rx.borrow().clone();
    process.state_tx.send_replace(state.exited(Some(7)));
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        process.wait_for_completion(),
    )
    .await
    .expect("exit wakes without stream closure");
    assert_eq!(process.completion(), Some(Some(7)));
    assert!(!process.cancellation_token().is_cancelled());
}
