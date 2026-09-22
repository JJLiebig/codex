use anyhow::Result;
use codex_windows_sandbox::LaunchDesktop;
use codex_windows_sandbox::ProcessExecutionMode;
use codex_windows_sandbox::SpawnRequest;
use codex_windows_sandbox::StderrMode;
use codex_windows_sandbox::StdinMode;
use codex_windows_sandbox::spawn_process_with_pipes;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Threading::ResumeThread;

use super::super::IpcSpawnedProcess;

pub(in super::super) fn spawn_current_user_process(
    req: &SpawnRequest,
) -> Result<IpcSpawnedProcess> {
    if req.tty {
        anyhow::bail!("runner: current-user execution is non-TTY only");
    }
    let stdin_mode = match req.stdin_open {
        true => StdinMode::Open,
        false => StdinMode::Closed,
    };
    let desktop = LaunchDesktop::current_user();
    let pipes = spawn_process_with_pipes(
        ProcessExecutionMode::CurrentUser,
        &req.command,
        &req.cwd,
        &req.env,
        stdin_mode,
        StderrMode::InheritOutput,
        req.child_console_mode,
        desktop,
        Some(req.codex_home.as_path()),
    )?;
    if unsafe { ResumeThread(pipes.process.hThread) } == u32::MAX {
        let _ = pipes.job().terminate();
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(IpcSpawnedProcess {
        log_dir: req.codex_home.clone(),
        pi: pipes.process,
        job: pipes.job(),
        preserve_descendants_on_exit: false,
        stdout_handle: INVALID_HANDLE_VALUE,
        stderr_handle: INVALID_HANDLE_VALUE,
        stdin_handle: pipes.stdin_write,
        conpty_owner: None,
        hpc_handle: None,
        _pipe_handles: Some(pipes),
    })
}
