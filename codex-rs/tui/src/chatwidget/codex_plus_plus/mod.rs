use super::ChatWidget;
use crate::exec_cell::ExecCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::McpToolCallCell;
use crate::history_cell::WebSearchCell;

mod compact_activity;
mod completion_wait;
mod usage_reset_resume;
pub(super) use usage_reset_resume::UsageResetWait;

impl ChatWidget {
    pub(super) fn compact_tool_activity_enabled(&self) -> bool {
        !self.raw_output_mode && self.compact_tool_activity_configured()
    }

    fn compact_tool_activity_configured(&self) -> bool {
        self.config.codex_plus_plus_tool_activity == codex_config::ToolActivityPresentation::Compact
    }

    pub(super) fn ensure_compact_activity_status(&mut self) {
        if self.compact_tool_activity_enabled() {
            self.bottom_pane.ensure_status_indicator();
        }
    }

    pub(super) fn compact_transient_status(&self) -> Option<String> {
        if !self.compact_tool_activity_enabled() {
            return None;
        }
        if !self.bottom_pane.status_indicator_visible() {
            return None;
        }

        self.transcript.active_cell.as_deref().and_then(|cell| {
            (cell
                .as_any()
                .downcast_ref::<ExecCell>()
                .is_some_and(|exec| {
                    exec.is_active()
                        && !exec.iter_calls().any(|call| {
                            call.is_user_shell_command() || call.is_unified_exec_interaction()
                        })
                })
                || cell
                    .as_any()
                    .downcast_ref::<McpToolCallCell>()
                    .is_some_and(McpToolCallCell::is_active)
                || cell
                    .as_any()
                    .downcast_ref::<WebSearchCell>()
                    .is_some_and(WebSearchCell::is_active)
                || compact_activity::compact_success_is_transcript_only(cell))
            .then(|| transient_status(cell))
            .flatten()
        })
    }
}

fn transient_status(cell: &dyn HistoryCell) -> Option<String> {
    let lines = cell.display_lines(u16::MAX);
    let line = lines.iter().find(|line| !line.spans.is_empty())?;
    let skip = usize::from(line.spans.get(1).is_some_and(|span| span.content == " ")) * 2;
    let text = line
        .spans
        .iter()
        .skip(skip)
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}
