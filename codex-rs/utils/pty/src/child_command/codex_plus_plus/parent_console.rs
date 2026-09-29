use super::Command;
use winapi::um::winbase::DETACHED_PROCESS;

impl Command {
    /// Prevent pipe children from opening the parent's console through CONIN$ or CONOUT$.
    pub(crate) fn detach_parent_console(&mut self) {
        self.inner.creation_flags(DETACHED_PROCESS);
    }
}
