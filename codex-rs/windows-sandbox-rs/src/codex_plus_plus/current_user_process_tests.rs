use super::batch_command_line;
use crate::desktop::LaunchDesktop;
use crate::ipc_framed::ChildConsoleMode;
use crate::process::ProcessExecutionMode;
use crate::process::create_process;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::IsProcessInJob;

#[test]
fn batch_arguments_keep_windows_wire_semantics() {
    let argv = ["probe", "%x%", r"C:\dir\"].map(String::from);
    assert_eq!(
        batch_command_line(Path::new(r"C:\%x%\probe.cmd"), &argv).unwrap(),
        r#"cmd.exe /e:ON /v:OFF /d /c ""C:\%%cd:~,%%x%%cd:~,%%\probe.cmd" "%%cd:~,%%x%%cd:~,%%" "C:\dir\"""#
    );
    assert!(batch_command_line(Path::new("x.cmd"), &["x".into(), "\"&calc".into()]).is_err());
}

#[test]
fn current_user_process_is_assigned_before_create_process_returns() {
    let temp = tempfile::tempdir().unwrap();
    let stdin = OpenOptions::new().read(true).open("NUL").unwrap();
    let stdout = File::create(temp.path().join("stdout")).unwrap();
    let stderr = File::create(temp.path().join("stderr")).unwrap();
    let argv = [
        std::env::var("ComSpec").unwrap(),
        "/d".into(),
        "/c".into(),
        "exit /b 0".into(),
    ];
    let created = unsafe {
        create_process(
            ProcessExecutionMode::CurrentUser,
            &argv,
            temp.path(),
            &std::env::vars().collect::<HashMap<_, _>>(),
            /*logs_base_dir*/ None,
            Some((
                stdin.as_raw_handle() as HANDLE,
                stdout.as_raw_handle() as HANDLE,
                stderr.as_raw_handle() as HANDLE,
            )),
            ChildConsoleMode::NoWindow,
            LaunchDesktop::current_user(),
        )
    }
    .unwrap();
    let mut in_job = 0;
    let query_result = unsafe {
        IsProcessInJob(
            created.process_info.hProcess,
            created.job.as_raw_handle() as HANDLE,
            &mut in_job,
        )
    };
    created.job.terminate().unwrap();
    unsafe {
        CloseHandle(created.process_info.hThread);
        CloseHandle(created.process_info.hProcess);
    }
    assert_eq!((query_result, in_job), (1, 1));
}
