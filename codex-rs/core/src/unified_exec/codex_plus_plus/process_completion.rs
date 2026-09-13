use super::UnifiedExecProcess;

impl UnifiedExecProcess {
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
