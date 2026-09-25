//! Launch through Windows Management Instrumentation when the terminal's job
//! chain cannot be escaped with CREATE_BREAKAWAY_FROM_JOB.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use windows_sys::Win32::System::Console::STD_ERROR_HANDLE;
use windows_sys::Win32::System::Console::SetStdHandle;
use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
use windows_sys::Win32::System::Threading::DETACHED_PROCESS;

pub(super) const STDERR_LOG_ENV: &str = "CODEX_PLUS_PLUS_BROKERED_DAEMON_STDERR_LOG";

#[allow(non_camel_case_types)]
#[derive(Deserialize)]
struct Win32_Process;

#[allow(non_camel_case_types)]
#[derive(Serialize)]
struct Win32_ProcessStartup {
    #[serde(rename = "CreateFlags")]
    create_flags: u32,
    #[serde(rename = "EnvironmentVariables")]
    environment_variables: Vec<String>,
}

#[derive(Serialize)]
struct CreateInput {
    #[serde(rename = "CommandLine")]
    command_line: String,
    #[serde(rename = "CurrentDirectory")]
    current_directory: String,
    #[serde(rename = "ProcessStartupInformation")]
    process_startup_information: Win32_ProcessStartup,
}

#[derive(Deserialize)]
struct CreateOutput {
    #[serde(rename = "ProcessId")]
    process_id: Option<u32>,
    #[serde(rename = "ReturnValue")]
    return_value: u32,
}

pub(super) fn preflight(executable: &Path) -> Result<()> {
    let mut command = Command::new(executable);
    command.current_dir(
        executable
            .parent()
            .context("daemon executable has no parent")?,
    );
    let pid = create(&command, CREATE_SUSPENDED | DETACHED_PROCESS)?;
    let process = super::Process::open(pid)?.context("brokered launch probe exited")?;
    process.terminate()?;
    Ok(())
}

pub(super) fn launch(command: &Command) -> Result<u32> {
    create(command, DETACHED_PROCESS)
}

pub fn redirect_stderr_from_env() -> Result<()> {
    let Some(path) = std::env::var_os(STDERR_LOG_ENV) else {
        return Ok(());
    };
    let file = OpenOptions::new()
        .append(true)
        .open(&path)
        .with_context(|| {
            format!(
                "failed to open brokered daemon stderr log {}",
                Path::new(&path).display()
            )
        })?;
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle() as _) } == 0 {
        return Err(io::Error::last_os_error())
            .context("failed to redirect brokered daemon stderr");
    }
    // SetStdHandle does not take ownership; retain the file through process exit.
    std::mem::forget(file);
    // SAFETY: this runs before the CLI starts its runtime or any other threads.
    unsafe { std::env::remove_var(STDERR_LOG_ENV) };
    Ok(())
}

fn create(command: &Command, flags: u32) -> Result<u32> {
    let command_line = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| unicode(arg, "daemon argument").map(|arg| quote_windows_arg(&arg)))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let current_directory = match command.get_current_dir() {
        Some(directory) => directory.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let input = CreateInput {
        command_line,
        current_directory: unicode(current_directory.as_os_str(), "daemon directory")?,
        process_startup_information: Win32_ProcessStartup {
            create_flags: flags,
            environment_variables: command_environment(command)?,
        },
    };
    let connection = wmi::WMIConnection::new().context("failed to connect to Windows WMI")?;
    let output: CreateOutput = connection
        .exec_class_method::<Win32_Process, _>("Create", &input)
        .context("WMI Win32_Process.Create failed")?;
    anyhow::ensure!(
        output.return_value == 0,
        "WMI Win32_Process.Create returned error {}",
        output.return_value
    );
    output
        .process_id
        .filter(|pid| *pid != 0)
        .context("WMI Win32_Process.Create returned no process id")
}

fn command_environment(command: &Command) -> Result<Vec<String>> {
    let mut variables = std::env::vars_os()
        .map(|(key, value)| {
            Ok((
                unicode(&key, "environment name")?,
                unicode(&value, "environment value")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    for (key, value) in command.get_envs() {
        let key = unicode(key, "environment name")?;
        variables.retain(|(existing, _)| !existing.eq_ignore_ascii_case(&key));
        if let Some(value) = value {
            variables.push((key, unicode(value, "environment value")?));
        }
    }
    variables.sort_unstable_by(|left, right| {
        left.0
            .to_ascii_lowercase()
            .cmp(&right.0.to_ascii_lowercase())
    });
    Ok(variables
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect())
}

fn unicode(value: &OsStr, label: &str) -> Result<String> {
    value
        .to_str()
        .map(str::to_owned)
        .with_context(|| format!("{label} is not valid Unicode"))
}

fn quote_windows_arg(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return value.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
        }
        backslashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}
