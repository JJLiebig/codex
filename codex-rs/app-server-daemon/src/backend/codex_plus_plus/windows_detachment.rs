//! A Windows daemon may remain in a harmless outer job after breaking away
//! from the terminal's kill-on-close job.

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
    DurableJob,
}

pub(super) fn preflight(executable: &Path) -> Result<LaunchKind> {
    let mut child = match suspended_command(executable, CREATE_BREAKAWAY_FROM_JOB).spawn() {
        Ok(child) => child,
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
                anyhow::bail!(JOB_ERROR);
            }
            return Err(error)
                .context("cannot launch detached daemon; existing daemon was not stopped");
        }
    };
    let associated = in_job(child.as_raw_handle() as _);
    child
        .kill()
        .context("failed to terminate suspended launch probe")?;
    child
        .wait()
        .context("failed to reap suspended launch probe")?;
    if associated? {
        ensure_residual_job_is_durable()?;
        Ok(LaunchKind::DurableJob)
    } else {
        Ok(LaunchKind::Detached)
    }
}

pub(super) fn verify_spawned(process: isize, launch: LaunchKind) -> Result<()> {
    if in_job(process)? && !matches!(launch, LaunchKind::DurableJob) {
        anyhow::bail!(JOB_ERROR);
    }
    Ok(())
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

// QueryInformationJobObject(NULL) reports the calling process's job, so run
// this probe with the same breakaway flag as the eventual daemon process.
fn ensure_residual_job_is_durable() -> Result<()> {
    // The supported Windows targets are 64-bit: this structure is 144 bytes,
    // with LimitFlags at byte 16 and KILL_ON_JOB_CLOSE at bit 0x2000.
    const SCRIPT: &str = r#"
$source = 'using System; using System.Runtime.InteropServices; public static class CodexJobProbe { [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr GetCurrentProcess(); [DllImport("kernel32.dll", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr process, IntPtr job, out bool result); [DllImport("kernel32.dll", SetLastError=true)] public static extern bool QueryInformationJobObject(IntPtr job, int kind, byte[] data, int size, out int returned); }'
Add-Type -TypeDefinition $source -ErrorAction Stop
if ([IntPtr]::Size -ne 8) { exit 2 }
$associated = $false
if (![CodexJobProbe]::IsProcessInJob([CodexJobProbe]::GetCurrentProcess(), [IntPtr]::Zero, [ref]$associated)) { exit 2 }
if (!$associated) { exit 0 }
$data = New-Object byte[] 144
$returned = 0
if (![CodexJobProbe]::QueryInformationJobObject([IntPtr]::Zero, 9, $data, $data.Length, [ref]$returned)) { exit 2 }
if (([BitConverter]::ToUInt32($data, 16) -band 0x2000) -ne 0) { exit 42 }
"#;
    let system_root = std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?;
    let powershell = Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = Command::new(powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB)
        .output()
        .context("failed to check residual daemon job")?;
    match output.status.code() {
        Some(0) => Ok(()),
        Some(42) => anyhow::bail!(JOB_ERROR),
        _ => anyhow::bail!(
            "failed to check residual daemon job (exit code {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}
