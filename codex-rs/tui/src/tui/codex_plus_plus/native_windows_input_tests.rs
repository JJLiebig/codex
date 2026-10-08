use super::*;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::mpsc;
use std::task::Wake;
use std::task::Waker;
use std::thread;
use std::time::Duration;
use tokio::time::timeout;
use tokio_stream::StreamExt;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Console::FlushConsoleInputBuffer;
use windows_sys::Win32::System::Console::GetNumberOfConsoleInputEvents;
use windows_sys::Win32::System::Console::INPUT_RECORD;
use windows_sys::Win32::System::Console::INPUT_RECORD_0;
use windows_sys::Win32::System::Console::KEY_EVENT;
use windows_sys::Win32::System::Console::KEY_EVENT_RECORD;
use windows_sys::Win32::System::Console::KEY_EVENT_RECORD_0;
use windows_sys::Win32::System::Console::LEFT_CTRL_PRESSED;
use windows_sys::Win32::System::Console::ReadConsoleInputW;
use windows_sys::Win32::System::Console::WriteConsoleInputW;
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::System::Threading::GetThreadIOPendingFlag;
use windows_sys::Win32::System::Threading::OpenThread;
use windows_sys::Win32::System::Threading::THREAD_QUERY_INFORMATION;
use windows_sys::Win32::System::Threading::THREAD_TERMINATE;

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0d;
const VK_ESCAPE: u16 = 0x1b;
const VK_SPACE: u16 = 0x20;
const VK_UP: u16 = 0x26;
const VK_DOWN: u16 = 0x28;
const VK_DELETE: u16 = 0x2e;

struct RestoreConsoleMode {
    handle: HANDLE,
    mode: u32,
}

impl Drop for RestoreConsoleMode {
    fn drop(&mut self) {
        unsafe {
            SetConsoleMode(self.handle, self.mode);
            FlushConsoleInputBuffer(self.handle);
        }
    }
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn key_record(virtual_key_code: u16, ch: char) -> INPUT_RECORD {
    key_record_with_control_state(virtual_key_code, ch, 0)
}

fn key_record_with_control_state(
    virtual_key_code: u16,
    ch: char,
    control_key_state: u32,
) -> INPUT_RECORD {
    INPUT_RECORD {
        EventType: KEY_EVENT as u16,
        Event: INPUT_RECORD_0 {
            KeyEvent: KEY_EVENT_RECORD {
                bKeyDown: 1,
                wRepeatCount: 1,
                wVirtualKeyCode: virtual_key_code,
                wVirtualScanCode: 0,
                uChar: KEY_EVENT_RECORD_0 {
                    UnicodeChar: ch as u16,
                },
                dwControlKeyState: control_key_state,
            },
        },
    }
}

fn write_records(handle: HANDLE, records: &[INPUT_RECORD]) -> Result<()> {
    let mut written = 0;
    if unsafe { WriteConsoleInputW(handle, records.as_ptr(), records.len() as u32, &mut written) }
        == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    if written != records.len() as u32 {
        return Err(std::io::Error::other("partial console input write"));
    }
    Ok(())
}

fn poll_once(stream: &mut OwnedEventStream) -> Poll<Option<Result<Event>>> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut cx = Context::from_waker(&waker);
    Pin::new(stream).poll_next(&mut cx)
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial]
async fn recovers_runtime_drift_and_keeps_native_keys_working() -> Result<()> {
    let Some((handle, original_mode)) = windows_console_input_mode()? else {
        return Ok(());
    };
    let _restore = RestoreConsoleMode {
        handle,
        mode: original_mode,
    };
    unsafe { FlushConsoleInputBuffer(handle) };
    ensure_native_windows_input_mode()?;

    let mut stream = OwnedEventStream::default();
    assert!(poll_once(&mut stream).is_pending());

    set_windows_console_input_mode(handle, original_mode | ENABLE_VIRTUAL_TERMINAL_INPUT)?;
    let stale_escape = [
        key_record(VK_ESCAPE, '\u{1b}'),
        key_record(0xdb, '['),
        key_record(b'B'.into(), 'B'),
    ];
    write_records(handle, &stale_escape)?;

    let mut pending = stale_escape.len() as u32;
    for _ in 0..100 {
        unsafe { GetNumberOfConsoleInputEvents(handle, &mut pending) };
        if pending < stale_escape.len() as u32 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        pending < stale_escape.len() as u32,
        "crossterm did not buffer a stale escape fragment"
    );

    assert!(poll_once(&mut stream).is_pending());
    let (_, recovered_mode) = windows_console_input_mode()?.expect("console disappeared");
    assert_eq!(recovered_mode & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);

    let native_keys = [
        (VK_UP, '\0', KeyCode::Up),
        (VK_DOWN, '\0', KeyCode::Down),
        (VK_TAB, '\t', KeyCode::Tab),
        (VK_SPACE, ' ', KeyCode::Char(' ')),
        (VK_BACK, '\u{8}', KeyCode::Backspace),
        (VK_DELETE, '\0', KeyCode::Delete),
        (VK_RETURN, '\r', KeyCode::Enter),
        (b'1'.into(), '1', KeyCode::Char('1')),
    ];
    let records = native_keys.map(|(virtual_key_code, ch, _)| key_record(virtual_key_code, ch));
    let ctrl_c = key_record_with_control_state(b'C'.into(), '\u{3}', LEFT_CTRL_PRESSED);
    write_records(handle, &records)?;
    write_records(handle, &[ctrl_c])?;
    let expected = native_keys
        .map(|(_, _, code)| Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
        .into_iter()
        .chain([Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ))])
        .collect::<Vec<_>>();
    let actual = timeout(Duration::from_secs(2), async {
        let mut actual = Vec::with_capacity(expected.len());
        for _ in 0..expected.len() {
            actual.push(stream.next().await.expect("event stream ended")?);
        }
        Result::Ok(actual)
    })
    .await
    .expect("timed out waiting for native key events")?;
    assert_eq!(actual, expected);

    drop(stream);
    set_windows_console_input_mode(handle, original_mode | ENABLE_VIRTUAL_TERMINAL_INPUT)?;
    let resumed_stream = OwnedEventStream::default();
    let (_, resumed_mode) = windows_console_input_mode()?.expect("console disappeared");
    assert_eq!(resumed_mode & ENABLE_VIRTUAL_TERMINAL_INPUT, 0);
    drop(resumed_stream);
    Ok(())
}

#[test]
#[serial_test::serial]
fn recovery_does_not_wait_for_a_competing_reader() -> Result<()> {
    let Some((handle, original_mode)) = windows_console_input_mode()? else {
        return Ok(());
    };
    let _restore = RestoreConsoleMode {
        handle,
        mode: original_mode,
    };
    unsafe { FlushConsoleInputBuffer(handle) };
    let mut stream = OwnedEventStream::default();
    set_windows_console_input_mode(handle, original_mode | ENABLE_VIRTUAL_TERMINAL_INPUT)?;

    let (reader_ready_tx, reader_ready_rx) = mpsc::channel();
    let (reader_done_tx, reader_done_rx) = mpsc::channel();
    let competing_reader = thread::spawn(move || {
        let mut record = unsafe { std::mem::zeroed() };
        let mut read = 0;
        reader_ready_tx
            .send(unsafe { GetCurrentThreadId() })
            .unwrap();
        unsafe { ReadConsoleInputW(handle, &mut record, 1, &mut read) };
        reader_done_tx.send(()).unwrap();
    });
    let reader_thread_id = reader_ready_rx.recv().unwrap();
    let reader_thread = unsafe {
        OpenThread(
            THREAD_QUERY_INFORMATION | THREAD_TERMINATE,
            0,
            reader_thread_id,
        )
    };
    assert!(!reader_thread.is_null());
    let mut io_pending = 0;
    let read_is_pending = (0..100).any(|_| {
        assert_ne!(
            unsafe { GetThreadIOPendingFlag(reader_thread, &mut io_pending) },
            0
        );
        if io_pending != 0 {
            true
        } else {
            thread::sleep(Duration::from_millis(10));
            false
        }
    });
    let (recovery_done_tx, recovery_done_rx) = mpsc::channel();
    let recovery_poll = thread::spawn(move || recovery_done_tx.send(poll_once(&mut stream)));
    let recovery_result = recovery_done_rx.recv_timeout(Duration::from_millis(100));
    let recovery_completed_in_time = recovery_result.is_ok();

    let mut reader_completed = (0..100).any(|_| {
        unsafe { CancelSynchronousIo(reader_thread) };
        reader_done_rx
            .recv_timeout(Duration::from_millis(10))
            .is_ok()
    });
    if !reader_completed {
        write_records(handle, &[key_record(0, '\u{e001}')])?;
        reader_completed = reader_done_rx
            .recv_timeout(Duration::from_millis(100))
            .is_ok();
    }
    unsafe { CloseHandle(reader_thread) };
    assert!(reader_completed);
    competing_reader
        .join()
        .expect("competing console reader panicked");
    let recovery_result = match recovery_result {
        Ok(result) => result,
        Err(_) => {
            write_records(handle, &[key_record(0, '\u{e002}')])?;
            recovery_done_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("recovery poll did not stop after cleanup input")
        }
    };
    recovery_poll
        .join()
        .expect("recovery poll thread panicked")
        .expect("recovery poll result receiver dropped");
    assert!(read_is_pending);
    assert!(recovery_completed_in_time);
    assert!(recovery_result.is_pending());
    unsafe { FlushConsoleInputBuffer(handle) };
    Ok(())
}
