use super::UnifiedExecProcess;

impl UnifiedExecProcess {
    pub(in crate::unified_exec) fn completion(&self) -> Option<Option<i32>> {
        let state = self.state_rx.borrow();
        state.has_exited.then_some(state.exit_code)
    }

    pub(in crate::unified_exec) async fn wait_for_completion(&self) {
        let mut state = self.state_rx.clone();
        let _ = state.wait_for(|state| state.has_exited).await;
    }
}

#[cfg(test)]
#[path = "process_completion_tests.rs"]
mod tests;
