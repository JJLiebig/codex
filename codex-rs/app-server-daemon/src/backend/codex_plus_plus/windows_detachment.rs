//! Escape the entire caller job chain before starting a long-lived daemon.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

const JOB_ERROR: &str =
    "host Job Object prevents daemon detachment; start from a host that allows breakaway";

#[derive(Clone, Copy, Debug)]
pub(crate) enum LaunchKind {
    Detached,
    Brokered,
}

pub(super) fn preflight(executable: &Path) -> Result<LaunchKind> {
    let launch = match suspended_command(executable, CREATE_BREAKAWAY_FROM_JOB).spawn() {
        Ok(mut child) => {
            let associated = in_job(child.as_raw_handle() as _);
            child
                .kill()
                .context("failed to terminate suspended launch probe")?;
            child
                .wait()
                .context("failed to reap suspended launch probe")?;
            if associated? {
                LaunchKind::Brokered
            } else {
                LaunchKind::Detached
            }
        }
        Err(error) => {
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                && in_job(unsafe { GetCurrentProcess() })?
                && let Ok(mut probe) = suspended_command(executable, /*flags*/ 0).spawn()
            {
                probe
                    .kill()
                    .context("failed to terminate suspended launch probe")?;
                probe
                    .wait()
                    .context("failed to reap suspended launch probe")?;
                LaunchKind::Brokered
            } else {
                return Err(error)
                    .context("cannot launch detached daemon; existing daemon was not stopped");
            }
        }
    };
    if matches!(launch, LaunchKind::Brokered) {
        super::wmi_broker::preflight(executable).context(JOB_ERROR)?;
    }
    Ok(launch)
}

pub(super) fn verify_spawned(process: isize, launch: LaunchKind) -> Result<()> {
    if matches!(launch, LaunchKind::Detached) && in_job(process)? {
        anyhow::bail!(JOB_ERROR);
    }
    Ok(())
}

pub(super) fn launch(
    command: &mut tokio::process::Command,
    kind: LaunchKind,
    stderr_log: &Path,
) -> Result<u32> {
    match kind {
        LaunchKind::Detached => command
            .spawn()?
            .id()
            .context("spawned app-server process has no pid"),
        LaunchKind::Brokered => {
            command.env(super::wmi_broker::STDERR_LOG_ENV, stderr_log);
            super::wmi_broker::launch(command.as_std())
        }
    }
}

fn in_job(process: isize) -> Result<bool> {
    let mut associated = 0;
    if unsafe {
        IsProcessInJob(process, /*jobhandle*/ 0, &mut associated)
    } == 0
    {
        return Err(io::Error::last_os_error())
            .context("failed to verify daemon launch capability");
    }
    Ok(associated != 0)
}

fn suspended_command(executable: &Path, flags: u32) -> Command {
    let mut command = Command::new(executable);
    command
        .creation_flags(CREATE_SUSPENDED | DETACHED_PROCESS | flags)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
