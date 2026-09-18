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
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        process.completion_output(/*session_id*/ 1000, Some(7)),
    )
    .await
    .expect("output excerpt does not wait for inherited pipes");
    assert_eq!(
        (
            output.session_id,
            output.exit_code,
            output.output_tail,
            output.truncated
        ),
        (1000, Some(7), String::new(), false)
    );
}

#[tokio::test]
async fn completion_wait_releases_on_cleanup_without_exit_event() {
    let process = std::sync::Arc::new(
        crate::unified_exec::process_tests::remote_process(
            WriteStatus::Accepted,
            /*terminate_error*/ None,
            SandboxType::None,
        )
        .await,
    );
    let weak = std::sync::Arc::downgrade(&process);
    let completion = process.wait_for_completion();
    process.terminate();
    assert_eq!(process.completion(), None);
    drop(process);
    assert!(
        weak.upgrade().is_none(),
        "waiter must not retain process buffers"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), completion)
        .await
        .expect("cleanup releases the waiter without an exit event");
}
