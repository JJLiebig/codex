use super::UnifiedExecProcess;

impl UnifiedExecProcess {
    pub(in crate::unified_exec) async fn completion_output(
        &self,
        session_id: i32,
        exit_code: Option<i32>,
    ) -> crate::context::codex_plus_plus::BackgroundProcessExit {
        // Reuse the stream-drain grace: inherited pipes must not delay an exit indefinitely.
        let closed = self.output.output_closed_notify.notified();
        tokio::pin!(closed);
        closed.as_mut().enable();
        if !self
            .output
            .output_closed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let _ =
                tokio::time::timeout(super::super::async_watcher::TRAILING_OUTPUT_GRACE, closed)
                    .await;
        }
        let output = self.output.output_buffer.lock().await;
        let bytes = output.to_bytes();
        let start = bytes.len().saturating_sub(2048);
        crate::context::codex_plus_plus::BackgroundProcessExit {
            session_id,
            exit_code,
            output_tail: String::from_utf8_lossy(&bytes[start..]).into_owned(),
            truncated: output.total_bytes() > bytes.len() - start,
        }
    }

    pub(in crate::unified_exec) fn completion(&self) -> Option<Option<i32>> {
        let state = self.state_rx.borrow();
        state.has_exited.then_some(state.exit_code)
    }

    pub(in crate::unified_exec) fn wait_for_completion(
        &self,
    ) -> impl std::future::Future<Output = ()> + Send + 'static + use<> {
        let mut state = self.state_rx.clone();
        let cancellation = self.output.cancellation_token.clone();
        async move {
            tokio::select! {
                _ = state.wait_for(|state| state.has_exited) => {}
                _ = cancellation.cancelled() => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "process_completion_tests.rs"]
mod tests;
