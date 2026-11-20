#![cfg(target_os = "windows")]

use anyhow::Result;
use codex_sandboxing::SandboxType;
use codex_sandboxing::SpawnRequest;
use codex_sandboxing::spawn_process;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::fs;
use std::io::BufWriter;
use std::io::Write;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::timeout;

const BURST_LINES: usize = 32 * 1024;
const BURST_PAYLOAD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PROBE_ENV: &str = "CODEX_NON_TTY_RUNNER_PROBE";

struct StagedRunner(Option<std::path::PathBuf>);

impl Drop for StagedRunner {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[link(name = "Kernel32")]
unsafe extern "system" {
    #[link_name = "GetConsoleProcessList"]
    fn get_console_process_list(process_list: *mut u32, process_count: u32) -> u32;
}

async fn collect_output(mut receiver: mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let mut output = Vec::new();
    while let Some(chunk) = receiver.recv().await {
        output.extend(chunk);
    }
    output
}

fn assert_complete_burst(output: &str, stream: &str) {
    let prefix = format!("native-{stream}-");
    let lines = output
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), BURST_LINES);
    for (index, line) in lines.into_iter().enumerate() {
        assert_eq!(line, format!("{prefix}{index:05}-{BURST_PAYLOAD}"));
    }
}

fn stage_runner_for_production_lookup() -> Result<Option<StagedRunner>> {
    let runner = std::env::var_os("CARGO_BIN_EXE_codex-command-runner")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_codex_command_runner"));
    let Some(runner) = runner else {
        return Ok(None);
    };
    let runner = std::path::PathBuf::from(runner);
    let destination = std::env::current_exe()?.with_file_name("codex-command-runner.exe");
    if runner == destination || destination.exists() {
        return Ok(Some(StagedRunner(None)));
    }
    fs::copy(runner, &destination)?;
    Ok(Some(StagedRunner(Some(destination))))
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::zombie_processes)]
async fn unrestricted_non_tty_uses_private_runner() -> Result<()> {
    if std::env::var_os(PROBE_ENV).is_some() {
        let mut console_pids = [0; 16];
        let count = unsafe {
            get_console_process_list(console_pids.as_mut_ptr(), console_pids.len() as u32)
        } as usize;
        let parent_pid: u32 = std::env::var("CODEX_PARENT_PID")?.parse()?;
        assert!(count <= console_pids.len() && !console_pids[..count].contains(&parent_pid));

        let mut stdout = BufWriter::new(std::io::stdout().lock());
        let mut stderr = BufWriter::new(std::io::stderr().lock());
        for index in 0..BURST_LINES {
            writeln!(stdout, "native-stdout-{index:05}-{BURST_PAYLOAD}")?;
            writeln!(stderr, "native-stderr-{index:05}-{BURST_PAYLOAD}")?;
        }
        stdout.flush()?;
        stderr.flush()?;
        std::process::Command::new(std::env::var_os("ComSpec").unwrap())
            .args([
                "/d",
                "/c",
                r#""%SystemRoot%\System32\ping.exe" -n 3 127.0.0.1 >nul & echo survived > "%CODEX_DESCENDANT_MARKER%""#,
            ])
            .spawn()?;
        return Ok(());
    }

    let Some(_staged_runner) = stage_runner_for_production_lookup()? else {
        eprintln!(
            "skipping: Cargo cannot provide the cross-package codex-command-runner; \
             Bazel supplies it through extra_binaries"
        );
        return Ok(());
    };
    let temp = TempDir::new()?;
    let probe = temp.path().join("native-probe.exe");
    fs::copy(std::env::current_exe()?, &probe)?;
    let marker = temp.path().join("descendant-survived");
    let script = temp.path().join("runner-probe.ps1");
    fs::write(
        &script,
        "[Console]::Out.WriteLine('powershell-stdout')\r\n[Console]::Error.WriteLine('powershell-stderr')\r\n$command = 'echo cmd-stdout & echo cmd-stderr 1>&2 & \"%CODEX_NATIVE_PROBE%\" unrestricted_non_tty_uses_private_runner --nocapture'\r\n& $env:ComSpec /d /s /c $command\r\nexit $LASTEXITCODE\r\n",
    )?;
    let mut env: HashMap<_, _> = std::env::vars().collect();
    env.insert(PROBE_ENV.into(), "1".into());
    env.insert("CODEX_NATIVE_PROBE".into(), probe.display().to_string());
    env.insert(
        "CODEX_DESCENDANT_MARKER".into(),
        marker.display().to_string(),
    );
    env.insert("CODEX_PARENT_PID".into(), std::process::id().to_string());
    let args = [
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script.display().to_string(),
    ];
    let arg0 = None;
    let started_at = Instant::now();
    let spawned = spawn_process(SpawnRequest {
        program: std::ffi::OsStr::new("powershell.exe"),
        args: &args,
        cwd: temp.path(),
        env: &env,
        arg0: &arg0,
        sandbox: SandboxType::None,
        windows_sandbox: None,
        tty: false,
        stdin_open: false,
        inherited_fds: codex_utils_pty::ChildFds::Inherited(&[]),
    })
    .await?;
    let codex_utils_pty::SpawnedProcess {
        session: _session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let stdout_task = tokio::spawn(collect_output(stdout_rx));
    let stderr_task = tokio::spawn(collect_output(stderr_rx));
    let exit_code = timeout(Duration::from_secs(10), exit_rx).await??;
    let stdout = timeout(Duration::from_secs(10), stdout_task).await??;
    let stderr = timeout(Duration::from_secs(10), stderr_task).await??;
    assert_eq!(exit_code, 0);
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "root process did not exit promptly"
    );

    let stdout = String::from_utf8(stdout)?;
    let stderr = String::from_utf8(stderr)?;
    assert!(stdout.contains("powershell-stdout"), "stdout={stdout:?}");
    assert!(stderr.contains("powershell-stderr"), "stderr={stderr:?}");
    assert!(stdout.contains("cmd-stdout"), "stdout={stdout:?}");
    assert!(stderr.contains("cmd-stderr"), "stderr={stderr:?}");
    assert!(!stdout.contains("native-stderr-"), "stdout={stdout:?}");
    assert!(!stderr.contains("native-stdout-"), "stderr={stderr:?}");
    assert_complete_burst(&stdout, "stdout");
    assert_complete_burst(&stderr, "stderr");

    std::thread::sleep(Duration::from_secs(3));
    assert!(!marker.exists(), "runner descendant escaped its job");
    Ok(())
}
