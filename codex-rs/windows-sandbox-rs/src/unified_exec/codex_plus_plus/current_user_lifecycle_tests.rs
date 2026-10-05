use super::*;

#[test]
fn current_user_runner_cancellation_terminates_descendants() {
    let Some(pwsh) = pwsh_path() else {
        eprintln!("skipping current-user cancellation test: PowerShell 7 is not installed");
        return;
    };
    current_thread_runtime().block_on(async move {
        let codex_home = sandbox_home("current-user-cancellation");
        let ready_marker = codex_home.path().join("descendant-started");
        let child_command = format!(
            "Set-Content -LiteralPath '{}' -Value $PID; Start-Sleep -Seconds 30",
            powershell_literal(&ready_marker),
        );
        let parent_command = start_powershell_child(
            &pwsh,
            codex_home.path(),
            &child_command,
            "Start-Sleep -Seconds 30",
        );
        let spawned = spawn_windows_current_user_runner_session(
            codex_home.path(),
            vec![
                pwsh.display().to_string(),
                "-NoProfile".into(),
                "-Command".into(),
                parent_command,
            ],
            &sandbox_cwd(),
            std::env::vars().collect(),
            /*stdin_open*/ false,
        )
        .await
        .expect("spawn current-user cancellation test");
        assert!(
            wait_for_path(&ready_marker, Duration::from_secs(10)),
            "descendant did not start"
        );
        let descendant_pid = fs::read_to_string(&ready_marker)
            .expect("read descendant pid")
            .trim()
            .parse()
            .expect("parse descendant pid");
        let descendant = open_process_for_wait(descendant_pid).expect("open descendant process");
        spawned.session.request_terminate();
        let (_, exit_code) =
            collect_stdout_and_exit(spawned, codex_home.path(), Duration::from_secs(10)).await;
        assert_ne!(exit_code, 0);
        wait_for_process_exit(&descendant, Duration::from_secs(10))
            .expect("descendant survived cancellation");
    });
}
