//! OSC 7501 blocked state from the existing bottom-pane input owner.

use super::BottomPane;
use super::BottomPaneView;

impl BottomPane {
    pub(crate) fn program_status_kind(
        &self,
    ) -> Option<crate::codex_plus_plus::program_status::Kind> {
        self.active_view()
            .and_then(BottomPaneView::program_status_kind)
            .or_else(|| {
                self.questions
                    .as_ref()
                    .filter(|q| q.unanswered_count() > 0)
                    .map(|_| crate::codex_plus_plus::program_status::Kind::Question)
            })
    }
}
