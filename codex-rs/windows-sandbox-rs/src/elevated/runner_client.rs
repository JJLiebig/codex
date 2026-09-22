use crate::app_package::registered_core_requested;
use crate::app_package::verify_registered_core_runner;
use crate::codex_plus_plus::current_user_runner;
use crate::desktop::DesktopPolicy;
use crate::desktop::shared_private_desktop_for_user;
use crate::identity::SandboxCreds;
use crate::ipc_framed::ErrorPayload;
use crate::ipc_framed::ErrorStage;
use crate::ipc_framed::FramedMessage;
use crate::ipc_framed::IPC_PROTOCOL_VERSION;
use crate::ipc_framed::Message;
use crate::ipc_framed::SpawnRequest;
use crate::ipc_framed::read_frame;
use crate::ipc_framed::write_frame;
use crate::proc_thread_attr::ProcThreadAttributeList;
use crate::runner_pipe::PIPE_ACCESS_INBOUND;
use crate::runner_pipe::PIPE_ACCESS_OUTBOUND;
use crate::runner_pipe::connect_pipe;
use crate::runner_pipe::create_named_pipe;
use crate::runner_pipe::find_runner_exe;
use crate::runner_pipe::pipe_pair;
use crate::winutil::quote_windows_arg;
use crate::winutil::to_wide;
use anyhow::Context;
use anyhow::Result;
use std::fs::File;
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::FromRawHandle;
use std::os::windows::io::OwnedHandle;
use std::path::Path;
use std::ptr;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS;
use windows_sys::Win32::Foundation::DuplicateHandle;
use windows_sys::Win32::Foundation::ERROR_ACCOUNT_DISABLED;
use windows_sys::Win32::Foundation::ERROR_LOGON_FAILURE;
use windows_sys::Win32::Foundation::ERROR_NO_SUCH_LOGON_SESSION;
use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Foundation::SetHandleInformation;
use windows_sys::Win32::Security::Authentication::Identity::GetUserNameExW;
use windows_sys::Win32::Security::Authentication::Identity::NameSamCompatible;
use windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode;
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::CreateProcessWithLogonW;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::GetCurrentThread;
use windows_sys::Win32::System::Threading::LOGON_WITH_PROFILE;
use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
use windows_sys::Win32::System::Threading::STARTF_FORCEOFFFEEDBACK;
use windows_sys::Win32::System::Threading::STARTF_USESTDHANDLES;
use windows_sys::Win32::System::Threading::STARTUPINFOEXW;
use windows_sys::Win32::System::Threading::STARTUPINFOW;
use windows_sys::Win32::System::Threading::TerminateProcess;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

const RUNNER_SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(15);
const RUNNER_PIPE_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RUNNER_ERROR_MODE_FLAGS: u32 = 0x0001 | 0x0002;
const WAIT_OBJECT_0: u32 = 0;

#[derive(Clone, Copy)]
pub(crate) enum RunnerLaunch<'a> {
    CurrentUser,
    Logon(&'a SandboxCreds),
}
#[derive(Debug)]
struct RunnerLogonError {
    code: u32,
}

impl std::fmt::Display for RunnerLogonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CreateProcessWithLogonW failed: {}", self.code)
    }
}

impl std::error::Error for RunnerLogonError {}

#[derive(Debug)]
pub(crate) struct RunnerStartupError {
    payload: ErrorPayload,
}

impl RunnerStartupError {
    pub(crate) fn new(payload: ErrorPayload) -> Self {
        Self { payload }
    }
}

impl std::fmt::Display for RunnerStartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "runner failed during {:?}: {}",
            self.payload.stage, self.payload.message
        )?;
        if let Some(code) = self.payload.windows_error_code {
            write!(f, " (Windows error {code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for RunnerStartupError {}

pub(crate) struct RunnerTransport {
    pipe_write: File,
    pipe_read: File,
    direct_output: Option<(File, File)>,
}

fn is_refreshable_windows_error(code: u32) -> bool {
    matches!(
        code,
        ERROR_ACCOUNT_DISABLED | ERROR_LOGON_FAILURE | ERROR_NO_SUCH_LOGON_SESSION
    )
}

fn command_targets_windows_apps(command: &[String]) -> bool {
    command.first().is_some_and(|program| {
        Path::new(program).components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("WindowsApps")
        })
    })
}

pub(crate) fn is_refreshable_sandbox_creds_error(err: &anyhow::Error, command: &[String]) -> bool {
    if err
        .downcast_ref::<RunnerLogonError>()
        .is_some_and(|err| is_refreshable_windows_error(err.code))
    {
        return true;
    }

    err.downcast_ref::<RunnerStartupError>().is_some_and(|err| {
        err.payload.stage == ErrorStage::SpawnChild
            && err.payload.windows_error_code.is_some_and(|code| {
                // AppX activation can return 1312 for a healthy sandbox token. Rotating the
                // account password cannot make the same WindowsApps command launch.
                is_refreshable_windows_error(code)
                    && (code != ERROR_NO_SUCH_LOGON_SESSION
                        || !command_targets_windows_apps(command))
            })
    })
}

pub(crate) fn retry_runner_spawn_once<T>(
    sandbox_creds: SandboxCreds,
    command: &[String],
    mut spawn: impl FnMut(SandboxCreds) -> Result<T>,
    refresh: impl FnOnce() -> Result<SandboxCreds>,
) -> Result<T> {
    let result = match spawn(sandbox_creds) {
        Ok(result) => Ok(result),
        Err(err) if is_refreshable_sandbox_creds_error(&err, command) => refresh().and_then(spawn),
        Err(err) => Err(err),
    };
    super::runner_metrics::record("startup", if result.is_ok() { "success" } else { "error" });
    result
}

impl RunnerTransport {
    pub(crate) fn send_spawn_request(&mut self, request: SpawnRequest) -> Result<()> {
        let spawn_request = FramedMessage {
            version: IPC_PROTOCOL_VERSION,
            message: Message::SpawnRequest {
                payload: Box::new(request),
            },
        };
        write_frame(&mut self.pipe_write, &spawn_request)
    }

    pub(crate) fn read_spawn_ready(&mut self) -> Result<()> {
        crate::framed_io::wait_for_complete_frame(
            &self.pipe_read,
            Instant::now() + RUNNER_SPAWN_READY_TIMEOUT,
        )
        .context("wait for runner spawn_ready")?;
        let msg = read_frame(&mut self.pipe_read)?
            .ok_or_else(|| anyhow::anyhow!("runner pipe closed before spawn_ready"))?;
        match msg.message {
            Message::SpawnReady { .. } => Ok(()),
            Message::Error { payload } => Err(RunnerStartupError::new(payload).into()),
            other => Err(anyhow::anyhow!(
                "expected spawn_ready from runner, got {other:?}"
            )),
        }
    }

    pub(crate) fn into_files(self) -> (File, File) {
        (self.pipe_write, self.pipe_read)
    }
    pub(crate) fn into_files_with_output(self) -> Result<(File, File, File, File)> {
        let (stdout, stderr) = self
            .direct_output
            .context("current-user runner output pipes are missing")?;
        Ok((self.pipe_write, self.pipe_read, stdout, stderr))
    }
}

fn runner_output_pipe() -> Result<(File, File)> {
    let mut read = 0;
    let mut write = 0;
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null_mut(), 0) } == 0 {
        return Err(std::io::Error::last_os_error()).context("CreatePipe failed for runner output");
    }
    let read = unsafe { File::from_raw_handle(read as _) };
    let write = unsafe { File::from_raw_handle(write as _) };
    let write_handle = write.as_raw_handle() as _;
    if unsafe { SetHandleInformation(write_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("SetHandleInformation failed for runner output");
    }
    Ok((read, write))
}

fn try_take_completed_connect_result(
    connect_thread: &mut Option<thread::JoinHandle<()>>,
    connect_result_rx: &mpsc::Receiver<Result<()>>,
    thread_handle: HANDLE,
    pipe_label: &str,
) -> Result<Option<Result<()>>> {
    let thread_wait = unsafe { WaitForSingleObject(thread_handle, 0) };
    if thread_wait != WAIT_OBJECT_0 {
        return Ok(None);
    }

    if let Some(connect_thread) = connect_thread.take() {
        let _ = connect_thread.join();
    }

    let result = connect_result_rx.recv().map_err(|_| {
        anyhow::anyhow!("runner {pipe_label} connect thread exited before reporting its result")
    })?;
    Ok(Some(result))
}

fn connect_pipe_with_timeout(
    h_pipe: HANDLE,
    expected_runner_pid: u32,
    pipe_label: &str,
) -> Result<()> {
    let pipe_label = pipe_label.to_string();
    let pipe_label_for_thread = pipe_label.clone();
    let (thread_handle_tx, thread_handle_rx) = mpsc::sync_channel(1);
    let (connect_result_tx, connect_result_rx) = mpsc::sync_channel(1);
    let mut connect_thread = Some(
        thread::Builder::new()
            .name(format!("codex-runner-connect-{pipe_label}"))
            .spawn(move || {
                let current_process = unsafe { GetCurrentProcess() };
                let mut thread_handle = 0;
                let duplicate_ok = unsafe {
                    DuplicateHandle(
                        current_process,
                        GetCurrentThread(),
                        current_process,
                        &mut thread_handle,
                        0,
                        0,
                        DUPLICATE_SAME_ACCESS,
                    )
                };
                if duplicate_ok == 0 {
                    let _ = thread_handle_tx.send(Err(anyhow::anyhow!(
                        "DuplicateHandle failed for runner {pipe_label_for_thread} connect thread: {}",
                        unsafe { GetLastError() }
                    )));
                    return;
                }

                // Publish the helper thread HANDLE before the blocking pipe connect so the
                // parent can cancel this specific operation if it times out.
                let _ = thread_handle_tx.send(Ok(thread_handle));

                let result = connect_pipe(h_pipe, expected_runner_pid)
                    .map_err(anyhow::Error::from)
                    .context(format!("connect {pipe_label_for_thread}"));
                let _ = connect_result_tx.send(result);
            })?,
    );
    let thread_handle = thread_handle_rx.recv().map_err(|_| {
        anyhow::anyhow!("runner {pipe_label} connect thread exited before publishing its handle")
    })??;

    let result = match connect_result_rx.recv_timeout(RUNNER_PIPE_CONNECT_TIMEOUT) {
        Ok(result) => {
            if let Some(connect_thread) = connect_thread.take() {
                let _ = connect_thread.join();
            }
            result
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            if let Some(result) = try_take_completed_connect_result(
                &mut connect_thread,
                &connect_result_rx,
                thread_handle,
                &pipe_label,
            )? {
                result
            } else {
                let cancel_ok = unsafe { CancelSynchronousIo(thread_handle) };
                if cancel_ok == 0 {
                    let err = unsafe { GetLastError() };
                    if err != ERROR_NOT_FOUND {
                        Err(anyhow::anyhow!(
                            "CancelSynchronousIo failed for runner {pipe_label} connect thread: {err}"
                        ))
                    } else if let Some(result) = try_take_completed_connect_result(
                        &mut connect_thread,
                        &connect_result_rx,
                        thread_handle,
                        &pipe_label,
                    )? {
                        result
                    } else {
                        Err(anyhow::anyhow!(
                            "timed out after {}ms connecting runner {pipe_label}",
                            RUNNER_PIPE_CONNECT_TIMEOUT.as_millis()
                        ))
                    }
                } else {
                    // Do not join the helper thread on the timeout path. Parent-side cleanup will
                    // close the pipe handles, which lets the blocked connect unwind without
                    // risking another indefinite wait here.
                    Err(anyhow::anyhow!(
                        "timed out after {}ms connecting runner {pipe_label}",
                        RUNNER_PIPE_CONNECT_TIMEOUT.as_millis()
                    ))
                }
            }
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            if let Some(connect_thread) = connect_thread.take() {
                let _ = connect_thread.join();
            }
            Err(anyhow::anyhow!(
                "runner {pipe_label} connect thread exited before reporting its result"
            ))
        }
    };

    unsafe {
        CloseHandle(thread_handle);
    }

    result
}

pub(crate) fn spawn_runner_transport(
    codex_home: &Path,
    cwd: &Path,
    launch: RunnerLaunch<'_>,
    log_dir: Option<&Path>,
    mut spawn_request: SpawnRequest,
    desktop_policy: Option<&DesktopPolicy>,
) -> Result<RunnerTransport> {
    fn current_username() -> Result<String> {
        let mut len: u32 = 0;
        unsafe {
            GetUserNameExW(NameSamCompatible, ptr::null_mut(), &mut len);
        }
        let mut buffer = vec![0; len as usize];
        if unsafe { GetUserNameExW(NameSamCompatible, buffer.as_mut_ptr(), &mut len) } == 0 {
            return Err(std::io::Error::last_os_error()).context("GetUserNameExW failed");
        }
        Ok(String::from_utf16_lossy(&buffer[..len as usize]))
    }
    spawn_request.private_desktop_name = match launch {
        RunnerLaunch::CurrentUser => None,
        RunnerLaunch::Logon(sandbox_creds) => desktop_policy
            .map(|policy| shared_private_desktop_for_user(&sandbox_creds.username, policy, log_dir))
            .transpose()?,
    };
    let (pipe_in_name, pipe_out_name) = pipe_pair();
    let pipe_username = match launch {
        RunnerLaunch::CurrentUser => current_username()?,
        RunnerLaunch::Logon(sandbox_creds) => sandbox_creds.username.clone(),
    };
    let h_pipe_in = create_named_pipe(&pipe_in_name, PIPE_ACCESS_OUTBOUND, &pipe_username)?;
    let pipe_write = unsafe { File::from_raw_handle(h_pipe_in as _) };
    let h_pipe_out = create_named_pipe(&pipe_out_name, PIPE_ACCESS_INBOUND, &pipe_username)?;
    let pipe_read = unsafe { File::from_raw_handle(h_pipe_out as _) };

    let helper_exe = find_runner_exe(codex_home, log_dir)?;
    let current_user_command = match launch {
        RunnerLaunch::CurrentUser => {
            Some(current_user_runner::resolve_runner_command(&helper_exe)?)
        }
        RunnerLaunch::Logon(_) => None,
    };
    let runner_exe = current_user_command
        .as_ref()
        .map_or(helper_exe.as_path(), |command| command.executable.as_path());
    let registered_alias = match launch {
        RunnerLaunch::Logon(sandbox_creds) if registered_core_requested() => Some(
            crate::app_package::registered_runner_alias(codex_home, &sandbox_creds.username)?,
        ),
        RunnerLaunch::CurrentUser | RunnerLaunch::Logon(_) => None,
    };
    let runner_cmdline = registered_alias
        .as_deref()
        .unwrap_or(runner_exe)
        .to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| "codex-command-runner.exe".to_string());
    let internal_arg = current_user_command
        .as_ref()
        .and_then(|command| command.internal_arg)
        .map(|arg| format!(" {}", quote_windows_arg(arg)))
        .unwrap_or_default();
    let runner_full_cmd = format!(
        "{}{} {} {}",
        quote_windows_arg(&runner_cmdline),
        internal_arg,
        quote_windows_arg(&format!("--pipe-in={pipe_in_name}")),
        quote_windows_arg(&format!("--pipe-out={pipe_out_name}"))
    );
    let mut cmdline_vec = to_wide(&runner_full_cmd);
    let exe_w = to_wide(&runner_cmdline);
    let cwd_w = to_wide(cwd);
    let output_pipes = match launch {
        RunnerLaunch::CurrentUser => {
            let (stdout_read, stdout_write) = runner_output_pipe()?;
            let (stderr_read, stderr_write) = runner_output_pipe()?;
            Some((stdout_read, stdout_write, stderr_read, stderr_write))
        }
        RunnerLaunch::Logon(_) => None,
    };
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = if output_pipes.is_some() {
        std::mem::size_of::<STARTUPINFOEXW>() as u32
    } else {
        std::mem::size_of::<STARTUPINFOW>() as u32
    };
    si.StartupInfo.dwFlags = STARTF_FORCEOFFFEEDBACK;
    let mut attrs = if let Some((_, stdout, _, stderr)) = output_pipes.as_ref() {
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_FORCEOFFFEEDBACK;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = stdout.as_raw_handle() as _;
        si.StartupInfo.hStdError = stderr.as_raw_handle() as _;
        let mut attrs = ProcThreadAttributeList::new(/*attr_count*/ 1)?;
        attrs.set_handle_list(vec![si.StartupInfo.hStdOutput, si.StartupInfo.hStdError])?;
        Some(attrs)
    } else {
        None
    };
    if let Some(attrs) = attrs.as_mut() {
        si.lpAttributeList = attrs.as_mut_ptr();
    }
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let previous_error_mode = unsafe { SetErrorMode(RUNNER_ERROR_MODE_FLAGS) };
    // Execution aliases require the registered account's profile.
    // Other launches retain their existing profile-free behavior.
    let spawn_res = unsafe {
        match launch {
            RunnerLaunch::CurrentUser => current_user_runner::create_process(
                &exe_w,
                &mut cmdline_vec,
                &cwd_w,
                &si.StartupInfo,
                &mut pi,
            ),
            RunnerLaunch::Logon(sandbox_creds) => {
                let user_w = to_wide(&sandbox_creds.username);
                let domain_w = to_wide(".");
                let password_w = to_wide(&sandbox_creds.password);
                CreateProcessWithLogonW(
                    user_w.as_ptr(),
                    domain_w.as_ptr(),
                    password_w.as_ptr(),
                    if registered_alias.is_some() {
                        LOGON_WITH_PROFILE
                    } else {
                        0
                    },
                    exe_w.as_ptr(),
                    cmdline_vec.as_mut_ptr(),
                    windows_sys::Win32::System::Threading::CREATE_NO_WINDOW
                        | windows_sys::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT,
                    ptr::null(),
                    cwd_w.as_ptr(),
                    &si.StartupInfo,
                    &mut pi,
                )
            }
        }
    };
    unsafe {
        SetErrorMode(previous_error_mode);
    }
    let direct_output =
        output_pipes.map(|(stdout_read, stdout_write, stderr_read, stderr_write)| {
            drop(stdout_write);
            drop(stderr_write);
            (stdout_read, stderr_read)
        });
    if spawn_res == 0 {
        let err = unsafe { GetLastError() };
        return match launch {
            RunnerLaunch::CurrentUser => Err(std::io::Error::from_raw_os_error(err as i32))
                .context("CreateProcessW failed for current-user runner"),
            RunnerLaunch::Logon(_) => Err(RunnerLogonError { code: err }.into()),
        };
    }
    // Keep the process pinned through the entire startup handshake. Pipes close
    // automatically on every error, including failure to create the second pipe.
    let _runner_process = unsafe { OwnedHandle::from_raw_handle(pi.hProcess as _) };
    let _runner_thread = unsafe { OwnedHandle::from_raw_handle(pi.hThread as _) };
    let expected_runner_pid = pi.dwProcessId;
    let mut transport = RunnerTransport {
        pipe_write,
        pipe_read,
        direct_output,
    };
    let startup_result = (|| -> Result<()> {
        // An update can retarget the alias after setup. Check the selected image
        // and authenticate its pipes before sending it the command.
        if registered_alias.is_some() {
            verify_registered_core_runner(pi.hProcess, runner_exe)?;
        }
        connect_pipe_with_timeout(h_pipe_in, expected_runner_pid, "pipe-in")?;
        connect_pipe_with_timeout(h_pipe_out, expected_runner_pid, "pipe-out")?;
        transport.send_spawn_request(spawn_request)?;
        transport.read_spawn_ready()?;
        Ok(())
    })();
    if let Err(err) = startup_result {
        unsafe {
            let _ = TerminateProcess(pi.hProcess, 1);
        }
        return Err(err);
    }

    if registered_alias.is_some() {
        crate::logging::debug_log(
            &format!(
                "registered_core_runner_ready pid={expected_runner_pid} package_verified=true pipes_authenticated=true"
            ),
            log_dir,
        );
    }

    Ok(transport)
}

#[cfg(test)]
mod tests {
    use super::RunnerLogonError;
    use super::RunnerStartupError;
    use super::is_refreshable_sandbox_creds_error;
    use crate::ipc_framed::ErrorPayload;
    use crate::ipc_framed::ErrorStage;
    use pretty_assertions::assert_eq;
    use windows_sys::Win32::Foundation::ERROR_ACCOUNT_DISABLED;
    use windows_sys::Win32::Foundation::ERROR_LOGON_FAILURE;
    use windows_sys::Win32::Foundation::ERROR_NO_SUCH_LOGON_SESSION;
    use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;

    #[test]
    fn refreshable_sandbox_creds_error_recognizes_credential_and_child_start_failures() {
        assert_eq!(
            [
                ERROR_ACCOUNT_DISABLED,
                ERROR_LOGON_FAILURE,
                ERROR_NO_SUCH_LOGON_SESSION,
                ERROR_NOT_FOUND,
            ]
            .map(|code| {
                let err =
                    anyhow::Error::new(RunnerLogonError { code }).context("runner launch failed");
                is_refreshable_sandbox_creds_error(&err, &[])
            }),
            [true, true, true, false]
        );

        assert_eq!(
            [
                (ErrorStage::SpawnChild, ERROR_NO_SUCH_LOGON_SESSION),
                (ErrorStage::SpawnChild, ERROR_NOT_FOUND),
                (ErrorStage::ReadSpawnRequest, ERROR_NO_SUCH_LOGON_SESSION),
            ]
            .map(|(stage, windows_error_code)| {
                let err = anyhow::Error::new(RunnerStartupError::new(ErrorPayload {
                    message: "runner startup failed".to_string(),
                    stage,
                    windows_error_code: Some(windows_error_code),
                }));
                is_refreshable_sandbox_creds_error(&err, &["cmd.exe".to_string()])
            }),
            [true, false, false]
        );

        let windows_apps_commands = [
            vec![
                r"C:\Users\user\AppData\Local\Microsoft\WindowsApps\pwsh.exe".to_string(),
            ],
            vec![
                r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7.6.3.0_x64__8wekyb3d8bbwe\pwsh.exe"
                    .to_string(),
            ],
        ];
        assert_eq!(
            windows_apps_commands.map(|command| {
                let err = anyhow::Error::new(RunnerStartupError::new(ErrorPayload {
                    message: "runner startup failed".to_string(),
                    stage: ErrorStage::SpawnChild,
                    windows_error_code: Some(ERROR_NO_SUCH_LOGON_SESSION),
                }));
                is_refreshable_sandbox_creds_error(&err, &command)
            }),
            [false, false]
        );
    }
}
