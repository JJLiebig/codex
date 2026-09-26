use super::JobObject;
use super::NtResumeProcess;
use std::io;
use tokio::process::Child;
use tokio::process::Command;
use winapi::shared::ntdef::NT_SUCCESS;
use winapi::um::winbase::CREATE_SUSPENDED;

impl JobObject {
    /// Starts a contained background child without opening a console window.
    pub fn spawn_contained_no_window(&self, command: &mut Command) -> io::Result<Child> {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.prepare_suspended_spawn(command);
        command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let child = command.spawn()?;
        let process_handle = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("missing child process handle"))?;
        self.assign_process(process_handle)?;

        let status = unsafe { NtResumeProcess(process_handle.cast()) };
        if !NT_SUCCESS(status) {
            return Err(io::Error::other(format!(
                "failed to resume contained process: NTSTATUS {status:#x}"
            )));
        }

        Ok(child)
    }
}
