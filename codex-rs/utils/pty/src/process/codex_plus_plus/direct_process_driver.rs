use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::super::ClosureTerminator;
use super::super::ProcessHandle;
use super::super::SpawnedProcess;

pub struct DirectProcessDriver {
    pub writer_tx: mpsc::Sender<Vec<u8>>,
    pub stdout_rx: mpsc::Receiver<Vec<u8>>,
    pub stderr_rx: mpsc::Receiver<Vec<u8>>,
    pub exit_rx: oneshot::Receiver<i32>,
    pub stdout_reader_handle: JoinHandle<()>,
    pub stderr_reader_handle: JoinHandle<()>,
    pub terminator: Option<Box<dyn FnMut() + Send + Sync>>,
    pub writer_handle: Option<JoinHandle<()>>,
}

async fn wait_for_exit_and_output(
    exit_rx: oneshot::Receiver<i32>,
    stdout_reader_handle: JoinHandle<()>,
    stderr_reader_handle: JoinHandle<()>,
) -> i32 {
    let code = exit_rx.await.unwrap_or(-1);
    let _ = stdout_reader_handle.await;
    let _ = stderr_reader_handle.await;
    code
}

pub fn spawn_from_direct_driver(driver: DirectProcessDriver) -> SpawnedProcess {
    let (exit_tx, exit_rx) = oneshot::channel();
    let exit_status = Arc::new(AtomicBool::new(false));
    let wait_exit_status = Arc::clone(&exit_status);
    let exit_code = Arc::new(StdMutex::new(None));
    let wait_exit_code = Arc::clone(&exit_code);
    let wait_handle = tokio::spawn(async move {
        let code = wait_for_exit_and_output(
            driver.exit_rx,
            driver.stdout_reader_handle,
            driver.stderr_reader_handle,
        )
        .await;
        wait_exit_status.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut guard) = wait_exit_code.lock() {
            *guard = Some(code);
        }
        let _ = exit_tx.send(code);
    });
    let handle = ProcessHandle::new(
        driver.writer_tx,
        Box::new(ClosureTerminator {
            inner: driver.terminator,
            #[cfg(windows)]
            interrupt_terminates: true,
        }),
        /*stdout_reader*/ None,
        /*stderr_reader*/ None,
        driver
            .writer_handle
            .unwrap_or_else(|| tokio::spawn(async {})),
        wait_handle,
        exit_status,
        exit_code,
        /*pty_handles*/ None,
        /*resizer*/ None,
    );
    SpawnedProcess {
        session: handle,
        stdout_rx: driver.stdout_rx,
        stderr_rx: driver.stderr_rx,
        exit_rx,
    }
}

#[cfg(test)]
#[path = "direct_process_driver_tests.rs"]
mod tests;
