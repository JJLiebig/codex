//! Native Windows console input ownership for the Codex++ TUI.

use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use tokio_stream::Stream;

pub(in crate::tui) struct OwnedEventStream {
    inner: Option<crossterm::event::EventStream>,
    #[cfg(windows)]
    discarding_stale_events: bool,
}

impl Default for OwnedEventStream {
    fn default() -> Self {
        #[cfg(windows)]
        if let Err(err) = ensure_native_windows_input_mode() {
            tracing::warn!(error = %err, "failed to restore native Windows terminal input mode");
        }
        Self {
            inner: Some(crossterm::event::EventStream::new()),
            #[cfg(windows)]
            discarding_stale_events: false,
        }
    }
}

impl Stream for OwnedEventStream {
    type Item = std::io::Result<crossterm::event::Event>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        #[cfg(windows)]
        let mut stale_event_budget = MAX_RECOVERY_DISCARDS_PER_POLL;
        loop {
            let result = Pin::new(
                self.inner
                    .get_or_insert_with(crossterm::event::EventStream::new),
            )
            .poll_next(cx);
            #[cfg(windows)]
            if self.discarding_stale_events {
                match &result {
                    Poll::Ready(Some(Ok(_))) => {
                        stale_event_budget -= 1;
                        if stale_event_budget == 0 {
                            cx.waker().wake_by_ref();
                            return Poll::Pending;
                        }
                        continue;
                    }
                    Poll::Pending => {
                        // Building the replacement stream synchronized with Crossterm's global
                        // reader, so no event from the old stream can be published after this.
                        self.discarding_stale_events = false;
                        return Poll::Pending;
                    }
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                        self.discarding_stale_events = false;
                        return result;
                    }
                }
            }
            #[cfg(windows)]
            if self.recover_runtime_drift() {
                match &result {
                    Poll::Ready(Some(Ok(_))) => {
                        self.discarding_stale_events = true;
                        stale_event_budget -= 1;
                        continue;
                    }
                    Poll::Pending => {
                        self.discarding_stale_events = true;
                        continue;
                    }
                    Poll::Ready(Some(Err(_))) | Poll::Ready(None) => return result,
                }
            }
            return result;
        }
    }
}

#[cfg(windows)]
impl OwnedEventStream {
    fn recover_runtime_drift(&mut self) -> bool {
        match ensure_native_windows_input_mode() {
            Ok(true) => {
                self.inner.take();
                super::super::flush_terminal_input_buffer();
                self.inner = Some(crossterm::event::EventStream::new());
                true
            }
            Ok(false) => false,
            Err(err) => {
                tracing::warn!(error = %err, "failed to recover native Windows terminal input mode");
                false
            }
        }
    }
}

#[cfg(windows)]
use std::io::Result;
#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(windows)]
use windows_sys::Win32::Foundation::HANDLE;
#[cfg(windows)]
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
#[cfg(windows)]
use windows_sys::Win32::System::Console::ENABLE_VIRTUAL_TERMINAL_INPUT;
#[cfg(windows)]
use windows_sys::Win32::System::Console::GetConsoleMode;
#[cfg(windows)]
use windows_sys::Win32::System::Console::GetStdHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Console::STD_INPUT_HANDLE;
#[cfg(windows)]
use windows_sys::Win32::System::Console::SetConsoleMode;

#[cfg(windows)]
static ORIGINAL_VIRTUAL_TERMINAL_INPUT: OnceLock<bool> = OnceLock::new();

#[cfg(windows)]
const MAX_RECOVERY_DISCARDS_PER_POLL: usize = 32;

#[cfg(windows)]
pub(in crate::tui) fn ensure_native_windows_input_mode() -> Result<bool> {
    let Some((handle, mode)) = windows_console_input_mode()? else {
        return Ok(false);
    };
    ORIGINAL_VIRTUAL_TERMINAL_INPUT.get_or_init(|| mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0);

    let native_mode = mode & !ENABLE_VIRTUAL_TERMINAL_INPUT;
    if native_mode == mode {
        return Ok(false);
    }
    set_windows_console_input_mode(handle, native_mode)?;
    Ok(true)
}

#[cfg(windows)]
pub(in crate::tui) fn restore_native_windows_input_mode() -> Result<()> {
    let Some(originally_enabled) = ORIGINAL_VIRTUAL_TERMINAL_INPUT.get().copied() else {
        return Ok(());
    };
    let Some((handle, mode)) = windows_console_input_mode()? else {
        return Ok(());
    };

    let restored_mode = if originally_enabled {
        mode | ENABLE_VIRTUAL_TERMINAL_INPUT
    } else {
        mode & !ENABLE_VIRTUAL_TERMINAL_INPUT
    };
    if restored_mode != mode {
        set_windows_console_input_mode(handle, restored_mode)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_console_input_mode() -> Result<Option<(HANDLE, u32)>> {
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Ok(None);
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Ok(None);
    }
    Ok(Some((handle, mode)))
}

#[cfg(windows)]
fn set_windows_console_input_mode(handle: HANDLE, mode: u32) -> Result<()> {
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, windows))]
#[path = "native_windows_input_tests.rs"]
mod tests;
