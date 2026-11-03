use crate::RolloutRecorderParams;

impl RolloutRecorderParams {
    pub fn with_subagent_cwd_override(mut self, explicit: bool) -> Self {
        if let Self::Create {
            codex_plus_plus_spawn_cwd,
            ..
        } = &mut self
        {
            *codex_plus_plus_spawn_cwd = explicit;
        }
        self
    }
}
